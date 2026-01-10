use std::collections::HashMap;
use chrono::{DateTime, Local};
use sysinfo::{System, ProcessesToUpdate};
use crate::halo::{GameTick, PlayerMeta, GameMeta, PerformanceMetrics};
use crate::events::extract_events;
use crate::replay::ReplayManager;
use std::time::{Instant, Duration};

pub struct SessionContext {
    pub game_meta_start_time: Option<DateTime<Local>>,
    pub old_can_score: bool,
    pub game_id: String,
    pub prev_tick: Option<GameTick>,
    pub event_history: Vec<String>,
    pub player_meta_map: HashMap<u32, PlayerMeta>,
    pub sys: System,
    pub replay_manager: ReplayManager,
    pub last_loop_time: Instant,
    pub pid: u32,
}

pub struct ProcessedTick {
    pub live_msg: serde_json::Value,
    pub perf: PerformanceMetrics,
    pub new_events: Vec<String>,
}

impl SessionContext {
    pub fn new(pid: u32) -> Self {
        Self {
            game_meta_start_time: None,
            old_can_score: false,
            game_id: String::new(),
            prev_tick: None,
            event_history: Vec::new(),
            player_meta_map: HashMap::new(),
            sys: System::new_all(),
            replay_manager: ReplayManager::new("replays"),
            last_loop_time: Instant::now(),
            pid,
        }
    }

    pub fn process_tick(&mut self, mut tick: GameTick, read_time: Duration) -> Result<ProcessedTick, serde_json::Error> {
        let post_process_start = Instant::now();

        // 1. New Game detection & History Reset
        if tick.game_engine_can_score && !self.old_can_score {
            self.event_history.clear();
            self.player_meta_map.clear();
            self.replay_manager.clear();
        }

        // 2. Meta Aggregation & Re-injection
        for player in &mut tick.players {
            let meta = self.player_meta_map.entry(player.player_index).or_default();
            player.derived_stats.shots_by_weapon = meta.shots_by_weapon.clone();
            player.derived_stats.damage_to_player = meta.damage_to_player.clone();
            player.derived_stats.damage_from_player = meta.damage_from_player.clone();
            player.derived_stats.damage_dealt = meta.damage_dealt;
            player.derived_stats.damage_received = meta.damage_received;
            player.derived_stats.camo_count = meta.camo_count;
            player.derived_stats.overshield_count = meta.overshield_count;
        }

        // 3. Event Extraction
        let new_events = extract_events(&self.prev_tick, &tick, &mut self.player_meta_map);
        for e in &new_events {
            self.event_history.push(e.clone());
        }
        if self.event_history.len() > 100 {
            let to_remove = self.event_history.len() - 100;
            self.event_history.drain(0..to_remove);
        }
        self.prev_tick = Some(tick.clone());

        // 4. Session Timing (start_time / game_id)
        if self.game_meta_start_time.is_none() && tick.game_engine_can_score {
            let elapsed_ticks = tick.game_time_info.game_time + 1;
            self.game_meta_start_time = Some(Local::now() - chrono::Duration::milliseconds((elapsed_ticks as i64 * 1000) / 30));
        } else if !tick.game_engine_can_score && self.old_can_score {
            self.game_meta_start_time = None;
        }

        if tick.game_engine_can_score {
            if let Some(start) = self.game_meta_start_time {
                self.game_id = start.format("%Y-%m-%d_%H-%M-%S").to_string();
            }
        } else if !self.old_can_score {
            self.game_id = String::new();
        }

        let game_ended_this_tick = self.old_can_score && !tick.game_engine_can_score;
        self.old_can_score = tick.game_engine_can_score;

        // 5. System Metrics
        let target_pid = sysinfo::Pid::from_u32(self.pid);
        self.sys.refresh_processes(ProcessesToUpdate::Some(&[target_pid]), true);
        let mem_usage = self.sys.process(target_pid)
            .map(|p| p.memory() as f64 / 1024.0 / 1024.0)
            .unwrap_or(0.0);

        // 6. Performance Calculation
        let post_process_time = post_process_start.elapsed();
        let total_loop_time = self.last_loop_time.elapsed();
        self.last_loop_time = Instant::now();

        let perf = PerformanceMetrics {
            loop_time: total_loop_time.as_secs_f64() * 1000.0,
            game_info_time: read_time.as_secs_f64() * 1000.0,
            post_steps_ms: post_process_time.as_secs_f64() * 1000.0,
            memory_mbytes: mem_usage,
        };

        let game_meta = GameMeta {
            start_time: self.game_meta_start_time.map(|t| t.to_rfc3339()),
            players: self.player_meta_map.clone(),
            events: self.event_history.clone(),
        };

        // 7. Prepare JSON
        let mut live_msg = serde_json::to_value(&tick)?;
        if let Some(obj) = live_msg.as_object_mut() {
            obj.insert("game_id".to_string(), serde_json::json!(self.game_id));
            obj.insert("performance".to_string(), serde_json::to_value(&perf).unwrap_or(serde_json::Value::Null));
            obj.insert("game_meta".to_string(), serde_json::to_value(&game_meta).unwrap_or(serde_json::Value::Null));
            obj.insert("events".to_string(), serde_json::to_value(&new_events).unwrap_or(serde_json::Value::Null));
            obj.insert("game_ended_this_tick".to_string(), serde_json::json!(game_ended_this_tick));
            obj.insert("start_time".to_string(), serde_json::to_value(self.game_meta_start_time.map(|t| t.to_rfc3339())).unwrap_or(serde_json::Value::Null));
        }

        // 8. Replay Management
        if tick.game_engine_can_score || game_ended_this_tick {
            let mut tick_data = live_msg.clone();
            if let Some(m) = tick_data.as_object_mut() {
                m.remove("events");
                m.remove("spawns");
                m.remove("items");
                m.remove("game_meta");
            }
            self.replay_manager.add_tick(tick_data);
        }

        if game_ended_this_tick {
            let last_id = if self.game_id.is_empty() { "unknown".to_string() } else { self.game_id.clone() };
            let _ = self.replay_manager.save_replay(&last_id, game_meta, self.event_history.clone(), tick.spawns, tick.items);
        }

        Ok(ProcessedTick {
            live_msg,
            perf,
            new_events,
        })
    }
}