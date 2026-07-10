use crate::config::{Config, ConfigKey};
use anyhow::Result;
use chrono::Local;
use serde_json::Value;
use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

pub type SharedRuntime = Arc<RuntimeState>;

const MAX_LOG_LINES: usize = 200;
const MAX_COMMAND_RECORDS: usize = 32;

#[derive(Debug)]
pub struct RuntimeState {
    status: RwLock<AppStatus>,
    commands: RwLock<VecDeque<CommandRecord>>,
    pipeline: PipelineMetrics,
    next_command_id: AtomicU64,
    next_log_sequence: AtomicU64,
    shutdown_requested: AtomicBool,
    discard_replay_requested: AtomicBool,
    pub controls: RuntimeControls,
    config: RwLock<Config>,
    active_config: RwLock<Config>,
    client_disconnect_requests: Mutex<HashSet<String>>,
    latest_game_info: RwLock<Option<Arc<Value>>>,
    started_at: Instant,
}

impl RuntimeState {
    pub fn new(config: Config) -> SharedRuntime {
        let started_at = Instant::now();
        Arc::new(Self {
            controls: RuntimeControls::from_config(&config),
            status: RwLock::new(AppStatus::default()),
            commands: RwLock::new(VecDeque::new()),
            pipeline: PipelineMetrics::new(started_at),
            next_command_id: AtomicU64::new(1),
            next_log_sequence: AtomicU64::new(1),
            shutdown_requested: AtomicBool::new(false),
            discard_replay_requested: AtomicBool::new(false),
            active_config: RwLock::new(config.clone()),
            config: RwLock::new(config),
            client_disconnect_requests: Mutex::new(HashSet::new()),
            latest_game_info: RwLock::new(None),
            started_at,
        })
    }

    pub fn snapshot(&self) -> RuntimeSnapshot {
        RuntimeSnapshot {
            status: self.status.read().unwrap().clone(),
            controls: self.controls.snapshot(),
            config: self.config.read().unwrap().clone(),
            active_config: self.active_config.read().unwrap().clone(),
            commands: self.commands.read().unwrap().iter().cloned().collect(),
            pipeline: self.pipeline.snapshot(),
            latest_game_info: self.latest_game_info.read().unwrap().clone(),
            uptime: self.started_at.elapsed(),
        }
    }

    pub fn update(&self, apply: impl FnOnce(&mut AppStatus)) {
        apply(&mut self.status.write().unwrap());
    }

    pub fn log(&self, source: impl Into<String>, message: impl Into<String>) {
        let source = source.into();
        let message = message.into();
        let level = infer_log_level(&message);
        self.log_with_level(level, source, message);
    }

    pub fn log_with_level(
        &self,
        level: LogLevel,
        source: impl Into<String>,
        message: impl Into<String>,
    ) {
        let mut status = self.status.write().unwrap();
        status.logs.push(LogLine {
            sequence: self.next_log_sequence.fetch_add(1, Ordering::Relaxed),
            when: Local::now().format("%H:%M:%S").to_string(),
            at: Instant::now(),
            level,
            source: source.into(),
            message: message.into(),
        });
        let overflow = status.logs.len().saturating_sub(MAX_LOG_LINES);
        if overflow > 0 {
            status.logs.drain(0..overflow);
        }
    }

    pub fn clear_logs(&self) {
        self.status.write().unwrap().logs.clear();
    }

    pub fn request_shutdown(&self) {
        self.shutdown_requested.store(true, Ordering::Release);
    }

    pub fn shutdown_requested(&self) -> bool {
        self.shutdown_requested.load(Ordering::Acquire)
    }

    pub fn request_replay_discard(&self) {
        self.discard_replay_requested.store(true, Ordering::Release);
    }

    pub fn take_replay_discard_request(&self) -> bool {
        self.discard_replay_requested.swap(false, Ordering::AcqRel)
    }

