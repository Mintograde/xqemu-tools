use crate::config::Config;
use crate::runtime::{AppCommand, CommandRequest, SharedRuntime, UpdatePhase};
use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use crossbeam_channel::{Receiver, RecvTimeoutError};
use self_update::backends::github;
use self_update::update::ReleaseUpdate;
use std::thread;
use std::time::{Duration, Instant};

const REPOSITORY_OWNER: &str = "Mintograde";
const REPOSITORY_NAME: &str = "xqemu-tools";
const BINARY_NAME: &str = "xemu-tools-rs";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const EMBEDDED_VERIFYING_KEY: Option<&str> = option_env!("XEMU_TOOLS_UPDATE_VERIFYING_KEY");

#[derive(Clone, Debug, Eq, PartialEq)]
struct CheckOutcome {
    latest_version: String,
    release_url: String,
    available: bool,
}

pub fn start_update_worker(
    config: Config,
    receiver: Receiver<CommandRequest>,
    runtime: SharedRuntime,
) -> thread::JoinHandle<()> {
    thread::spawn(move || run_update_worker(config, receiver, runtime))
}

fn run_update_worker(config: Config, receiver: Receiver<CommandRequest>, runtime: SharedRuntime) {
    let signature_configured = embedded_verifying_key().is_ok();
    runtime.update(|status| {
        status.update.signature_configured = signature_configured;
        if !config.update_checks_enabled {
            status.update.phase = UpdatePhase::Disabled;
            status.update.detail = "automatic checks disabled".to_string();
        }
    });

    if config.update_checks_enabled {
        mark_checking(&runtime);
        record_check_result(&runtime, check_latest_release(), None);
    }

    while !runtime.shutdown_requested() {
        let request = match receiver.recv_timeout(Duration::from_millis(250)) {
            Ok(request) => request,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        runtime.start_command(request.id);
        match request.command {
            AppCommand::CheckForUpdates => {
                mark_checking(&runtime);
                record_check_result(&runtime, check_latest_release(), Some(request.id));
            }
            AppCommand::InstallUpdate => {
                if install_latest_release(&runtime, request.id) {
                    break;
                }
            }
            _ => runtime.fail_command(request.id, "command is not handled by the update worker"),
        }
    }
}

fn mark_checking(runtime: &SharedRuntime) {
    runtime.update(|status| {
        status.update.phase = UpdatePhase::Checking;
        status.update.detail = "checking GitHub releases".to_string();
        status.update.last_error = None;
    });
}

fn check_latest_release() -> Result<CheckOutcome> {
    let updater = build_updater(None)?;
    let release = updater
        .get_latest_release()
        .context("failed to query the latest GitHub release")?;
    let available = self_update::version::bump_is_greater(CURRENT_VERSION, &release.version)
        .context("release tag is not a valid semantic version")?;
    if available
        && release
            .asset_for(&updater.target(), Some(BINARY_NAME))
            .is_none()
    {
        bail!(
            "release {} has no {} asset for {}",
            release.version,
            BINARY_NAME,
            updater.target()
        );
    }
    let outcome = CheckOutcome {
        release_url: release_url(&release.version),
        latest_version: release.version,
        available,
    };
    Ok(outcome)
}

fn install_latest_release(runtime: &SharedRuntime, command_id: u64) -> bool {
    runtime.update(|status| {
        status.update.phase = UpdatePhase::Installing;
        status.update.detail = "downloading and verifying update".to_string();
        status.update.last_error = None;
    });
    runtime.log("update", "downloading signed update");

    let result = embedded_verifying_key()
        .and_then(|key| build_updater(Some(key)))
        .and_then(|updater| updater.update().context("update installation failed"));
    match result {
        Ok(status) if status.updated() => {
            let version = status.version().to_string();
            runtime.update(|app_status| {
                app_status.update.phase = UpdatePhase::Installed;
                app_status.update.latest_version = version.clone();
                app_status.update.detail = "installed; restarting".to_string();
                app_status.update.last_error = None;
            });
            runtime.log("update", format!("installed version {version}; restarting"));
            runtime.finish_command(command_id, format!("installed {version}; restarting"));
            runtime.request_restart();
            true
        }
        Ok(status) => {
            let version = status.version().to_string();
            runtime.update(|app_status| {
                app_status.update.phase = UpdatePhase::UpToDate;
                app_status.update.latest_version = version.clone();
                app_status.update.detail = "already up to date".to_string();
                app_status.update.last_error = None;
            });
            runtime.finish_command(command_id, format!("already on {version}"));
            false
        }
        Err(err) => {
            let detail = format!("{err:#}");
            runtime.update(|status| {
                status.update.phase = UpdatePhase::Error;
                status.update.detail = "installation failed".to_string();
                status.update.last_error = Some(detail.clone());
            });
            runtime.log("update", format!("installation failed: {detail}"));
            runtime.fail_command(command_id, detail);
            false
        }
    }
}

fn build_updater(verifying_key: Option<[u8; 32]>) -> Result<Box<dyn ReleaseUpdate>> {
    let mut builder = github::Update::configure();
    builder
        .repo_owner(REPOSITORY_OWNER)
        .repo_name(REPOSITORY_NAME)
        .bin_name(BINARY_NAME)
        .identifier(BINARY_NAME)
        .current_version(CURRENT_VERSION)
        .show_download_progress(false)
        .show_output(false)
        .no_confirm(true);
    if let Some(key) = verifying_key {
        builder.verifying_keys([key]);
    }
    builder.build().context("invalid update configuration")
}

fn embedded_verifying_key() -> Result<[u8; 32]> {
    let encoded = EMBEDDED_VERIFYING_KEY
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context("this build has no update verifying key; installation is disabled")?;
    let bytes = STANDARD
        .decode(encoded)
        .context("the embedded update verifying key is not valid base64")?;
    bytes.try_into().map_err(|bytes: Vec<u8>| {
        anyhow::anyhow!("update verifying key is {} bytes, expected 32", bytes.len())
    })
}

fn record_check_result(
    runtime: &SharedRuntime,
    result: Result<CheckOutcome>,
    command_id: Option<u64>,
) {
    match result {
        Ok(outcome) => {
            let detail = if outcome.available {
                format!("version {} available", outcome.latest_version)
            } else {
                format!("up to date at {}", outcome.latest_version)
            };
            runtime.update(|status| {
                status.update.phase = if outcome.available {
                    UpdatePhase::Available
                } else {
                    UpdatePhase::UpToDate
                };
                status.update.latest_version = outcome.latest_version.clone();
                status.update.release_url = outcome.release_url;
                status.update.detail = detail.clone();
                status.update.last_checked = Some(Instant::now());
                status.update.last_error = None;
            });
            runtime.log("update", detail.clone());
            if let Some(id) = command_id {
                runtime.finish_command(id, detail);
            }
        }
        Err(err) => {
            let detail = format!("{err:#}");
            runtime.update(|status| {
                status.update.phase = UpdatePhase::Error;
                status.update.detail = "update check failed".to_string();
                status.update.last_checked = Some(Instant::now());
                status.update.last_error = Some(detail.clone());
            });
            runtime.log("update", format!("check failed: {detail}"));
            if let Some(id) = command_id {
                runtime.fail_command(id, detail);
            }
        }
    }
}

fn release_url(version: &str) -> String {
    let tag = if version.starts_with('v') {
        version.to_string()
    } else {
        format!("v{version}")
    };
    format!("https://github.com/{REPOSITORY_OWNER}/{REPOSITORY_NAME}/releases/tag/{tag}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_links_use_v_prefixed_tags() {
        assert_eq!(
            release_url("1.2.3"),
            "https://github.com/Mintograde/xqemu-tools/releases/tag/v1.2.3"
        );
        assert_eq!(release_url("v1.2.3"), release_url("1.2.3"));
    }

    #[test]
    fn semantic_version_check_rejects_older_releases() -> Result<()> {
        assert!(self_update::version::bump_is_greater("1.2.3", "1.3.0")?);
        assert!(!self_update::version::bump_is_greater("1.2.3", "1.2.3")?);
        assert!(!self_update::version::bump_is_greater("1.2.3", "1.2.2")?);
        Ok(())
    }
}
