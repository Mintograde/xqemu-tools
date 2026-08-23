use crate::config::ConfigKey;
use crate::runtime::{AppCommand, LogLevel, LogLine, PipelineEdge, RuntimeSnapshot, UpdatePhase};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::VecDeque;
use std::time::Instant;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum View {
    Overview,
    Pipeline,
    Game,
    Connections,
    Replay,
    Metrics,
    Logs,
    Settings,
}

impl View {
    pub const ALL: [Self; 8] = [
        Self::Overview,
        Self::Pipeline,
        Self::Game,
        Self::Connections,
        Self::Replay,
        Self::Metrics,
        Self::Logs,
        Self::Settings,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Pipeline => "Pipeline",
            Self::Game => "Game",
            Self::Connections => "Connections",
            Self::Replay => "Replay",
            Self::Metrics => "Metrics",
            Self::Logs => "Logs",
            Self::Settings => "Settings",
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|view| *view == self).unwrap_or(0)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Modal {
    Help,
    Quit,
    DiscardReplay,
    InstallUpdate,
}

#[derive(Clone, Debug)]
pub enum UiAction {
    None,
    Command(AppCommand),
    Shutdown,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct EdgeRate {
    pub enqueued: f64,
    pub dequeued: f64,
}

#[derive(Debug)]
struct PipelineSample {
    sampled_at: Instant,
    enqueued: [u64; 3],
    dequeued: [u64; 3],
}

#[derive(Debug)]
pub struct TuiApp {
    pub view: View,
    pub modal: Option<Modal>,
    pub log_filter: String,
    pub editing_filter: bool,
    pub log_follow: bool,
    pub log_offset: usize,
    pub log_level: Option<LogLevel>,
    pub selected_player: usize,
    pub selected_client: usize,
    pub selected_upload: usize,
    pub selected_setting: usize,
    pub show_raw_game: bool,
    pub raw_game_scroll: usize,
    pub raw_game_lines: Vec<String>,
    pub raw_game_filter: String,
    pub editing_raw_game_filter: bool,
    pub editing_setting: bool,
    pub setting_buffer: String,
    pub metrics: VecDeque<MetricSample>,
    rates: [EdgeRate; 3],
    last_pipeline_sample: Option<PipelineSample>,
    last_metric_sample: Option<Instant>,
    last_raw_game_tick: i64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MetricSample {
    pub game_info_us: u64,
    pub loop_us: u64,
    pub post_us: u64,
    pub cpu_hundredths: u64,
    pub memory_tenths: u64,
    pub reads: u64,
    pub replay_queue: u64,
    pub local_queue: u64,
    pub relay_queue: u64,
}

impl Default for TuiApp {
    fn default() -> Self {
        Self {
            view: View::Overview,
            modal: None,
            log_filter: String::new(),
            editing_filter: false,
            log_follow: true,
            log_offset: 0,
            log_level: None,
            selected_player: 0,
            selected_client: 0,
            selected_upload: 0,
            selected_setting: 0,
            show_raw_game: false,
            raw_game_scroll: 0,
            raw_game_lines: Vec::new(),
            raw_game_filter: String::new(),
            editing_raw_game_filter: false,
            editing_setting: false,
            setting_buffer: String::new(),
            metrics: VecDeque::new(),
            rates: [EdgeRate::default(); 3],
            last_pipeline_sample: None,
            last_metric_sample: None,
            last_raw_game_tick: i64::MIN,
        }
    }
}

impl TuiApp {
    pub fn selected_tab(&self) -> usize {
        self.view.index()
    }

