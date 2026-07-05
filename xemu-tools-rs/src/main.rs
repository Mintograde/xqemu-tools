mod config;
mod events;
mod halo;
mod memory;
mod process;
mod qmp;
mod replay;
mod runtime;
mod tui;
mod util;
mod ws;

use anyhow::Result;
use config::Config;
use crossbeam_channel::{Receiver, Sender, unbounded};
use halo::HaloReader;
use memory::MemoryReader;
use qmp::QmpClient;
use runtime::{AppCommand, Health, RelayCommand, RuntimeState, SharedRuntime};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::FILETIME;
use windows_sys::Win32::System::ProcessStatus::{
    K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

fn main() -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let _ = dotenvy::from_filename("xemu-tools-rs.env");
    let _ = dotenvy::dotenv();
    let config = Config::default();
    let runtime = RuntimeState::new(config.clone());
    let (command_tx, command_rx) = unbounded();
    let _tui = tui::start_tui(runtime.clone(), command_tx);
    runtime.log("main", "starting runtime");
    let mut halo = connect_halo(&config, &runtime)?;

    let (replay_tx, replay_rx) = unbounded();
    let (local_ws_tx, local_ws_rx) = unbounded();
    let (relay_tx, relay_rx) = unbounded();
    let (relay_command_tx, relay_command_rx) = unbounded();
    let _replay_worker =
        replay::start_replay_worker(config.clone(), replay_rx, relay_tx.clone(), runtime.clone());
    let _local_ws_server = ws::start_local_ws_server(config.clone(), local_ws_rx, runtime.clone());
    let _relay_client = if config.ws_relay_enabled {
        Some(ws::start_relay_client(
            config.clone(),
            relay_rx,
            relay_tx.clone(),
            relay_command_rx,
            runtime.clone(),
        ))
    } else {
        runtime.update(|status| {
            status.relay.health = Health::Disabled;
            status.relay.last_changed = Some(Instant::now());
        });
        None
    };

    main_loop(
        &config,
        &mut halo,
        replay_tx,
        local_ws_tx,
        relay_tx,
        command_rx,
        relay_command_tx,
        runtime,
    )
}

fn connect_halo(config: &Config, runtime: &SharedRuntime) -> Result<HaloReader> {
    runtime.update(|status| {
        status.xemu.health = Health::Starting;
        status.xemu.detail = "waiting for xemu.exe".to_string();
        status.xemu.last_changed = Some(Instant::now());
    });
    runtime.log("xemu", "waiting for xemu.exe");
    let xemu = loop {
        if let Some(process) = process::find_xemu_process() {
            runtime.update(|status| {
                status.xemu.health = Health::Running;
                status.xemu.pid = Some(process.pid);
                status.xemu.detail = "process found".to_string();
                status.xemu.last_error = None;
                status.xemu.last_changed = Some(Instant::now());
            });
            runtime.log("xemu", format!("attached to pid {} ({:#x})", process.pid, process.pid));
            break process;
        }
        std::thread::sleep(Duration::from_secs(1));
    };

    let qmp_endpoint = format!("{}:{}", config.qmp_host, config.qmp_port);
    runtime.update(|status| {
        status.qmp.health = Health::Starting;
        status.qmp.endpoint = qmp_endpoint.clone();
        status.qmp.detail = "connecting".to_string();
        status.qmp.last_changed = Some(Instant::now());
    });
    let qmp = match QmpClient::connect_with_retry(config.qmp_host.clone(), config.qmp_port) {
        Ok(qmp) => {
            runtime.update(|status| {
                status.qmp.health = Health::Connected;
                status.qmp.detail = "connected".to_string();
                status.qmp.last_error = None;
                status.qmp.last_changed = Some(Instant::now());
            });
            runtime.log("qmp", format!("connected to {qmp_endpoint}"));
            qmp
        }
        Err(err) => {
            runtime.update(|status| {
                status.qmp.health = Health::Error;
                status.qmp.detail = "connect failed".to_string();
                status.qmp.last_error = Some(format!("{err:#}"));
                status.qmp.last_changed = Some(Instant::now());
            });
            return Err(err);
        }
    };

    let memory = MemoryReader::new(xemu.pid, qmp)?;
    let mut halo = HaloReader::new(memory, config.clone())?;
    let game_time_host_address = halo.game_time_host_address()?;
    runtime.update(|status| {
        status.main.health = Health::Running;
        status.main.game_time_host_address = Some(game_time_host_address);
        status.main.last_error = None;
    });
    runtime.log(
        "main",
        format!("game_time host address {game_time_host_address:#x}"),
    );
    Ok(halo)
}

