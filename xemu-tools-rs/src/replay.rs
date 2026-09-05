use crate::config::Config;
use crate::runtime::{Health, PipelineEdge, ReplayFileStatus, SharedRuntime};
use crate::util::timedelta_seconds_floor;
use anyhow::{Context, Result};
use chrono::NaiveDateTime;
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};
use uuid::Uuid;

const FINAL_REPLAY_COMPRESSION_LEVEL: i32 = 11;
const TICK_SPOOL_COMPRESSION_LEVEL: i32 = 1;

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
    let mut tick_spool: Option<TickSpool> = None;
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
    let mut recording_started_at: Option<Instant> = None;
    let mut last_spool_sample = Instant::now() - Duration::from_secs(2);
    let mut last_spool_bytes = 0u64;

    loop {
        if runtime.shutdown_requested() {
            break;
        }
        if runtime.take_replay_discard_request() {
            if first_tick.is_some() {
                runtime.log("replay", "discarding in-progress replay by user request");
            }
            reset_recording(
                &mut tick_spool,
                &mut first_tick,
                &mut last_tick,
                &mut ticks_recorded,
                &mut tick_buffer_complete,
            );
            recording_started_at = None;
            last_spool_bytes = 0;
            runtime.update(|status| {
                status.replay.recording = false;
                status.replay.current_game_id.clear();
                status.replay.ticks_recorded = 0;
                status.replay.ticks_buffered = 0;
                status.replay.spool_path.clear();
                status.replay.spool_bytes = 0;
                status.replay.last_changed = Some(Instant::now());
            });
            continue;
        }
        let mut game_info = match receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(game_info) => game_info,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if runtime.shutdown_requested() {
            break;
        }
        runtime.record_pipeline_dequeue(PipelineEdge::Replay, receiver.len());
        if !runtime.controls.save_replays() {
            if first_tick.is_some() {
                runtime.log(
                    "replay",
                    "discarded in-progress replay because saving is disabled",
                );
            }
            reset_recording(
                &mut tick_spool,
                &mut first_tick,
                &mut last_tick,
                &mut ticks_recorded,
                &mut tick_buffer_complete,
            );
            recording_started_at = None;
            last_spool_bytes = 0;
            runtime.update(|status| {
                status.replay.health = Health::Running;
                status.replay.recording = false;
                status.replay.queue_depth = receiver.len();
                status.replay.current_game_id.clear();
                status.replay.ticks_recorded = 0;
                status.replay.ticks_buffered = 0;
                status.replay.started_at = None;
                status.replay.spool_path.clear();
                status.replay.spool_bytes = 0;
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
            events = map
                .remove("events")
                .unwrap_or_else(|| Value::Array(Vec::new()));
            spawns = map
                .remove("spawns")
                .unwrap_or_else(|| Value::Array(Vec::new()));
            items = map
                .remove("items")
                .unwrap_or_else(|| Value::Array(Vec::new()));
            meta = map
                .remove("game_meta")
                .unwrap_or_else(|| Value::Array(Vec::new()));
            gametype_settings = map
                .remove("gametype_settings")
                .unwrap_or_else(|| Value::Array(Vec::new()));
            network_game_client = map
                .remove("network_game_client")
                .unwrap_or_else(|| Value::Array(Vec::new()));
        }

        if first_tick.is_none() {
            first_tick = Some(game_info.clone());
            recording_started_at = Some(Instant::now());
            tick_buffer_complete = runtime.controls.save_all_ticks();
            if tick_buffer_complete {
                tick_spool = Some(TickSpool::new(&config.replay_directory, &game_id)?);
            }
        }
        last_tick = Some(game_info.clone());
        ticks_recorded += 1;

        if runtime.controls.save_all_ticks() && tick_buffer_complete {
            tick_spool
                .as_mut()
                .context("tick spool missing while tick buffering is enabled")?
                .push_tick(&game_info)?;
        } else {
            tick_buffer_complete = false;
            discard_tick_spool(&mut tick_spool);
        }

        let spool_path = tick_spool
            .as_ref()
            .and_then(TickSpool::path)
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        if last_spool_sample.elapsed() >= Duration::from_secs(1) {
            last_spool_sample = Instant::now();
            let sampled_spool_bytes = tick_spool
                .as_ref()
                .and_then(TickSpool::path)
                .and_then(|path| fs::metadata(path).ok())
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            runtime.record_pipeline_bytes(
                PipelineEdge::Replay,
                sampled_spool_bytes.saturating_sub(last_spool_bytes),
            );
            last_spool_bytes = sampled_spool_bytes;
        }
        runtime.update(|status| {
            status.replay.health = Health::Running;
            status.replay.recording = true;
            status.replay.current_game_id = game_id.clone();
            status.replay.ticks_recorded = ticks_recorded;
            status.replay.ticks_buffered = tick_spool.as_ref().map(TickSpool::ticks).unwrap_or(0);
            status.replay.queue_depth = receiver.len();
            status.replay.started_at = recording_started_at;
            status.replay.spool_path = spool_path;
            status.replay.spool_bytes = last_spool_bytes;
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
        let finished_tick_spool = if tick_buffer_complete {
            tick_spool.take().map(TickSpool::finish).transpose()?
        } else {
            discard_tick_spool(&mut tick_spool);
            None
        };

        fs::create_dir_all(&config.replay_directory)
            .with_context(|| format!("failed to create {:?}", config.replay_directory))?;
        let filename = config
            .replay_directory
            .join(format!("{game_id}_final.json.zst"));
        let save_started_at = Instant::now();
        let uncompressed_bytes = write_final_replay_file(
            &filename,
            &ReplayParts {
                summary: &summary,
                game_meta: &meta,
                gametype_settings: &gametype_settings,
                network_game_client: &network_game_client,
                events: &events,
                spawns: &spawns,
                items: &items,
            },
            finished_tick_spool.as_ref(),
        )?;
        let save_duration = save_started_at.elapsed();
        let data_bytes = fs::metadata(&filename)
            .map(|metadata| metadata.len())
            .unwrap_or(uncompressed_bytes);
        runtime.update(|status| {
            status.replay.saved_replays += 1;
            status.replay.last_saved_file = filename.display().to_string();
            status.replay.last_save_bytes = data_bytes;
            status.replay.last_uncompressed_bytes = uncompressed_bytes;
            status.replay.last_save_duration = save_duration;
            status.replay.recent_files.push(ReplayFileStatus {
                path: filename.display().to_string(),
                bytes: data_bytes,
                ticks: ticks_recorded,
                saved_at: Instant::now(),
                duration: save_duration,
            });
            if status.replay.recent_files.len() > 20 {
                status.replay.recent_files.remove(0);
            }
            status.replay.last_changed = Some(Instant::now());
        });
        runtime.log(
            "replay",
            format!(
                "saved {data_bytes} compressed bytes ({uncompressed_bytes} JSON bytes) to {}",
                filename.display()
            ),
        );
        if config.ws_relay_enabled && runtime.controls.replay_uploads() {
            if let Err(err) = enqueue_replay_upload_request(&filename, &relay_sender, &runtime) {
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
            runtime.log(
                "replay",
                "replay upload skipped because uploads are disabled",
            );
        }
        reset_recording(
            &mut tick_spool,
            &mut first_tick,
            &mut last_tick,
            &mut ticks_recorded,
            &mut tick_buffer_complete,
        );
        recording_started_at = None;
        last_spool_bytes = 0;
        runtime.update(|status| {
            status.replay.recording = false;
            status.replay.current_game_id.clear();
            status.replay.ticks_recorded = 0;
            status.replay.ticks_buffered = 0;
            status.replay.queue_depth = receiver.len();
            status.replay.started_at = None;
            status.replay.spool_path.clear();
            status.replay.spool_bytes = 0;
        });
    }
    if first_tick.is_some() {
        runtime.log("replay", "discarding in-progress replay during shutdown");
    }
    reset_recording(
        &mut tick_spool,
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
        status.replay.started_at = None;
        status.replay.spool_path.clear();
        status.replay.spool_bytes = 0;
        status.replay.last_changed = Some(Instant::now());
    });
    Ok(())
}

fn reset_recording(
    tick_spool: &mut Option<TickSpool>,
    first_tick: &mut Option<Value>,
    last_tick: &mut Option<Value>,
    ticks_recorded: &mut u64,
    tick_buffer_complete: &mut bool,
) {
    discard_tick_spool(tick_spool);
    *first_tick = None;
    *last_tick = None;
    *ticks_recorded = 0;
    *tick_buffer_complete = true;
}

fn discard_tick_spool(tick_spool: &mut Option<TickSpool>) {
    let _ = tick_spool.take();
}

// Stores the JSON elements that will later be placed inside "ticks": [...],
// comma-delimited but not wrapped in array brackets.
struct TickSpool {
    path: Option<PathBuf>,
    encoder: Option<zstd::stream::write::Encoder<'static, BufWriter<File>>>,
    ticks: usize,
}

impl TickSpool {
    fn new(replay_directory: &Path, game_id: &str) -> Result<Self> {
        fs::create_dir_all(replay_directory)
            .with_context(|| format!("failed to create {:?}", replay_directory))?;
        let path =
            replay_directory.join(format!(".{game_id}_{}.ticks.json.zst.tmp", Uuid::new_v4()));
        let file = File::create(&path)
            .with_context(|| format!("failed to create tick spool {}", path.display()))?;
        let writer = BufWriter::new(file);
        let encoder = zstd::stream::write::Encoder::new(writer, TICK_SPOOL_COMPRESSION_LEVEL)
            .with_context(|| format!("failed to start tick spool {}", path.display()))?;
        Ok(Self {
            path: Some(path),
            encoder: Some(encoder),
            ticks: 0,
        })
    }

    fn push_tick(&mut self, tick: &Value) -> Result<()> {
        let encoder = self
            .encoder
            .as_mut()
            .context("tick spool already finished")?;
        if self.ticks > 0 {
            encoder.write_all(b",")?;
        }
        serde_json::to_writer(&mut *encoder, tick)?;
        self.ticks += 1;
        Ok(())
    }

    fn finish(mut self) -> Result<FinishedTickSpool> {
        let path = self
            .path
            .as_ref()
            .cloned()
            .context("tick spool path missing")?;
        let encoder = self.encoder.take().context("tick spool already finished")?;
        let mut writer = encoder
            .finish()
            .with_context(|| format!("failed to finish tick spool {}", path.display()))?;
        writer
            .flush()
            .with_context(|| format!("failed to flush tick spool {}", path.display()))?;
        self.path = None;
        Ok(FinishedTickSpool {
            path,
            ticks: self.ticks,
        })
    }

    fn ticks(&self) -> usize {
        self.ticks
    }

    fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

impl Drop for TickSpool {
    fn drop(&mut self) {
        let _ = self.encoder.take();
        if let Some(path) = self.path.take() {
            let _ = fs::remove_file(path);
        }
    }
}

struct FinishedTickSpool {
    path: PathBuf,
    ticks: usize,
}

impl Drop for FinishedTickSpool {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

struct ReplayParts<'a> {
    summary: &'a Value,
    game_meta: &'a Value,
    gametype_settings: &'a Value,
    network_game_client: &'a Value,
    events: &'a Value,
    spawns: &'a Value,
    items: &'a Value,
}

fn write_final_replay_file(
    path: &Path,
    parts: &ReplayParts<'_>,
    tick_spool: Option<&FinishedTickSpool>,
) -> Result<u64> {
    let file = File::create(path)
        .with_context(|| format!("failed to create replay file {}", path.display()))?;
    let writer = BufWriter::new(file);
    let encoder = zstd::stream::write::Encoder::new(writer, FINAL_REPLAY_COMPRESSION_LEVEL)
        .with_context(|| format!("failed to start replay compressor {}", path.display()))?;
    let mut output = CountingWriter::new(encoder);

    output.write_all(b"{\"summary\":")?;
    serde_json::to_writer(&mut output, parts.summary)?;
    output.write_all(b",\"game_meta\":")?;
    serde_json::to_writer(&mut output, parts.game_meta)?;
    output.write_all(b",\"gametype_settings\":")?;
    serde_json::to_writer(&mut output, parts.gametype_settings)?;
    output.write_all(b",\"network_game_client\":")?;
    serde_json::to_writer(&mut output, parts.network_game_client)?;
    output.write_all(b",\"events\":")?;
    serde_json::to_writer(&mut output, parts.events)?;
    output.write_all(b",\"spawns\":")?;
    serde_json::to_writer(&mut output, parts.spawns)?;
    output.write_all(b",\"items\":")?;
    serde_json::to_writer(&mut output, parts.items)?;
    output.write_all(b",\"ticks\":[")?;
    if let Some(tick_spool) = tick_spool
        && tick_spool.ticks > 0
    {
        copy_tick_spool(tick_spool, &mut output)?;
    }
    output.write_all(b"]}")?;

    let bytes_written = output.bytes_written();
    let encoder = output.into_inner();
    let mut writer = encoder
        .finish()
        .with_context(|| format!("failed to finish replay compressor {}", path.display()))?;
    writer
        .flush()
        .with_context(|| format!("failed to flush replay file {}", path.display()))?;
    Ok(bytes_written)
}

fn copy_tick_spool(tick_spool: &FinishedTickSpool, output: &mut impl Write) -> Result<()> {
    let file = File::open(&tick_spool.path)
        .with_context(|| format!("failed to open tick spool {}", tick_spool.path.display()))?;
    let mut decoder = zstd::stream::read::Decoder::new(BufReader::new(file))
        .with_context(|| format!("failed to read tick spool {}", tick_spool.path.display()))?;
    io::copy(&mut decoder, output)
        .with_context(|| format!("failed to copy tick spool {}", tick_spool.path.display()))?;
    Ok(())
}

struct CountingWriter<W> {
    inner: W,
    bytes_written: u64,
}

impl<W> CountingWriter<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            bytes_written: 0,
        }
    }

    fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buf)?;
        self.bytes_written += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn enqueue_replay_upload_request(
    path: &Path,
    relay_sender: &Sender<Value>,
    runtime: &SharedRuntime,
) -> Result<()> {
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

    let result = relay_sender.try_send(json!({
        "type": "replay_upload_presign_request",
        "request_id": Uuid::new_v4().to_string(),
        "source_external_id": source_external_id,
        "filename": filename,
        "content_type": "application/zstd",
        "size_bytes": metadata.len(),
        "sha256": sha256,
        "_local_file_path": path.to_string_lossy(),
    }));
    runtime.record_pipeline_enqueue(PipelineEdge::Relay, relay_sender.len(), result.is_ok());
    result.context("failed to enqueue replay upload request")?;
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
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_u64().map(|value| value as i64))
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn final_replay_preserves_complete_breakable_snapshots_and_unknown() -> Result<()> {
        let fixture: Value = serde_json::from_str(include_str!("../../tests/fixtures/breakable_surfaces.json"))?;
        let dir = test_dir()?;
        let out_path = dir.join("breakable.json.zst");
        let mut spool = TickSpool::new(&dir, "breakable")?;
        let mut ticks = Vec::new();
        for (index, snapshot) in fixture["cases"].as_array().unwrap().iter()
            .map(|case| case["snapshot"].clone()).chain([Value::Null]).enumerate() {
            let tick = json!({"game_time_info": {"game_time": 100 + index * 10}, "broken_surfaces": snapshot});
            spool.push_tick(&tick)?;
            ticks.push(tick);
        }
        let finished = spool.finish()?;
        write_final_replay_file(&out_path, &TestReplayValues::new().parts(), Some(&finished))?;
        let replay: Value = serde_json::from_slice(&zstd::stream::decode_all(File::open(out_path)?)?)?;
        assert_eq!(replay["ticks"], json!(ticks));
        drop(finished);
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn streamed_replay_matches_existing_serialized_json_with_spooled_ticks() -> Result<()> {
        let dir = test_dir()?;
        let out_path = dir.join("replay.json.zst");
        let tick_one = json!({
            "game_id": "game-1",
            "current_time": "2026-07-07 10:00:00.000000",
            "game_time_info": {"game_time": 0},
            "players": [{"player_index": 0, "score": 1}],
        });
        let tick_two = json!({
            "game_id": "game-1",
            "current_time": "2026-07-07 10:00:00.033333",
            "game_time_info": {"game_time": 1},
            "players": [{"player_index": 0, "score": 2}],
        });

        let mut spool = TickSpool::new(&dir, "game-1")?;
        spool.push_tick(&tick_one)?;
        spool.push_tick(&tick_two)?;
        let finished_spool = spool.finish()?;
        let spool_path = finished_spool.path.clone();

        let values = TestReplayValues::new();
        let parts = values.parts();
        let expected = expected_replay_bytes(&parts, json!([tick_one, tick_two]))?;
        let bytes_written = write_final_replay_file(&out_path, &parts, Some(&finished_spool))?;
        let actual = zstd::stream::decode_all(File::open(&out_path)?)?;

        assert_eq!(actual, expected);
        assert_eq!(bytes_written, expected.len() as u64);
        drop(finished_spool);
        assert!(!spool_path.exists());

        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn streamed_replay_matches_existing_serialized_json_without_ticks() -> Result<()> {
        let dir = test_dir()?;
        let out_path = dir.join("replay.json.zst");
        let values = TestReplayValues::new();
        let parts = values.parts();
        let expected = expected_replay_bytes(&parts, json!([]))?;

        let bytes_written = write_final_replay_file(&out_path, &parts, None)?;
        let actual = zstd::stream::decode_all(File::open(&out_path)?)?;

        assert_eq!(actual, expected);
        assert_eq!(bytes_written, expected.len() as u64);

        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn shutdown_removes_in_progress_tick_spool() -> Result<()> {
        let dir = test_dir()?;
        let config = Config {
            replay_directory: dir.clone(),
            ..Config::default()
        };
        let runtime = crate::runtime::RuntimeState::new(config.clone());
        let (tick_sender, tick_receiver) = crossbeam_channel::unbounded();
        let (relay_sender, _relay_receiver) = crossbeam_channel::unbounded();
        let worker = start_replay_worker(config, tick_receiver, relay_sender, runtime.clone());

        tick_sender.send(json!({
            "game_id": "shutdown-test",
            "game_ended_this_tick": false,
        }))?;
        let deadline = Instant::now() + Duration::from_secs(2);
        while !runtime.snapshot().status.replay.recording && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(runtime.snapshot().status.replay.recording);
        assert_eq!(temporary_spools(&dir)?.len(), 1);

        runtime.request_shutdown();
        drop(tick_sender);
        worker.join().expect("replay worker should stop cleanly");

        assert!(temporary_spools(&dir)?.is_empty());
        assert!(!runtime.snapshot().status.replay.recording);
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn discard_command_removes_spool_without_stopping_worker() -> Result<()> {
        let dir = test_dir()?;
        let config = Config {
            replay_directory: dir.clone(),
            ..Config::default()
        };
        let runtime = crate::runtime::RuntimeState::new(config.clone());
        let (tick_sender, tick_receiver) = crossbeam_channel::unbounded();
        let (relay_sender, _relay_receiver) = crossbeam_channel::unbounded();
        let worker = start_replay_worker(config, tick_receiver, relay_sender, runtime.clone());
        tick_sender.send(json!({
            "game_id": "discard-test",
            "game_ended_this_tick": false,
        }))?;
        wait_for_recording_state(&runtime, true);
        assert_eq!(temporary_spools(&dir)?.len(), 1);

        runtime.request_replay_discard();
        wait_for_recording_state(&runtime, false);
        assert!(temporary_spools(&dir)?.is_empty());

        runtime.request_shutdown();
        drop(tick_sender);
        worker.join().expect("replay worker should stop cleanly");
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    struct TestReplayValues {
        summary: Value,
        game_meta: Value,
        gametype_settings: Value,
        network_game_client: Value,
        events: Value,
        spawns: Value,
        items: Value,
    }

    impl TestReplayValues {
        fn new() -> Self {
            Self {
                summary: json!({
                    "game_id": "game-1",
                    "ticks_recorded": 2,
                }),
                game_meta: json!({"players": []}),
                gametype_settings: json!({"score_to_win": 50}),
                network_game_client: json!({"machines": []}),
                events: json!(["0: Game started"]),
                spawns: json!([{"tick": 0, "player": 0}]),
                items: json!([{"name": "pistol"}]),
            }
        }

        fn parts(&self) -> ReplayParts<'_> {
            ReplayParts {
                summary: &self.summary,
                game_meta: &self.game_meta,
                gametype_settings: &self.gametype_settings,
                network_game_client: &self.network_game_client,
                events: &self.events,
                spawns: &self.spawns,
                items: &self.items,
            }
        }
    }

    fn expected_replay_bytes(parts: &ReplayParts<'_>, ticks: Value) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(&json!({
            "summary": parts.summary,
            "game_meta": parts.game_meta,
            "gametype_settings": parts.gametype_settings,
            "network_game_client": parts.network_game_client,
            "events": parts.events,
            "spawns": parts.spawns,
            "items": parts.items,
            "ticks": ticks,
        }))?)
    }

    fn test_dir() -> Result<PathBuf> {
        let dir = std::env::temp_dir().join(format!("xemu-tools-rs-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    fn temporary_spools(dir: &Path) -> Result<Vec<PathBuf>> {
        Ok(fs::read_dir(dir)?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(".ticks.json.zst.tmp"))
            })
            .collect())
    }

    fn wait_for_recording_state(runtime: &SharedRuntime, recording: bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while runtime.snapshot().status.replay.recording != recording && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(runtime.snapshot().status.replay.recording, recording);
    }
}