    pub fn observe(&mut self, snapshot: &RuntimeSnapshot) {
        let now = Instant::now();
        self.selected_player =
            clamp_selection(self.selected_player, snapshot.status.game.players.len());
        self.selected_client =
            clamp_selection(self.selected_client, snapshot.status.local_ws.clients.len());
        self.selected_upload =
            clamp_selection(self.selected_upload, snapshot.status.relay.uploads.len());
        if self.show_raw_game && self.last_raw_game_tick == i64::MIN {
            self.raw_game_lines = snapshot
                .latest_game_info
                .as_ref()
                .and_then(|value| serde_json::to_string_pretty(value.as_ref()).ok())
                .map(|json| json.lines().map(ToOwned::to_owned).collect())
                .unwrap_or_default();
            self.last_raw_game_tick = snapshot.status.main.game_time;
            self.raw_game_scroll = self
                .raw_game_scroll
                .min(self.raw_game_lines.len().saturating_sub(1));
        }
        let enqueued =
            std::array::from_fn(|index| snapshot.pipeline.edge(PipelineEdge::ALL[index]).enqueued);
        let dequeued =
            std::array::from_fn(|index| snapshot.pipeline.edge(PipelineEdge::ALL[index]).dequeued);
        if let Some(previous) = &self.last_pipeline_sample {
            let elapsed = now.duration_since(previous.sampled_at).as_secs_f64();
            if elapsed > 0.0 {
                for index in 0..PipelineEdge::ALL.len() {
                    self.rates[index] = EdgeRate {
                        enqueued: enqueued[index].saturating_sub(previous.enqueued[index]) as f64
                            / elapsed,
                        dequeued: dequeued[index].saturating_sub(previous.dequeued[index]) as f64
                            / elapsed,
                    };
                }
            }
        }
        self.last_pipeline_sample = Some(PipelineSample {
            sampled_at: now,
            enqueued,
            dequeued,
        });
        if self
            .last_metric_sample
            .is_none_or(|sampled_at| sampled_at.elapsed() >= std::time::Duration::from_secs(1))
        {
            let main = &snapshot.status.main;
            self.metrics.push_back(MetricSample {
                game_info_us: (main.game_info_ms.max(0.0) * 1000.0) as u64,
                loop_us: (main.loop_ms.max(0.0) * 1000.0) as u64,
                post_us: (main.post_steps_ms.max(0.0) * 1000.0) as u64,
                cpu_hundredths: (main.app_cpu_percent.max(0.0) * 100.0) as u64,
                memory_tenths: (main.app_working_set_mbytes.max(0.0) * 10.0) as u64,
                reads: main.read_count,
                replay_queue: snapshot.pipeline.edge(PipelineEdge::Replay).queue_depth as u64,
                local_queue: snapshot
                    .pipeline
                    .edge(PipelineEdge::LocalWebSocket)
                    .queue_depth as u64,
                relay_queue: snapshot.pipeline.edge(PipelineEdge::Relay).queue_depth as u64,
            });
            while self.metrics.len() > 120 {
                self.metrics.pop_front();
            }
            self.last_metric_sample = Some(now);
        }
    }

    pub fn rate(&self, edge: PipelineEdge) -> EdgeRate {
        self.rates[edge_index(edge)]
    }

    pub fn handle_key(&mut self, key: KeyEvent, snapshot: &RuntimeSnapshot) -> UiAction {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return UiAction::Shutdown;
        }
        if self.editing_filter {
            return self.handle_filter_key(key);
        }
        if self.editing_raw_game_filter {
            return self.handle_raw_game_filter_key(key);
        }
        if self.editing_setting {
            return self.handle_setting_key(key);
        }
        if let Some(modal) = self.modal {
            return self.handle_modal_key(modal, key);
        }

