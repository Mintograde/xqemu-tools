use crate::config::Config;
use crate::runtime::{
    Health, LocalWsClientStatus, PipelineEdge, RelayCommand, ReplayUploadStatus, SharedRuntime,
    UploadPhase,
};
use crate::util::py_datetime_to_iso;
use anyhow::{Context, Result, anyhow};
use crossbeam_channel::{Receiver, Sender};
use futures_util::{SinkExt, StreamExt};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::{accept_async, connect_async, tungstenite::Message};

const LIVE_STATUS_TICKS_PER_SECOND: f64 = 30.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RelayLoopExit {
    Disconnected,
    ReconnectRequested,
}

#[derive(Debug, Default)]
struct RelayState {
    producer_key: Option<String>,
    require_key: bool,
    always_include_key: bool,
    replay_uploads: HashMap<String, PendingReplayUpload>,
    max_replay_upload_retries: u8,
}

#[derive(Clone, Debug)]
struct PendingReplayUpload {
    path: PathBuf,
    payload: Value,
    attempts: u8,
}

pub fn start_local_ws_server(
    config: Config,
    receiver: Receiver<Value>,
    runtime: SharedRuntime,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let tokio_runtime = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
        tokio_runtime.block_on(async move {
            if let Err(err) = run_local_ws_server(config, receiver, runtime.clone()).await {
                runtime.update(|status| {
                    status.local_ws.health = Health::Error;
                    status.local_ws.last_error = Some(format!("{err:#}"));
                    status.local_ws.last_changed = Some(Instant::now());
                });
                runtime.log("local_ws", format!("server failed: {err:#}"));
            }
        });
    })
}

pub fn start_relay_client(
    config: Config,
    receiver: Receiver<Value>,
    sender: Sender<Value>,
    control_receiver: Receiver<RelayCommand>,
    runtime: SharedRuntime,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let tokio_runtime = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
        tokio_runtime.block_on(async move {
            if let Err(err) =
                run_relay_client(config, receiver, sender, control_receiver, runtime.clone()).await
            {
                runtime.update(|status| {
                    status.relay.health = Health::Error;
                    status.relay.last_error = Some(format!("{err:#}"));
                    status.relay.last_changed = Some(Instant::now());
                });
                runtime.log("relay", format!("client failed: {err:#}"));
            }
        });
    })
}

