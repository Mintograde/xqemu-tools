mod config;
mod events;
mod halo;
mod memory;
mod process;
mod qmp;
mod replay;
mod util;
mod ws;

use anyhow::Result;
use config::Config;
use crossbeam_channel::unbounded;
use halo::HaloReader;
use memory::MemoryReader;
use qmp::QmpClient;
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

fn main() -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let _ = dotenvy::from_filename("xemu-tools-2-rs.env");
    let _ = dotenvy::dotenv();
    let config = Config::default();
    let xemu = process::wait_for_xemu()?;
    let qmp = QmpClient::connect_with_retry(config.qmp_host.clone(), config.qmp_port)?;
    let memory = MemoryReader::new(xemu.pid, qmp)?;
    let mut halo = HaloReader::new(memory, config.clone())?;

    let (replay_tx, replay_rx) = unbounded();
    let (local_ws_tx, local_ws_rx) = unbounded();
    let (relay_tx, relay_rx) = unbounded();
    let _replay_worker = replay::start_replay_worker(config.clone(), replay_rx);
    let _local_ws_server = ws::start_local_ws_server(config.clone(), local_ws_rx);
    let _relay_client = if config.ws_relay_enabled {
        Some(ws::start_relay_client(config.clone(), relay_rx))
    } else {
        None
    };

    println!(
        "game_time host address: {:#x}",
        halo.game_time_host_address()?
    );
    main_loop(&mut halo, replay_tx, local_ws_tx, relay_tx)
}

fn main_loop(
    halo: &mut HaloReader,
    replay_tx: crossbeam_channel::Sender<Value>,
    local_ws_tx: crossbeam_channel::Sender<Value>,
    relay_tx: crossbeam_channel::Sender<Value>,
) -> Result<()> {
    let mut counter = 0u64;
    let mut last_game_time = 0i64;
    let mut last_real_time = Instant::now();
    let mut last_post_steps = 0.0f64;
    let mut benchmark_tick_count = 0u64;
    let mut benchmark_loop_count = 0u64;
    let mut last_game_info: Option<Value> = None;
    let mut events: Vec<Value> = Vec::new();
    let mut last_metrics_print = Instant::now();
    let mut dropped_ticks_total = 0i64;

    loop {
        match tick_once(
            halo,
            &replay_tx,
            &local_ws_tx,
            &relay_tx,
            &mut counter,
            &mut last_game_time,
            &mut last_real_time,
            &mut last_post_steps,
            &mut benchmark_tick_count,
            &mut benchmark_loop_count,
            &mut last_game_info,
            &mut events,
            &mut dropped_ticks_total,
            &mut last_metrics_print,
        ) {
            Ok(()) => {}
            Err(err) => {
                eprintln!("main loop error: {err:#}");
                halo.invalidate_memory_cache();
                halo.mem.clear_translations();
                let _ = halo.mem.reconnect_qmp();
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn tick_once(
    halo: &mut HaloReader,
    replay_tx: &crossbeam_channel::Sender<Value>,
    local_ws_tx: &crossbeam_channel::Sender<Value>,
    relay_tx: &crossbeam_channel::Sender<Value>,
    counter: &mut u64,
    last_game_time: &mut i64,
    last_real_time: &mut Instant,
    last_post_steps: &mut f64,
    benchmark_tick_count: &mut u64,
    benchmark_loop_count: &mut u64,
    last_game_info: &mut Option<Value>,
    events: &mut Vec<Value>,
    dropped_ticks_total: &mut i64,
    last_metrics_print: &mut Instant,
) -> Result<()> {
    let game_time = halo.read_loop_game_time()?;
    *benchmark_loop_count += 1;
    *counter += 1;

    if game_time != *last_game_time {
        *benchmark_tick_count += 1;
        let real_time = Instant::now();
        *counter = 0;
        halo.mem.read_counter = 0;

        halo.populate_memory_cache()?;
        let mut game_info = halo.get_game_info()?;
        halo.invalidate_memory_cache();

        let extracted_game_time = game_info
            .get("game_time_info")
            .and_then(|info| info.get("game_time"))
            .and_then(Value::as_i64)
            .unwrap_or(game_time);
        if extracted_game_time != game_time {
            println!(
                "  WARNING: mismatched game time (expected {game_time}, got {extracted_game_time})"
            );
        }

        let game_info_time = real_time.elapsed().as_secs_f64() * 1000.0;
        if game_info_time > 33.0 {
            println!("  WARNING: this update took longer than one tick: {game_info_time:.3}ms");
        }
        if game_time > *last_game_time + 1 {
            let missed = game_time - *last_game_time - 1;
            *dropped_ticks_total += missed;
            println!("  WARNING: missed {missed} ticks between {last_game_time} and {game_time}");
        }

        if let Some(old_game_info) = last_game_info.as_ref() {
            let old_running = old_game_info
                .get("game_engine_running")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let new_running = game_info
                .get("game_engine_running")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if old_running && !new_running {
                events.clear();
            } else {
                let result = halo.extract_events(old_game_info, &mut game_info);
                events.extend(result.events.into_iter().map(Value::String));
            }
        }
        game_info
            .as_object_mut()
            .unwrap()
            .insert("events".to_string(), Value::Array(events.clone()));

        let loop_time = real_time.duration_since(*last_real_time).as_secs_f64() * 1000.0;
        game_info.as_object_mut().unwrap().insert(
            "performance".to_string(),
            json!({
                "game_info_time": game_info_time,
                "loop_time": loop_time,
                "post_steps_ms": *last_post_steps,
                "memory_mbytes": current_memory_mbytes(),
            }),
        );

        *last_real_time = real_time;
        let post_steps_start = Instant::now();
        let _ = replay_tx.try_send(game_info.clone());
        let _ = local_ws_tx.try_send(game_info.clone());
        let _ = relay_tx.try_send(game_info.clone());
        *last_game_info = Some(game_info);
        *last_post_steps = post_steps_start.elapsed().as_secs_f64() * 1000.0;

        if last_metrics_print.elapsed() >= Duration::from_secs(5) {
            let loops_per_tick = if *benchmark_tick_count > 0 {
                *benchmark_loop_count as f64 / *benchmark_tick_count as f64
            } else {
                0.0
            };
            println!(
                "metrics: tick={game_time} game_info_ms={game_info_time:.3} loop_ms={loop_time:.3} post_ms={:.3} loops_per_tick={loops_per_tick:.1} dropped_total={}",
                *last_post_steps,
                *dropped_ticks_total
            );
            *last_metrics_print = Instant::now();
        }
    }

    *last_game_time = game_time;
    Ok(())
}

fn current_memory_mbytes() -> f64 {
    unsafe {
        let mut counters = std::mem::zeroed::<PROCESS_MEMORY_COUNTERS>();
        counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        let ok = K32GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut counters,
            std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        );
        if ok == 0 {
            0.0
        } else {
            counters.PagefileUsage as f64 / 1024.0 / 1024.0
        }
    }
}
