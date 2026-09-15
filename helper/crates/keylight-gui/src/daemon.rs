//! Make sure a keylightd of *our* version is running before the UI starts.

use crate::api::ApiClient;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

/// Outcome shown in the Settings tab.
#[derive(Debug, Clone)]
pub enum DaemonStatus {
    Running(String),
    Started(String),
    Unavailable(String),
}

impl DaemonStatus {
    pub fn text(&self) -> String {
        match self {
            DaemonStatus::Running(v) => format!("Daemon running (v{v})"),
            DaemonStatus::Started(v) => format!("Daemon started (v{v})"),
            DaemonStatus::Unavailable(why) => format!("Daemon unavailable: {why}"),
        }
    }
}

fn daemon_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("keylightd")))
        .filter(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from("keylightd"))
}

fn spawn_daemon() -> Result<(), String> {
    use std::os::unix::process::CommandExt as _;
    Command::new(daemon_path())
        .arg("serve")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // Own process group: closing the window (or a Ctrl-C in a terminal)
        // must not take the daemon down with it.
        .process_group(0)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn wait_for_daemon(api: &ApiClient, attempts: usize) -> Option<String> {
    for _ in 0..attempts {
        thread::sleep(Duration::from_millis(250));
        if let Ok(h) = api.health() {
            return Some(h.version);
        }
    }
    None
}

/// Probe the daemon; start it if missing; restart it if its version differs
/// from ours (an old daemon can linger after an upgrade).
pub fn ensure_running() -> DaemonStatus {
    let api = ApiClient::with_timeout(Duration::from_millis(800));

    match api.health() {
        Ok(h) if h.version == limelight_core::VERSION => return DaemonStatus::Running(h.version),
        Ok(h) => {
            let old = if h.version.is_empty() {
                "pre-0.2".to_string()
            } else {
                h.version.clone()
            };
            eprintln!(
                "keylightd {old} is running but we are v{}; asking it to stop",
                limelight_core::VERSION
            );
            let _ = api.shutdown();
            // Wait for the port to free up.
            let mut gone = false;
            for _ in 0..20 {
                thread::sleep(Duration::from_millis(150));
                if api.health().is_err() {
                    gone = true;
                    break;
                }
            }
            if !gone {
                // Daemons before 0.2 have no shutdown endpoint and keep the port.
                return DaemonStatus::Unavailable(format!(
                    "an older keylightd ({old}) still holds port {}. Stop it (e.g. `flatpak kill {}` or reboot) and reopen LimeLight",
                    limelight_core::daemon_port(),
                    crate::autostart::APP_ID
                ));
            }
        }
        Err(_) => eprintln!("keylightd not reachable, starting it"),
    }

    if let Err(err) = spawn_daemon() {
        return DaemonStatus::Unavailable(format!("could not start keylightd: {err}"));
    }
    match wait_for_daemon(&api, 20) {
        Some(version) => DaemonStatus::Started(version),
        None => DaemonStatus::Unavailable("keylightd did not come up in time".into()),
    }
}
