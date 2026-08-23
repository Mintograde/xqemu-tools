mod config;
mod events;
mod halo;
mod memory;
mod process;
mod qmp;
mod replay;
mod runtime;
mod tui;
mod update;
mod util;
mod ws;

use anyhow::{Context, Result};
use config::{Config, ConfigKey};
use crossbeam_channel::{Receiver, Sender, unbounded};
use halo::HaloReader;
use memory::MemoryReader;
use qmp::QmpClient;
use runtime::{
    AppCommand, CommandRequest, GameDetailStatus, Health, PipelineEdge, PlayerStatus, RelayCommand,
    RuntimeState, SharedRuntime,
};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::FILETIME;
use windows_sys::Win32::System::ProcessStatus::{
    K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

fn main() -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let relaunch_executable = std::env::current_exe().ok();
    let relaunch_arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    let config = Config::load()?;
    let runtime = RuntimeState::new(config.clone());
    let (command_tx, command_rx) = unbounded();
    let (update_tx, update_rx) = unbounded();
    let update_worker = update::start_update_worker(config.clone(), update_rx, runtime.clone());
    let tui = tui::start_tui(runtime.clone(), command_tx);
    runtime.log("main", "starting runtime");
    let mut halo = match connect_halo(&config, &runtime, &command_rx, &update_tx) {
        Ok(Some(halo)) => halo,
        Ok(None) => {
            finish_background_workers(&runtime, tui, update_worker, None, Vec::new());
            relaunch_if_requested(&runtime, relaunch_executable, &relaunch_arguments)?;
            return Ok(());
        }
        Err(err) => {
            finish_background_workers(&runtime, tui, update_worker, None, Vec::new());
            return Err(err);
        }
    };

    let (replay_tx, replay_rx) = unbounded();
    let (local_ws_tx, local_ws_rx) = unbounded();
    let (relay_tx, relay_rx) = unbounded();
    let (relay_command_tx, relay_command_rx) = unbounded();
    let replay_worker =
        replay::start_replay_worker(config.clone(), replay_rx, relay_tx.clone(), runtime.clone());
    let local_ws_server = ws::start_local_ws_server(config.clone(), local_ws_rx, runtime.clone());
    let relay_client = if config.ws_relay_enabled {
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

    let main_result = main_loop(
        &config,
        &mut halo,
        replay_tx,
        local_ws_tx,
        relay_tx,
        command_rx,
        relay_command_tx,
        update_tx,
        runtime.clone(),
    );
    let mut network_workers = vec![local_ws_server];
    if let Some(relay_client) = relay_client {
        network_workers.push(relay_client);
    }
    finish_background_workers(
        &runtime,
        tui,
        update_worker,
        Some(replay_worker),
        network_workers,
    );
    relaunch_if_requested(&runtime, relaunch_executable, &relaunch_arguments)?;
    main_result
}

fn finish_background_workers(
    runtime: &SharedRuntime,
    tui: std::thread::JoinHandle<()>,
    update_worker: std::thread::JoinHandle<()>,
    replay_worker: Option<std::thread::JoinHandle<()>>,
    network_workers: Vec<std::thread::JoinHandle<()>>,
) {
    runtime.request_shutdown();
    if replay_worker.is_some_and(|worker| worker.join().is_err()) {
        runtime.log("replay", "worker panicked during shutdown");
    }
    if update_worker.join().is_err() {
        runtime.log("update", "worker panicked during shutdown");
    }
    for worker in network_workers {
        if worker.join().is_err() {
            runtime.log("network", "worker panicked during shutdown");
        }
    }
    if tui.join().is_err() {
        runtime.log("tui", "worker panicked during shutdown");
    }
}

fn relaunch_if_requested(
    runtime: &SharedRuntime,
    executable: Option<std::path::PathBuf>,
    arguments: &[std::ffi::OsString],
) -> Result<()> {
    if !runtime.restart_requested() {
        return Ok(());
    }
    let executable = executable.context("cannot restart because the executable path is unknown")?;
    std::process::Command::new(&executable)
        .args(arguments)
        .spawn()
        .with_context(|| format!("failed to restart {}", executable.display()))?;
    Ok(())
}

fn connect_halo(
    config: &Config,
    runtime: &SharedRuntime,
    command_rx: &Receiver<CommandRequest>,
    update_tx: &Sender<CommandRequest>,
) -> Result<Option<HaloReader>> {
    runtime.update(|status| {
        status.xemu.health = Health::Starting;
        status.xemu.detail = "waiting for xemu.exe".to_string();
        status.xemu.last_changed = Some(Instant::now());
    });
    runtime.log("xemu", "waiting for xemu.exe");
    let xemu = loop {
        if process_connect_commands(command_rx, update_tx, runtime) {
            return Ok(None);
        }
        if let Some(process) = process::find_xemu_process() {
            runtime.update(|status| {
                status.xemu.health = Health::Running;
                status.xemu.pid = Some(process.pid);
                status.xemu.detail = "process found".to_string();
                status.xemu.last_error = None;
                status.xemu.last_changed = Some(Instant::now());
            });
            runtime.log(
                "xemu",
                format!("attached to pid {} ({:#x})", process.pid, process.pid),
            );
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
    let qmp =
        match QmpClient::connect_with_retry_until(config.qmp_host.clone(), config.qmp_port, || {
            process_connect_commands(command_rx, update_tx, runtime)
        }) {
            Ok(Some(qmp)) => {
                runtime.update(|status| {
                    status.qmp.health = Health::Connected;
                    status.qmp.detail = "connected".to_string();
                    status.qmp.last_error = None;
                    status.qmp.last_changed = Some(Instant::now());
                });
                runtime.log("qmp", format!("connected to {qmp_endpoint}"));
                qmp
            }
            Ok(None) => return Ok(None),
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
    Ok(Some(halo))
}

fn process_connect_commands(
    command_rx: &Receiver<CommandRequest>,
    update_tx: &Sender<CommandRequest>,
    runtime: &SharedRuntime,
) -> bool {
    let mut shutdown = false;
    while let Ok(request) = command_rx.try_recv() {
        if matches!(
            &request.command,
            AppCommand::CheckForUpdates | AppCommand::InstallUpdate
        ) {
            forward_update_command(request, update_tx, runtime);
            continue;
        }
        runtime.start_command(request.id);
        if matches!(request.command, AppCommand::Shutdown) {
            runtime.request_shutdown();
            runtime.log("main", "shutdown requested while connecting");
            runtime.finish_command(request.id, "shutdown accepted");
            shutdown = true;
        } else {
            runtime.fail_command(
                request.id,
                "command is unavailable while xemu and QMP are connecting",
            );
        }
    }
    shutdown || runtime.shutdown_requested()
}

fn forward_update_command(
    request: CommandRequest,
    update_tx: &Sender<CommandRequest>,
    runtime: &SharedRuntime,
) {
    let command_id = request.id;
    if update_tx.send(request).is_err() {
        runtime.fail_command(command_id, "update worker is not available");
    }
}

#[allow(clippy::too_many_arguments)]
fn main_loop(
    config: &Config,
    halo: &mut HaloReader,
    replay_tx: Sender<Value>,
    local_ws_tx: Sender<Value>,
    relay_tx: Sender<Value>,
    command_rx: Receiver<CommandRequest>,
    relay_command_tx: Sender<RelayCommand>,
    update_tx: Sender<CommandRequest>,
    runtime: SharedRuntime,
) -> Result<()> {
    let mut counter = 0u64;
    let mut last_game_time: Option<i64> = None;
    let mut last_real_time = Instant::now();
    let mut last_post_steps = 0.0f64;
    let mut benchmark_tick_count = 0u64;
    let mut benchmark_loop_count = 0u64;
    let mut last_game_info: Option<Arc<Value>> = None;
    let mut events: Vec<Value> = Vec::new();
    let mut last_metrics_print = Instant::now();
    let mut dropped_ticks_total = 0i64;
    let mut resource_sampler = ProcessResourceSampler::new();

    loop {
        if runtime.shutdown_requested() {
            return Ok(());
        }
        if process_commands(
            config,
            halo,
            &mut last_game_time,
            &command_rx,
            &relay_command_tx,
            &update_tx,
            &runtime,
        )? {
            return Ok(());
        }

        match tick_once(
            halo,
            &replay_tx,
            &local_ws_tx,
            &relay_tx,
            config.ws_relay_enabled,
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
    command_rx: &Receiver<CommandRequest>,
    relay_command_tx: &Sender<RelayCommand>,
    update_tx: &Sender<CommandRequest>,
    runtime: &SharedRuntime,
) -> Result<bool> {
    while let Ok(request) = command_rx.try_recv() {
        if matches!(
            &request.command,
            AppCommand::CheckForUpdates | AppCommand::InstallUpdate
        ) {
            forward_update_command(request, update_tx, runtime);
            continue;
        }
        let command_id = request.id;
        runtime.start_command(command_id);
        match request.command {
            AppCommand::Shutdown => {
                runtime.request_shutdown();
                runtime.log("main", "shutdown requested");
                runtime.finish_command(command_id, "shutdown accepted");
                return Ok(true);
            }
            AppCommand::ReconnectRelay => {
                if relay_command_tx
                    .try_send(RelayCommand::ReconnectNow)
                    .is_ok()
                {
                    runtime.log("relay", "manual reconnect requested");
                    runtime.finish_command(command_id, "reconnect requested");
                } else {
                    runtime.fail_command(command_id, "relay worker is not available");
                }
            }
            AppCommand::ReconnectXemu => {
                runtime.log("xemu", "manual full reconnect requested");
                match connect_halo(config, runtime, command_rx, update_tx) {
                    Ok(Some(new_halo)) => {
                        *halo = new_halo;
                        *last_game_time = None;
                        runtime.log("xemu", "full reconnect complete");
                        runtime.finish_command(command_id, "xemu and QMP reconnected");
                    }
                    Ok(None) => {
                        runtime.fail_command(command_id, "reconnect interrupted by shutdown");
                        return Ok(true);
                    }
                    Err(err) => {
                        runtime.update(|status| {
                            status.main.health = Health::Error;
                            status.main.last_error = Some(format!("{err:#}"));
                        });
                        runtime.log("xemu", format!("full reconnect failed: {err:#}"));
                        runtime.fail_command(command_id, format!("{err:#}"));
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
                        runtime.finish_command(command_id, "QMP reconnected");
                    }
                    Err(err) => {
                        runtime.update(|status| {
                            status.qmp.health = Health::Error;
                            status.qmp.detail = "manual reconnect failed".to_string();
                            status.qmp.last_error = Some(format!("{err:#}"));
                            status.qmp.last_changed = Some(Instant::now());
                        });
                        runtime.log("qmp", format!("manual reconnect failed: {err:#}"));
                        runtime.fail_command(command_id, format!("{err:#}"));
                    }
                }
            }
            AppCommand::ToggleReplaySaving => {
                let enabled = !runtime.controls.save_replays();
                match runtime.update_config_value(ConfigKey::SaveReplays, &enabled.to_string()) {
                    Ok(_) => {
                        runtime.log("replay", format!("replay saving {}", on_off(enabled)));
                        runtime.finish_command(
                            command_id,
                            format!("replay saving {}", on_off(enabled)),
                        );
                    }
                    Err(err) => runtime.fail_command(command_id, format!("{err:#}")),
                }
            }
            AppCommand::ToggleSaveAllTicks => {
                let enabled = !runtime.controls.save_all_ticks();
                match runtime.update_config_value(ConfigKey::SaveAllTicks, &enabled.to_string()) {
                    Ok(_) => {
                        runtime.log("replay", format!("save all ticks {}", on_off(enabled)));
                        runtime
                            .finish_command(command_id, format!("tick saving {}", on_off(enabled)));
                    }
                    Err(err) => runtime.fail_command(command_id, format!("{err:#}")),
                }
            }
            AppCommand::ToggleReplayUploads => {
                let enabled = !runtime.controls.replay_uploads();
                match runtime.update_config_value(ConfigKey::ReplayUploads, &enabled.to_string()) {
                    Ok(_) => {
                        runtime.log("replay", format!("replay uploads {}", on_off(enabled)));
                        runtime.finish_command(command_id, format!("uploads {}", on_off(enabled)));
                    }
                    Err(err) => runtime.fail_command(command_id, format!("{err:#}")),
                }
            }
            AppCommand::ClearLogs => {
                runtime.clear_logs();
                runtime.finish_command(command_id, "logs cleared");
            }
            AppCommand::SetConfigValue { key, value } => {
                match runtime.update_config_value(key, &value) {
                    Ok((path, restart_required)) => {
                        let detail = if restart_required {
                            format!("saved to {path}; application restart required")
                        } else {
                            format!("saved and applied from {path}")
                        };
                        runtime.log("config", detail.clone());
                        runtime.finish_command(command_id, detail);
                    }
                    Err(err) => runtime.fail_command(command_id, format!("{err:#}")),
                }
            }
            AppCommand::ReloadConfig => match runtime.reload_config() {
                Ok(pending) => {
                    let detail = if pending.is_empty() {
                        "configuration reloaded and applied".to_string()
                    } else {
                        format!(
                            "configuration reloaded; restart required for {}",
                            pending
                                .iter()
                                .map(|key| key.label())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    };
                    runtime.log("config", detail.clone());
                    runtime.finish_command(command_id, detail);
                }
                Err(err) => runtime.fail_command(command_id, format!("{err:#}")),
            },
            AppCommand::RetryUpload(request_id) => {
                if relay_command_tx
                    .try_send(RelayCommand::RetryUpload(request_id))
                    .is_ok()
                {
                    runtime.finish_command(command_id, "upload retry requested");
                } else {
                    runtime.fail_command(command_id, "relay worker is not available");
                }
            }
            AppCommand::CancelUpload(request_id) => {
                if relay_command_tx
                    .try_send(RelayCommand::CancelUpload(request_id))
                    .is_ok()
                {
                    runtime.finish_command(command_id, "upload cancellation requested");
                } else {
                    runtime.fail_command(command_id, "relay worker is not available");
                }
            }
            AppCommand::DisconnectClient(address) => {
                runtime.request_client_disconnect(address.clone());
                runtime.finish_command(command_id, format!("disconnect requested for {address}"));
            }
            AppCommand::DiscardReplay => {
                runtime.request_replay_discard();
                runtime.finish_command(command_id, "replay discard requested");
            }
            AppCommand::CheckForUpdates | AppCommand::InstallUpdate => unreachable!(),
        }
    }
    Ok(runtime.shutdown_requested())
}

#[allow(clippy::too_many_arguments)]
fn tick_once(
    halo: &mut HaloReader,
    replay_tx: &Sender<Value>,
    local_ws_tx: &Sender<Value>,
    relay_tx: &Sender<Value>,
    relay_enabled: bool,
    runtime: &SharedRuntime,
    counter: &mut u64,
    last_game_time: &mut Option<i64>,
    last_real_time: &mut Instant,
    last_post_steps: &mut f64,
    benchmark_tick_count: &mut u64,
    benchmark_loop_count: &mut u64,
    last_game_info: &mut Option<Arc<Value>>,
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
        let game_info = Arc::new(game_info);
        enqueue_pipeline_value(
            replay_tx,
            game_info.as_ref().clone(),
            PipelineEdge::Replay,
            runtime,
        );
        enqueue_pipeline_value(
            local_ws_tx,
            game_info.as_ref().clone(),
            PipelineEdge::LocalWebSocket,
            runtime,
        );
        if relay_enabled {
            enqueue_pipeline_value(
                relay_tx,
                game_info.as_ref().clone(),
                PipelineEdge::Relay,
                runtime,
            );
        }
        runtime.set_latest_game_info(game_info.clone());
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

fn enqueue_pipeline_value(
    sender: &Sender<Value>,
    value: Value,
    edge: PipelineEdge,
    runtime: &SharedRuntime,
) {
    let accepted = sender.try_send(value).is_ok();
    runtime.record_pipeline_enqueue(edge, sender.len(), accepted);
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
    let game_detail = game_detail_status(game_info);
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
        status.game = game_detail;
    });
}

fn game_detail_status(game_info: &Value) -> GameDetailStatus {
    let players = game_info
        .get("players")
        .and_then(Value::as_array)
        .map(|players| {
            players
                .iter()
                .map(|player| {
                    let object = player.get("player_object_data");
                    PlayerStatus {
                        index: int_field(player, "player_index"),
                        name: string_field(player, "name"),
                        team: int_field(player, "team"),
                        score: int_field(player, "score"),
                        kills: int_field(player, "kills"),
                        deaths: int_field(player, "deaths"),
                        assists: int_field(player, "assists"),
                        shots_fired: int_field(player, "shots_fired"),
                        shots_hit: int_field(player, "shots_hit"),
                        quit: int_field(player, "player_quit") != 0,
                        has_camo: bool_path(player, &["derived_stats", "has_camo"]),
                        has_overshield: bool_path(player, &["derived_stats", "has_overshield"]),
                        health: object
                            .and_then(|value| value.get("health"))
                            .and_then(Value::as_f64)
                            .unwrap_or(0.0),
                        shields: object
                            .and_then(|value| value.get("shields"))
                            .and_then(Value::as_f64)
                            .unwrap_or(0.0),
                        position: [
                            object
                                .and_then(|value| value.get("x"))
                                .and_then(Value::as_f64)
                                .unwrap_or(0.0),
                            object
                                .and_then(|value| value.get("y"))
                                .and_then(Value::as_f64)
                                .unwrap_or(0.0),
                            object
                                .and_then(|value| value.get("z"))
                                .and_then(Value::as_f64)
                                .unwrap_or(0.0),
                        ],
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let recent_events = game_info
        .get("events")
        .and_then(Value::as_array)
        .map(|events| {
            events
                .iter()
                .rev()
                .take(25)
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect()
        })
        .unwrap_or_default();
    GameDetailStatus {
        game_type: display_value(game_info.get("game_type")),
        variant: display_value(game_info.get("variant")),
        stage: string_field(game_info, "global_stage"),
        has_teams: int_field(game_info, "game_engine_has_teams") != 0,
        local_player_count: int_field(game_info, "local_player_count").max(0) as usize,
        object_count: array_len(game_info, "objects"),
        item_count: array_len(game_info, "items"),
        spawn_count: array_len(game_info, "spawns"),
        players,
        recent_events,
    }
}

fn int_field(value: &Value, field: &str) -> i64 {
    value.get(field).and_then(Value::as_i64).unwrap_or(0)
}

fn bool_path(value: &Value, path: &[&str]) -> bool {
    path.iter()
        .try_fold(value, |current, key| current.get(*key))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn array_len(value: &Value, field: &str) -> usize {
    value
        .get(field)
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0)
}

fn display_value(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(value)) => value.clone(),
        Some(Value::Null) | None => String::new(),
        Some(value) => value.to_string(),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use runtime::CommandPhase;

    #[test]
    fn shutdown_is_processed_while_waiting_for_connections() {
        let runtime = RuntimeState::new(Config::default());
        let (sender, receiver) = unbounded();
        let (update_sender, _update_receiver) = unbounded();
        let request = runtime.queue_command(AppCommand::Shutdown);
        sender.send(request).unwrap();

        assert!(process_connect_commands(
            &receiver,
            &update_sender,
            &runtime
        ));
        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.commands[0].phase, CommandPhase::Succeeded);
        assert!(runtime.shutdown_requested());
    }

    #[test]
    fn update_commands_are_available_while_waiting_for_connections() {
        let runtime = RuntimeState::new(Config::default());
        let (sender, receiver) = unbounded();
        let (update_sender, update_receiver) = unbounded();
        let request = runtime.queue_command(AppCommand::CheckForUpdates);
        let command_id = request.id;
        sender.send(request).unwrap();

        assert!(!process_connect_commands(
            &receiver,
            &update_sender,
            &runtime
        ));
        let forwarded = update_receiver.try_recv().unwrap();
        assert_eq!(forwarded.id, command_id);
        assert!(matches!(forwarded.command, AppCommand::CheckForUpdates));
        assert_eq!(runtime.snapshot().commands[0].phase, CommandPhase::Queued);
    }

    #[test]
    fn game_detail_snapshot_extracts_player_and_entity_summaries() {
        let detail = game_detail_status(&json!({
            "game_type": "slayer",
            "variant": 2,
            "global_stage": "bloodgulch",
            "game_engine_has_teams": 1,
            "local_player_count": 1,
            "objects": [{}, {}],
            "items": [{}],
            "spawns": [{}, {}, {}],
            "events": ["one", "two"],
            "players": [{
                "player_index": 0,
                "name": "Player",
                "team": 1,
                "score": 10,
                "kills": 4,
                "deaths": 2,
                "assists": 1,
                "shots_fired": 20,
                "shots_hit": 10,
                "derived_stats": {"has_camo": true, "has_overshield": false},
                "player_object_data": {"health": 0.75, "shields": 1.0, "x": 1.0, "y": 2.0, "z": 3.0},
            }],
        }));
        assert_eq!(detail.players.len(), 1);
        assert_eq!(detail.players[0].kills, 4);
        assert_eq!(detail.players[0].position, [1.0, 2.0, 3.0]);
        assert_eq!(detail.object_count, 2);
        assert_eq!(detail.spawn_count, 3);
        assert_eq!(detail.recent_events, vec!["one", "two"]);
    }
}