fn main_loop(
    config: &Config,
    halo: &mut HaloReader,
    replay_tx: Sender<Value>,
    local_ws_tx: Sender<Value>,
    relay_tx: Sender<Value>,
    command_rx: Receiver<AppCommand>,
    relay_command_tx: Sender<RelayCommand>,
    runtime: SharedRuntime,
) -> Result<()> {
    let mut counter = 0u64;
    let mut last_game_time: Option<i64> = None;
    let mut last_real_time = Instant::now();
    let mut last_post_steps = 0.0f64;
    let mut benchmark_tick_count = 0u64;
    let mut benchmark_loop_count = 0u64;
    let mut last_game_info: Option<Value> = None;
    let mut events: Vec<Value> = Vec::new();
    let mut last_metrics_print = Instant::now();
    let mut dropped_ticks_total = 0i64;
    let mut resource_sampler = ProcessResourceSampler::new();

    loop {
        if process_commands(
            config,
            halo,
            &mut last_game_time,
            &command_rx,
            &relay_command_tx,
            &runtime,
        )? {
            return Ok(());
        }

        match tick_once(
            halo,
            &replay_tx,
            &local_ws_tx,
            &relay_tx,
            &runtime,
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
            &mut resource_sampler,
        ) {
            Ok(()) => {}
            Err(err) => {
                runtime.update(|status| {
                    status.main.health = Health::Error;
                    status.main.last_error = Some(format!("{err:#}"));
                });
                runtime.log("main", format!("loop error: {err:#}"));
                halo.invalidate_memory_cache();
                halo.mem.clear_translations();
                match halo.mem.reconnect_qmp() {
                    Ok(()) => {
                        runtime.update(|status| {
                            status.qmp.health = Health::Connected;
                            status.qmp.detail = "reconnected after loop error".to_string();
                            status.qmp.reconnects += 1;
                            status.qmp.last_error = None;
                            status.qmp.last_changed = Some(Instant::now());
                        });
                    }
                    Err(qmp_err) => {
                        runtime.update(|status| {
                            status.qmp.health = Health::Error;
                            status.qmp.detail = "reconnect failed".to_string();
                            status.qmp.last_error = Some(format!("{qmp_err:#}"));
                            status.qmp.last_changed = Some(Instant::now());
                        });
                    }
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

fn process_commands(
    config: &Config,
    halo: &mut HaloReader,
    last_game_time: &mut Option<i64>,
    command_rx: &Receiver<AppCommand>,
    relay_command_tx: &Sender<RelayCommand>,
    runtime: &SharedRuntime,
) -> Result<bool> {
    while let Ok(command) = command_rx.try_recv() {
        match command {
            AppCommand::Shutdown => {
                runtime.log("main", "shutdown requested");
                return Ok(true);
            }
            AppCommand::ReconnectRelay => {
                let _ = relay_command_tx.try_send(RelayCommand::ReconnectNow);
                runtime.log("relay", "manual reconnect requested");
            }
            AppCommand::ReconnectXemu => {
                runtime.log("xemu", "manual full reconnect requested");
                match connect_halo(config, runtime) {
                    Ok(new_halo) => {
                        *halo = new_halo;
                        *last_game_time = None;
                        runtime.log("xemu", "full reconnect complete");
                    }
                    Err(err) => {
                        runtime.update(|status| {
                            status.main.health = Health::Error;
                            status.main.last_error = Some(format!("{err:#}"));
                        });
                        runtime.log("xemu", format!("full reconnect failed: {err:#}"));
                    }
                }
            }
            AppCommand::ReconnectQmp => {
                runtime.log("qmp", "manual reconnect requested");
                halo.mem.clear_translations();
                match halo.mem.reconnect_qmp() {
                    Ok(()) => {
                        runtime.update(|status| {
                            status.qmp.health = Health::Connected;
                            status.qmp.detail = "manually reconnected".to_string();
                            status.qmp.reconnects += 1;
                            status.qmp.last_error = None;
                            status.qmp.last_changed = Some(Instant::now());
                        });
                        runtime.log("qmp", "manual reconnect complete");
                    }
                    Err(err) => {
                        runtime.update(|status| {
                            status.qmp.health = Health::Error;
                            status.qmp.detail = "manual reconnect failed".to_string();
                            status.qmp.last_error = Some(format!("{err:#}"));
                            status.qmp.last_changed = Some(Instant::now());
                        });
                        runtime.log("qmp", format!("manual reconnect failed: {err:#}"));
                    }
                }
            }
            AppCommand::ToggleReplaySaving => {
                let enabled = runtime.controls.toggle_save_replays();
                runtime.log("replay", format!("replay saving {}", on_off(enabled)));
            }
            AppCommand::ToggleSaveAllTicks => {
                let enabled = runtime.controls.toggle_save_all_ticks();
                runtime.log("replay", format!("save all ticks {}", on_off(enabled)));
            }
            AppCommand::ToggleReplayUploads => {
                let enabled = runtime.controls.toggle_replay_uploads();
                runtime.log("replay", format!("replay uploads {}", on_off(enabled)));
            }
            AppCommand::ClearLogs => {
                runtime.clear_logs();
            }
        }
    }
    Ok(false)
}

#[allow(clippy::too_many_arguments)]
fn tick_once(
    halo: &mut HaloReader,
    replay_tx: &Sender<Value>,
    local_ws_tx: &Sender<Value>,
    relay_tx: &Sender<Value>,
    runtime: &SharedRuntime,
    counter: &mut u64,
    last_game_time: &mut Option<i64>,
    last_real_time: &mut Instant,
    last_post_steps: &mut f64,
    benchmark_tick_count: &mut u64,
    benchmark_loop_count: &mut u64,
    last_game_info: &mut Option<Value>,
    events: &mut Vec<Value>,
    dropped_ticks_total: &mut i64,
    last_metrics_print: &mut Instant,
    resource_sampler: &mut ProcessResourceSampler,
) -> Result<()> {
    let game_time = halo.read_loop_game_time()?;
    *benchmark_loop_count += 1;
    *counter += 1;
    if let Some(usage) = resource_sampler.sample_if_due(Duration::from_secs(1)) {
        update_resource_status(runtime, &usage);
    }

    if Some(game_time) != *last_game_time {
        let previous_game_time = *last_game_time;
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
            runtime.log(
                "main",
                format!("mismatched game time expected {game_time}, got {extracted_game_time}"),
            );
        }

        let game_info_time = real_time.elapsed().as_secs_f64() * 1000.0;
        if game_info_time > 33.0 {
            runtime.log(
                "perf",
                format!("update took longer than one tick: {game_info_time:.3}ms"),
            );
        }
        if let Some(previous_game_time) = previous_game_time {
            if game_time > previous_game_time + 1 {
                let missed = game_time - previous_game_time - 1;
                *dropped_ticks_total += missed;
                runtime.log(
                    "perf",
                    format!("missed {missed} ticks between {previous_game_time} and {game_time}"),
                );
            }
        } else {
            runtime.log("perf", format!("dropped tick baseline set at {game_time}"));
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
        let resource_usage = resource_sampler.latest();
        game_info.as_object_mut().unwrap().insert(
            "performance".to_string(),
            json!({
                "game_info_time": game_info_time,
                "loop_time": loop_time,
                "post_steps_ms": *last_post_steps,
                "memory_mbytes": resource_usage.app_private_mbytes,
                "app_cpu_percent": resource_usage.app_cpu_percent,
                "app_cpu_cores": resource_usage.app_cpu_cores,
                "app_working_set_mbytes": resource_usage.app_working_set_mbytes,
                "app_private_mbytes": resource_usage.app_private_mbytes,
                "app_pagefile_mbytes": resource_usage.app_pagefile_mbytes,
            }),
        );

        *last_real_time = real_time;
        let post_steps_start = Instant::now();
        let _ = replay_tx.try_send(game_info.clone());
        let _ = local_ws_tx.try_send(game_info.clone());
        let _ = relay_tx.try_send(game_info.clone());
        *last_game_info = Some(game_info);
        *last_post_steps = post_steps_start.elapsed().as_secs_f64() * 1000.0;
        update_main_status(
            runtime,
            halo,
            last_game_info.as_ref().unwrap(),
            game_time,
            game_info_time,
            loop_time,
            *last_post_steps,
            &resource_usage,
            *benchmark_loop_count,
            *benchmark_tick_count,
            *dropped_ticks_total,
        );

        if last_metrics_print.elapsed() >= Duration::from_secs(5) {
            let loops_per_tick = if *benchmark_tick_count > 0 {
                *benchmark_loop_count as f64 / *benchmark_tick_count as f64
            } else {
                0.0
            };
            runtime.log(
                "metrics",
                format!(
                    "metrics: tick={game_time} game_info_ms={game_info_time:.3} loop_ms={loop_time:.3} post_ms={:.3} loops_per_tick={loops_per_tick:.1} dropped_total={}",
                    *last_post_steps,
                    *dropped_ticks_total
                ),
            );
            *last_metrics_print = Instant::now();
        }
    }

    *last_game_time = Some(game_time);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn update_main_status(
    runtime: &SharedRuntime,
    halo: &HaloReader,
    game_info: &Value,
    game_time: i64,
    game_info_ms: f64,
    loop_ms: f64,
    post_steps_ms: f64,
    resource_usage: &AppResourceUsage,
    loop_count: u64,
    tick_count: u64,
    dropped_ticks_total: i64,
) {
    let loops_per_tick = if tick_count > 0 {
        loop_count as f64 / tick_count as f64
    } else {
        0.0
    };
    runtime.update(|status| {
        status.main.health = Health::Running;
        status.main.game_time = game_time;
        status.main.loop_count = loop_count;
        status.main.tick_count = tick_count;
        status.main.loops_per_tick = loops_per_tick;
        status.main.dropped_ticks_total = dropped_ticks_total;
        status.main.game_info_ms = game_info_ms;
        status.main.loop_ms = loop_ms;
        status.main.post_steps_ms = post_steps_ms;
        status.main.memory_mbytes = resource_usage.app_private_mbytes;
        status.main.app_cpu_percent = resource_usage.app_cpu_percent;
        status.main.app_cpu_cores = resource_usage.app_cpu_cores;
        status.main.app_working_set_mbytes = resource_usage.app_working_set_mbytes;
        status.main.app_private_mbytes = resource_usage.app_private_mbytes;
        status.main.app_pagefile_mbytes = resource_usage.app_pagefile_mbytes;
        status.main.read_count = halo.mem.read_counter;
        status.main.game_id = string_field(game_info, "game_id");
        status.main.map_name = string_field(game_info, "multiplayer_map_name");
        status.main.game_status = game_status(game_info).to_string();
        status.main.player_count = game_info
            .get("players")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0);
        status.main.event_count = game_info
            .get("events")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0);
        status.main.last_error = None;
        status.main.last_tick_at = Some(Instant::now());
    });
}

fn update_resource_status(runtime: &SharedRuntime, usage: &AppResourceUsage) {
    runtime.update(|status| {
        status.main.memory_mbytes = usage.app_private_mbytes;
        status.main.app_cpu_percent = usage.app_cpu_percent;
        status.main.app_cpu_cores = usage.app_cpu_cores;
        status.main.app_working_set_mbytes = usage.app_working_set_mbytes;
        status.main.app_private_mbytes = usage.app_private_mbytes;
        status.main.app_pagefile_mbytes = usage.app_pagefile_mbytes;
    });
}

fn string_field(value: &Value, field: &str) -> String {
    value
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn game_status(game_info: &Value) -> &'static str {
    if game_info
        .get("game_ended_this_tick")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        "ended"
    } else if game_info
        .get("game_engine_can_score")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        "live"
    } else if game_info
        .get("game_engine_running")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        "postgame"
    } else {
        "stale"
    }
}

fn on_off(enabled: bool) -> &'static str {
    if enabled { "enabled" } else { "disabled" }
}