async fn run_local_ws_server(
    config: Config,
    receiver: Receiver<Value>,
    runtime: SharedRuntime,
) -> Result<()> {
    let bind_addr = format!("{}:{}", config.websocket_host, config.websocket_port);
    runtime.update(|status| {
        status.local_ws.health = Health::Starting;
        status.local_ws.bind_addr = bind_addr.clone();
        status.local_ws.last_changed = Some(Instant::now());
    });
    let listener = match TcpListener::bind(&bind_addr).await {
        Ok(listener) => listener,
        Err(err) => {
            runtime.update(|status| {
                status.local_ws.health = Health::Error;
                status.local_ws.last_error = Some(format!("{err:#}"));
                status.local_ws.last_changed = Some(Instant::now());
            });
            return Err(err)
                .with_context(|| format!("failed to bind websocket server on {bind_addr}"));
        }
    };
    runtime.update(|status| {
        status.local_ws.health = Health::Running;
        status.local_ws.last_error = None;
        status.local_ws.last_changed = Some(Instant::now());
    });
    runtime.log("local_ws", format!("server started {bind_addr}"));
    let (tx, _) = broadcast::channel::<String>(64);
    let producer = tx.clone();
    let producer_runtime = runtime.clone();
    thread::spawn(move || {
        while let Ok(value) = receiver.recv() {
            producer_runtime.record_pipeline_dequeue(PipelineEdge::LocalWebSocket, receiver.len());
            match serde_json::to_string(&value) {
                Ok(message) => {
                    producer_runtime
                        .record_pipeline_bytes(PipelineEdge::LocalWebSocket, message.len() as u64);
                    let _ = producer.send(message);
                    producer_runtime.update(|status| {
                        status.local_ws.messages_sent += 1;
                    });
                }
                Err(err) => {
                    producer_runtime.update(|status| {
                        status.local_ws.last_error = Some(format!("{err:#}"));
                        status.local_ws.last_changed = Some(Instant::now());
                    });
                    producer_runtime.log("local_ws", format!("serialization failed: {err:#}"));
                }
            }
        }
    });

    loop {
        let accepted = tokio::select! {
            result = listener.accept() => Some(result?),
            _ = wait_for_shutdown(runtime.clone()) => None,
        };
        let Some((stream, address)) = accepted else {
            break;
        };
        let address_key = address.to_string();
        runtime.update(|status| {
            status.local_ws.client_count += 1;
            status.local_ws.clients.push(LocalWsClientStatus {
                address: address_key.clone(),
                connected_at: Instant::now(),
                last_sent_at: None,
                messages_sent: 0,
                bytes_sent: 0,
                lagged_messages: 0,
            });
            status.local_ws.last_changed = Some(Instant::now());
        });
        runtime.log("local_ws", format!("client connected {address}"));
        let mut rx = tx.subscribe();
        let client_runtime = runtime.clone();
        tokio::spawn(async move {
            let mut messages_sent = 0u64;
            let mut bytes_sent = 0u64;
            let mut last_reported_at = Instant::now();
            let mut control_interval = tokio::time::interval(Duration::from_millis(250));
            match accept_async(stream).await {
                Ok(mut ws) => loop {
                    let received = tokio::select! {
                        _ = control_interval.tick() => {
                            if client_runtime.shutdown_requested() {
                                break;
                            }
                            if client_runtime.take_client_disconnect_request(&address_key) {
                                client_runtime.log(
                                    "local_ws",
                                    format!("disconnecting client {address_key} by request"),
                                );
                                break;
                            }
                            continue;
                        }
                        received = rx.recv() => received,
                    };
                    match received {
                        Ok(message) => {
                            let message_bytes = message.len() as u64;
                            if ws.send(Message::Text(message.into())).await.is_err() {
                                break;
                            }
                            messages_sent += 1;
                            bytes_sent += message_bytes;
                            if last_reported_at.elapsed() >= Duration::from_secs(1) {
                                update_local_client(
                                    &client_runtime,
                                    &address_key,
                                    messages_sent,
                                    bytes_sent,
                                    0,
                                );
                                last_reported_at = Instant::now();
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            client_runtime
                                .record_pipeline_drop(PipelineEdge::LocalWebSocket, skipped);
                            update_local_client(
                                &client_runtime,
                                &address_key,
                                messages_sent,
                                bytes_sent,
                                skipped,
                            );
                            client_runtime.log(
                                "local_ws",
                                format!("client {address} lagged by {skipped} messages"),
                            );
                            break;
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                },
                Err(err) => {
                    client_runtime.update(|status| {
                        status.local_ws.last_error = Some(format!("{err:#}"));
                        status.local_ws.last_changed = Some(Instant::now());
                    });
                    client_runtime.log("local_ws", format!("accept failed: {err:#}"));
                }
            }
            client_runtime.update(|status| {
                status
                    .local_ws
                    .clients
                    .retain(|client| client.address != address_key);
                status.local_ws.client_count = status.local_ws.clients.len();
                status.local_ws.last_changed = Some(Instant::now());
            });
            client_runtime.log("local_ws", format!("client disconnected {address}"));
        });
    }
    Ok(())
}

fn update_local_client(
    runtime: &SharedRuntime,
    address: &str,
    messages_sent: u64,
    bytes_sent: u64,
    lagged_messages: u64,
) {
    runtime.update(|status| {
        if let Some(client) = status
            .local_ws
            .clients
            .iter_mut()
            .find(|client| client.address == address)
        {
            client.messages_sent = messages_sent;
            client.bytes_sent = bytes_sent;
            client.lagged_messages += lagged_messages;
            client.last_sent_at = Some(Instant::now());
        }
    });
}

async fn run_relay_client(
    config: Config,
    receiver: Receiver<Value>,
    sender: Sender<Value>,
    control_receiver: Receiver<RelayCommand>,
    runtime: SharedRuntime,
) -> Result<()> {
    let uri = relay_uri(&config);
    runtime.update(|status| {
        status.relay.health = Health::Starting;
        status.relay.uri = uri.clone();
        status.relay.last_changed = Some(Instant::now());
    });
    let state = Arc::new(Mutex::new(RelayState {
        max_replay_upload_retries: 2,
        ..RelayState::default()
    }));

    loop {
        if runtime.shutdown_requested() {
            break;
        }
        runtime.update(|status| {
            status.relay.health = Health::Starting;
            status.relay.attempts += 1;
            status.relay.producer_key_present = false;
            status.relay.next_reconnect_at = None;
            status.relay.last_changed = Some(Instant::now());
        });
        runtime.log("relay", format!("connecting to {uri}"));
        let mut reconnect_requested = false;
        let connection = tokio::select! {
            result = connect_async(&uri) => Some(result),
            _ = wait_for_shutdown(runtime.clone()) => None,
        };
        let Some(connection) = connection else {
            break;
        };
        match connection {
            Ok((ws, _response)) => {
                runtime.update(|status| {
                    status.relay.health = Health::Connected;
                    status.relay.last_error = None;
                    status.relay.reconnect_backoff_secs = 0;
                    status.relay.next_reconnect_at = None;
                    status.relay.last_changed = Some(Instant::now());
                });
                runtime.log("relay", "connection established");
                let (mut write, mut read) = ws.split();
                let (outbound_control_tx, outbound_control_rx) = mpsc::unbounded_channel::<Value>();
                let welcome_message = tokio::select! {
                    message = read.next() => message,
                    _ = wait_for_shutdown(runtime.clone()) => break,
                };
                if let Some(Ok(raw)) = welcome_message {
                    if let Ok(welcome) = message_to_json(&raw) {
                        if welcome.get("type").and_then(Value::as_str) == Some("welcome")
                            && welcome.get("role").and_then(Value::as_str) == Some("producer")
                        {
                            let producer_key = welcome
                                .get("producerKey")
                                .and_then(Value::as_str)
                                .map(ToOwned::to_owned);
                            let expires_at =
                                welcome.get("expiresAt").cloned().unwrap_or(Value::Null);
                            state.lock().unwrap().producer_key = producer_key.clone();
                            runtime.update(|status| {
                                status.relay.producer_key_present = producer_key.is_some();
                                status.relay.producer_key_expires_at = expires_at.to_string();
                                status.relay.last_received_at = Some(Instant::now());
                                status.relay.messages_received += 1;
                            });
                            runtime.log(
                                "relay",
                                format!("producer key received expiresAt={expires_at}"),
                            );
                        } else {
                            runtime.log("relay", format!("unexpected welcome message: {welcome}"));
                        }
                    } else {
                        runtime.log("relay", format!("unexpected first message: {raw:?}"));
                    }
                }

                let recv_state = state.clone();
                let recv_control_tx = outbound_control_tx.clone();
                let recv_runtime = runtime.clone();
                let recv_task = tokio::spawn(async move {
                    while let Some(message) = read.next().await {
                        match message {
                            Ok(message) => {
                                if let Ok(value) = message_to_json(&message) {
                                    handle_relay_message(
                                        value,
                                        recv_state.clone(),
                                        recv_control_tx.clone(),
                                        recv_runtime.clone(),
                                    )
                                    .await;
                                }
                            }
                            Err(err) => {
                                recv_runtime.update(|status| {
                                    status.relay.last_error = Some(format!("{err:#}"));
                                    status.relay.last_changed = Some(Instant::now());
                                });
                                recv_runtime.log("relay", format!("receive error: {err:#}"));
                                break;
                            }
                        }
                    }
                });

                let send_result = send_relay_loop(
                    &mut write,
                    state.clone(),
                    &receiver,
                    outbound_control_rx,
                    &control_receiver,
                    runtime.clone(),
                )
                .await;
                recv_task.abort();
                match send_result {
                    Ok(RelayLoopExit::ReconnectRequested) => {
                        reconnect_requested = true;
                        runtime.update(|status| {
                            status.relay.health = Health::Disconnected;
                            status.relay.reconnects += 1;
                            status.relay.last_changed = Some(Instant::now());
                        });
                        runtime.log("relay", "manual reconnect starting");
                    }
                    Ok(RelayLoopExit::Disconnected) => {
                        runtime.update(|status| {
                            status.relay.health = Health::Disconnected;
                            status.relay.last_changed = Some(Instant::now());
                        });
                        runtime.log("relay", "sender loop disconnected");
                    }
                    Err(err) => {
                        runtime.update(|status| {
                            status.relay.health = Health::Error;
                            status.relay.last_error = Some(format!("{err:#}"));
                            status.relay.last_changed = Some(Instant::now());
                        });
                        runtime.log("relay", format!("send error: {err:#}"));
                    }
                }
            }
            Err(err) => {
                runtime.update(|status| {
                    status.relay.health = Health::Error;
                    status.relay.last_error = Some(err.to_string());
                    status.relay.last_changed = Some(Instant::now());
                });
                runtime.log("relay", format!("connection failed: {err}; retrying"));
            }
        }

        if runtime.shutdown_requested() {
            break;
        }

        let (dropped, retained_upload_request_ids) =
            flush_stale_relay_messages(&receiver, &sender, &runtime);
        if dropped > 0 {
            runtime.update(|status| {
                status.relay.dropped_stale_messages += dropped as u64;
            });
            runtime.log("relay", format!("flushed {dropped} stale relay messages"));
        }
        let requeued =
            requeue_pending_replay_uploads(&state, &sender, &retained_upload_request_ids, &runtime);
        if requeued > 0 {
            runtime.log(
                "relay",
                format!("requeued {requeued} pending replay upload requests"),
            );
        }
        let retry_delay = if reconnect_requested {
            Duration::from_millis(100)
        } else {
            Duration::from_secs(5)
        };
        runtime.update(|status| {
            status.relay.pending_uploads = state.lock().unwrap().replay_uploads.len();
            status.relay.reconnect_backoff_secs = retry_delay.as_secs();
            status.relay.next_reconnect_at = Some(Instant::now() + retry_delay);
        });
        tokio::select! {
            _ = tokio::time::sleep(retry_delay) => {}
            _ = wait_for_shutdown(runtime.clone()) => break,
        }
    }
    Ok(())
}

async fn wait_for_shutdown(runtime: SharedRuntime) {
    while !runtime.shutdown_requested() {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn handle_relay_message(
    value: Value,
    state: Arc<Mutex<RelayState>>,
    outbound_control_tx: mpsc::UnboundedSender<Value>,
    runtime: SharedRuntime,
) {
    runtime.update(|status| {
        status.relay.messages_received += 1;
        status.relay.last_received_at = Some(Instant::now());
    });
    match value.get("type").and_then(Value::as_str) {
        Some("error") if value.get("code").and_then(Value::as_str) == Some("BAD_KEY") => {
            state.lock().unwrap().require_key = true;
            runtime.update(|status| status.relay.require_key = true);
            runtime.log(
                "relay",
                "server requires per-message key; including key in subsequent messages",
            );
        }
        Some("replay_upload_presign_response") => {
            let request_id = value
                .get("request_id")
                .and_then(Value::as_str)
                .unwrap_or("<missing>")
                .to_string();
            runtime.log(
                "relay",
                format!("replay upload presign response request_id={request_id}"),
            );
            if let Err(err) = handle_replay_upload_presign_response(
                value,
                state,
                outbound_control_tx,
                runtime.clone(),
            )
            .await
            {
                runtime.log(
                    "relay",
                    format!("presign response handling failed request_id={request_id}: {err:#}"),
                );
            }
        }
        Some("replay_upload_presign_error") => {
            let request_id = value
                .get("request_id")
                .and_then(Value::as_str)
                .unwrap_or("<missing>");
            update_upload_status(
                &runtime,
                request_id,
                None,
                None,
                UploadPhase::Failed,
                None,
                value
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("presign request failed"),
            );
            runtime.log(
                "relay",
                format!(
                    "replay upload presign error request_id={} status={} error={}",
                    value
                        .get("request_id")
                        .and_then(Value::as_str)
                        .unwrap_or("<missing>"),
                    value
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("<missing>"),
                    value
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("<missing>")
                ),
            );
        }
        _ => runtime.log("relay", format!("recv {}", compact_json(&value, 180))),
    }
}

async fn handle_replay_upload_presign_response(
    msg: Value,
    state: Arc<Mutex<RelayState>>,
    outbound_control_tx: mpsc::UnboundedSender<Value>,
    runtime: SharedRuntime,
) -> Result<()> {
    let request_id = msg
        .get("request_id")
        .and_then(Value::as_str)
        .context("presign response missing request_id")?
        .to_string();
    let pending = {
        state
            .lock()
            .unwrap()
            .replay_uploads
            .get(&request_id)
            .cloned()
    };
    let Some(pending) = pending else {
        runtime.log(
            "relay",
            format!("no local replay path for request_id={request_id}"),
        );
        return Ok(());
    };
    update_upload_status(
        &runtime,
        &request_id,
        None,
        None,
        UploadPhase::Uploading,
        Some(pending.attempts),
        "uploading replay bytes",
    );
    let presigned_request = msg
        .get("presigned_request")
        .cloned()
        .context("presign response missing presigned_request")?;

    match upload_replay_file(&pending.path, &presigned_request).await {
        Ok(status) => {
            let upload_id = msg
                .get("upload")
                .and_then(|upload| upload.get("id"))
                .cloned()
                .unwrap_or(Value::Null);
            outbound_control_tx
                .send(json!({
                    "type": "replay_upload_client_status",
                    "request_id": request_id,
                    "upload_id": upload_id,
                    "status": "uploaded",
                }))
                .map_err(|_| anyhow!("relay control channel closed"))?;
            state.lock().unwrap().replay_uploads.remove(&request_id);
            runtime.update(|app_status| {
                app_status.relay.pending_uploads = state.lock().unwrap().replay_uploads.len();
            });
            update_upload_status(
                &runtime,
                &request_id,
                None,
                None,
                UploadPhase::Uploaded,
                Some(pending.attempts),
                format!("HTTP {status}"),
            );
            runtime.log(
                "relay",
                format!("replay uploaded request_id={request_id} status={status}"),
            );
        }
        Err(err) => {
            let retry = {
                let mut state = state.lock().unwrap();
                let max_retries = state.max_replay_upload_retries;
                let mut remove_pending = false;
                let retry = if let Some(stored) = state.replay_uploads.get_mut(&request_id) {
                    stored.attempts = stored.attempts.saturating_add(1);
                    if stored.attempts <= max_retries {
                        Some((stored.payload.clone(), stored.attempts))
                    } else {
                        remove_pending = true;
                        None
                    }
                } else {
                    None
                };
                if remove_pending {
                    state.replay_uploads.remove(&request_id);
                }
                retry
            };

            if let Some((payload, attempts)) = retry {
                let delay_secs = (1u64 << attempts.min(5)).min(30);
                update_upload_status(
                    &runtime,
                    &request_id,
                    None,
                    None,
                    UploadPhase::Retrying,
                    Some(attempts),
                    format!("retrying in {delay_secs}s: {err:#}"),
                );
                runtime.log(
                    "relay",
                    format!(
                        "replay upload failed request_id={request_id}: {err:#}; retrying in {delay_secs}s"
                    ),
                );
                tokio::time::sleep(Duration::from_secs(delay_secs)).await;
                outbound_control_tx
                    .send(payload)
                    .map_err(|_| anyhow!("relay control channel closed"))?;
            } else {
                runtime.update(|app_status| {
                    app_status.relay.pending_uploads = state.lock().unwrap().replay_uploads.len();
                });
                update_upload_status(
                    &runtime,
                    &request_id,
                    None,
                    None,
                    UploadPhase::Failed,
                    None,
                    format!("{err:#}"),
                );
                runtime.log(
                    "relay",
                    format!("replay upload failed request_id={request_id}: {err:#}; giving up"),
                );
            }
        }
    }
    Ok(())
}

async fn upload_replay_file(path: &Path, presigned_request: &Value) -> Result<u16> {
    let url = presigned_request
        .get("url")
        .and_then(Value::as_str)
        .context("presigned request missing url")?;
    let method = presigned_request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("PUT")
        .parse()?;
    let mut headers = HeaderMap::new();
    if let Some(source_headers) = presigned_request.get("headers").and_then(Value::as_object) {
        for (name, value) in source_headers {
            let Some(value) = value.as_str() else {
                continue;
            };
            headers.insert(
                HeaderName::from_bytes(name.as_bytes())?,
                HeaderValue::from_str(value)?,
            );
        }
    }
    let body = tokio::fs::read(path)
        .await
        .with_context(|| format!("failed to read replay file {}", safe_path_name(path)))?;
    let response = reqwest::Client::new()
        .request(method, url)
        .headers(headers)
        .body(body)
        .send()
        .await
        .map_err(|_| anyhow!("upload request failed"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(anyhow!("upload failed with status {}", status.as_u16()));
    }
    Ok(status.as_u16())
}

async fn send_replay_upload_presign_request<W>(
    write: &mut W,
    state: Arc<Mutex<RelayState>>,
    mut payload: Value,
    runtime: SharedRuntime,
) -> Result<()>
where
    W: SinkExt<Message> + Unpin,
    <W as futures_util::Sink<Message>>::Error: std::error::Error + Send + Sync + 'static,
{
    let local_path = payload
        .as_object_mut()
        .and_then(|map| map.remove("_local_file_path"))
        .and_then(|value| match value {
            Value::String(path) => Some(PathBuf::from(path)),
            _ => None,
        });
    let request_id = payload
        .get("request_id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let already_tracked = request_id
        .as_ref()
        .map(|request_id| {
            state
                .lock()
                .unwrap()
                .replay_uploads
                .contains_key(request_id)
        })
        .unwrap_or(false);
    if let (Some(path), Some(request_id)) = (local_path, request_id) {
        let file_name = safe_path_name(&path);
        let size_bytes = payload
            .get("size_bytes")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        state.lock().unwrap().replay_uploads.insert(
            request_id.clone(),
            PendingReplayUpload {
                path,
                payload: payload.clone(),
                attempts: 0,
            },
        );
        runtime.update(|status| {
            status.relay.pending_uploads = state.lock().unwrap().replay_uploads.len();
        });
        update_upload_status(
            &runtime,
            &request_id,
            Some(file_name),
            Some(size_bytes),
            UploadPhase::WaitingForUrl,
            Some(0),
            "waiting for presigned upload URL",
        );
    } else if !already_tracked {
        runtime.log(
            "relay",
            "replay upload request missing local path or request_id; sending without tracking",
        );
    }
    send_control_message(write, state, payload, runtime).await
}

async fn send_control_message<W>(
    write: &mut W,
    state: Arc<Mutex<RelayState>>,
    mut payload: Value,
    runtime: SharedRuntime,
) -> Result<()>
where
    W: SinkExt<Message> + Unpin,
    <W as futures_util::Sink<Message>>::Error: std::error::Error + Send + Sync + 'static,
{
    add_key_if_needed(&mut payload, &state.lock().unwrap());
    write
        .send(Message::Text(serde_json::to_string(&payload)?.into()))
        .await?;
    runtime.update(|status| {
        status.relay.messages_sent += 1;
    });
    Ok(())
}

fn flush_stale_relay_messages(
    receiver: &Receiver<Value>,
    sender: &Sender<Value>,
    runtime: &SharedRuntime,
) -> (usize, HashSet<String>) {
    let mut dropped = 0;
    let mut retained = Vec::new();
    let mut retained_upload_request_ids = HashSet::new();
    while let Ok(payload) = receiver.try_recv() {
        runtime.record_pipeline_dequeue(PipelineEdge::Relay, receiver.len());
        if is_replay_upload_presign_request(&payload) {
            if let Some(request_id) = payload.get("request_id").and_then(Value::as_str) {
                retained_upload_request_ids.insert(request_id.to_string());
            }
            retained.push(payload);
        } else {
            dropped += 1;
        }
    }
    runtime.record_pipeline_drop(PipelineEdge::Relay, dropped as u64);
    for payload in retained {
        let accepted = sender.try_send(payload).is_ok();
        runtime.record_pipeline_enqueue(PipelineEdge::Relay, sender.len(), accepted);
    }
    (dropped, retained_upload_request_ids)
}

fn requeue_pending_replay_uploads(
    state: &Arc<Mutex<RelayState>>,
    sender: &Sender<Value>,
    queued_request_ids: &HashSet<String>,
    runtime: &SharedRuntime,
) -> usize {
    let payloads: Vec<Value> = state
        .lock()
        .unwrap()
        .replay_uploads
        .iter()
        .filter(|(request_id, _pending)| !queued_request_ids.contains(*request_id))
        .map(|(_request_id, pending)| pending.payload.clone())
        .collect();
    let mut requeued = 0;
    for payload in payloads {
        let accepted = sender.try_send(payload).is_ok();
        runtime.record_pipeline_enqueue(PipelineEdge::Relay, sender.len(), accepted);
        if accepted {
            requeued += 1;
        }
    }
    requeued
}

fn is_replay_upload_presign_request(payload: &Value) -> bool {
    payload.get("type").and_then(Value::as_str) == Some("replay_upload_presign_request")
}

fn safe_path_name(path: &Path) -> String {
    path.file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("<unknown>")
        .to_string()
}

#[allow(clippy::too_many_arguments)]
fn update_upload_status(
    runtime: &SharedRuntime,
    request_id: &str,
    file_name: Option<String>,
    size_bytes: Option<u64>,
    phase: UploadPhase,
    attempts: Option<u8>,
    detail: impl Into<String>,
) {
    let detail = detail.into();
    runtime.update(|status| {
        if let Some(upload) = status
            .relay
            .uploads
            .iter_mut()
            .find(|upload| upload.request_id == request_id)
        {
            if let Some(file_name) = file_name {
                upload.file_name = file_name;
            }
            if let Some(size_bytes) = size_bytes {
                upload.size_bytes = size_bytes;
            }
            if let Some(attempts) = attempts {
                upload.attempts = attempts;
            }
            upload.phase = phase;
            upload.detail = detail;
            upload.updated_at = Instant::now();
        } else {
            status.relay.uploads.push(ReplayUploadStatus {
                request_id: request_id.to_string(),
                file_name: file_name.unwrap_or_default(),
                size_bytes: size_bytes.unwrap_or(0),
                attempts: attempts.unwrap_or(0),
                phase,
                detail,
                updated_at: Instant::now(),
            });
            if status.relay.uploads.len() > 32 {
                status.relay.uploads.remove(0);
            }
        }
    });
}

async fn send_relay_loop<W>(
    write: &mut W,
    state: Arc<Mutex<RelayState>>,
    receiver: &Receiver<Value>,
    mut outbound_control_rx: mpsc::UnboundedReceiver<Value>,
    control_receiver: &Receiver<RelayCommand>,
    runtime: SharedRuntime,
) -> Result<RelayLoopExit>
where
    W: SinkExt<Message> + Unpin,
    <W as futures_util::Sink<Message>>::Error: std::error::Error + Send + Sync + 'static,
{
    runtime.log("relay", "sender loop started");
    let mut last_live_status: Option<Value> = None;
    let mut last_live_status_sent_at = Instant::now() - Duration::from_secs(60);
    let mut last_live_status_game_id: Option<String> = None;
    let mut last_live_status_spawn_parameters_hash: Option<String> = None;
    let mut terminal_status_sent_for_game_id: Option<String> = None;

    loop {
        if runtime.shutdown_requested() {
            return Ok(RelayLoopExit::Disconnected);
        }
        while let Ok(command) = control_receiver.try_recv() {
            match command {
                RelayCommand::ReconnectNow => return Ok(RelayLoopExit::ReconnectRequested),
                RelayCommand::RetryUpload(request_id) => {
                    let payload = state
                        .lock()
                        .unwrap()
                        .replay_uploads
                        .get(&request_id)
                        .map(|pending| pending.payload.clone());
                    if let Some(payload) = payload {
                        update_upload_status(
                            &runtime,
                            &request_id,
                            None,
                            None,
                            UploadPhase::WaitingForUrl,
                            None,
                            "manual retry requested",
                        );
                        send_control_message(write, state.clone(), payload, runtime.clone())
                            .await?;
                    } else {
                        update_upload_status(
                            &runtime,
                            &request_id,
                            None,
                            None,
                            UploadPhase::Failed,
                            None,
                            "upload is no longer pending",
                        );
                    }
                }
                RelayCommand::CancelUpload(request_id) => {
                    state.lock().unwrap().replay_uploads.remove(&request_id);
                    runtime.update(|status| {
                        status.relay.pending_uploads = state.lock().unwrap().replay_uploads.len();
                    });
                    update_upload_status(
                        &runtime,
                        &request_id,
                        None,
                        None,
                        UploadPhase::Cancelled,
                        None,
                        "cancelled by user",
                    );
                }
            }
        }

        while let Ok(payload) = outbound_control_rx.try_recv() {
            send_control_message(write, state.clone(), payload, runtime.clone()).await?;
        }

        match receiver.try_recv() {
            Ok(payload) => {
                runtime.record_pipeline_dequeue(PipelineEdge::Relay, receiver.len());
                if is_replay_upload_presign_request(&payload) {
                    if runtime.controls.replay_uploads() {
                        send_replay_upload_presign_request(
                            write,
                            state.clone(),
                            payload,
                            runtime.clone(),
                        )
                        .await?;
                    } else {
                        runtime.log("relay", "replay upload request skipped; uploads disabled");
                    }
                    continue;
                }

                let live_status = build_live_status_message(&payload);
                let live_status_status = live_status.get("status").and_then(Value::as_str);
                let live_status_is_terminal =
                    matches!(live_status_status, Some("postgame" | "ended" | "stale"));
                let live_status_is_live = live_status_status == Some("live");
                let live_status_source_external_id = live_status
                    .get("source_external_id")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty());
                let live_status_game_id = match live_status_source_external_id {
                    Some(source_external_id) => source_external_id.to_string(),
                    None if live_status_is_terminal => last_live_status_game_id
                        .clone()
                        .unwrap_or_else(|| "__terminal__".to_string()),
                    None => live_status
                        .get("started_at")
                        .and_then(Value::as_str)
                        .unwrap_or("__unknown__")
                        .to_string(),
                };
                if Some(live_status_game_id.clone()) != last_live_status_game_id {
                    terminal_status_sent_for_game_id = None;
                    last_live_status_game_id = Some(live_status_game_id.clone());
                    last_live_status_spawn_parameters_hash = None;
                }

                let live_status_spawn_parameters_hash = live_status
                    .get("spawn_parameters_hash")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                let live_status_spawn_parameters_changed = live_status_spawn_parameters_hash
                    .is_some()
                    && live_status_spawn_parameters_hash != last_live_status_spawn_parameters_hash;
                let live_status_due = last_live_status_sent_at.elapsed() >= Duration::from_secs(10);
                let terminal_status_due = live_status_is_terminal
                    && terminal_status_sent_for_game_id.as_deref() != Some(&live_status_game_id);

                last_live_status = Some(live_status.clone());
                if terminal_status_due
                    || (live_status_is_live && live_status_spawn_parameters_changed)
                    || (live_status_due && !live_status_is_terminal)
                {
                    send_live_status(write, state.clone(), live_status, runtime.clone()).await?;
                    last_live_status_sent_at = Instant::now();
                    if live_status_spawn_parameters_hash.is_some() {
                        last_live_status_spawn_parameters_hash = live_status_spawn_parameters_hash;
                    }
                    if live_status_is_terminal {
                        terminal_status_sent_for_game_id = Some(live_status_game_id);
                    }
                }

                let mut payload = strip_tick(&payload);
                add_key_if_needed(&mut payload, &state.lock().unwrap());
                let message_bytes = serde_json::to_vec(&payload)?;
                let compressed = zstd::bulk::compress(&message_bytes, 12)?;
                runtime.record_pipeline_bytes(PipelineEdge::Relay, compressed.len() as u64);
                write.send(Message::Binary(compressed.into())).await?;
                runtime.update(|status| {
                    status.relay.messages_sent += 1;
                    status.relay.compressed_ticks_sent += 1;
                });
            }
            Err(crossbeam_channel::TryRecvError::Empty) => {
                if let Some(live_status) = &last_live_status {
                    let status = live_status.get("status").and_then(Value::as_str);
                    if !matches!(status, Some("postgame" | "ended" | "stale"))
                        && last_live_status_sent_at.elapsed() >= Duration::from_secs(10)
                    {
                        send_live_status(
                            write,
                            state.clone(),
                            live_status.clone(),
                            runtime.clone(),
                        )
                        .await?;
                        last_live_status_sent_at = Instant::now();
                        last_live_status_spawn_parameters_hash = live_status
                            .get("spawn_parameters_hash")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned);
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                runtime.log("relay", "sender channel disconnected");
                return Ok(RelayLoopExit::Disconnected);
            }
        }
    }
}

async fn send_live_status<W>(
    write: &mut W,
    state: Arc<Mutex<RelayState>>,
    mut live_status: Value,
    runtime: SharedRuntime,
) -> Result<()>
where
    W: SinkExt<Message> + Unpin,
    <W as futures_util::Sink<Message>>::Error: std::error::Error + Send + Sync + 'static,
{
    if let Some(map) = live_status.as_object_mut() {
        map.insert(
            "observed_at".to_string(),
            Value::String(utc_now_python_iso()),
        );
    }
    send_control_message(write, state, live_status, runtime.clone()).await?;
    runtime.update(|status| {
        status.relay.live_status_sent += 1;
    });
    Ok(())
}

fn relay_uri(config: &Config) -> String {
    let host = config
        .ws_relay_base_url
        .replace("https://", "wss://")
        .replace("http://", "ws://");
    format!(
        "{}/ws/{}?role=producer&compress_messages=True&compress_messages_binary=True&buffer_messages=False",
        host.trim_end_matches('/'),
        urlencoding::encode(&config.ws_relay_room)
    )
}

fn message_to_json(message: &Message) -> Result<Value> {
    match message {
        Message::Text(text) => Ok(serde_json::from_str(text)?),
        Message::Binary(bytes) => Ok(serde_json::from_slice(bytes)?),
        _ => Ok(Value::Null),
    }
}

fn compact_json(value: &Value, max_len: usize) -> String {
    let mut text = value.to_string();
    if text.len() > max_len {
        text.truncate(max_len.saturating_sub(3));
        text.push_str("...");
    }
    text
}

fn add_key_if_needed(payload: &mut Value, state: &RelayState) {
    if !(state.require_key || state.always_include_key) {
        return;
    }
    let Some(key) = &state.producer_key else {
        return;
    };
    if let Some(map) = payload.as_object_mut() {
        map.insert("key".to_string(), Value::String(key.clone()));
    }
}

fn strip_tick(data: &Value) -> Value {
    let mut root = Map::new();
    copy_field(&mut root, data, "broken_surfaces");
    copy_field(&mut root, data, "game_type");
    copy_field(&mut root, data, "variant");
    copy_field(&mut root, data, "game_engine_has_teams");
    copy_field(&mut root, data, "multiplayer_map_name");
    copy_subfields(
        &mut root,
        data,
        "game_time_info",
        &["game_time", "real_time_elapsed"],
    );
    copy_subfields(
        &mut root,
        data,
        "map_info",
        &[
            "cache_version",
            "build_version",
            "scenario_name",
            "checksum",
        ],
    );
    copy_field(&mut root, data, "damage_counts");
    copy_datetime_field(&mut root, data, "current_time");
    copy_datetime_field(&mut root, data, "start_time");
    copy_field(&mut root, data, "game_id");
    copy_field(&mut root, data, "performance");
    copy_players(&mut root, data);
    copy_objects(&mut root, data);
    copy_field(&mut root, data, "game_ended_this_tick");
    copy_field(&mut root, data, "events");
    copy_spawns(&mut root, data);
    copy_field(&mut root, data, "items");
    copy_field(&mut root, data, "gametype_settings");
    copy_network_game_client(&mut root, data);
    copy_game_meta(&mut root, data);
    Value::Object(root)
}

fn copy_field(root: &mut Map<String, Value>, data: &Value, field: &str) {
    if let Some(value) = data.get(field) {
        root.insert(field.to_string(), value.clone());
    }
}

fn copy_datetime_field(root: &mut Map<String, Value>, data: &Value, field: &str) {
    if let Some(value) = data.get(field).and_then(Value::as_str) {
        root.insert(field.to_string(), Value::String(py_datetime_to_iso(value)));
    } else {
        copy_field(root, data, field);
    }
}

fn copy_subfields(root: &mut Map<String, Value>, data: &Value, field: &str, subfields: &[&str]) {
    if let Some(source) = data.get(field).and_then(Value::as_object) {
        let mut target = Map::new();
        for subfield in subfields {
            if let Some(value) = source.get(*subfield) {
                target.insert((*subfield).to_string(), value.clone());
            }
        }
        root.insert(field.to_string(), Value::Object(target));
    }
}

fn copy_players(root: &mut Map<String, Value>, data: &Value) {
    let Some(players) = data.get("players").and_then(Value::as_array) else {
        return;
    };
    let mut stripped = Vec::new();
    for player in players {
        let mut map = Map::new();
        for field in [
            "player_index",
            "local_player",
            "name",
            "team",
            "respawn_timer",
            "camo_timer",
            "kill_streak",
            "multikill",
            "time_of_last_kill",
            "kills",
            "assists",
            "team_kills",
            "deaths",
            "suicides",
            "score",
            "ctf_score",
            "player_object_data",
            "model_nodes",
            "derived_stats",
            "input_data",
        ] {
            if let Some(value) = player.get(field) {
                map.insert(field.to_string(), value.clone());
            }
        }
        if let Some(damage_table) = player.get("damage_table").and_then(Value::as_array) {
            let mut rows = Vec::new();
            for row in damage_table {
                let mut row_map = Map::new();
                copy_into(&mut row_map, row, "damage_time");
                copy_into(&mut row_map, row, "damage_amount");
                rows.push(Value::Object(row_map));
            }
            map.insert("damage_table".to_string(), Value::Array(rows));
        }
        if let Some(camera) = player
            .get("observer_camera_info")
            .and_then(Value::as_object)
        {
            let mut camera_map = Map::new();
            for field in ["x", "y", "z", "x_aim", "y_aim", "z_aim", "fov"] {
                if let Some(value) = camera.get(field) {
                    camera_map.insert(field.to_string(), value.clone());
                }
            }
            map.insert(
                "observer_camera_info".to_string(),
                Value::Object(camera_map),
            );
        }
        if let Some(fpw) = player.get("first_person_weapon").and_then(Value::as_object) {
            let mut fpw_map = Map::new();
            copy_into_obj(&mut fpw_map, fpw, "weapon_rendered");
            copy_into_obj(&mut fpw_map, fpw, "weapon_object_id");
            map.insert("first_person_weapon".to_string(), Value::Object(fpw_map));
        }
        stripped.push(Value::Object(map));
    }
    root.insert("players".to_string(), Value::Array(stripped));
}

fn copy_objects(root: &mut Map<String, Value>, data: &Value) {
    let Some(objects) = data.get("objects").and_then(Value::as_array) else {
        return;
    };
    let mut stripped = Vec::new();
    for object in objects {
        let mut map = Map::new();
        for field in [
            "object_id",
            "flags",
            "state_flags",
            "x",
            "y",
            "z",
            "forward_x",
            "forward_y",
            "forward_z",
            "up_x",
            "up_y",
            "up_z",
            "object_type_string",
            "tag_name",
        ] {
            if let Some(value) = object.get(field) {
                map.insert(field.to_string(), value.clone());
            }
        }
        stripped.push(Value::Object(map));
    }
    root.insert("objects".to_string(), Value::Array(stripped));
}

fn copy_spawns(root: &mut Map<String, Value>, data: &Value) {
    let Some(spawns) = data.get("spawns").and_then(Value::as_array) else {
        return;
    };
    let mut stripped = Vec::new();
    for spawn in spawns {
        let mut map = Map::new();
        for field in [
            "spawn_id",
            "x",
            "y",
            "z",
            "facing",
            "team_index",
            "gametypes",
        ] {
            if let Some(value) = spawn.get(field) {
                map.insert(field.to_string(), value.clone());
            }
        }
        stripped.push(Value::Object(map));
    }
    root.insert("spawns".to_string(), Value::Array(stripped));
}

fn copy_network_game_client(root: &mut Map<String, Value>, data: &Value) {
    let Some(client) = data.get("network_game_client").and_then(Value::as_object) else {
        return;
    };

    let mut client_map = Map::new();
    if let Some(network_game_data) = client.get("network_game_data").and_then(Value::as_object) {
        let mut network_game_data_map = Map::new();
        for field in ["network_machines", "network_players"] {
            if let Some(value) = network_game_data.get(field) {
                network_game_data_map.insert(field.to_string(), value.clone());
            }
        }
        client_map.insert(
            "network_game_data".to_string(),
            Value::Object(network_game_data_map),
        );
    }
    root.insert("network_game_client".to_string(), Value::Object(client_map));
}

fn copy_game_meta(root: &mut Map<String, Value>, data: &Value) {
    let Some(game_meta) = data.get("game_meta").and_then(Value::as_object) else {
        return;
    };

    let mut game_meta_map = Map::new();
    if let Some(players) = game_meta.get("players").and_then(Value::as_object) {
        let mut players_map = Map::new();
        for (player_index, player) in players {
            let Some(player) = player.as_object() else {
                continue;
            };
            let mut player_map = Map::new();
            for field in [
                "damage_dealt",
                "damage_received",
                "kills_by_tick",
                "deaths_by_tick",
                "score_by_tick",
                "camo_count",
                "overshield_count",
            ] {
                if let Some(value) = player.get(field) {
                    player_map.insert(field.to_string(), value.clone());
                }
            }
            players_map.insert(player_index.clone(), Value::Object(player_map));
        }
        game_meta_map.insert("players".to_string(), Value::Object(players_map));
    }
    root.insert("game_meta".to_string(), Value::Object(game_meta_map));
}

fn copy_into(target: &mut Map<String, Value>, source: &Value, field: &str) {
    if let Some(value) = source.get(field) {
        target.insert(field.to_string(), value.clone());
    }
}

fn copy_into_obj(target: &mut Map<String, Value>, source: &Map<String, Value>, field: &str) {
    if let Some(value) = source.get(field) {
        target.insert(field.to_string(), value.clone());
    }
}

fn build_live_status_message(game_info: &Value) -> Value {
    let game_time_info = game_info.get("game_time_info").and_then(Value::as_object);
    let current_tick = game_time_info
        .and_then(|info| info.get("game_time"))
        .and_then(optional_i64);
    let game_id = optional_string(game_info.get("game_id"));
    let player_summary = player_summary(game_info);
    let map_info = game_info.get("map_info").and_then(Value::as_object);
    let map_resolution_inputs = game_info
        .get("map_resolution_inputs")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let map_resolution_object = map_resolution_inputs.as_object();
    let map_resolution_map_info = map_resolution_object
        .and_then(|object| object.get("map_info"))
        .and_then(Value::as_object);
    let spawn_parameters_hash = game_info
        .get("spawn_parameters_hash")
        .cloned()
        .unwrap_or(Value::Null);
    let spawn_points = map_resolution_object
        .and_then(|object| object.get("spawn_points"))
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    let build_version = map_resolution_map_info
        .and_then(|info| info.get("build_version"))
        .cloned()
        .or_else(|| map_info.and_then(|info| info.get("build_version")).cloned())
        .unwrap_or(Value::Null);
    let cache_version = map_resolution_map_info
        .and_then(|info| info.get("cache_version"))
        .cloned()
        .or_else(|| map_info.and_then(|info| info.get("cache_version")).cloned())
        .unwrap_or(Value::Null);

    json!({
        "type": "live_status",
        "status": game_status(game_info),
        "source_external_id": game_id,
        "map_engine_name": map_resolution_object.and_then(|object| object.get("map_engine_name")).cloned().unwrap_or_else(|| game_info.get("multiplayer_map_name").cloned().unwrap_or(Value::Null)),
        "build_version": build_version,
        "cache_version": cache_version,
        "spawn_parameters_hash": spawn_parameters_hash,
        "map_resolution_inputs": map_resolution_inputs,
        "spawn_points": spawn_points,
        "game_type": optional_string(game_info.get("game_type")),
        "variant": game_info.get("variant").cloned().unwrap_or(Value::Null),
        "variant_name": optional_string(game_info.get("global_stage")),
        "started_at": optional_datetime(game_info.get("start_time")),
        "observed_at": optional_datetime(game_info.get("current_time")),
        "current_game_time_seconds": current_tick.map(|tick| if tick >= 0 { Value::from(tick as f64 / LIVE_STATUS_TICKS_PER_SECOND) } else { Value::Null }).unwrap_or(Value::Null),
        "current_tick": current_tick,
        "player_summary": player_summary,
        "team_summary": team_summary(game_info, &player_summary),
        "raw_status": {
            "game_engine_running": game_info.get("game_engine_running").cloned().unwrap_or(Value::Null),
            "game_engine_can_score": game_info.get("game_engine_can_score").cloned().unwrap_or(Value::Null),
            "game_ended_this_tick": game_info.get("game_ended_this_tick").cloned().unwrap_or(Value::Null),
        },
        "game_metadata": {
            "source": "xqemu-tools",
            "legacy_game_id": game_id,
            "map_name": game_info.get("multiplayer_map_name").cloned().unwrap_or(Value::Null),
            "map_info": {
                "scenario_name": map_info.and_then(|info| info.get("scenario_name")).cloned().unwrap_or(Value::Null),
                "checksum": map_info.and_then(|info| info.get("checksum")).cloned().unwrap_or(Value::Null),
                "build_version": map_info.and_then(|info| info.get("build_version")).cloned().unwrap_or(Value::Null),
                "cache_version": map_info.and_then(|info| info.get("cache_version")).cloned().unwrap_or(Value::Null),
            },
            "game_type": game_info.get("game_type").cloned().unwrap_or(Value::Null),
            "variant": game_info.get("variant").cloned().unwrap_or(Value::Null),
            "game_engine_has_teams": game_info.get("game_engine_has_teams").cloned().unwrap_or(Value::Null),
            "spawn_parameters_hash": game_info.get("spawn_parameters_hash").cloned().unwrap_or(Value::Null),
            "map_resolution_inputs": game_info.get("map_resolution_inputs").cloned().unwrap_or(Value::Null),
        },
    })
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

fn player_summary(game_info: &Value) -> Value {
    let mut players = Vec::new();
    if let Some(source_players) = game_info.get("players").and_then(Value::as_array) {
        for player in source_players {
            let player_index = player.get("player_index").and_then(optional_i64);
            let derived_stats = player.get("derived_stats").and_then(Value::as_object);
            players.push(json!({
                "player_index": player_index,
                "name": player.get("name").cloned().unwrap_or(Value::Null),
                "team_index": player.get("team").cloned().unwrap_or(Value::Null),
                "local_player": player.get("local_player").cloned().unwrap_or(Value::Null),
                "score": player.get("score").cloned().unwrap_or(Value::Null),
                "kills": player.get("kills").cloned().unwrap_or(Value::Null),
                "deaths": player.get("deaths").cloned().unwrap_or(Value::Null),
                "assists": player.get("assists").cloned().unwrap_or(Value::Null),
                "team_kills": player.get("team_kills").cloned().unwrap_or(Value::Null),
                "suicides": player.get("suicides").cloned().unwrap_or(Value::Null),
                "respawn_timer": player.get("respawn_timer").cloned().unwrap_or(Value::Null),
                "has_camo": derived_stats.and_then(|stats| stats.get("has_camo")).and_then(Value::as_bool).unwrap_or(false),
                "has_overshield": derived_stats.and_then(|stats| stats.get("has_overshield")).and_then(Value::as_bool).unwrap_or(false),
                "is_host": derived_stats.and_then(|stats| stats.get("is_host")).and_then(Value::as_bool).unwrap_or(false),
                "damage_dealt": player_index.map(|index| player_damage(game_info, index, "damage_dealt")).unwrap_or(Value::from(0)),
                "damage_received": player_index.map(|index| player_damage(game_info, index, "damage_received")).unwrap_or(Value::from(0)),
            }));
        }
    }
    Value::Array(players)
}

fn team_summary(game_info: &Value, player_summary: &Value) -> Value {
    if !game_info
        .get("game_engine_has_teams")
        .and_then(optional_i64)
        .map(|value| value != 0)
        .unwrap_or(false)
    {
        return Value::Array(Vec::new());
    }
    let mut teams: std::collections::BTreeMap<i64, (i64, i64, i64, i64)> = Default::default();
    if let Some(players) = player_summary.as_array() {
        for player in players {
            let Some(team_index) = player.get("team_index").and_then(optional_i64) else {
                continue;
            };
            let entry = teams.entry(team_index).or_insert((0, 0, 0, 0));
            entry.0 += 1;
            entry.1 += player.get("score").and_then(optional_i64).unwrap_or(0);
            entry.2 += player.get("kills").and_then(optional_i64).unwrap_or(0);
            entry.3 += player.get("deaths").and_then(optional_i64).unwrap_or(0);
        }
    }
    Value::Array(
        teams
            .into_iter()
            .map(|(team_index, (player_count, score, kills, deaths))| {
                json!({
                    "team_index": team_index,
                    "player_count": player_count,
                    "score": score,
                    "kills": kills,
                    "deaths": deaths,
                })
            })
            .collect(),
    )
}

fn player_damage(game_info: &Value, player_index: i64, field: &str) -> Value {
    game_info
        .get("game_meta")
        .and_then(|meta| meta.get("players"))
        .and_then(|players| players.get(player_index.to_string()))
        .and_then(|player| player.get(field))
        .cloned()
        .unwrap_or_else(|| Value::from(0))
}

fn optional_string(value: Option<&Value>) -> Value {
    match value {
        Some(Value::String(value)) if !value.trim().is_empty() => {
            Value::String(value.trim().to_string())
        }
        Some(Value::Number(_)) | Some(Value::Bool(_)) => Value::String(value.unwrap().to_string()),
        _ => Value::Null,
    }
}

fn optional_datetime(value: Option<&Value>) -> Value {
    match value.and_then(Value::as_str) {
        Some(value) if !value.trim().is_empty() => Value::String(py_datetime_to_iso(value.trim())),
        _ => Value::Null,
    }
}

fn optional_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

fn utc_now_python_iso() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.6f+00:00")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::RuntimeState;

    #[test]
    fn strip_tick_preserves_complete_breakable_snapshots_and_unknown() {
        let fixture: Value = serde_json::from_str(include_str!("../../tests/fixtures/breakable_surfaces.json")).unwrap();
        for snapshot in fixture["cases"].as_array().unwrap().iter()
            .map(|case| case["snapshot"].clone()).chain([Value::Null]) {
            let tick = json!({"broken_surfaces": snapshot});
            assert_eq!(strip_tick(&tick), tick);
        }
        assert!(strip_tick(&json!({})).get("broken_surfaces").is_none());
    }

    #[test]
    fn upload_status_is_created_and_updated_by_request_id() {
        let runtime = RuntimeState::new(Config::default());
        update_upload_status(
            &runtime,
            "request-1",
            Some("game.json.zst".to_string()),
            Some(1024),
            UploadPhase::WaitingForUrl,
            Some(0),
            "waiting",
        );
        update_upload_status(
            &runtime,
            "request-1",
            None,
            None,
            UploadPhase::Uploaded,
            Some(1),
            "complete",
        );

        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.status.relay.uploads.len(), 1);
        let upload = &snapshot.status.relay.uploads[0];
        assert_eq!(upload.file_name, "game.json.zst");
        assert_eq!(upload.size_bytes, 1024);
        assert_eq!(upload.attempts, 1);
        assert_eq!(upload.phase, UploadPhase::Uploaded);
    }

    #[test]
    fn strip_tick_keeps_selected_game_meta_fields_for_each_player() {
        let payload = json!({
            "game_meta": {
                "start_time": "not retained",
                "players": {
                    "0": {
                        "damage_dealt": 125.5,
                        "damage_received": 80.0,
                        "kills_by_tick": {"30": [[1.0, 2.0, 3.0]]},
                        "deaths_by_tick": {"60": [[4.0, 5.0, 6.0]]},
                        "score_by_tick": {"30": 2},
                        "camo_count": 2,
                        "overshield_count": 3,
                        "shots_by_tick": {"10": 1}
                    },
                    "7": {
                        "damage_dealt": 80.0,
                        "damage_received": 125.5,
                        "kills_by_tick": {},
                        "deaths_by_tick": {"30": [[7.0, 8.0, 9.0]]},
                        "score_by_tick": {"30": 1},
                        "camo_count": 1,
                        "overshield_count": 0,
                        "active_projectiles": ["not retained"]
                    }
                }
            }
        });

        assert_eq!(
            strip_tick(&payload),
            json!({
                "game_meta": {
                    "players": {
                        "0": {
                            "damage_dealt": 125.5,
                            "damage_received": 80.0,
                            "kills_by_tick": {"30": [[1.0, 2.0, 3.0]]},
                            "deaths_by_tick": {"60": [[4.0, 5.0, 6.0]]},
                            "score_by_tick": {"30": 2},
                            "camo_count": 2,
                            "overshield_count": 3
                        },
                        "7": {
                            "damage_dealt": 80.0,
                            "damage_received": 125.5,
                            "kills_by_tick": {},
                            "deaths_by_tick": {"30": [[7.0, 8.0, 9.0]]},
                            "score_by_tick": {"30": 1},
                            "camo_count": 1,
                            "overshield_count": 0
                        }
                    }
                }
            })
        );
    }
}
