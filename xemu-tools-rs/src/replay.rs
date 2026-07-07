use crate::config::Config;
use crate::runtime::{Health, SharedRuntime};
use crate::util::timedelta_seconds_floor;
use anyhow::{Context, Result};
use chrono::NaiveDateTime;
use crossbeam_channel::{Receiver, Sender};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::path::Path;
use std::thread;
use std::time::Instant;
use uuid::Uuid;

pub fn start_replay_worker(
    config: Config,
    receiver: Receiver<Value>,
    relay_sender: Sender<Value>,
    runtime: SharedRuntime,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        runtime.update(|status| {
            status.replay.health = Health::Running;
            status.replay.last_changed = Some(Instant::now());
        });
        if let Err(err) = replay_worker(config, receiver, relay_sender, runtime.clone()) {
            runtime.update(|status| {
                status.replay.health = Health::Error;
                status.replay.last_error = Some(format!("{err:#}"));
                status.replay.last_changed = Some(Instant::now());
            });
            runtime.log("replay", format!("worker failed: {err:#}"));
        }
    })
}

fn replay_worker(
    config: Config,
    receiver: Receiver<Value>,
    relay_sender: Sender<Value>,
    runtime: SharedRuntime,
) -> Result<()> {
    let mut game_ticks: Vec<Value> = Vec::new();
    let mut first_tick: Option<Value> = None;
    let mut last_tick: Option<Value> = None;
    let mut ticks_recorded = 0u64;
    let mut tick_buffer_complete = true;
    let mut events = Value::Array(Vec::new());
    let mut spawns = Value::Array(Vec::new());
    let mut items = Value::Array(Vec::new());
    let mut meta = Value::Array(Vec::new());
    let mut gametype_settings = Value::Array(Vec::new());
    let mut network_game_client = Value::Array(Vec::new());

    while let Ok(mut game_info) = receiver.recv() {
        if !runtime.controls.save_replays() {
            if first_tick.is_some() {
                runtime.log("replay", "discarded in-progress replay because saving is disabled");
            }
            reset_recording(
                &mut game_ticks,
                &mut first_tick,
                &mut last_tick,
                &mut ticks_recorded,
                &mut tick_buffer_complete,
            );
            runtime.update(|status| {
                status.replay.health = Health::Running;
                status.replay.recording = false;
                status.replay.queue_depth = receiver.len();
                status.replay.current_game_id.clear();
                status.replay.ticks_recorded = 0;
                status.replay.ticks_buffered = 0;
                status.replay.last_error = None;
            });
            continue;
        }

        let game_id = game_info
            .get("game_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if game_id.is_empty() {
            continue;
        }
        let ended = game_info
            .get("game_ended_this_tick")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        if let Some(map) = game_info.as_object_mut() {
            events = map.remove("events").unwrap_or_else(|| Value::Array(Vec::new()));
            spawns = map.remove("spawns").unwrap_or_else(|| Value::Array(Vec::new()));
            items = map.remove("items").unwrap_or_else(|| Value::Array(Vec::new()));
            meta = map.remove("game_meta").unwrap_or_else(|| Value::Array(Vec::new()));
            gametype_settings = map
                .remove("gametype_settings")
                .unwrap_or_else(|| Value::Array(Vec::new()));
            network_game_client = map
                .remove("network_game_client")
                .unwrap_or_else(|| Value::Array(Vec::new()));
        }

        if first_tick.is_none() {
            first_tick = Some(game_info.clone());
            tick_buffer_complete = runtime.controls.save_all_ticks();
        }
        last_tick = Some(game_info.clone());
        ticks_recorded += 1;

        if runtime.controls.save_all_ticks() && tick_buffer_complete {
            game_ticks.push(game_info);
        } else {
            tick_buffer_complete = false;
            game_ticks.clear();
        }

        runtime.update(|status| {
            status.replay.health = Health::Running;
            status.replay.recording = true;
            status.replay.current_game_id = game_id.clone();
            status.replay.ticks_recorded = ticks_recorded;
            status.replay.ticks_buffered = game_ticks.len();
            status.replay.queue_depth = receiver.len();
            status.replay.last_error = None;
        });

        if !ended {
            continue;
        }

        let Some(first_tick_ref) = first_tick.as_ref() else {
            continue;
        };
        let Some(last_tick_ref) = last_tick.as_ref() else {
            continue;
        };
        let summary =
            build_summary_from_parts(&game_id, first_tick_ref, last_tick_ref, ticks_recorded);
        let ticks = if tick_buffer_complete {
            Value::Array(std::mem::take(&mut game_ticks))
        } else {
            Value::Array(Vec::new())
        };
        let game = json!({
            "summary": summary,
            "game_meta": meta,
            "gametype_settings": gametype_settings,
            "network_game_client": network_game_client,
            "events": events,
            "spawns": spawns,
            "items": items,
            "ticks": ticks,
        });

        fs::create_dir_all(&config.replay_directory)
            .with_context(|| format!("failed to create {:?}", config.replay_directory))?;
        let filename = config
            .replay_directory
            .join(format!("{game_id}_final.json.zst"));
        let data_bytes = serde_json::to_vec(&game)?;
        let compressed = zstd::bulk::compress(&data_bytes, 11)?;
        fs::write(&filename, compressed)
            .with_context(|| format!("failed to write {}", filename.display()))?;
        runtime.update(|status| {
            status.replay.saved_replays += 1;
            status.replay.last_saved_file = filename.display().to_string();
            status.replay.last_save_bytes = data_bytes.len() as u64;
            status.replay.last_changed = Some(Instant::now());
        });
        runtime.log(
            "replay",
            format!("saved {} bytes to {}", data_bytes.len(), filename.display()),
        );
        if config.ws_relay_enabled && runtime.controls.replay_uploads() {
            if let Err(err) = enqueue_replay_upload_request(&filename, &relay_sender) {
                runtime.update(|status| {
                    status.replay.last_error = Some(format!("{err:#}"));
                    status.replay.last_changed = Some(Instant::now());
                });
                runtime.log(
                    "replay",
                    format!(
                        "failed to enqueue replay upload request for {}: {err:#}",
                        safe_file_name(&filename)
                    ),
                );
            } else {
                runtime.update(|status| {
                    status.replay.upload_requests += 1;
                    status.replay.last_changed = Some(Instant::now());
                });
            }
        } else if config.ws_relay_enabled {
            runtime.log("replay", "replay upload skipped because uploads are disabled");
        }
        reset_recording(
            &mut game_ticks,
            &mut first_tick,
            &mut last_tick,
            &mut ticks_recorded,
            &mut tick_buffer_complete,
        );
        runtime.update(|status| {
            status.replay.recording = false;
            status.replay.current_game_id.clear();
            status.replay.ticks_recorded = 0;
            status.replay.ticks_buffered = 0;
            status.replay.queue_depth = receiver.len();
        });
    }
    Ok(())
}