#[derive(Clone, Copy, Debug, Default)]
struct AppResourceUsage {
    app_cpu_percent: f64,
    app_cpu_cores: f64,
    app_working_set_mbytes: f64,
    app_private_mbytes: f64,
    app_pagefile_mbytes: f64,
}

#[derive(Debug)]
struct ProcessResourceSampler {
    last_sample_at: Instant,
    last_cpu_time_100ns: Option<u64>,
    logical_cpu_count: f64,
    latest: AppResourceUsage,
}

impl ProcessResourceSampler {
    fn new() -> Self {
        Self {
            last_sample_at: Instant::now(),
            last_cpu_time_100ns: current_process_cpu_time_100ns(),
            logical_cpu_count: std::thread::available_parallelism()
                .map(|count| count.get() as f64)
                .unwrap_or(1.0)
                .max(1.0),
            latest: current_process_resource_usage(),
        }
    }

    fn sample_if_due(&mut self, interval: Duration) -> Option<AppResourceUsage> {
        if self.last_sample_at.elapsed() >= interval {
            Some(self.sample())
        } else {
            None
        }
    }

    fn latest(&self) -> AppResourceUsage {
        self.latest
    }

    fn sample(&mut self) -> AppResourceUsage {
        let now = Instant::now();
        let mut usage = current_process_resource_usage();
        if let Some(cpu_time_100ns) = current_process_cpu_time_100ns() {
            if let Some(last_cpu_time_100ns) = self.last_cpu_time_100ns {
                let elapsed = now.duration_since(self.last_sample_at).as_secs_f64();
                if elapsed > 0.0 && cpu_time_100ns >= last_cpu_time_100ns {
                    let cpu_seconds = (cpu_time_100ns - last_cpu_time_100ns) as f64 / 10_000_000.0;
                    usage.app_cpu_cores = cpu_seconds / elapsed;
                    usage.app_cpu_percent = usage.app_cpu_cores / self.logical_cpu_count * 100.0;
                } else {
                    usage.app_cpu_cores = self.latest.app_cpu_cores;
                    usage.app_cpu_percent = self.latest.app_cpu_percent;
                }
            }
            self.last_cpu_time_100ns = Some(cpu_time_100ns);
        } else {
            usage.app_cpu_cores = self.latest.app_cpu_cores;
            usage.app_cpu_percent = self.latest.app_cpu_percent;
        }
        self.last_sample_at = now;
        self.latest = usage;
        usage
    }
}

