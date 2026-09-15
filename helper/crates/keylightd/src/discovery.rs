//! mDNS discovery of `_elg._tcp` devices.
//!
//! Two modes share the same merge logic in [`AppState::upsert_discovered`]:
//! - a **persistent browser** thread that keeps listening for announcements,
//!   so IP changes, renames and disappearances are picked up without a scan;
//! - a **one-shot scan** used by `POST /v1/lights/refresh` and the CLI.

use crate::state::AppState;
use mdns_sd::{ResolvedService, ServiceDaemon, ServiceEvent};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

pub const SERVICE_TYPE: &str = "_elg._tcp.local.";

/// IPv4 first, IPv6 without zone id.
fn addresses_of(info: &ResolvedService) -> Vec<String> {
    let mut v4: Vec<String> = Vec::new();
    let mut v6: Vec<String> = Vec::new();
    for addr in info.get_addresses() {
        let ip = addr.to_ip_addr();
        if ip.is_loopback() {
            continue;
        }
        if addr.is_ipv4() {
            v4.push(ip.to_string());
        } else {
            v6.push(ip.to_string());
        }
    }
    v4.sort();
    v6.sort();
    v4.extend(v6);
    v4
}

fn handle_resolved(state: &AppState, info: &ResolvedService) -> bool {
    let addresses = addresses_of(info);
    if addresses.is_empty() {
        return false;
    }
    state
        .upsert_discovered(
            info.get_fullname(),
            info.get_hostname(),
            info.get_port(),
            addresses,
        )
        .is_some()
}

/// Background thread: browse forever, recreating the mDNS daemon on failure.
pub fn spawn_browser(state: Arc<AppState>) {
    thread::Builder::new()
        .name("mdns-browser".into())
        .spawn(move || loop {
            if state.shutdown_requested() {
                return;
            }
            match browse_until_closed(&state) {
                Ok(()) => {}
                Err(err) => eprintln!("[keylightd] mDNS browser stopped: {err}; retrying in 10s"),
            }
            for _ in 0..40 {
                if state.shutdown_requested() {
                    return;
                }
                thread::sleep(Duration::from_millis(250));
            }
        })
        .expect("failed to spawn mDNS browser thread");
}

fn browse_until_closed(state: &AppState) -> Result<(), String> {
    let daemon = ServiceDaemon::new().map_err(|e| e.to_string())?;
    let receiver = daemon.browse(SERVICE_TYPE).map_err(|e| e.to_string())?;
    loop {
        if state.shutdown_requested() {
            let _ = daemon.shutdown();
            return Ok(());
        }
        match receiver.recv_timeout(Duration::from_secs(1)) {
            Ok(ServiceEvent::ServiceResolved(info)) => {
                handle_resolved(state, &info);
            }
            Ok(ServiceEvent::ServiceRemoved(_, fullname)) => {
                state.mark_unreachable_by_mdns(&fullname);
            }
            Ok(_) => {}
            Err(_) if !receiver.is_disconnected() => {} // timeout, keep listening
            Err(_) => {
                let _ = daemon.shutdown();
                return Err("mDNS channel closed".into());
            }
        }
    }
}

/// Blocking scan for `timeout`, merging every resolution. Returns the number
/// of distinct services seen.
pub fn scan_once(state: &AppState, timeout: Duration) -> Result<usize, String> {
    let daemon = ServiceDaemon::new().map_err(|e| e.to_string())?;
    let receiver = daemon.browse(SERVICE_TYPE).map_err(|e| e.to_string())?;
    let deadline = Instant::now() + timeout;
    let mut seen: Vec<String> = Vec::new();

    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(remaining) {
            Ok(ServiceEvent::ServiceResolved(info)) => {
                let name = info.get_fullname().to_string();
                if !seen.contains(&name) {
                    seen.push(name);
                }
                handle_resolved(state, &info);
            }
            Ok(ServiceEvent::SearchStopped(_)) => break,
            Ok(_) => {}
            Err(_) => break,
        }
    }

    let _ = daemon.stop_browse(SERVICE_TYPE);
    let _ = daemon.shutdown();
    Ok(seen.len())
}
