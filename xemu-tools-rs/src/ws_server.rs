use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use std::net::SocketAddr;

type PeerMap = Arc<Mutex<HashMap<SocketAddr, mpsc::UnboundedSender<Message>>>>;

pub struct WsServer {
    peers: PeerMap,
}

impl WsServer {
    pub fn new() -> Self {
        Self {
            peers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn run(&self, port: u16) {
        let addr = format!("0.0.0.0:{}", port);
        let listener = TcpListener::bind(&addr).await.expect("Failed to bind WS server");
        println!("WS Broadcast Server listening on: {}", addr);

        while let Ok((stream, addr)) = listener.accept().await {
            tokio::spawn(Self::handle_connection(self.peers.clone(), stream, addr));
        }
    }

    async fn handle_connection(peers: PeerMap, raw_stream: TcpStream, addr: SocketAddr) {
        println!("Incoming WS connection from: {}", addr);

        let ws_stream = match tokio_tungstenite::accept_async(raw_stream).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Error during WS handshake: {}", e);
                return;
            }
        };

        let (tx, mut rx) = mpsc::unbounded_channel();
        peers.lock().unwrap().insert(addr, tx);

        let (mut ws_tx, mut ws_rx) = ws_stream.split();

        let peers_inner = peers.clone();
        tokio::select! {
            _ = async {
                while let Some(msg) = rx.recv().await {
                    if let Err(e) = ws_tx.send(msg).await {
                        eprintln!("Error sending WS message: {}", e);
                        break;
                    }
                }
            } => (),
            _ = async {
                while let Some(msg) = ws_rx.next().await {
                    if let Ok(msg) = msg {
                        if msg.is_close() { break; }
                    } else { break; }
                }
            } => (),
        }

        println!("WS connection closed: {}", addr);
        peers_inner.lock().unwrap().remove(&addr);
    }

    pub fn broadcast(&self, msg: String) {
        let peers = self.peers.lock().unwrap();
        let message = Message::Text(msg);
        for tx in peers.values() {
            let _ = tx.send(message.clone());
        }
    }
}
