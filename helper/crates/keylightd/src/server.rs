//! Localhost HTTP API. Requests are served by a small fixed pool of worker
//! threads so a slow light never blocks the GUI's slider updates.

use crate::discovery;
use crate::state::{AppState, Target};
use limelight_core::api::{
    decode_path_segment, AddLightRequest, AliasRequest, EnabledRequest, GroupRequest,
    HealthResponse, NameRequest, RefreshRequest, RefreshResponse, UpdateRequest,
};
use limelight_core::config::{Preset, Settings};
use limelight_core::elgato::DeviceSettings;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::io::Read as _;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

const WORKERS: usize = 4;
const MAX_BODY_BYTES: usize = 64 * 1024;
const MIN_REFRESH_SECS: u64 = 1;
const MAX_REFRESH_SECS: u64 = 15;
const DEFAULT_REFRESH_SECS: u64 = 3;

type Resp = Response<std::io::Cursor<Vec<u8>>>;

pub fn run(state: Arc<AppState>, port: u16) -> Result<(), String> {
    let server = Server::http(("127.0.0.1", port))
        .map_err(|e| format!("Failed to bind 127.0.0.1:{port} (already in use?): {e}"))?;
    let server = Arc::new(server);
    println!(
        "keylightd {} listening on http://127.0.0.1:{port}",
        limelight_core::VERSION
    );

    let limiter = Arc::new(Mutex::new(RateLimiter::new()));
    let mut handles = Vec::with_capacity(WORKERS);
    for n in 0..WORKERS {
        let server = Arc::clone(&server);
        let state = Arc::clone(&state);
        let limiter = Arc::clone(&limiter);
        handles.push(
            thread::Builder::new()
                .name(format!("api-{n}"))
                .spawn(move || loop {
                    match server.recv_timeout(Duration::from_millis(500)) {
                        Ok(Some(request)) => handle(&state, &limiter, port, request),
                        Ok(None) => {
                            if state.shutdown_requested() {
                                return;
                            }
                        }
                        Err(err) => {
                            eprintln!("[keylightd] accept error: {err}");
                            if state.shutdown_requested() {
                                return;
                            }
                        }
                    }
                })
                .map_err(|e| e.to_string())?,
        );
    }

    while !state.shutdown_requested() {
        thread::sleep(Duration::from_millis(250));
    }
    server.unblock();
    for handle in handles {
        let _ = handle.join();
    }
    println!("keylightd shutting down");
    Ok(())
}

fn handle(state: &AppState, limiter: &Mutex<RateLimiter>, port: u16, mut request: Request) {
    let method = request.method().clone();
    let url = request.url().to_string();
    let path = url.split('?').next().unwrap_or("").to_string();

    if !host_allowed(&request, port) {
        respond(request, error(403, "Forbidden: bad Host header"));
        return;
    }

    if !limiter
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .allow(&method, &path)
    {
        respond(request, error(429, "Too many requests. Please slow down."));
        return;
    }

    let body = match read_body(&mut request) {
        Ok(body) => body,
        Err(resp) => {
            respond(request, resp);
            return;
        }
    };

    let response = route(state, &method, &path, &body);
    let shutting_down = state.shutdown_requested();
    respond(request, response);
    if shutting_down {
        // Give the response a moment to flush, then let `run` unwind.
        thread::sleep(Duration::from_millis(100));
    }
}

fn respond(request: Request, response: Resp) {
    let _ = request.respond(response);
}

/// Only accept requests addressed to us by loopback name. Blocks DNS-rebinding
/// pages from driving the lights through the browser.
fn host_allowed(request: &Request, port: u16) -> bool {
    let host = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Host"))
        .map(|h| h.value.as_str().trim().to_ascii_lowercase());
    let Some(host) = host else {
        return true; // HTTP/1.0 clients; still loopback-bound
    };
    let (name, hport) = if let Some(rest) = host.strip_prefix('[') {
        let end = rest.find(']').unwrap_or(rest.len());
        let name = &rest[..end];
        let hport = rest[end..].strip_prefix("]:").map(str::to_string);
        (name.to_string(), hport)
    } else if let Some((n, p)) = host.rsplit_once(':') {
        (n.to_string(), Some(p.to_string()))
    } else {
        (host.clone(), None)
    };
    let name_ok = name == "localhost"
        || name
            .parse::<IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false);
    let port_ok = hport.map(|p| p == port.to_string()).unwrap_or(true);
    name_ok && port_ok
}