    pub fn update_config_value(&self, key: ConfigKey, value: &str) -> Result<(String, bool)> {
        let mut config = self.config.read().unwrap().clone();
        config.apply_value(key, value)?;
        let path = config.save()?;
        if !key.requires_restart() {
            self.controls.apply_config(&config);
            self.active_config
                .write()
                .unwrap()
                .apply_value(key, &config.value(key))?;
        }
        *self.config.write().unwrap() = config;
        Ok((path.display().to_string(), key.requires_restart()))
    }

    pub fn reload_config(&self) -> Result<Vec<ConfigKey>> {
        let loaded = Config::load()?;
        self.controls.apply_config(&loaded);
        {
            let mut active = self.active_config.write().unwrap();
            for key in ConfigKey::ALL {
                if !key.requires_restart() {
                    active.apply_value(key, &loaded.value(key))?;
                }
            }
        }
        *self.config.write().unwrap() = loaded;
        Ok(self.pending_restart_keys())
    }

    pub fn pending_restart_keys(&self) -> Vec<ConfigKey> {
        let config = self.config.read().unwrap();
        let active = self.active_config.read().unwrap();
        ConfigKey::ALL
            .into_iter()
            .filter(|key| config.value(*key) != active.value(*key))
            .collect()
    }

    pub fn request_client_disconnect(&self, address: String) {
        self.client_disconnect_requests
            .lock()
            .unwrap()
            .insert(address);
    }

    pub fn take_client_disconnect_request(&self, address: &str) -> bool {
        self.client_disconnect_requests
            .lock()
            .unwrap()
            .remove(address)
    }

    pub fn set_latest_game_info(&self, value: Arc<Value>) {
        *self.latest_game_info.write().unwrap() = Some(value);
    }

    pub fn queue_command(&self, command: AppCommand) -> CommandRequest {
        let id = self.next_command_id.fetch_add(1, Ordering::Relaxed);
        let now = Instant::now();
        let mut commands = self.commands.write().unwrap();
        commands.push_back(CommandRecord {
            id,
            command: command.clone(),
            phase: CommandPhase::Queued,
            detail: "waiting for the main loop".to_string(),
            requested_at: now,
            updated_at: now,
        });
        while commands.len() > MAX_COMMAND_RECORDS {
            commands.pop_front();
        }
        CommandRequest { id, command }
    }

    pub fn start_command(&self, id: u64) {
        self.update_command(id, CommandPhase::Running, "in progress");
    }

    pub fn finish_command(&self, id: u64, detail: impl Into<String>) {
        self.update_command(id, CommandPhase::Succeeded, detail);
    }

    pub fn fail_command(&self, id: u64, detail: impl Into<String>) {
        self.update_command(id, CommandPhase::Failed, detail);
    }

    fn update_command(&self, id: u64, phase: CommandPhase, detail: impl Into<String>) {
        if let Some(command) = self
            .commands
            .write()
            .unwrap()
            .iter_mut()
            .find(|command| command.id == id)
        {
            command.phase = phase;
            command.detail = detail.into();
            command.updated_at = Instant::now();
        }
    }

    pub fn record_pipeline_enqueue(&self, edge: PipelineEdge, queue_depth: usize, accepted: bool) {
        self.pipeline
            .edge(edge)
            .record_enqueue(queue_depth, accepted);
    }

    pub fn record_pipeline_dequeue(&self, edge: PipelineEdge, queue_depth: usize) {
        self.pipeline.edge(edge).record_dequeue(queue_depth);
    }

    pub fn record_pipeline_drop(&self, edge: PipelineEdge, count: u64) {
        self.pipeline
            .edge(edge)
            .dropped
            .fetch_add(count, Ordering::Relaxed);
    }

    pub fn record_pipeline_bytes(&self, edge: PipelineEdge, bytes: u64) {
        self.pipeline
            .edge(edge)
            .processed_bytes
            .fetch_add(bytes, Ordering::Relaxed);
    }
}

#[derive(Debug)]
pub struct RuntimeControls {
    save_replays: AtomicBool,
    save_all_ticks: AtomicBool,
    replay_uploads: AtomicBool,
}

