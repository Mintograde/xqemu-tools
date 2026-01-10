use serde::{Deserialize, Serialize};
use crate::halo::{PlayerInfo, Spawn, Item, GameMeta, GameTimeInfo};
use std::fs;
use std::path::Path;
use anyhow::Result;
use std::io::Write;
use zstd::stream::write::Encoder;

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct ReplaySummary {
    pub game_id: String,
    pub is_full_game: bool,
    pub recording_started: String,
    pub recording_ended: String,
    pub game_duration_ingame: String,
    pub recording_duration: String,
    pub ticks_elapsed: u32,
    pub ticks_recorded: usize,
    pub ticks_dropped: u32,
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Replay {
    pub summary: ReplaySummary,
    pub game_meta: GameMeta,
    pub events: Vec<String>,
    pub spawns: Vec<Spawn>,
    pub items: Vec<Item>,
    pub ticks: Vec<serde_json::Value>,
}

pub struct ReplayManager {
    accumulated_ticks: Vec<serde_json::Value>,
    replay_dir: String,
}

impl ReplayManager {
    pub fn new(replay_dir: &str) -> Self {
        fs::create_dir_all(replay_dir).ok();
        Self {
            accumulated_ticks: Vec::new(),
            replay_dir: replay_dir.to_string(),
        }
    }

    pub fn add_tick(&mut self, tick: serde_json::Value) {
        self.accumulated_ticks.push(tick);
    }

    pub fn save_replay(&mut self, game_id: &str, game_meta: GameMeta, events: Vec<String>, spawns: Vec<Spawn>, items: Vec<Item>) -> Result<()> {
        if self.accumulated_ticks.is_empty() {
            return Ok(());
        }

        let first_tick = self.accumulated_ticks.first().unwrap();
        let last_tick = self.accumulated_ticks.last().unwrap();

        let start_time_str = first_tick["current_time"].as_str().unwrap_or("").to_string();
        let end_time_str = last_tick["current_time"].as_str().unwrap_or("").to_string();
        
        let first_gt = first_tick["game_time_info"]["game_time"].as_u64().unwrap_or(0) as u32;
        let last_gt = last_tick["game_time_info"]["game_time"].as_u64().unwrap_or(0) as u32;

        let summary = ReplaySummary {
            game_id: game_id.to_string(),
            is_full_game: first_gt == 0,
            recording_started: start_time_str,
            recording_ended: end_time_str,
            game_duration_ingame: format!("{}s", last_gt / 30),
            recording_duration: "unknown".to_string(), // Simplified for now
            ticks_elapsed: last_gt - first_gt + 1,
            ticks_recorded: self.accumulated_ticks.len(),
            ticks_dropped: (last_gt - first_gt + 1).saturating_sub(self.accumulated_ticks.len() as u32),
        };

        let replay = Replay {
            summary,
            game_meta,
            events,
            spawns,
            items,
            ticks: self.accumulated_ticks.clone(),
        };

        let filename = format!("{}_final.json.zst", game_id);
        let path = Path::new(&self.replay_dir).join(filename);
        
        let json_data = serde_json::to_vec(&replay)?;
        
        let file = fs::File::create(path)?;
        let mut encoder = Encoder::new(file, 11)?;
        encoder.write_all(&json_data)?;
        encoder.finish()?;

        println!("Replay saved successfully for game {}", game_id);
        self.accumulated_ticks.clear();
        Ok(())
    }

    pub fn clear(&mut self) {
        self.accumulated_ticks.clear();
    }
}