fn read_body(request: &mut Request) -> Result<String, Resp> {
    let mut limited = request.as_reader().take((MAX_BODY_BYTES + 1) as u64);
    let mut bytes = Vec::new();
    limited
        .read_to_end(&mut bytes)
        .map_err(|e| server_error("reading request body", e))?;
    if bytes.len() > MAX_BODY_BYTES {
        return Err(error(413, "Request body too large."));
    }
    String::from_utf8(bytes).map_err(|_| error(400, "Request body must be valid UTF-8."))
}

// ---------------------------------------------------------------------------
// Routing
// ---------------------------------------------------------------------------

fn route(state: &AppState, method: &Method, path: &str, body: &str) -> Resp {
    let segs: Vec<String> = path
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .map(decode_path_segment)
        .collect();
    let s: Vec<&str> = segs.iter().map(String::as_str).collect();

    match (method, s.as_slice()) {
        (Method::Get, ["v1", "health"]) => {
            let (enabled, _) = state.read(|c| (c.lights.iter().filter(|l| l.enabled).count(), ()));
            json(
                200,
                &HealthResponse {
                    status: "ok".into(),
                    version: limelight_core::VERSION.into(),
                    reachable: state.reachable_count(),
                    enabled,
                },
            )
        }
        (Method::Post, ["v1", "shutdown"]) => {
            state.request_shutdown();
            json(200, &serde_json::json!({ "shutdown": true }))
        }

        // ---- lights ----
        (Method::Get, ["v1", "lights"]) => json(200, &state.read(|c| c.lights.clone())),
        (Method::Post, ["v1", "lights"]) => {
            let req: AddLightRequest = match parse(body) {
                Ok(v) => v,
                Err(r) => return r,
            };
            let ip = match validate_lan_ip(&req.ip) {
                Ok(ip) => ip.to_string(),
                Err(msg) => return error(400, msg),
            };
            match state.add_by_ip(ip) {
                Ok(record) => json(200, &record),
                Err(msg) => error(502, &msg),
            }
        }
        (Method::Post, ["v1", "lights", "refresh"]) => {
            let timeout = if body.trim().is_empty() {
                DEFAULT_REFRESH_SECS
            } else {
                serde_json::from_str::<RefreshRequest>(body)
                    .ok()
                    .and_then(|r| r.timeout)
                    .unwrap_or(DEFAULT_REFRESH_SECS)
            }
            .clamp(MIN_REFRESH_SECS, MAX_REFRESH_SECS);
            match discovery::scan_once(state, Duration::from_secs(timeout)) {
                Ok(found) => {
                    // Also probe every enabled light so reachability is current.
                    let _ = state.states();
                    json(
                        200,
                        &RefreshResponse {
                            refreshed: true,
                            found,
                            lights: state.read(|c| c.lights.clone()),
                        },
                    )
                }
                Err(err) => server_error("refresh discovery", err),
            }
        }
        (Method::Get, ["v1", "lights", "states"]) => json(200, &state.states()),

        (Method::Get, ["v1", "lights", id]) => match state.record(id) {
            Some(r) => json(200, &r),
            None => error(404, &format!("No light found with id '{id}'")),
        },
        (Method::Get, ["v1", "lights", id, "settings"]) => match state.device_settings(id) {
            Ok(s) => json(200, &s),
            Err(msg) => error(502, &msg),
        },
        (Method::Put, ["v1", "lights", id, "settings"]) => {
            let req: DeviceSettings = match parse(body) {
                Ok(v) => v,
                Err(r) => return r,
            };
            if let Some(b) = req.power_on_behavior {
                if !(1..=2).contains(&b) {
                    return error(
                        400,
                        "'powerOnBehavior' must be 1 (restore last) or 2 (use defaults)",
                    );
                }
            }
            if matches!(req.power_on_brightness, Some(b) if b > 100) {
                return error(400, "'powerOnBrightness' must be 0..=100");
            }
            match state.set_device_settings(id, &req) {
                Ok(s) => json(200, &s),
                Err(msg) => error(502, &msg),
            }
        }
        (Method::Put, ["v1", "lights", id]) => {
            let update: UpdateRequest = match parse(body) {
                Ok(v) => v,
                Err(r) => return r,
            };
            apply_update(state, Target::Light((*id).to_string()), update)
        }
        (Method::Delete, ["v1", "lights", id]) => {
            let removed = state.write(|c| c.remove_light(id));
            if removed {
                json(200, &serde_json::json!({ "deleted": true }))
            } else {
                error(404, &format!("No light found with id '{id}'"))
            }
        }
        (Method::Put, ["v1", "lights", id, "enabled"]) => {
            let req: EnabledRequest = match parse(body) {
                Ok(v) => v,
                Err(r) => return r,
            };
            let record = state.write(|c| {
                c.find_light_mut(id).map(|l| {
                    l.enabled = req.enabled;
                    l.clone()
                })
            });
            match record {
                Some(r) => json(200, &r),
                None => error(404, &format!("No light found with id '{id}'")),
            }
        }
        (Method::Put, ["v1", "lights", id, "alias"]) => {
            let req: AliasRequest = match parse(body) {
                Ok(v) => v,
                Err(r) => return r,
            };
            let alias = req
                .alias
                .map(|a| a.trim().to_string())
                .filter(|a| !a.is_empty());
            let record = state.write(|c| {
                c.find_light_mut(id).map(|l| {
                    l.alias = alias;
                    l.clone()
                })
            });
            match record {
                Some(r) => json(200, &r),
                None => error(404, &format!("No light found with id '{id}'")),
            }
        }
        (Method::Put, ["v1", "lights", id, "name"]) => {
            let req: NameRequest = match parse(body) {
                Ok(v) => v,
                Err(r) => return r,
            };
            match state.rename_device(id, &req.name) {
                Ok(r) => json(200, &r),
                Err(msg) => error(502, &msg),
            }
        }
        (Method::Post, ["v1", "lights", id, "identify"]) => match state.identify(id) {
            Ok(()) => json(200, &serde_json::json!({ "identified": true })),
            Err(msg) => error(502, &msg),
        },

        // ---- groups ----
        (Method::Get, ["v1", "groups"]) => json(200, &state.read(|c| c.groups.clone())),
        (Method::Post, ["v1", "groups"]) => {
            let req: GroupRequest = match parse(body) {
                Ok(v) => v,
                Err(r) => return r,
            };
            match state.write(|c| c.save_group(req.name, req.members)) {
                Ok(group) => json(200, &group),
                Err(msg) => error(400, &msg),
            }
        }
        (Method::Put, ["v1", "groups", name]) => {
            let update: UpdateRequest = match parse(body) {
                Ok(v) => v,
                Err(r) => return r,
            };
            apply_update(state, Target::Group((*name).to_string()), update)
        }
        (Method::Delete, ["v1", "groups", name]) => {
            if state.write(|c| c.remove_group(name)) {
                json(200, &serde_json::json!({ "deleted": true }))
            } else {
                error(404, &format!("No group named '{name}'"))
            }
        }

        // ---- presets ----
        (Method::Get, ["v1", "presets"]) => json(200, &state.read(|c| c.presets.clone())),
        (Method::Post, ["v1", "presets"]) => {
            let preset: Preset = match parse(body) {
                Ok(v) => v,
                Err(r) => return r,
            };
            match state.write(|c| c.save_preset(preset)) {
                Ok(p) => json(200, &p),
                Err(msg) => error(400, &msg),
            }
        }
        (Method::Put, ["v1", "presets"]) => {
            let presets: Vec<Preset> = match parse(body) {
                Ok(v) => v,
                Err(r) => return r,
            };
            match state.write(|c| c.set_presets(presets).map(|_| c.presets.clone())) {
                Ok(list) => json(200, &list),
                Err(msg) => error(400, &msg),
            }
        }
        (Method::Delete, ["v1", "presets", name]) => {
            if state.write(|c| c.remove_preset(name)) {
                json(200, &serde_json::json!({ "deleted": true }))
            } else {
                error(404, &format!("No preset named '{name}'"))
            }
        }

        // ---- all ----
        (Method::Put, ["v1", "all"]) => {
            let update: UpdateRequest = match parse(body) {
                Ok(v) => v,
                Err(r) => return r,
            };
            apply_update(state, Target::All, update)
        }

        // ---- settings ----
        (Method::Get, ["v1", "settings"]) => json(200, &state.settings()),
        (Method::Put, ["v1", "settings"]) => {
            let req: Settings = match parse(body) {
                Ok(v) => v,
                Err(r) => return r,
            };
            let saved = state.write(|c| {
                c.settings = req;
                c.settings.clone()
            });
            json(200, &saved)
        }

        _ => error(404, "Not found"),
    }
}

