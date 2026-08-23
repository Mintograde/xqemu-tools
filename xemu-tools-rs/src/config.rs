use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

const ROOM_ADJECTIVES: [&str; 16] = [
    "bold", "brisk", "calm", "clear", "cool", "fair", "fast", "fresh", "grand", "keen", "light",
    "quiet", "rapid", "sharp", "swift", "wise",
];
const ROOM_NOUNS: [&str; 16] = [
    "beacon", "canyon", "cedar", "comet", "harbor", "maple", "meadow", "mesa", "orbit", "peak",
    "river", "signal", "summit", "trail", "valley", "wave",
];

#[derive(Clone, Debug, PartialEq)]
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
    pub update_checks_enabled: bool,
    pub source_path: Option<PathBuf>,
    pub environment_overrides: Vec<ConfigKey>,
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
            ws_relay_room: random_room_name(),
            compute_spawn_parameters_hash: true,
            save_replays: true,
            save_all_ticks: true,
            replay_uploads_enabled: true,
            update_checks_enabled: true,
            source_path: None,
            environment_overrides: Vec::new(),
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let mut config = Self::default();
        if let Some(path) = env::var_os("XEMU_TOOLS_CONFIG") {
            let path = PathBuf::from(path);
            apply_config_file(&mut config, path.clone(), true)?;
            config.source_path = Some(path);
        } else if let Some(path) = find_config_file() {
            apply_config_file(&mut config, path.clone(), false)?;
            config.source_path = Some(path);
        }
        config.apply_env_overrides();
        Ok(config)
    }

    pub fn save(&mut self) -> Result<PathBuf> {
        let path = self.source_path.clone().unwrap_or_else(|| {
            env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join("config.toml")
        });
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create config directory {}", parent.display())
            })?;
        }
        let contents = toml::to_string_pretty(&PersistedConfig::from(&*self))?;
        fs::write(&path, contents)
            .with_context(|| format!("failed to write config file {}", path.display()))?;
        self.source_path = Some(path.clone());
        Ok(path)
    }

    pub fn apply_value(&mut self, key: ConfigKey, value: &str) -> Result<()> {
        let value = value.trim();
        match key {
            ConfigKey::WebsocketHost => self.websocket_host = non_empty(key, value)?,
            ConfigKey::WebsocketPort => self.websocket_port = parse_port(key, value)?,
            ConfigKey::QmpHost => self.qmp_host = non_empty(key, value)?,
            ConfigKey::QmpPort => self.qmp_port = parse_port(key, value)?,
            ConfigKey::ReplayDirectory => {
                self.replay_directory = PathBuf::from(non_empty(key, value)?)
            }
            ConfigKey::RelayEnabled => self.ws_relay_enabled = parse_bool(key, value)?,
            ConfigKey::RelayBaseUrl => self.ws_relay_base_url = non_empty(key, value)?,
            ConfigKey::RelayRoom => self.ws_relay_room = non_empty(key, value)?,
            ConfigKey::SpawnHash => self.compute_spawn_parameters_hash = parse_bool(key, value)?,
            ConfigKey::SaveReplays => self.save_replays = parse_bool(key, value)?,
            ConfigKey::SaveAllTicks => self.save_all_ticks = parse_bool(key, value)?,
            ConfigKey::ReplayUploads => self.replay_uploads_enabled = parse_bool(key, value)?,
            ConfigKey::UpdateChecks => self.update_checks_enabled = parse_bool(key, value)?,
        }
        Ok(())
    }

    pub fn value(&self, key: ConfigKey) -> String {
        match key {
            ConfigKey::WebsocketHost => self.websocket_host.clone(),
            ConfigKey::WebsocketPort => self.websocket_port.to_string(),
            ConfigKey::QmpHost => self.qmp_host.clone(),
            ConfigKey::QmpPort => self.qmp_port.to_string(),
            ConfigKey::ReplayDirectory => self.replay_directory.display().to_string(),
            ConfigKey::RelayEnabled => self.ws_relay_enabled.to_string(),
            ConfigKey::RelayBaseUrl => self.ws_relay_base_url.clone(),
            ConfigKey::RelayRoom => self.ws_relay_room.clone(),
            ConfigKey::SpawnHash => self.compute_spawn_parameters_hash.to_string(),
            ConfigKey::SaveReplays => self.save_replays.to_string(),
            ConfigKey::SaveAllTicks => self.save_all_ticks.to_string(),
            ConfigKey::ReplayUploads => self.replay_uploads_enabled.to_string(),
            ConfigKey::UpdateChecks => self.update_checks_enabled.to_string(),
        }
    }

    fn apply_env_overrides(&mut self) {
        if let Ok(value) = env::var("WEBSOCKET_HOST") {
            self.websocket_host = value;
            self.environment_overrides.push(ConfigKey::WebsocketHost);
        }
        if let Some(value) = env_u16("WEBSOCKET_PORT") {
            self.websocket_port = value;
            self.environment_overrides.push(ConfigKey::WebsocketPort);
        }
        if let Ok(value) = env::var("QMP_HOST") {
            self.qmp_host = value;
            self.environment_overrides.push(ConfigKey::QmpHost);
        }
        if let Some(value) = env_u16("QMP_PORT") {
            self.qmp_port = value;
            self.environment_overrides.push(ConfigKey::QmpPort);
        }
        if let Ok(value) = env::var("REPLAY_DIRECTORY") {
            self.replay_directory = PathBuf::from(value);
            self.environment_overrides.push(ConfigKey::ReplayDirectory);
        }
        if let Some(value) = env_bool_override("ENABLE_WEBSOCKET_RELAY") {
            self.ws_relay_enabled = value;
            self.environment_overrides.push(ConfigKey::RelayEnabled);
        }
        if let Ok(value) = env::var("WS_RELAY_BASE_URL") {
            self.ws_relay_base_url = value;
            self.environment_overrides.push(ConfigKey::RelayBaseUrl);
        }
        if let Ok(value) = env::var("WS_RELAY_ROOM") {
            self.ws_relay_room = room_name_or_random(&value);
            self.environment_overrides.push(ConfigKey::RelayRoom);
        }
        if let Some(value) = env_bool_override("COMPUTE_SPAWN_PARAMETERS_HASH") {
            self.compute_spawn_parameters_hash = value;
            self.environment_overrides.push(ConfigKey::SpawnHash);
        }
        if let Some(value) = env_bool_override("ENABLE_REPLAY_SAVING") {
            self.save_replays = value;
            self.environment_overrides.push(ConfigKey::SaveReplays);
        }
        if let Some(value) = env_bool_override("SAVE_ALL_TICKS") {
            self.save_all_ticks = value;
            self.environment_overrides.push(ConfigKey::SaveAllTicks);
        }
        if let Some(value) = env_bool_override("ENABLE_REPLAY_UPLOADS") {
            self.replay_uploads_enabled = value;
            self.environment_overrides.push(ConfigKey::ReplayUploads);
        }
        if let Some(value) = env_bool_override("ENABLE_UPDATE_CHECKS") {
            self.update_checks_enabled = value;
            self.environment_overrides.push(ConfigKey::UpdateChecks);
        }
    }

    pub fn origin(&self, key: ConfigKey) -> &'static str {
        if self.environment_overrides.contains(&key) {
            "environment"
        } else if self.source_path.is_some() {
            "config file"
        } else {
            "default"
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigKey {
    WebsocketHost,
    WebsocketPort,
    QmpHost,
    QmpPort,
    ReplayDirectory,
    RelayEnabled,
    RelayBaseUrl,
    RelayRoom,
    SpawnHash,
    SaveReplays,
    SaveAllTicks,
    ReplayUploads,
    UpdateChecks,
}

impl ConfigKey {
    pub const ALL: [Self; 13] = [
        Self::WebsocketHost,
        Self::WebsocketPort,
        Self::QmpHost,
        Self::QmpPort,
        Self::ReplayDirectory,
        Self::RelayEnabled,
        Self::RelayBaseUrl,
        Self::RelayRoom,
        Self::SpawnHash,
        Self::SaveReplays,
        Self::SaveAllTicks,
        Self::ReplayUploads,
        Self::UpdateChecks,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::WebsocketHost => "local WS host",
            Self::WebsocketPort => "local WS port",
            Self::QmpHost => "QMP host",
            Self::QmpPort => "QMP port",
            Self::ReplayDirectory => "replay directory",
            Self::RelayEnabled => "relay enabled",
            Self::RelayBaseUrl => "relay base URL",
            Self::RelayRoom => "relay room",
            Self::SpawnHash => "spawn hash",
            Self::SaveReplays => "save replays",
            Self::SaveAllTicks => "save all ticks",
            Self::ReplayUploads => "replay uploads",
            Self::UpdateChecks => "update checks",
        }
    }

    pub fn requires_restart(self) -> bool {
        !matches!(
            self,
            Self::SaveReplays | Self::SaveAllTicks | Self::ReplayUploads
        )
    }

    pub fn is_boolean(self) -> bool {
        matches!(
            self,
            Self::RelayEnabled
                | Self::SpawnHash
                | Self::SaveReplays
                | Self::SaveAllTicks
                | Self::ReplayUploads
                | Self::UpdateChecks
        )
    }
}