        match key.code {
            KeyCode::Char('?') => self.modal = Some(Modal::Help),
            KeyCode::Char('q') => self.modal = Some(Modal::Quit),
            KeyCode::Char('D') if self.view == View::Replay => {
                self.modal = Some(Modal::DiscardReplay)
            }
            KeyCode::Char('U') => match snapshot.status.update.phase {
                UpdatePhase::Available => self.modal = Some(Modal::InstallUpdate),
                UpdatePhase::Checking | UpdatePhase::Installing | UpdatePhase::Installed => {}
                _ => return UiAction::Command(AppCommand::CheckForUpdates),
            },
            KeyCode::Right | KeyCode::Tab | KeyCode::Char('l') => self.change_view(1),
            KeyCode::Left | KeyCode::BackTab | KeyCode::Char('h') => self.change_view(-1),
            KeyCode::Char(value @ '1'..='8') => {
                self.view = View::ALL[(value as usize) - ('1' as usize)]
            }
            KeyCode::Char('r') => return UiAction::Command(AppCommand::ReconnectRelay),
            KeyCode::Char('x') => return UiAction::Command(AppCommand::ReconnectXemu),
            KeyCode::Char('m') => return UiAction::Command(AppCommand::ReconnectQmp),
            KeyCode::Char('s') => return UiAction::Command(AppCommand::ToggleReplaySaving),
            KeyCode::Char('t') => return UiAction::Command(AppCommand::ToggleSaveAllTicks),
            KeyCode::Char('u') => return UiAction::Command(AppCommand::ToggleReplayUploads),
            KeyCode::Char('c') if self.view == View::Logs => {
                return UiAction::Command(AppCommand::ClearLogs);
            }
            KeyCode::Char('c') if self.view == View::Metrics => self.metrics.clear(),
            KeyCode::Char('f') if self.view == View::Logs => self.editing_filter = true,
            KeyCode::Char('v') if self.view == View::Logs => self.cycle_log_level(),
            KeyCode::Char('g') if self.view == View::Game => {
                self.show_raw_game = !self.show_raw_game;
                self.raw_game_scroll = 0;
                self.last_raw_game_tick = i64::MIN;
                self.raw_game_lines.clear();
            }
            KeyCode::Char('f') if self.view == View::Game && self.show_raw_game => {
                self.raw_game_scroll = 0;
                self.last_raw_game_tick = i64::MIN;
                self.raw_game_lines.clear();
            }
            KeyCode::Char('/') if self.view == View::Game && self.show_raw_game => {
                self.editing_raw_game_filter = true;
            }
            KeyCode::Char('R') if self.view == View::Settings => {
                return UiAction::Command(AppCommand::ReloadConfig);
            }
            KeyCode::Enter | KeyCode::Char('e') if self.view == View::Settings => {
                let key = ConfigKey::ALL[self.selected_setting.min(ConfigKey::ALL.len() - 1)];
                if key.is_boolean() {
                    let value =
                        (!snapshot.config.value(key).parse::<bool>().unwrap_or(false)).to_string();
                    return UiAction::Command(AppCommand::SetConfigValue { key, value });
                }
                self.setting_buffer = snapshot.config.value(key);
                self.editing_setting = true;
            }
            KeyCode::Char('p') if self.view == View::Replay => {
                if let Some(upload) = snapshot.status.relay.uploads.get(self.selected_upload) {
                    return UiAction::Command(AppCommand::RetryUpload(upload.request_id.clone()));
                }
            }
            KeyCode::Char('d') if self.view == View::Replay => {
                if let Some(upload) = snapshot.status.relay.uploads.get(self.selected_upload) {
                    return UiAction::Command(AppCommand::CancelUpload(upload.request_id.clone()));
                }
            }
            KeyCode::Char('d') if self.view == View::Connections => {
                if let Some(client) = snapshot.status.local_ws.clients.get(self.selected_client) {
                    return UiAction::Command(AppCommand::DisconnectClient(client.address.clone()));
                }
            }
            KeyCode::Up | KeyCode::Char('k') if self.view == View::Logs => self.scroll_logs(1),
            KeyCode::Down | KeyCode::Char('j') if self.view == View::Logs => self.scroll_logs(-1),
            KeyCode::PageUp if self.view == View::Logs => self.scroll_logs(10),
            KeyCode::PageDown if self.view == View::Logs => self.scroll_logs(-10),
            KeyCode::PageUp if self.view == View::Game && self.show_raw_game => {
                self.raw_game_scroll = self.raw_game_scroll.saturating_sub(20);
            }
            KeyCode::PageDown if self.view == View::Game && self.show_raw_game => {
                self.raw_game_scroll = self.raw_game_scroll.saturating_add(20);
            }
            KeyCode::Home if self.view == View::Logs => {
                self.log_follow = false;
                self.log_offset = usize::MAX;
            }
            KeyCode::End if self.view == View::Logs => {
                self.log_follow = true;
                self.log_offset = 0;
            }
            KeyCode::Up | KeyCode::Char('k') if self.view == View::Game && self.show_raw_game => {
                self.raw_game_scroll = self.raw_game_scroll.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') if self.view == View::Game && self.show_raw_game => {
                self.raw_game_scroll = self.raw_game_scroll.saturating_add(1);
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1, snapshot),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1, snapshot),
            _ => {}
        }
        UiAction::None
    }

    pub fn log_matches(&self, line: &LogLine) -> bool {
        let filter = self.log_filter.to_ascii_lowercase();
        let level_matches = self.log_level.is_none_or(|level| line.level == level);
        let text_matches = filter.is_empty()
            || line.source.to_ascii_lowercase().contains(&filter)
            || line.message.to_ascii_lowercase().contains(&filter)
            || line.when.to_ascii_lowercase().contains(&filter);
        level_matches && text_matches
    }

    pub fn log_window(&mut self, total: usize, visible: usize) -> (usize, usize) {
        let max_offset = total.saturating_sub(visible);
        if self.log_offset == usize::MAX {
            self.log_offset = max_offset;
        } else {
            self.log_offset = self.log_offset.min(max_offset);
        }
        if self.log_follow {
            self.log_offset = 0;
        }
        let end = total.saturating_sub(self.log_offset);
        (end.saturating_sub(visible), end)
    }