fn reset_recording(
    game_ticks: &mut Vec<Value>,
    first_tick: &mut Option<Value>,
    last_tick: &mut Option<Value>,
    ticks_recorded: &mut u64,
    tick_buffer_complete: &mut bool,
) {
    game_ticks.clear();
    *first_tick = None;
    *last_tick = None;
    *ticks_recorded = 0;
    *tick_buffer_complete = true;
}

fn enqueue_replay_upload_request(path: &Path, relay_sender: &Sender<Value>) -> Result<()> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("failed to stat replay file {}", safe_file_name(path)))?;
    let filename = path
        .file_name()
        .and_then(|value| value.to_str())
        .context("replay file has no valid UTF-8 basename")?
        .to_string();
    let source_external_id = filename
        .strip_suffix("_final.json.zst")
        .or_else(|| filename.strip_suffix(".json.zst"))
        .unwrap_or(&filename)
        .to_string();

    let mut file = fs::File::open(path)
        .with_context(|| format!("failed to open replay file {}", safe_file_name(path)))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("failed to hash replay file {}", safe_file_name(path)))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let sha256 = format!("{:x}", hasher.finalize());

    relay_sender
        .try_send(json!({
            "type": "replay_upload_presign_request",
            "request_id": Uuid::new_v4().to_string(),
            "source_external_id": source_external_id,
            "filename": filename,
            "content_type": "application/zstd",
            "size_bytes": metadata.len(),
            "sha256": sha256,
            "_local_file_path": path.to_string_lossy(),
        }))
        .context("failed to enqueue replay upload request")?;
    Ok(())
}

fn safe_file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("<unknown>")
        .to_string()
}

fn build_summary_from_parts(
    game_id: &str,
    first: &Value,
    last: &Value,
    ticks_recorded: u64,
) -> Value {
    let first_tick = tick_number(first);
    let last_tick = tick_number(last);
    let ticks_elapsed = last_tick - first_tick + 1;
    let ticks_recorded = ticks_recorded as i64;
    let mut summary = Map::new();
    summary.insert("game_id".to_string(), Value::String(game_id.to_string()));
    summary.insert(
        "generated_by".to_string(),
        Value::String("xemu-tools-rs".to_string()),
    );
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