fn non_empty(key: ConfigKey, value: &str) -> Result<String> {
    if value.is_empty() {
        anyhow::bail!("{} cannot be empty", key.label());
    }
    Ok(value.to_string())
}

fn random_room_name() -> String {
    let random = Uuid::new_v4();
    let bytes = random.as_bytes();
    let adjective = ROOM_ADJECTIVES[usize::from(bytes[0]) % ROOM_ADJECTIVES.len()];
    let noun = ROOM_NOUNS[usize::from(bytes[1]) % ROOM_NOUNS.len()];
    let number = u16::from_le_bytes([bytes[2], bytes[3]]) % 1000;
    format!("{adjective}-{noun}-{number:03}")
}

fn room_name_or_random(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        random_room_name()
    } else {
        value.to_string()
    }
}

fn parse_port(key: ConfigKey, value: &str) -> Result<u16> {
    let port = value
        .parse::<u16>()
        .with_context(|| format!("{} must be a port number", key.label()))?;
    if port == 0 {
        anyhow::bail!("{} must be between 1 and 65535", key.label());
    }
    Ok(port)
}

fn parse_bool(key: ConfigKey, value: &str) -> Result<bool> {
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => anyhow::bail!("{} must be true or false", key.label()),
    }
}

#[derive(Serialize)]
struct PersistedConfig<'a> {
    websocket: PersistedWebsocket<'a>,
    qmp: PersistedQmp<'a>,
    replay: PersistedReplay<'a>,
    relay: PersistedRelay<'a>,
    features: PersistedFeatures,
    updates: PersistedUpdates,
}

