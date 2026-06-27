use crate::config::Config;
use crate::util::timedelta_seconds_floor;
use anyhow::{Context, Result};
use chrono::NaiveDateTime;
use crossbeam_channel::Receiver;
use serde_json::{json, Map, Value};
use std::fs;
use std::thread;

pub fn start_replay_worker(config: Config, receiver: Receiver<Value>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        if let Err(err) = replay_worker(config, receiver) {
            eprintln!("[replay] worker failed: {err:#}");
        }
    })
}

fn replay_worker(config: Config, receiver: Receiver<Value>) -> Result<()> {
    let mut game_ticks: Vec<Value> = Vec::new();
    let mut events = Value::Array(Vec::new());
    let mut spawns = Value::Array(Vec::new());
    let mut items = Value::Array(Vec::new());
    let mut meta = Value::Array(Vec::new());
    let mut gametype_settings = Value::Array(Vec::new());

    while let Ok(mut game_info) = receiver.recv() {
        let game_id = game_info
            .get("game_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if game_id.is_empty() {
            continue;
        }

        if let Some(map) = game_info.as_object_mut() {
            events = map.remove("events").unwrap_or_else(|| Value::Array(Vec::new()));
            spawns = map.remove("spawns").unwrap_or_else(|| Value::Array(Vec::new()));
            items = map.remove("items").unwrap_or_else(|| Value::Array(Vec::new()));
            meta = map.remove("game_meta").unwrap_or_else(|| Value::Array(Vec::new()));
            gametype_settings = map
                .remove("gametype_settings")
                .unwrap_or_else(|| Value::Array(Vec::new()));
        }

        game_ticks.push(game_info);
        let Some(last_tick) = game_ticks.last() else {
            continue;
        };
        if !last_tick
            .get("game_ended_this_tick")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            continue;
        }

        let summary = build_summary(&game_id, &game_ticks);
        println!("{}", serde_json::to_string_pretty(&summary)?);
        let game = json!({
            "summary": summary,
            "game_meta": meta,
            "gametype_settings": gametype_settings,
            "events": events,
            "spawns": spawns,
            "items": items,
            "ticks": game_ticks,
        });

        fs::create_dir_all(&config.replay_directory)
            .with_context(|| format!("failed to create {:?}", config.replay_directory))?;
        let filename = config
            .replay_directory
            .join(format!("{game_id}_final.json.zst"));
        let data_bytes = serde_json::to_vec(&game)?;
        println!("Saving {} bytes to {}", data_bytes.len(), filename.display());
        let compressed = zstd::bulk::compress(&data_bytes, 11)?;
        fs::write(&filename, compressed)
            .with_context(|| format!("failed to write {}", filename.display()))?;
        game_ticks = Vec::new();
    }
    Ok(())
}

fn build_summary(game_id: &str, game_ticks: &[Value]) -> Value {
    let first = &game_ticks[0];
    let last = game_ticks.last().unwrap();
    let first_tick = tick_number(first);
    let last_tick = tick_number(last);
    let ticks_elapsed = last_tick - first_tick + 1;
    let ticks_recorded = game_ticks.len() as i64;
    let mut summary = Map::new();
    summary.insert("game_id".to_string(), Value::String(game_id.to_string()));
    summary.insert("is_full_game".to_string(), Value::Bool(first_tick == 0));
    summary.insert(
        "recording_started".to_string(),
        first.get("current_time").cloned().unwrap_or(Value::Null),
    );
    summary.insert(
        "recording_ended".to_string(),
        last.get("current_time").cloned().unwrap_or(Value::Null),
    );
    summary.insert(
        "game_duration_ingame".to_string(),
        Value::String(timedelta_seconds_floor((last_tick.max(0) as u64) / 30)),
    );
    summary.insert(
        "recording_duration".to_string(),
        Value::String(recording_duration(first, last).unwrap_or_default()),
    );
    summary.insert("ticks_elapsed".to_string(), json!(ticks_elapsed));
    summary.insert("ticks_recorded".to_string(), json!(ticks_recorded));
    summary.insert(
        "ticks_dropped".to_string(),
        json!(ticks_elapsed - ticks_recorded),
    );
    Value::Object(summary)
}

fn tick_number(tick: &Value) -> i64 {
    tick.get("game_time_info")
        .and_then(|info| info.get("game_time"))
        .and_then(|value| value.as_i64().or_else(|| value.as_u64().map(|value| value as i64)))
        .unwrap_or(0)
}

fn recording_duration(first: &Value, last: &Value) -> Option<String> {
    let first = first.get("current_time")?.as_str()?;
    let last = last.get("current_time")?.as_str()?;
    let first = NaiveDateTime::parse_from_str(first, "%Y-%m-%d %H:%M:%S%.f").ok()?;
    let last = NaiveDateTime::parse_from_str(last, "%Y-%m-%d %H:%M:%S%.f").ok()?;
    let duration = last - first;
    Some(timedelta_seconds_floor(duration.num_seconds().max(0) as u64))
}
