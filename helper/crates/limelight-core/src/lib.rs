//! limelight-core — everything shared between `keylightd` and `keylight-gui`.
//!
//! - [`config`]: persisted config model, atomic save, schema migrations.
//! - [`elgato`]: blocking HTTP client for the Elgato local API (port 9123).
//! - [`api`]: request/response types of the `keylightd` localhost API.
//! - [`convert`]: kelvin / mired / slider conversions.

pub mod api;
pub mod config;
pub mod convert;
pub mod elgato;

/// Version of the whole workspace (single-sourced from `[workspace.package]`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Default localhost port of the daemon API.
pub const DEFAULT_DAEMON_PORT: u16 = 9124;

/// Environment variable that overrides the daemon port for both binaries.
pub const DAEMON_PORT_ENV: &str = "LIMELIGHT_PORT";

/// Resolve the daemon port: `LIMELIGHT_PORT` if set and valid, else the default.
pub fn daemon_port() -> u16 {
    std::env::var(DAEMON_PORT_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u16>().ok())
        .filter(|p| *p != 0)
        .unwrap_or(DEFAULT_DAEMON_PORT)
}

/// Base URL of the daemon API, e.g. `http://127.0.0.1:9124`.
pub fn daemon_base_url() -> String {
    format!("http://127.0.0.1:{}", daemon_port())
}

/// Seconds since the Unix epoch (0 if the clock is broken).
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