#[derive(Serialize)]
struct PersistedWebsocket<'a> {
    host: &'a str,
    port: u16,
}

#[derive(Serialize)]
struct PersistedQmp<'a> {
    host: &'a str,
    port: u16,
}

#[derive(Serialize)]
struct PersistedReplay<'a> {
    directory: &'a Path,
    save: bool,
    save_all_ticks: bool,
    uploads: bool,
}

#[derive(Serialize)]
struct PersistedRelay<'a> {
    enabled: bool,
    base_url: &'a str,
    room: &'a str,
}

#[derive(Serialize)]
struct PersistedFeatures {
    compute_spawn_parameters_hash: bool,
}

#[derive(Serialize)]
struct PersistedUpdates {
    enabled: bool,
}

impl<'a> From<&'a Config> for PersistedConfig<'a> {
    fn from(config: &'a Config) -> Self {
        Self {
            websocket: PersistedWebsocket {
                host: &config.websocket_host,
                port: config.websocket_port,
            },
            qmp: PersistedQmp {
                host: &config.qmp_host,
                port: config.qmp_port,
            },
            replay: PersistedReplay {
                directory: &config.replay_directory,
                save: config.save_replays,
                save_all_ticks: config.save_all_ticks,
                uploads: config.replay_uploads_enabled,
            },
            relay: PersistedRelay {
                enabled: config.ws_relay_enabled,
                base_url: &config.ws_relay_base_url,
                room: &config.ws_relay_room,
            },
            features: PersistedFeatures {
                compute_spawn_parameters_hash: config.compute_spawn_parameters_hash,
            },
            updates: PersistedUpdates {
                enabled: config.update_checks_enabled,
            },
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
    updates: UpdateConfig,
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

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct UpdateConfig {
    enabled: Option<bool>,
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
        paths.push(
            PathBuf::from(app_data)
                .join("xemu-tools-rs")
                .join("config.toml"),
        );
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
        config.ws_relay_room = room_name_or_random(&room);
    }
    if let Some(compute_spawn_parameters_hash) = file_config.features.compute_spawn_parameters_hash
    {
        config.compute_spawn_parameters_hash = compute_spawn_parameters_hash;
    }
    if let Some(enabled) = file_config.updates.enabled {
        config.update_checks_enabled = enabled;
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
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn example_config_parses() -> Result<()> {
        let _: FileConfig = toml::from_str(include_str!("../config.example.toml"))?;
        Ok(())
    }

    fn assert_random_room_name(room: &str) {
        let parts = room.split('-').collect::<Vec<_>>();
        assert_eq!(parts.len(), 3);
        assert!(ROOM_ADJECTIVES.contains(&parts[0]));
        assert!(ROOM_NOUNS.contains(&parts[1]));
        assert_eq!(parts[2].len(), 3);
        assert!(parts[2].bytes().all(|byte| byte.is_ascii_digit()));
    }

    #[test]
    fn missing_relay_room_uses_random_name() -> Result<()> {
        let file_config: FileConfig = toml::from_str("[relay]\nenabled = true")?;
        let mut config = Config::default();

        apply_file_config(&mut config, file_config, Path::new("."));

        assert_random_room_name(&config.ws_relay_room);
        Ok(())
    }

    #[test]
    fn empty_relay_room_uses_random_name() -> Result<()> {
        let file_config: FileConfig = toml::from_str("[relay]\nroom = \"   \"")?;
        let mut config = Config::default();

        apply_file_config(&mut config, file_config, Path::new("."));

        assert_random_room_name(&config.ws_relay_room);
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

    #[test]
    fn config_values_validate_and_persist() -> Result<()> {
        let unique = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let dir = env::temp_dir().join(format!("xemu-tools-config-{unique}"));
        fs::create_dir_all(&dir)?;
        let path = dir.join("config.toml");
        let mut config = Config {
            source_path: Some(path.clone()),
            ..Config::default()
        };

        config.apply_value(ConfigKey::WebsocketPort, "9012")?;
        config.apply_value(ConfigKey::RelayEnabled, "off")?;
        assert!(config.apply_value(ConfigKey::QmpPort, "0").is_err());
        assert!(config.apply_value(ConfigKey::SaveReplays, "maybe").is_err());
        config.save()?;

        let parsed: FileConfig = toml::from_str(&fs::read_to_string(&path)?)?;
        assert_eq!(parsed.websocket.port, Some(9012));
        assert_eq!(parsed.relay.enabled, Some(false));
        fs::remove_dir_all(dir)?;
        Ok(())
    }
}
