use std::env;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Config {
    pub websocket_host: String,
    pub websocket_port: u16,
    pub qmp_host: String,
    pub qmp_port: u16,
    pub replay_directory: PathBuf,
    pub ws_relay_enabled: bool,
    pub ws_relay_base_url: String,
    pub ws_relay_room: String,
    pub compute_spawn_parameters_hash: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            websocket_host: env::var("WEBSOCKET_HOST").unwrap_or_else(|_| "localhost".to_string()),
            websocket_port: env::var("WEBSOCKET_PORT")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(9000),
            qmp_host: env::var("QMP_HOST").unwrap_or_else(|_| "localhost".to_string()),
            qmp_port: env::var("QMP_PORT")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(4445),
            replay_directory: env::var("REPLAY_DIRECTORY")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("V:/replays")),
            ws_relay_enabled: env_bool("ENABLE_WEBSOCKET_RELAY", true),
            ws_relay_base_url: env::var("WS_RELAY_BASE_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:8787".to_string()),
            ws_relay_room: env::var("WS_RELAY_ROOM").unwrap_or_else(|_| "test-room2".to_string()),
            compute_spawn_parameters_hash: env_bool("COMPUTE_SPAWN_PARAMETERS_HASH", true),
        }
    }
}

fn env_bool(name: &str, default: bool) -> bool {
    match env::var(name) {
        Ok(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => default,
    }
}
