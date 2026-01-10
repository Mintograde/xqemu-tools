use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use futures_util::SinkExt;
use std::time::Duration;
use tokio::time::sleep;
use serde_json::Value;

pub struct WsClient {
    url: String,
    rx: mpsc::Receiver<Value>,
}

impl WsClient {
    pub fn new(url: String, rx: mpsc::Receiver<Value>) -> Self {
        Self { url, rx }
    }

    pub async fn run(mut self) {
        loop {
            println!("Connecting to WebSocket: {}", self.url);
            match connect_async(&self.url).await {
                Ok((mut ws_stream, _)) => {
                    println!("WebSocket connected");
                    
                    // TODO: Handle welcome message and key if needed
                    
                    while let Some(msg) = self.rx.recv().await {
                        // Compress
                        let json_bytes = match serde_json::to_vec(&msg) {
                            Ok(b) => b,
                            Err(e) => {
                                eprintln!("Failed to serialize message: {}", e);
                                continue;
                            }
                        };

                        let compressed = match zstd::encode_all(&json_bytes[..], 12) {
                             Ok(c) => c,
                             Err(e) => {
                                 eprintln!("Failed to compress message: {}", e);
                                 continue;
                             }
                        };

                        if let Err(e) = ws_stream.send(Message::Binary(compressed)).await {
                            eprintln!("WebSocket send error: {}", e);
                            break; // Reconnect
                        }
                    }
                    
                    // If rx is closed, exit
                    if self.rx.is_closed() {
                        break;
                    }
                }
                Err(e) => {
                    eprintln!("WebSocket connect error: {}", e);
                    sleep(Duration::from_secs(5)).await;
                }
            }
        }
    }
}