    fn handle_filter_key(&mut self, key: KeyEvent) -> UiAction {
        match key.code {
            KeyCode::Esc | KeyCode::Enter => self.editing_filter = false,
            KeyCode::Backspace => {
                self.log_filter.pop();
                self.log_follow = true;
                self.log_offset = 0;
            }
            KeyCode::Char(value) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.log_filter.push(value);
                self.log_follow = true;
                self.log_offset = 0;
            }
            _ => {}
        }
        UiAction::None
    }

    fn handle_setting_key(&mut self, key: KeyEvent) -> UiAction {
        match key.code {
            KeyCode::Esc => {
                self.editing_setting = false;
                self.setting_buffer.clear();
            }
            KeyCode::Enter => {
                self.editing_setting = false;
                let key = ConfigKey::ALL[self.selected_setting.min(ConfigKey::ALL.len() - 1)];
                return UiAction::Command(AppCommand::SetConfigValue {
                    key,
                    value: std::mem::take(&mut self.setting_buffer),
                });
            }
            KeyCode::Backspace => {
                self.setting_buffer.pop();
            }
            KeyCode::Char(value) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.setting_buffer.push(value);
            }
            _ => {}
        }
        UiAction::None
    }

    fn handle_raw_game_filter_key(&mut self, key: KeyEvent) -> UiAction {
        match key.code {
            KeyCode::Esc | KeyCode::Enter => self.editing_raw_game_filter = false,
            KeyCode::Backspace => {
                self.raw_game_filter.pop();
                self.raw_game_scroll = 0;
            }
            KeyCode::Char(value) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.raw_game_filter.push(value);
                self.raw_game_scroll = 0;
            }
            _ => {}
        }
        UiAction::None
    }

    fn handle_modal_key(&mut self, modal: Modal, key: KeyEvent) -> UiAction {
        match modal {
            Modal::Help => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Char('?') | KeyCode::Enter) {
                    self.modal = None;
                }
            }
            Modal::Quit => match key.code {
                KeyCode::Char('y') | KeyCode::Enter => {
                    self.modal = None;
                    return UiAction::Shutdown;
                }
                KeyCode::Char('n') | KeyCode::Esc | KeyCode::Char('q') => self.modal = None,
                _ => {}
            },
            Modal::DiscardReplay => match key.code {
                KeyCode::Char('y') | KeyCode::Enter => {
                    self.modal = None;
                    return UiAction::Command(AppCommand::DiscardReplay);
                }
                KeyCode::Char('n') | KeyCode::Esc => self.modal = None,
                _ => {}
            },
            Modal::InstallUpdate => match key.code {
                KeyCode::Char('y') | KeyCode::Enter => {
                    self.modal = None;
                    return UiAction::Command(AppCommand::InstallUpdate);
                }
                KeyCode::Char('n') | KeyCode::Esc => self.modal = None,
                _ => {}
            },
        }
        UiAction::None
    }

    fn change_view(&mut self, delta: isize) {
        let count = View::ALL.len() as isize;
        let index = (self.view.index() as isize + delta).rem_euclid(count) as usize;
        self.view = View::ALL[index];
    }

    fn scroll_logs(&mut self, delta: isize) {
        if delta > 0 {
            self.log_offset = self.log_offset.saturating_add(delta as usize);
            self.log_follow = false;
        } else {
            self.log_offset = self.log_offset.saturating_sub(delta.unsigned_abs());
            self.log_follow = self.log_offset == 0;
        }
    }

    fn cycle_log_level(&mut self) {
        self.log_level = match self.log_level {
            None => Some(LogLevel::Info),
            Some(LogLevel::Info) => Some(LogLevel::Warning),
            Some(LogLevel::Warning) => Some(LogLevel::Error),
            Some(LogLevel::Error) => None,
        };
    }

    fn move_selection(&mut self, delta: isize, snapshot: &RuntimeSnapshot) {
        let (selection, count) = match self.view {
            View::Game => (
                &mut self.selected_player,
                snapshot.status.game.players.len(),
            ),
            View::Connections => (
                &mut self.selected_client,
                snapshot.status.local_ws.clients.len(),
            ),
            View::Replay => (
                &mut self.selected_upload,
                snapshot.status.relay.uploads.len(),
            ),
            View::Settings => (&mut self.selected_setting, ConfigKey::ALL.len()),
            _ => return,
        };
        if count == 0 {
            *selection = 0;
            return;
        }
        *selection = (*selection as isize + delta).clamp(0, count as isize - 1) as usize;
    }
}

