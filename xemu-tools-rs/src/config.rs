use anyhow::{Context, Result};
use serde::Deserialize;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

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
    pub save_replays: bool,
    pub save_all_ticks: bool,
    pub replay_uploads_enabled: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            websocket_host: "localhost".to_string(),
            websocket_port: 9000,
            qmp_host: "localhost".to_string(),
            qmp_port: 4445,
            replay_directory: PathBuf::from("./replays"),
            ws_relay_enabled: true,
            ws_relay_base_url: "http://127.0.0.1:8787".to_string(),
            ws_relay_room: "test-room2".to_string(),
            compute_spawn_parameters_hash: true,
            save_replays: true,
            save_all_ticks: true,
            replay_uploads_enabled: true,
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let mut config = Self::default();
        if let Some(path) = env::var_os("XEMU_TOOLS_CONFIG") {
            apply_config_file(&mut config, PathBuf::from(path), true)?;
        } else if let Some(path) = find_config_file() {
            apply_config_file(&mut config, path, false)?;
        }
        config.apply_env_overrides();
        Ok(config)
    }

    fn apply_env_overrides(&mut self) {
        if let Ok(value) = env::var("WEBSOCKET_HOST") {
            self.websocket_host = value;
        }
        if let Some(value) = env_u16("WEBSOCKET_PORT") {
            self.websocket_port = value;
        }
        if let Ok(value) = env::var("QMP_HOST") {
            self.qmp_host = value;
        }
        if let Some(value) = env_u16("QMP_PORT") {
            self.qmp_port = value;
        }
        if let Ok(value) = env::var("REPLAY_DIRECTORY") {
            self.replay_directory = PathBuf::from(value);
        }
        if let Some(value) = env_bool_override("ENABLE_WEBSOCKET_RELAY") {
            self.ws_relay_enabled = value;
        }
        if let Ok(value) = env::var("WS_RELAY_BASE_URL") {
            self.ws_relay_base_url = value;
        }
        if let Ok(value) = env::var("WS_RELAY_ROOM") {
            self.ws_relay_room = value;
        }
        if let Some(value) = env_bool_override("COMPUTE_SPAWN_PARAMETERS_HASH") {
            self.compute_spawn_parameters_hash = value;
        }
        if let Some(value) = env_bool_override("ENABLE_REPLAY_SAVING") {
            self.save_replays = value;
        }
        if let Some(value) = env_bool_override("SAVE_ALL_TICKS") {
            self.save_all_ticks = value;
        }
        if let Some(value) = env_bool_override("ENABLE_REPLAY_UPLOADS") {
            self.replay_uploads_enabled = value;
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct FileConfig {
    websocket: WebsocketConfig,
    qmp: QmpConfig,
    replay: ReplayConfig,
    relay: RelayConfig,
    features: FeatureConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct WebsocketConfig {
    host: Option<String>,
    port: Option<u16>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct QmpConfig {
    host: Option<String>,
    port: Option<u16>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ReplayConfig {
    directory: Option<PathBuf>,
    save: Option<bool>,
    save_all_ticks: Option<bool>,
    uploads: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RelayConfig {
    enabled: Option<bool>,
    base_url: Option<String>,
    room: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct FeatureConfig {
    compute_spawn_parameters_hash: Option<bool>,
}

fn find_config_file() -> Option<PathBuf> {
    candidate_config_paths()
        .into_iter()
        .find(|path| path.is_file())
}

fn candidate_config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(current_dir) = env::current_dir() {
        paths.push(current_dir.join("config.toml"));
    }
    if let Ok(exe_path) = env::current_exe()
        && let Some(exe_dir) = exe_path.parent()
    {
        paths.push(exe_dir.join("config.toml"));
    }
    if let Some(app_data) = env::var_os("APPDATA") {
        paths.push(PathBuf::from(app_data).join("xemu-tools-rs").join("config.toml"));
    }
    paths
}

fn apply_config_file(config: &mut Config, path: PathBuf, required: bool) -> Result<()> {
    if !path.is_file() {
        if required {
            anyhow::bail!("config file does not exist: {}", path.display());
        }
        return Ok(());
    }
    let config_dir = path.parent().unwrap_or_else(|| Path::new("."));
    let contents = fs::read_to_string(&path)
        .with_context(|| format!("failed to read config file {}", path.display()))?;
    let file_config: FileConfig = toml::from_str(&contents)
        .with_context(|| format!("failed to parse config file {}", path.display()))?;
    apply_file_config(config, file_config, config_dir);
    Ok(())
}

fn apply_file_config(config: &mut Config, file_config: FileConfig, config_dir: &Path) {
    if let Some(host) = file_config.websocket.host {
        config.websocket_host = host;
    }
    if let Some(port) = file_config.websocket.port {
        config.websocket_port = port;
    }
    if let Some(host) = file_config.qmp.host {
        config.qmp_host = host;
    }
    if let Some(port) = file_config.qmp.port {
        config.qmp_port = port;
    }
    if let Some(directory) = file_config.replay.directory {
        config.replay_directory = resolve_config_path(config_dir, directory);
    }
    if let Some(save) = file_config.replay.save {
        config.save_replays = save;
    }
    if let Some(save_all_ticks) = file_config.replay.save_all_ticks {
        config.save_all_ticks = save_all_ticks;
    }
    if let Some(uploads) = file_config.replay.uploads {
        config.replay_uploads_enabled = uploads;
    }
    if let Some(enabled) = file_config.relay.enabled {
        config.ws_relay_enabled = enabled;
    }
    if let Some(base_url) = file_config.relay.base_url {
        config.ws_relay_base_url = base_url;
    }
    if let Some(room) = file_config.relay.room {
        config.ws_relay_room = room;
    }
    if let Some(compute_spawn_parameters_hash) =
        file_config.features.compute_spawn_parameters_hash
    {
        config.compute_spawn_parameters_hash = compute_spawn_parameters_hash;
    }
}

fn resolve_config_path(config_dir: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        config_dir.join(path)
    }
}

fn env_u16(name: &str) -> Option<u16> {
    env::var(name).ok().and_then(|value| value.parse().ok())
}

fn env_bool_override(name: &str) -> Option<bool> {
    env::var(name).ok().map(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses() -> Result<()> {
        let _: FileConfig = toml::from_str(include_str!("../config.example.toml"))?;
        Ok(())
    }

    #[test]
    fn file_config_overrides_defaults_and_resolves_relative_replay_directory() -> Result<()> {
        let file_config: FileConfig = toml::from_str(
            r#"
            [websocket]
            host = "0.0.0.0"
            port = 9010

            [qmp]
            host = "xemu.local"
            port = 4455

            [replay]
            directory = "./custom-replays"
            save = false
            save_all_ticks = false
            uploads = false

            [relay]
            enabled = false
            base_url = "wss://relay.example.test"
            room = "arena"

            [features]
            compute_spawn_parameters_hash = false
            "#,
        )?;
        let mut config = Config::default();

        apply_file_config(&mut config, file_config, Path::new("config-root"));

        assert_eq!(config.websocket_host, "0.0.0.0");
        assert_eq!(config.websocket_port, 9010);
        assert_eq!(config.qmp_host, "xemu.local");
        assert_eq!(config.qmp_port, 4455);
        assert_eq!(
            config.replay_directory,
            PathBuf::from("config-root").join("./custom-replays")
        );
        assert!(!config.save_replays);
        assert!(!config.save_all_ticks);
        assert!(!config.replay_uploads_enabled);
        assert!(!config.ws_relay_enabled);
        assert_eq!(config.ws_relay_base_url, "wss://relay.example.test");
        assert_eq!(config.ws_relay_room, "arena");
        assert!(!config.compute_spawn_parameters_hash);
        Ok(())
    }
}