impl RuntimeControls {
    fn from_config(config: &Config) -> Self {
        Self {
            save_replays: AtomicBool::new(config.save_replays),
            save_all_ticks: AtomicBool::new(config.save_all_ticks),
            replay_uploads: AtomicBool::new(config.replay_uploads_enabled),
        }
    }

    pub fn snapshot(&self) -> ControlSnapshot {
        ControlSnapshot {
            save_replays: self.save_replays(),
            save_all_ticks: self.save_all_ticks(),
            replay_uploads: self.replay_uploads(),
        }
    }

    pub fn save_replays(&self) -> bool {
        self.save_replays.load(Ordering::Relaxed)
    }

    pub fn save_all_ticks(&self) -> bool {
        self.save_all_ticks.load(Ordering::Relaxed)
    }

    pub fn replay_uploads(&self) -> bool {
        self.replay_uploads.load(Ordering::Relaxed)
    }

    fn apply_config(&self, config: &Config) {
        self.save_replays
            .store(config.save_replays, Ordering::Relaxed);
        self.save_all_ticks
            .store(config.save_all_ticks, Ordering::Relaxed);
        self.replay_uploads
            .store(config.replay_uploads_enabled, Ordering::Relaxed);
    }
}

#[derive(Clone, Debug)]
pub struct RuntimeSnapshot {
    pub status: AppStatus,
    pub controls: ControlSnapshot,
    pub config: Config,
    pub active_config: Config,
    pub commands: Vec<CommandRecord>,
    pub pipeline: PipelineSnapshot,
    pub latest_game_info: Option<Arc<Value>>,
    pub uptime: Duration,
}

#[derive(Clone, Copy, Debug)]
pub struct ControlSnapshot {
    pub save_replays: bool,
    pub save_all_ticks: bool,
    pub replay_uploads: bool,
}

#[derive(Clone, Debug)]
pub enum AppCommand {
    Shutdown,
    ReconnectRelay,
    ReconnectXemu,
    ReconnectQmp,
    ToggleReplaySaving,
    ToggleSaveAllTicks,
    ToggleReplayUploads,
    ClearLogs,
    SetConfigValue { key: ConfigKey, value: String },
    ReloadConfig,
    RetryUpload(String),
    CancelUpload(String),
    DisconnectClient(String),
    DiscardReplay,
}

impl AppCommand {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Shutdown => "shutdown",
            Self::ReconnectRelay => "reconnect relay",
            Self::ReconnectXemu => "reconnect xemu",
            Self::ReconnectQmp => "reconnect QMP",
            Self::ToggleReplaySaving => "toggle replay saving",
            Self::ToggleSaveAllTicks => "toggle tick saving",
            Self::ToggleReplayUploads => "toggle replay uploads",
            Self::ClearLogs => "clear logs",
            Self::SetConfigValue { .. } => "update configuration",
            Self::ReloadConfig => "reload configuration",
            Self::RetryUpload(_) => "retry upload",
            Self::CancelUpload(_) => "cancel upload",
            Self::DisconnectClient(_) => "disconnect client",
            Self::DiscardReplay => "discard replay",
        }
    }
}

