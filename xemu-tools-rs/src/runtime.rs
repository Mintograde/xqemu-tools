use crate::config::Config;
use chrono::Local;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

pub type SharedRuntime = Arc<RuntimeState>;

const MAX_LOG_LINES: usize = 200;

#[derive(Debug)]
pub struct RuntimeState {
    status: RwLock<AppStatus>,
    pub controls: RuntimeControls,
    config: Config,
    started_at: Instant,
}

impl RuntimeState {
    pub fn new(config: Config) -> SharedRuntime {
        Arc::new(Self {
            controls: RuntimeControls::from_config(&config),
            status: RwLock::new(AppStatus::default()),
            config,
            started_at: Instant::now(),
        })
    }

    pub fn snapshot(&self) -> RuntimeSnapshot {
        RuntimeSnapshot {
            status: self.status.read().unwrap().clone(),
            controls: self.controls.snapshot(),
            config: self.config.clone(),
            uptime: self.started_at.elapsed(),
        }
    }

    pub fn update(&self, apply: impl FnOnce(&mut AppStatus)) {
        apply(&mut self.status.write().unwrap());
    }

    pub fn log(&self, source: impl Into<String>, message: impl Into<String>) {
        let mut status = self.status.write().unwrap();
        status.logs.push(LogLine {
            when: Local::now().format("%H:%M:%S").to_string(),
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

    pub fn toggle_save_replays(&self) -> bool {
        toggle(&self.save_replays)
    }

    pub fn toggle_save_all_ticks(&self) -> bool {
        toggle(&self.save_all_ticks)
    }

    pub fn toggle_replay_uploads(&self) -> bool {
        toggle(&self.replay_uploads)
    }
}

fn toggle(value: &AtomicBool) -> bool {
    let new_value = !value.load(Ordering::Relaxed);
    value.store(new_value, Ordering::Relaxed);
    new_value
}

#[derive(Clone, Debug)]
pub struct RuntimeSnapshot {
    pub status: AppStatus,
    pub controls: ControlSnapshot,
    pub config: Config,
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
}

#[derive(Clone, Debug)]
pub enum RelayCommand {
    ReconnectNow,
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

#[derive(Clone, Debug)]
pub struct AppStatus {
    pub xemu: XemuStatus,
    pub qmp: QmpStatus,
    pub local_ws: LocalWsStatus,
    pub relay: RelayStatus,
    pub replay: ReplayStatus,
    pub main: MainLoopStatus,
    pub logs: Vec<LogLine>,
}

impl Default for AppStatus {
    fn default() -> Self {
        Self {
            xemu: XemuStatus::default(),
            qmp: QmpStatus::default(),
            local_ws: LocalWsStatus::default(),
            relay: RelayStatus::default(),
            replay: ReplayStatus::default(),
            main: MainLoopStatus::default(),
            logs: Vec::new(),
        }
    }
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
    pub when: String,
    pub source: String,
    pub message: String,
}
