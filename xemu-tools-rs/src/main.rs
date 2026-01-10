mod memory;
mod process;
mod qmp;
mod halo;
mod ws;
mod ws_server;
mod ui;
mod events;
mod replay;
mod session;

use anyhow::Result;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::time::sleep;

use crate::halo::HaloState;
use crate::memory::MemoryReader;
use crate::process::find_xemu_process;
use crate::qmp::QmpClient;
use crate::ui::{MyApp, UiState};
use crate::ws::WsClient;
use crate::ws_server::WsServer;
use crate::session::SessionContext;

fn main() -> Result<()> {
    env_logger::init();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let (ui_tx, ui_rx) = mpsc::channel(10);
    let (ws_tx, ws_rx) = mpsc::channel(100);

    let ws_url = "ws://localhost:9000/ws/test-room?role=producer&compress_messages=true&compress_messages_binary=true&buffer_messages=true".to_string();
    rt.spawn(async move {
        let client = WsClient::new(ws_url, ws_rx);
        client.run().await;
    });

    let broadcast_server = Arc::new(WsServer::new());
    let broadcast_server_clone = broadcast_server.clone();
    rt.spawn(async move {
        broadcast_server_clone.run(9001).await;
    });

    rt.spawn(async move {
        loop {
            println!("Looking for Xemu...");
            let process_info = match find_xemu_process() {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("Xemu not found: {}. Retrying...", e);
                    sleep(Duration::from_secs(1)).await;
                    continue;
                }
            };

            println!("Found Xemu PID: {}. Found {} QMP addresses.", process_info.pid, process_info.qmp_addrs.len());

            let mut qmp_client = None;
            for addr in &process_info.qmp_addrs {
                match QmpClient::connect(&addr.host, addr.port).await {
                    Ok(c) => {
                        println!("Connected to QMP at {}:{}.", addr.host, addr.port);
                        qmp_client = Some(Arc::new(c));
                        break;
                    }
                    Err(e) => eprintln!("Failed to connect to QMP at {}:{}: {}", addr.host, addr.port, e),
                }
            }

            let qmp = match qmp_client {
                Some(q) => q,
                None => {
                    sleep(Duration::from_secs(1)).await;
                    continue;
                }
            };

            let reader = match MemoryReader::new(process_info.pid, qmp.clone()) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("Failed to attach: {}. Retrying...", e);
                    sleep(Duration::from_secs(1)).await;
                    continue;
                }
            };

            let mut halo = HaloState::new(reader);
            let mut session = SessionContext::new(process_info.pid);
            let mut last_raw_time: u32 = 0;

            println!("Attached. Polling...");
            loop {
                // 1. Polling
                let current_raw_gt = match halo.get_game_time_uncached().await {
                    Ok(val) => val,
                    Err(_) => break,
                };
                if current_raw_gt == last_raw_time {
                    sleep(Duration::from_millis(1)).await;
                    continue;
                }
                last_raw_time = current_raw_gt;

                // 2. Data Extraction
                let read_start = Instant::now();
                halo.populate_memory_cache().await.ok();
                let tick_data = match halo.get_game_info().await {
                    Ok(td) => td,
                    Err(_) => {
                        halo.invalidate_memory_cache();
                        continue;
                    }
                };
                halo.invalidate_memory_cache();
                let read_duration = read_start.elapsed();

                // 3. Post-processing
                let processed = match session.process_tick(tick_data.clone(), read_duration) {
                    Ok(p) => p,
                    Err(_) => continue,
                };

                // 4. Broadcasting & UI
                let raw_json = serde_json::to_string_pretty(&processed.live_msg).unwrap_or_default();
                broadcast_server.broadcast(raw_json.clone());

                let ui_state = UiState {
                    game_time: Some(tick_data.game_time_info),
                    objects: tick_data.objects,
                    players: tick_data.players,
                    spawns: tick_data.spawns,
                    items: tick_data.items,
                    fog: tick_data.fog,
                    team_scores: tick_data.team_scores,
                    network_client: tick_data.network_client,
                    input: tick_data.local_input,
                    camera: tick_data.local_camera,
                    performance: processed.perf,
                    events: processed.new_events,
                    raw_json,
                };

                let _ = ui_tx.send(ui_state).await;
                let _ = ws_tx.try_send(processed.live_msg);
            }
            sleep(Duration::from_secs(1)).await;
        }
    });

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default().with_inner_size([1024.0, 768.0]),
        ..Default::default()
    };
    
    eframe::run_native(
        "Xemu Tools Rust",
        options,
        Box::new(|cc| Ok(Box::new(MyApp::new(cc, ui_rx)))),
    ).map_err(|e| anyhow::anyhow!("Eframe error: {}", e))?;

    Ok(())
}