fn edge_index(edge: PipelineEdge) -> usize {
    PipelineEdge::ALL
        .iter()
        .position(|candidate| *candidate == edge)
        .unwrap_or(0)
}

fn clamp_selection(selection: usize, count: usize) -> usize {
    selection.min(count.saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::runtime::RuntimeState;
    use serde_json::json;
    use std::sync::Arc;
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn tabs_wrap_in_both_directions() {
        let mut app = TuiApp::default();
        let snapshot = RuntimeState::new(Config::default()).snapshot();
        app.handle_key(key(KeyCode::Left), &snapshot);
        assert_eq!(app.view, View::Settings);
        app.handle_key(key(KeyCode::Right), &snapshot);
        assert_eq!(app.view, View::Overview);
    }

    #[test]
    fn quit_requires_confirmation() {
        let mut app = TuiApp::default();
        let snapshot = RuntimeState::new(Config::default()).snapshot();
        assert!(matches!(
            app.handle_key(key(KeyCode::Char('q')), &snapshot),
            UiAction::None
        ));
        assert_eq!(app.modal, Some(Modal::Quit));
        assert!(matches!(
            app.handle_key(key(KeyCode::Char('y')), &snapshot),
            UiAction::Shutdown
        ));
    }

    #[test]
    fn available_update_requires_confirmation_before_install() {
        let runtime = RuntimeState::new(Config::default());
        runtime.update(|status| {
            status.update.phase = UpdatePhase::Available;
            status.update.latest_version = "0.2.0".to_string();
        });
        let snapshot = runtime.snapshot();
        let mut app = TuiApp::default();

        assert!(matches!(
            app.handle_key(key(KeyCode::Char('U')), &snapshot),
            UiAction::None
        ));
        assert_eq!(app.modal, Some(Modal::InstallUpdate));
        assert!(matches!(
            app.handle_key(key(KeyCode::Char('y')), &snapshot),
            UiAction::Command(AppCommand::InstallUpdate)
        ));
    }

    #[test]
    fn update_shortcut_checks_when_no_release_is_available() {
        let snapshot = RuntimeState::new(Config::default()).snapshot();
        let mut app = TuiApp::default();
        assert!(matches!(
            app.handle_key(key(KeyCode::Char('U')), &snapshot),
            UiAction::Command(AppCommand::CheckForUpdates)
        ));
    }

    #[test]
    fn log_window_scrolls_from_the_newest_entry() {
        let mut app = TuiApp::default();
        assert_eq!(app.log_window(20, 5), (15, 20));
        app.scroll_logs(3);
        assert_eq!(app.log_window(20, 5), (12, 17));
        app.scroll_logs(-3);
        assert_eq!(app.log_window(20, 5), (15, 20));
        assert!(app.log_follow);
    }

    #[test]
    fn boolean_setting_dispatches_a_persisted_config_update() {
        let mut app = TuiApp {
            view: View::Settings,
            selected_setting: ConfigKey::ALL
                .iter()
                .position(|key| *key == ConfigKey::SaveReplays)
                .unwrap(),
            ..TuiApp::default()
        };
        let snapshot = RuntimeState::new(Config::default()).snapshot();
        let action = app.handle_key(key(KeyCode::Enter), &snapshot);
        assert!(matches!(
            action,
            UiAction::Command(AppCommand::SetConfigValue {
                key: ConfigKey::SaveReplays,
                value,
            }) if value == "false"
        ));
    }

    #[test]
    fn raw_game_json_is_materialized_only_when_requested() {
        let runtime = RuntimeState::new(Config::default());
        runtime.set_latest_game_info(Arc::new(json!({"game_id": "game-1", "players": []})));
        runtime.update(|status| status.main.game_time = 42);
        let snapshot = runtime.snapshot();
        let mut app = TuiApp {
            view: View::Game,
            show_raw_game: true,
            ..TuiApp::default()
        };

        app.observe(&snapshot);
        assert!(
            app.raw_game_lines
                .iter()
                .any(|line| line.contains("game-1"))
        );
        let lines = app.raw_game_lines.clone();
        app.observe(&snapshot);
        assert_eq!(app.raw_game_lines, lines);
    }
}