fn current_process_resource_usage() -> AppResourceUsage {
    let mut usage = current_process_memory_usage();
    usage.app_cpu_percent = 0.0;
    usage.app_cpu_cores = 0.0;
    usage
}

fn current_process_memory_usage() -> AppResourceUsage {
    unsafe {
        let mut counters = std::mem::zeroed::<PROCESS_MEMORY_COUNTERS_EX>();
        counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        let ok = K32GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut counters as *mut PROCESS_MEMORY_COUNTERS_EX as *mut PROCESS_MEMORY_COUNTERS,
            std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        );
        if ok == 0 {
            AppResourceUsage::default()
        } else {
            AppResourceUsage {
                app_cpu_percent: 0.0,
                app_cpu_cores: 0.0,
                app_working_set_mbytes: bytes_to_mbytes(counters.WorkingSetSize),
                app_private_mbytes: bytes_to_mbytes(counters.PrivateUsage),
                app_pagefile_mbytes: bytes_to_mbytes(counters.PagefileUsage),
            }
        }
    }
}

fn current_process_cpu_time_100ns() -> Option<u64> {
    unsafe {
        let mut creation_time = std::mem::zeroed::<FILETIME>();
        let mut exit_time = std::mem::zeroed::<FILETIME>();
        let mut kernel_time = std::mem::zeroed::<FILETIME>();
        let mut user_time = std::mem::zeroed::<FILETIME>();
        let ok = GetProcessTimes(
            GetCurrentProcess(),
            &mut creation_time,
            &mut exit_time,
            &mut kernel_time,
            &mut user_time,
        );
        if ok == 0 {
            None
        } else {
            Some(filetime_to_u64(kernel_time) + filetime_to_u64(user_time))
        }
    }
}

fn filetime_to_u64(filetime: FILETIME) -> u64 {
    ((filetime.dwHighDateTime as u64) << 32) | filetime.dwLowDateTime as u64
}

fn bytes_to_mbytes(bytes: usize) -> f64 {
    bytes as f64 / 1024.0 / 1024.0
}