fn apply_update(state: &AppState, target: Target, update: UpdateRequest) -> Resp {
    if update.is_empty() {
        return error(
            400,
            "Update needs at least one of on, brightness, kelvin, mired, hue, saturation",
        );
    }
    if matches!(update.on, Some(v) if v > 1) {
        return error(400, "'on' must be 0 or 1");
    }
    let targets = match state.resolve_targets(&target) {
        Ok(t) => t,
        Err(msg) => return error(404, &msg),
    };
    let device_update = AppState::to_device_update(&update);
    let response = state.apply(&targets, &device_update);
    let status = if response.ok { 200 } else { 502 };
    json(status, &response)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn parse<T: DeserializeOwned>(body: &str) -> Result<T, Resp> {
    serde_json::from_str::<T>(body).map_err(|e| error(400, &format!("Invalid JSON body: {e}")))
}

fn json<T: Serialize>(status: u16, value: &T) -> Resp {
    let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    Response::from_data(body)
        .with_status_code(StatusCode(status))
        .with_header(Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap())
        .with_header(Header::from_bytes(&b"X-Content-Type-Options"[..], &b"nosniff"[..]).unwrap())
        .with_header(Header::from_bytes(&b"Cache-Control"[..], &b"no-store"[..]).unwrap())
}

fn error(status: u16, message: &str) -> Resp {
    json(status, &serde_json::json!({ "error": message }))
}

fn server_error<E: std::fmt::Display>(context: &str, err: E) -> Resp {
    eprintln!("[keylightd] {context}: {err}");
    error(500, "Internal server error.")
}

/// Manual adds must point at the LAN, never at the internet or ourselves.
pub fn validate_lan_ip(ip: &str) -> Result<IpAddr, &'static str> {
    let parsed: IpAddr = ip.trim().parse().map_err(|_| "Invalid IP address.")?;
    match parsed {
        IpAddr::V4(v4) => {
            if v4.is_unspecified() || v4.is_loopback() || v4.is_multicast() || v4.is_broadcast() {
                return Err("IP address is not allowed.");
            }
            if v4.is_private() || v4.is_link_local() {
                Ok(parsed)
            } else {
                Err("IP must be a LAN address (private or link-local).")
            }
        }
        IpAddr::V6(v6) => {
            if v6.is_unspecified() || v6.is_loopback() || v6.is_multicast() {
                return Err("IP address is not allowed.");
            }
            if v6.is_unique_local() || v6.is_unicast_link_local() {
                Ok(parsed)
            } else {
                Err("IP must be a LAN address (unique-local or link-local).")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Rate limiting (per process, not per client — everything is localhost)
// ---------------------------------------------------------------------------

struct Bucket {
    window_start: Instant,
    count: u32,
}

impl Bucket {
    fn allow(&mut self, max: u32, window: Duration) -> bool {
        let now = Instant::now();
        if now.duration_since(self.window_start) >= window {
            self.window_start = now;
            self.count = 0;
        }
        if self.count >= max {
            return false;
        }
        self.count += 1;
        true
    }
}

struct RateLimiter {
    control: Bucket,
    refresh: Bucket,
}

impl RateLimiter {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            control: Bucket {
                window_start: now,
                count: 0,
            },
            refresh: Bucket {
                window_start: now,
                count: 0,
            },
        }
    }

    fn allow(&mut self, method: &Method, path: &str) -> bool {
        if *method == Method::Get && path == "/v1/health" {
            return true;
        }
        if *method == Method::Post && path == "/v1/lights/refresh" {
            self.refresh.allow(5, Duration::from_secs(10))
        } else {
            self.control.allow(400, Duration::from_secs(1))
        }
    }
}
