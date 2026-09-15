//! Make sure a keylightd of *our* version is running, and bring it back if it
//! goes away. The daemon's stdout/stderr go to a log file so a crash is
//! diagnosable after the fact.

use crate::api::ApiClient;
use std::net::{SocketAddr, TcpStream};
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

/// `$XDG_STATE_HOME/limelight/keylightd.log` (falls back to the cache dir).
pub fn log_path() -> PathBuf {
    dirs::state_dir()
        .or_else(dirs::cache_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join("limelight")
        .join("keylightd.log")
}

fn port_addr() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], limelight_core::daemon_port()))
}

/// True when nothing accepts connections on the daemon port.
fn port_free() -> bool {
    TcpStream::connect_timeout(&port_addr(), Duration::from_millis(200)).is_err()
}

fn wait_port_free(max: Duration) -> bool {
    let deadline = std::time::Instant::now() + max;
    while std::time::Instant::now() < deadline {
        if port_free() {
            return true;
        }
        thread::sleep(Duration::from_millis(100));
    }
    port_free()
}

fn spawn_daemon() -> Result<(), String> {
    use std::os::unix::process::CommandExt as _;
    let log = log_path();
    if let Some(dir) = log.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let open_log = || {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
            .ok()
    };
    let (out, err) = match (open_log(), open_log()) {
        (Some(a), Some(b)) => (Stdio::from(a), Stdio::from(b)),
        _ => (Stdio::null(), Stdio::null()),
    };
    Command::new(daemon_path())
        .arg("serve")
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
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

/// Start the daemon and wait for it; one retry if the first attempt does not
/// come up (covers a port that was still being released).
fn start_and_wait(api: &ApiClient) -> DaemonStatus {
    for attempt in 1..=2 {
        if !wait_port_free(Duration::from_secs(3)) {
            // Something answers on the port but not our health check.
            return DaemonStatus::Unavailable(format!(
                "port {} is held by another process",
                limelight_core::daemon_port()
            ));
        }
        if let Err(err) = spawn_daemon() {
            return DaemonStatus::Unavailable(format!("could not start keylightd: {err}"));
        }
        if let Some(version) = wait_for_daemon(api, 20) {
            return DaemonStatus::Started(version);
        }
        eprintln!(
            "keylightd did not answer after start (attempt {attempt}); see {}",
            log_path().display()
        );
    }
    DaemonStatus::Unavailable(format!(
        "keylightd did not come up; see {}",
        log_path().display()
    ))
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
            // Wait for the *port* to be released, not just for health to fail:
            // the listener can outlive the last successful request.
            if !wait_port_free(Duration::from_secs(5)) {
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

    start_and_wait(&api)
}