#[derive(Clone, Debug)]
pub struct CommandRequest {
    pub id: u64,
    pub command: AppCommand,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandPhase {
    Queued,
    Running,
    Succeeded,
    Failed,
}

#[derive(Clone, Debug)]
pub struct CommandRecord {
    pub id: u64,
    pub command: AppCommand,
    pub phase: CommandPhase,
    pub detail: String,
    pub requested_at: Instant,
    pub updated_at: Instant,
}

#[derive(Clone, Debug)]
pub enum RelayCommand {
    ReconnectNow,
    RetryUpload(String),
    CancelUpload(String),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Health {
    #[default]
    Unknown,
    Starting,
    Running,
    Connected,
    Disconnected,
    Disabled,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PipelineEdge {
    Replay,
    LocalWebSocket,
    Relay,
}

impl PipelineEdge {
    pub const ALL: [Self; 3] = [Self::Replay, Self::LocalWebSocket, Self::Relay];

    pub fn label(self) -> &'static str {
        match self {
            Self::Replay => "replay",
            Self::LocalWebSocket => "local ws",
            Self::Relay => "relay",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Replay => 0,
            Self::LocalWebSocket => 1,
            Self::Relay => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PipelineEdgeSnapshot {
    pub enqueued: u64,
    pub dequeued: u64,
    pub send_failures: u64,
    pub dropped: u64,
    pub processed_bytes: u64,
    pub queue_depth: usize,
    pub high_water: usize,
    pub last_enqueue_age: Option<Duration>,
    pub last_dequeue_age: Option<Duration>,
}

#[derive(Clone, Debug, Default)]
pub struct PipelineSnapshot {
    edges: [PipelineEdgeSnapshot; 3],
}

impl PipelineSnapshot {
    pub fn edge(&self, edge: PipelineEdge) -> PipelineEdgeSnapshot {
        self.edges[edge.index()]
    }
}

#[derive(Debug)]
struct PipelineMetrics {
    started_at: Instant,
    edges: [PipelineEdgeMetrics; 3],
}

impl PipelineMetrics {
    fn new(started_at: Instant) -> Self {
        Self {
            started_at,
            edges: std::array::from_fn(|_| PipelineEdgeMetrics::new(started_at)),
        }
    }

    fn edge(&self, edge: PipelineEdge) -> &PipelineEdgeMetrics {
        &self.edges[edge.index()]
    }

    fn snapshot(&self) -> PipelineSnapshot {
        PipelineSnapshot {
            edges: std::array::from_fn(|index| self.edges[index].snapshot(self.started_at)),
        }
    }
}

#[derive(Debug)]
struct PipelineEdgeMetrics {
    started_at: Instant,
    enqueued: AtomicU64,
    dequeued: AtomicU64,
    send_failures: AtomicU64,
    dropped: AtomicU64,
    processed_bytes: AtomicU64,
    queue_depth: AtomicUsize,
    high_water: AtomicUsize,
    last_enqueue_ms: AtomicU64,
    last_dequeue_ms: AtomicU64,
}

impl PipelineEdgeMetrics {
    fn new(started_at: Instant) -> Self {
        Self {
            started_at,
            enqueued: AtomicU64::new(0),
            dequeued: AtomicU64::new(0),
            send_failures: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            processed_bytes: AtomicU64::new(0),
            queue_depth: AtomicUsize::new(0),
            high_water: AtomicUsize::new(0),
            last_enqueue_ms: AtomicU64::new(0),
            last_dequeue_ms: AtomicU64::new(0),
        }
    }

    fn record_enqueue(&self, queue_depth: usize, accepted: bool) {
        if accepted {
            self.enqueued.fetch_add(1, Ordering::Relaxed);
            self.last_enqueue_ms
                .store(self.elapsed_millis(), Ordering::Relaxed);
        } else {
            self.send_failures.fetch_add(1, Ordering::Relaxed);
        }
        self.record_depth(queue_depth);
    }

    fn record_dequeue(&self, queue_depth: usize) {
        self.dequeued.fetch_add(1, Ordering::Relaxed);
        self.last_dequeue_ms
            .store(self.elapsed_millis(), Ordering::Relaxed);
        self.record_depth(queue_depth);
    }

    fn record_depth(&self, queue_depth: usize) {
        self.queue_depth.store(queue_depth, Ordering::Relaxed);
        let mut high_water = self.high_water.load(Ordering::Relaxed);
        while queue_depth > high_water {
            match self.high_water.compare_exchange_weak(
                high_water,
                queue_depth,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(current) => high_water = current,
            }
        }
    }

    fn elapsed_millis(&self) -> u64 {
        (self.started_at.elapsed().as_millis().min(u64::MAX as u128) as u64).saturating_add(1)
    }

    fn snapshot(&self, started_at: Instant) -> PipelineEdgeSnapshot {
        let now = started_at.elapsed();
        PipelineEdgeSnapshot {
            enqueued: self.enqueued.load(Ordering::Relaxed),
            dequeued: self.dequeued.load(Ordering::Relaxed),
            send_failures: self.send_failures.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
            processed_bytes: self.processed_bytes.load(Ordering::Relaxed),
            queue_depth: self.queue_depth.load(Ordering::Relaxed),
            high_water: self.high_water.load(Ordering::Relaxed),
            last_enqueue_age: activity_age(now, self.last_enqueue_ms.load(Ordering::Relaxed)),
            last_dequeue_age: activity_age(now, self.last_dequeue_ms.load(Ordering::Relaxed)),
        }
    }
}

fn activity_age(now: Duration, encoded_millis: u64) -> Option<Duration> {
    if encoded_millis == 0 {
        None
    } else {
        Some(now.saturating_sub(Duration::from_millis(encoded_millis - 1)))
    }
}

#[derive(Clone, Debug, Default)]
pub struct AppStatus {
    pub xemu: XemuStatus,
    pub qmp: QmpStatus,
    pub local_ws: LocalWsStatus,
    pub relay: RelayStatus,
    pub replay: ReplayStatus,
    pub main: MainLoopStatus,
    pub game: GameDetailStatus,
    pub logs: Vec<LogLine>,
}

#[derive(Clone, Debug)]
pub struct XemuStatus {
    pub health: Health,
    pub pid: Option<u32>,
    pub detail: String,
    pub last_error: Option<String>,
    pub last_changed: Option<Instant>,
}

impl Default for XemuStatus {
    fn default() -> Self {
        Self {
            health: Health::Unknown,
            pid: None,
            detail: "not initialized".to_string(),
            last_error: None,
            last_changed: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct QmpStatus {
    pub health: Health,
    pub endpoint: String,
    pub detail: String,
    pub last_error: Option<String>,
    pub reconnects: u64,
    pub last_changed: Option<Instant>,
}

impl Default for QmpStatus {
    fn default() -> Self {
        Self {
            health: Health::Unknown,
            endpoint: String::new(),
            detail: "not initialized".to_string(),
            last_error: None,
            reconnects: 0,
            last_changed: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct LocalWsStatus {
    pub health: Health,
    pub bind_addr: String,
    pub client_count: usize,
    pub messages_sent: u64,
    pub clients: Vec<LocalWsClientStatus>,
    pub last_error: Option<String>,
    pub last_changed: Option<Instant>,
}

impl Default for LocalWsStatus {
    fn default() -> Self {
        Self {
            health: Health::Unknown,
            bind_addr: String::new(),
            client_count: 0,
            messages_sent: 0,
            clients: Vec::new(),
            last_error: None,
            last_changed: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct RelayStatus {
    pub health: Health,
    pub uri: String,
    pub attempts: u64,
    pub reconnects: u64,
    pub messages_sent: u64,
    pub live_status_sent: u64,
    pub compressed_ticks_sent: u64,
    pub pending_uploads: usize,
    pub dropped_stale_messages: u64,
    pub messages_received: u64,
    pub producer_key_present: bool,
    pub producer_key_expires_at: String,
    pub require_key: bool,
    pub reconnect_backoff_secs: u64,
    pub next_reconnect_at: Option<Instant>,
    pub last_received_at: Option<Instant>,
    pub uploads: Vec<ReplayUploadStatus>,
    pub last_error: Option<String>,
    pub last_changed: Option<Instant>,
}

impl Default for RelayStatus {
    fn default() -> Self {
        Self {
            health: Health::Unknown,
            uri: String::new(),
            attempts: 0,
            reconnects: 0,
            messages_sent: 0,
            live_status_sent: 0,
            compressed_ticks_sent: 0,
            pending_uploads: 0,
            dropped_stale_messages: 0,
            messages_received: 0,
            producer_key_present: false,
            producer_key_expires_at: String::new(),
            require_key: false,
            reconnect_backoff_secs: 0,
            next_reconnect_at: None,
            last_received_at: None,
            uploads: Vec::new(),
            last_error: None,
            last_changed: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ReplayStatus {
    pub health: Health,
    pub recording: bool,
    pub current_game_id: String,
    pub ticks_recorded: u64,
    pub ticks_buffered: usize,
    pub queue_depth: usize,
    pub saved_replays: u64,
    pub upload_requests: u64,
    pub last_saved_file: String,
    pub last_save_bytes: u64,
    pub started_at: Option<Instant>,
    pub spool_path: String,
    pub spool_bytes: u64,
    pub last_save_duration: Duration,
    pub last_uncompressed_bytes: u64,
    pub recent_files: Vec<ReplayFileStatus>,
    pub last_error: Option<String>,
    pub last_changed: Option<Instant>,
}

impl Default for ReplayStatus {
    fn default() -> Self {
        Self {
            health: Health::Unknown,
            recording: false,
            current_game_id: String::new(),
            ticks_recorded: 0,
            ticks_buffered: 0,
            queue_depth: 0,
            saved_replays: 0,
            upload_requests: 0,
            last_saved_file: String::new(),
            last_save_bytes: 0,
            started_at: None,
            spool_path: String::new(),
            spool_bytes: 0,
            last_save_duration: Duration::ZERO,
            last_uncompressed_bytes: 0,
            recent_files: Vec::new(),
            last_error: None,
            last_changed: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct MainLoopStatus {
    pub health: Health,
    pub game_time: i64,
    pub loop_count: u64,
    pub tick_count: u64,
    pub loops_per_tick: f64,
    pub dropped_ticks_total: i64,
    pub game_info_ms: f64,
    pub loop_ms: f64,
    pub post_steps_ms: f64,
    pub memory_mbytes: f64,
    pub app_cpu_percent: f64,
    pub app_cpu_cores: f64,
    pub app_working_set_mbytes: f64,
    pub app_private_mbytes: f64,
    pub app_pagefile_mbytes: f64,
    pub read_count: u64,
    pub game_id: String,
    pub map_name: String,
    pub game_status: String,
    pub player_count: usize,
    pub event_count: usize,
    pub game_time_host_address: Option<u64>,
    pub last_error: Option<String>,
    pub last_tick_at: Option<Instant>,
}

#[derive(Clone, Debug, Default)]
pub struct GameDetailStatus {
    pub game_type: String,
    pub variant: String,
    pub stage: String,
    pub has_teams: bool,
    pub local_player_count: usize,
    pub object_count: usize,
    pub item_count: usize,
    pub spawn_count: usize,
    pub players: Vec<PlayerStatus>,
    pub recent_events: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct PlayerStatus {
    pub index: i64,
    pub name: String,
    pub team: i64,
    pub score: i64,
    pub kills: i64,
    pub deaths: i64,
    pub assists: i64,
    pub shots_fired: i64,
    pub shots_hit: i64,
    pub quit: bool,
    pub has_camo: bool,
    pub has_overshield: bool,
    pub health: f64,
    pub shields: f64,
    pub position: [f64; 3],
}

#[derive(Clone, Debug)]
pub struct LocalWsClientStatus {
    pub address: String,
    pub connected_at: Instant,
    pub last_sent_at: Option<Instant>,
    pub messages_sent: u64,
    pub bytes_sent: u64,
    pub lagged_messages: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UploadPhase {
    WaitingForUrl,
    Uploading,
    Retrying,
    Uploaded,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug)]
pub struct ReplayUploadStatus {
    pub request_id: String,
    pub file_name: String,
    pub size_bytes: u64,
    pub attempts: u8,
    pub phase: UploadPhase,
    pub detail: String,
    pub updated_at: Instant,
}

#[derive(Clone, Debug)]
pub struct ReplayFileStatus {
    pub path: String,
    pub bytes: u64,
    pub ticks: u64,
    pub saved_at: Instant,
    pub duration: Duration,
}

impl Default for MainLoopStatus {
    fn default() -> Self {
        Self {
            health: Health::Unknown,
            game_time: -1,
            loop_count: 0,
            tick_count: 0,
            loops_per_tick: 0.0,
            dropped_ticks_total: 0,
            game_info_ms: 0.0,
            loop_ms: 0.0,
            post_steps_ms: 0.0,
            memory_mbytes: 0.0,
            app_cpu_percent: 0.0,
            app_cpu_cores: 0.0,
            app_working_set_mbytes: 0.0,
            app_private_mbytes: 0.0,
            app_pagefile_mbytes: 0.0,
            read_count: 0,
            game_id: String::new(),
            map_name: String::new(),
            game_status: String::new(),
            player_count: 0,
            event_count: 0,
            game_time_host_address: None,
            last_error: None,
            last_tick_at: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct LogLine {
    pub sequence: u64,
    pub when: String,
    pub at: Instant,
    pub level: LogLevel,
    pub source: String,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogLevel {
    Info,
    Warning,
    Error,
}

fn infer_log_level(message: &str) -> LogLevel {
    let message = message.to_ascii_lowercase();
    if message.contains("failed") || message.contains("error") || message.contains("giving up") {
        LogLevel::Error
    } else if message.contains("dropped")
        || message.contains("missed")
        || message.contains("lagged")
        || message.contains("retry")
    {
        LogLevel::Warning
    } else {
        LogLevel::Info
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn pipeline_metrics_track_flow_without_status_locking() {
        let runtime = RuntimeState::new(Config::default());
        runtime.record_pipeline_enqueue(PipelineEdge::Replay, 2, true);
        runtime.record_pipeline_enqueue(PipelineEdge::Replay, 2, false);
        runtime.record_pipeline_dequeue(PipelineEdge::Replay, 1);
        runtime.record_pipeline_drop(PipelineEdge::Replay, 3);
        runtime.record_pipeline_bytes(PipelineEdge::Replay, 4096);

        let edge = runtime.snapshot().pipeline.edge(PipelineEdge::Replay);
        assert_eq!(edge.enqueued, 1);
        assert_eq!(edge.dequeued, 1);
        assert_eq!(edge.send_failures, 1);
        assert_eq!(edge.dropped, 3);
        assert_eq!(edge.processed_bytes, 4096);
        assert_eq!(edge.queue_depth, 1);
        assert_eq!(edge.high_water, 2);
        assert!(edge.last_enqueue_age.is_some());
        assert!(edge.last_dequeue_age.is_some());
    }

    #[test]
    fn command_history_records_lifecycle() {
        let runtime = RuntimeState::new(Config::default());
        let request = runtime.queue_command(AppCommand::ReconnectRelay);
        runtime.start_command(request.id);
        runtime.finish_command(request.id, "requested");

        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.commands.len(), 1);
        assert_eq!(snapshot.commands[0].phase, CommandPhase::Succeeded);
        assert_eq!(snapshot.commands[0].detail, "requested");
    }

    #[test]
    fn config_updates_distinguish_hot_and_restart_required_values() -> Result<()> {
        let unique = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let dir = std::env::temp_dir().join(format!("xemu-tools-runtime-config-{unique}"));
        std::fs::create_dir_all(&dir)?;
        let config = Config {
            source_path: Some(dir.join("config.toml")),
            ..Config::default()
        };
        let runtime = RuntimeState::new(config);

        runtime.update_config_value(ConfigKey::SaveReplays, "false")?;
        runtime.update_config_value(ConfigKey::QmpPort, "4455")?;
        let snapshot = runtime.snapshot();
        assert!(!snapshot.controls.save_replays);
        assert!(!snapshot.config.save_replays);
        assert!(!snapshot.active_config.save_replays);
        assert_eq!(snapshot.config.qmp_port, 4455);
        assert_ne!(snapshot.active_config.qmp_port, 4455);
        assert_eq!(runtime.pending_restart_keys(), vec![ConfigKey::QmpPort]);

        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn client_disconnect_requests_are_one_shot() {
        let runtime = RuntimeState::new(Config::default());
        runtime.request_client_disconnect("127.0.0.1:9001".to_string());
        assert!(runtime.take_client_disconnect_request("127.0.0.1:9001"));
        assert!(!runtime.take_client_disconnect_request("127.0.0.1:9001"));
    }
}
