use clap::{Parser, Subcommand};
use flume::RecvTimeoutError;
use mdns_sd::{ServiceDaemon, ServiceEvent};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;
use tiny_http::{Method, Response, Server, StatusCode};

const KELVIN_MIN: u16 = 2900;
const KELVIN_MAX: u16 = 7000;
const MIRED_MIN: u16 = (1_000_000u32 / KELVIN_MAX as u32) as u16;
const MIRED_MAX: u16 = (1_000_000u32 / KELVIN_MIN as u32) as u16;

fn default_enabled() -> bool {
    true
}

#[derive(Parser, Debug)]
#[command(name = "keylightd", version, about = "Elgato Key Light control spike")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Fetch current light state from /elgato/lights
    Get {
        /// Device IP address (e.g. 192.168.1.61)
        #[arg(long)]
        ip: Option<String>,
        /// Persisted light id (from `list`)
        #[arg(long)]
        id: Option<String>,
    },
    /// Fetch device info from /elgato/accessory-info
    Info {
        /// Device IP address (e.g. 192.168.1.61)
        #[arg(long)]
        ip: Option<String>,
        /// Persisted light id (from `list`)
        #[arg(long)]
        id: Option<String>,
    },
    /// Discover Elgato lights on the local network via mDNS
    Discover {
        /// How long to wait for responses (seconds)
        #[arg(long, default_value_t = 3)]
        timeout: u64,
    },
    /// Refresh persisted lights by re-running discovery
    Refresh {
        /// How long to wait for responses (seconds)
        #[arg(long, default_value_t = 3)]
        timeout: u64,
    },
    /// Run the local HTTP API server
    Serve {
        /// Port to bind on localhost
        #[arg(long, default_value_t = 9124)]
        port: u16,
    },
    /// Show persisted lights from the last discovery
    List,
    /// Assign a friendly name to a persisted light
    Name {
        /// Persisted light id (from `list`)
        #[arg(long)]
        id: String,
        /// Friendly name (e.g. leftlight)
        #[arg(long)]
        name: String,
    },
    /// Add or update a group of lights
    GroupAdd {
        /// Group name (e.g. office)
        #[arg(long)]
        name: String,
        /// Members by id/name/alias (repeat for multiple)
        #[arg(long = "id", required = true)]
        members: Vec<String>,
    },
    /// List configured groups
    GroupList,
    /// Update light state via /elgato/lights
    Set {
        /// Device IP address (e.g. 192.168.1.61)
        #[arg(long)]
        ip: Option<String>,
        /// Persisted light id (from `list`)
        #[arg(long)]
        id: Option<String>,
        /// Group name (from `group-list`)
        #[arg(long)]
        group: Option<String>,
        /// Target all persisted lights
        #[arg(long, default_value_t = false)]
        all: bool,
        /// 0 = off, 1 = on
        #[arg(long)]
        on: Option<u8>,
        /// Brightness percentage (0-100)
        #[arg(long)]
        brightness: Option<u8>,
        /// Color temperature in Kelvin (2900-7000)
        #[arg(long)]
        kelvin: Option<u16>,
        /// Color temperature in mired (143-344)
        #[arg(long)]
        mired: Option<u16>,
    },
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct LightsPayload<T> {
    number_of_lights: u8,
    lights: Vec<T>,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct LightState {
    on: u8,
    brightness: u8,
    temperature: u16,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
#[serde(rename_all = "camelCase")]
struct LightUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    on: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    brightness: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<u16>,
}

#[derive(Serialize, Deserialize, Debug, Default)]
struct Config {
    lights: Vec<LightRecord>,
    #[serde(default)]
    groups: Vec<Group>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct LightRecord {
    id: String,
    alias: Option<String>,
    name: String,
    hostname: String,
    port: u16,
    addresses: Vec<String>,
    last_seen_unix: u64,
    #[serde(default = "default_enabled")]
    enabled: bool,
    #[serde(default)]
    accessory_info: Option<Value>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Group {
    name: String,
    members: Vec<String>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    let client = Client::builder().timeout(Duration::from_secs(3)).build()?;
    match cli.command {
        Command::Get { ip, id } => {
            let ip = resolve_ip(ip, id)?;
            let base_url = format!("http://{}:9123/elgato", ip);
            let payload: LightsPayload<LightState> = client
                .get(format!("{}/lights", base_url))
                .send()?
                .error_for_status()?
                .json()?;
            print_lights(&payload);
        }
        Command::Info { ip, id } => {
            let ip = resolve_ip(ip, id)?;
            let base_url = format!("http://{}:9123/elgato", ip);
            let info: Value = client
                .get(format!("{}/accessory-info", base_url))
                .send()?
                .error_for_status()?
                .json()?;
            println!("{}", serde_json::to_string_pretty(&info)?);
        }
        Command::Discover { timeout } => {
            discover_lights(&client, Duration::from_secs(timeout))?;
        }
        Command::Refresh { timeout } => {
            discover_lights(&client, Duration::from_secs(timeout))?;
        }
        Command::Serve { port } => {
            run_api_server(&client, port)?;
        }
        Command::List => {
            let config = load_config()?;
            if config.lights.is_empty() {
                println!("No persisted lights found. Run `discover` first.");
            } else {
                for light in config.lights {
                    let accessory_info = light
                        .accessory_info
                        .as_ref()
                        .and_then(|value| serde_json::to_string(value).ok())
                        .unwrap_or_else(|| "-".to_string());
                    println!(
                        "id={}, alias={}, name={}, host={}, port={}, addresses=[{}], last_seen_unix={}, accessory_info={}",
                        light.id,
                        light.alias.as_deref().unwrap_or("-"),
                        light.name,
                        light.hostname,
                        light.port,
                        light.addresses.join(", "),
                        light.last_seen_unix,
                        accessory_info
                    );
                }
            }
        }
        Command::Name { id, name } => {
            let mut config = load_config()?;
            let record_id = {
                let record = config
                    .lights
                    .iter_mut()
                    .find(|light| {
                        light.id == id || light.name == id || light.alias.as_deref() == Some(&id)
                    })
                    .ok_or_else(|| format!("No persisted light found with id '{}'", id))?;
                record.alias = Some(name);
                record.id.clone()
            };
            save_config(&config)?;
            println!("Updated alias for {}", record_id);
        }
        Command::GroupAdd { name, members } => {
            let mut config = load_config()?;
            let mut members = members;
            members.sort();
            members.dedup();
            let group = Group {
                name: name.clone(),
                members,
            };
            match config.groups.iter_mut().find(|group| group.name == name) {
                Some(existing) => *existing = group,
                None => config.groups.push(group),
            }
            save_config(&config)?;
            println!("Saved group '{}'", name);
        }
        Command::GroupList => {
            let config = load_config()?;
            if config.groups.is_empty() {
                println!("No groups configured. Use `group-add` first.");
            } else {
                for group in config.groups {
                    println!(
                        "group={}, members=[{}]",
                        group.name,
                        group.members.join(", ")
                    );
                }
            }
        }
        Command::Set {
            ip,
            id,
            group,
            all,
            on,
            brightness,
            kelvin,
            mired,
        } => {
            if on.is_none() && brightness.is_none() && kelvin.is_none() && mired.is_none() {
                return Err(
                    "set requires at least one of --on, --brightness, --kelvin, --mired".into(),
                );
            }
            if let Some(value) = on {
                if value > 1 {
                    return Err("--on must be 0 or 1".into());
                }
            }
            let temperature = mired
                .map(clamp_mired)
                .or_else(|| kelvin.map(kelvin_to_mired));
            let update = LightUpdate {
                on,
                brightness: brightness.map(|v| v.min(100)),
                temperature,
            };
            let targets = resolve_targets(ip, id, group, all)?;
            for ip in targets {
                let response = set_light(&client, &ip, &update)?;
                print_lights(&response);
            }
        }
    }

    Ok(())
}

fn discover_lights(client: &Client, timeout: Duration) -> Result<(), Box<dyn Error>> {
    let daemon = ServiceDaemon::new()?;
    let receiver = daemon.browse("_elg._tcp.local.")?;
    let deadline = std::time::Instant::now() + timeout;
    let mut found_any = false;
    let mut config = load_config().unwrap_or_default();

    while std::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        match receiver.recv_timeout(remaining) {
            Ok(event) => match event {
                ServiceEvent::ServiceResolved(info) => {
                    found_any = true;
                    let addrs = info
                        .get_addresses()
                        .iter()
                        .map(|addr| addr.to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    println!(
                        "name={}, host={}, port={}, addresses=[{}]",
                        info.get_fullname(),
                        info.get_hostname(),
                        info.get_port(),
                        addrs
                    );
                    upsert_record(client, &mut config, &info);
                }
                ServiceEvent::SearchStopped(_) => break,
                _ => {}
            },
            Err(RecvTimeoutError::Timeout) => break,
            Err(err) => return Err(err.into()),
        }
    }

    if !found_any {
        println!("No _elg._tcp.local. services discovered within timeout.");
    } else {
        save_config(&config)?;
    }

    daemon.stop_browse("_elg._tcp.local.")?;
    Ok(())
}

fn run_api_server(client: &Client, port: u16) -> Result<(), Box<dyn Error>> {
    let server = Server::http(("127.0.0.1", port)).map_err(|err| -> Box<dyn Error> {
        format!("Failed to bind 127.0.0.1:{port} (is the port already in use?): {err}").into()
    })?;
    println!("keylightd API listening on http://127.0.0.1:{port}");

    for mut request in server.incoming_requests() {
        let method = request.method().clone();
        let url = request.url().to_string();
        let (path, _query) = url.split_once('?').unwrap_or((url.as_str(), ""));

        let mut body = String::new();
        let _ = std::io::Read::read_to_string(&mut request.as_reader(), &mut body);

        let response = handle_api_request(client, &method, path, &body);
        request.respond(response).ok();
    }

    Ok(())
}

fn handle_api_request(
    client: &Client,
    method: &Method,
    path: &str,
    body: &str,
) -> Response<std::io::Cursor<Vec<u8>>> {
    match (method, path) {
        (Method::Get, "/v1/health") => {
            json_response(StatusCode(200), &serde_json::json!({"status": "ok"}))
        }
        (Method::Get, "/v1/lights") => match load_config() {
            Ok(config) => json_response(StatusCode(200), &config.lights),
            Err(err) => json_error(StatusCode(500), err),
        },
        (Method::Post, "/v1/lights") => {
            let request: AddLightRequest = match serde_json::from_str(body) {
                Ok(value) => value,
                Err(_) => return json_error(StatusCode(400), "Invalid JSON body for add light"),
            };
            match add_light_by_ip(client, request.ip) {
                Ok(record) => json_response(StatusCode(200), &record),
                Err(err) => json_error(StatusCode(400), err),
            }
        }
        (Method::Post, "/v1/lights/refresh") => {
            let timeout = if body.trim().is_empty() {
                3u64
            } else {
                serde_json::from_str::<RefreshRequest>(body)
                    .map(|req| req.timeout)
                    .unwrap_or(3)
            };
            match discover_lights(client, Duration::from_secs(timeout)) {
                Ok(_) => json_response(StatusCode(200), &serde_json::json!({"refreshed": true})),
                Err(err) => json_error(StatusCode(500), err),
            }
        }
        (Method::Get, "/v1/groups") => match load_config() {
            Ok(config) => json_response(StatusCode(200), &config.groups),
            Err(err) => json_error(StatusCode(500), err),
        },
        (Method::Post, "/v1/groups") => {
            let request: GroupRequest = match serde_json::from_str(body) {
                Ok(value) => value,
                Err(_) => return json_error(StatusCode(400), "Invalid JSON body for group"),
            };
            match save_group(request.name, request.members) {
                Ok(group) => json_response(StatusCode(200), &group),
                Err(err) => json_error(StatusCode(400), err),
            }
        }
        (Method::Delete, path) if path.starts_with("/v1/groups/") => {
            let raw_name = &path["/v1/groups/".len()..];
            let group_name = urlencoding::decode(raw_name)
                .map(|value| value.into_owned())
                .unwrap_or_else(|_| raw_name.to_string());
            match delete_group(group_name) {
                Ok(_) => json_response(StatusCode(200), &serde_json::json!({"deleted": true})),
                Err(err) => json_error(StatusCode(404), err),
            }
        }
        (Method::Put, path) if path.starts_with("/v1/lights/") => {
            let raw_id = &path["/v1/lights/".len()..];
            if let Some(raw_id) = raw_id.strip_suffix("/enabled") {
                let id = urlencoding::decode(raw_id)
                    .map(|value| value.into_owned())
                    .unwrap_or_else(|_| raw_id.to_string());
                let request: EnabledRequest = match serde_json::from_str(body) {
                    Ok(value) => value,
                    Err(_) => {
                        return json_error(StatusCode(400), "Invalid JSON body for enabled request")
                    }
                };
                match set_light_enabled(id, request.enabled) {
                    Ok(record) => return json_response(StatusCode(200), &record),
                    Err(err) => return json_error(StatusCode(400), err),
                }
            }
            let id = urlencoding::decode(raw_id)
                .map(|value| value.into_owned())
                .unwrap_or_else(|_| raw_id.to_string());
            let update: UpdateRequest = match serde_json::from_str(body) {
                Ok(value) => value,
                Err(_) => {
                    return json_error(StatusCode(400), "Invalid JSON body for update request");
                }
            };
            match apply_update_to_targets(client, Some(id), None, false, update) {
                Ok(results) => json_response(StatusCode(200), &results),
                Err(err) => json_error(StatusCode(400), err),
            }
        }
        (Method::Put, path) if path.starts_with("/v1/groups/") => {
            let raw_name = &path["/v1/groups/".len()..];
            let group_name = urlencoding::decode(raw_name)
                .map(|value| value.into_owned())
                .unwrap_or_else(|_| raw_name.to_string());
            let update: UpdateRequest = match serde_json::from_str(body) {
                Ok(value) => value,
                Err(_) => {
                    return json_error(StatusCode(400), "Invalid JSON body for update request")
                }
            };
            match apply_update_to_targets(client, None, Some(group_name), false, update) {
                Ok(results) => json_response(StatusCode(200), &results),
                Err(err) => json_error(StatusCode(400), err),
            }
        }
        (Method::Put, "/v1/all") => {
            let update: UpdateRequest = match serde_json::from_str(body) {
                Ok(value) => value,
                Err(_) => {
                    return json_error(StatusCode(400), "Invalid JSON body for update request")
                }
            };
            match apply_update_to_targets(client, None, None, true, update) {
                Ok(results) => json_response(StatusCode(200), &results),
                Err(err) => json_error(StatusCode(400), err),
            }
        }
        _ => json_error(StatusCode(404), "Not found"),
    }
}

#[derive(Deserialize)]
struct UpdateRequest {
    on: Option<u8>,
    brightness: Option<u8>,
    kelvin: Option<u16>,
    mired: Option<u16>,
}

#[derive(Deserialize)]
struct RefreshRequest {
    timeout: u64,
}

#[derive(Deserialize)]
struct AddLightRequest {
    ip: String,
}

#[derive(Deserialize)]
struct GroupRequest {
    name: String,
    members: Vec<String>,
}

#[derive(Deserialize)]
struct EnabledRequest {
    enabled: bool,
}

fn json_response<T: Serialize>(
    status: StatusCode,
    value: &T,
) -> Response<std::io::Cursor<Vec<u8>>> {
    let body = serde_json::to_vec_pretty(value).unwrap_or_else(|_| b"{}".to_vec());
    Response::from_data(body)
        .with_status_code(status)
        .with_header(
            tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap(),
        )
}

fn json_error<E: std::fmt::Display>(
    status: StatusCode,
    err: E,
) -> Response<std::io::Cursor<Vec<u8>>> {
    json_response(status, &serde_json::json!({ "error": err.to_string() }))
}

fn print_lights(payload: &LightsPayload<LightState>) {
    for (index, light) in payload.lights.iter().enumerate() {
        let kelvin = mired_to_kelvin(light.temperature);
        println!(
            "light[{}]: on={}, brightness={}, temperature_mired={}, temperature_kelvin={}",
            index, light.on, light.brightness, light.temperature, kelvin
        );
    }
}

fn kelvin_to_mired(kelvin: u16) -> u16 {
    let clamped = kelvin.clamp(KELVIN_MIN, KELVIN_MAX) as u32;
    let mired = ((1_000_000u32 + clamped / 2) / clamped) as u16;
    clamp_mired(mired)
}

fn mired_to_kelvin(mired: u16) -> u16 {
    let clamped = clamp_mired(mired) as u32;
    ((1_000_000u32 + clamped / 2) / clamped) as u16
}

fn clamp_mired(mired: u16) -> u16 {
    mired.clamp(MIRED_MIN, MIRED_MAX)
}

fn resolve_ip(ip: Option<String>, id: Option<String>) -> Result<String, Box<dyn Error>> {
    match (ip, id) {
        (Some(ip), None) => Ok(ip),
        (None, Some(id)) => {
            let config = load_config()?;
            resolve_ip_from_config(&config, &id)
                .ok_or_else(|| format!("No persisted light found with id '{}'", id).into())
        }
        (Some(_), Some(_)) => Err("Use either --ip or --id, not both".into()),
        (None, None) => Err("You must provide either --ip or --id".into()),
    }
}

fn resolve_targets(
    ip: Option<String>,
    id: Option<String>,
    group: Option<String>,
    all: bool,
) -> Result<Vec<String>, Box<dyn Error>> {
    let target_count = [ip.is_some(), id.is_some(), group.is_some(), all]
        .iter()
        .filter(|&&value| value)
        .count();
    if target_count != 1 {
        return Err("Provide exactly one of --ip, --id, --group, or --all".into());
    }

    if let Some(ip) = ip {
        return Ok(vec![ip]);
    }
    if let Some(id) = id {
        return Ok(vec![resolve_ip(None, Some(id))?]);
    }

    let config = load_config()?;
    if all {
        let mut ips = config
            .lights
            .iter()
            .filter(|light| light.enabled)
            .filter_map(select_address)
            .collect::<Vec<_>>();
        if ips.is_empty() {
            return Err("No persisted lights found. Run `discover` first.".into());
        }
        ips.sort();
        ips.dedup();
        return Ok(ips);
    }

    let group_name = group.unwrap_or_default();
    let group = config
        .groups
        .iter()
        .find(|group| group.name == group_name)
        .ok_or_else(|| format!("No group named '{}'", group_name))?;
    let mut ips = Vec::new();
    for member in &group.members {
        if let Some(ip) = resolve_ip_from_config(&config, member) {
            ips.push(ip);
        }
    }
    ips.sort();
    ips.dedup();
    if ips.is_empty() {
        return Err(format!("Group '{}' has no enabled members", group.name).into());
    }
    Ok(ips)
}

fn select_address(record: &LightRecord) -> Option<String> {
    select_address_from_list(&record.addresses)
}

fn select_address_from_list(addresses: &[String]) -> Option<String> {
    addresses
        .iter()
        .find(|addr| addr.contains('.'))
        .cloned()
        .or_else(|| addresses.first().cloned())
}

fn resolve_ip_from_config(config: &Config, ident: &str) -> Option<String> {
    let record = config.lights.iter().find(|light| {
        light.id == ident || light.name == ident || light.alias.as_deref() == Some(ident)
    })?;
    if !record.enabled {
        return None;
    }
    select_address(record)
}

fn fetch_accessory_info(client: &Client, ip: &str) -> Option<Value> {
    let base_url = format!("http://{}:9123/elgato", ip);
    client
        .get(format!("{}/accessory-info", base_url))
        .send()
        .ok()?
        .error_for_status()
        .ok()?
        .json()
        .ok()
}

fn set_light(
    client: &Client,
    ip: &str,
    update: &LightUpdate,
) -> Result<LightsPayload<LightState>, Box<dyn Error>> {
    let base_url = format!("http://{}:9123/elgato", ip);
    let payload = LightsPayload {
        number_of_lights: 1,
        lights: vec![update.clone()],
    };
    let response: LightsPayload<LightState> = client
        .put(format!("{}/lights", base_url))
        .json(&payload)
        .send()?
        .error_for_status()?
        .json()?;
    Ok(response)
}

fn apply_update_to_targets(
    client: &Client,
    id: Option<String>,
    group: Option<String>,
    all: bool,
    update: UpdateRequest,
) -> Result<Vec<LightsPayload<LightState>>, Box<dyn Error>> {
    let update = LightUpdate {
        on: update.on,
        brightness: update.brightness.map(|v| v.min(100)),
        temperature: update
            .mired
            .map(clamp_mired)
            .or_else(|| update.kelvin.map(kelvin_to_mired)),
    };
    let targets = resolve_targets(None, id, group, all)?;
    let mut results = Vec::new();
    for ip in targets {
        results.push(set_light(client, &ip, &update)?);
    }
    Ok(results)
}

fn save_group(name: String, mut members: Vec<String>) -> Result<Group, Box<dyn Error>> {
    let mut config = load_config()?;
    members.sort();
    members.dedup();
    let group = Group {
        name: name.clone(),
        members,
    };
    match config.groups.iter_mut().find(|group| group.name == name) {
        Some(existing) => *existing = group.clone(),
        None => config.groups.push(group.clone()),
    }
    save_config(&config)?;
    Ok(group)
}

fn delete_group(name: String) -> Result<(), Box<dyn Error>> {
    let mut config = load_config()?;
    let original_len = config.groups.len();
    config.groups.retain(|group| group.name != name);
    if config.groups.len() == original_len {
        return Err(format!("No group named '{}'", name).into());
    }
    save_config(&config)?;
    Ok(())
}

fn add_light_by_ip(client: &Client, ip: String) -> Result<LightRecord, Box<dyn Error>> {
    let info = fetch_accessory_info(client, &ip)
        .ok_or_else(|| "Unable to fetch accessory-info from device".to_string())?;
    let serial = info
        .get("serialNumber")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let display_name = info
        .get("displayName")
        .and_then(|v| v.as_str())
        .filter(|value| !value.is_empty())
        .or_else(|| info.get("productName").and_then(|v| v.as_str()))
        .unwrap_or("Elgato Light");
    let id = format!("manual-{}", serial);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let record = LightRecord {
        id: id.clone(),
        alias: None,
        name: display_name.to_string(),
        hostname: ip.clone(),
        port: 9123,
        addresses: vec![ip],
        last_seen_unix: now,
        enabled: true,
        accessory_info: Some(info),
    };

    let mut config = load_config()?;
    match config.lights.iter_mut().find(|item| item.id == id) {
        Some(existing) => *existing = record.clone(),
        None => config.lights.push(record.clone()),
    }
    save_config(&config)?;
    Ok(record)
}

fn set_light_enabled(id: String, enabled: bool) -> Result<LightRecord, Box<dyn Error>> {
    let mut config = load_config()?;
    let record_clone = {
        let record = config
            .lights
            .iter_mut()
            .find(|light| light.id == id || light.name == id || light.alias.as_deref() == Some(&id))
            .ok_or_else(|| format!("No persisted light found with id '{}'", id))?;
        record.enabled = enabled;
        record.clone()
    };
    save_config(&config)?;
    Ok(record_clone)
}
fn upsert_record(client: &Client, config: &mut Config, info: &mdns_sd::ResolvedService) {
    let id = info.get_fullname().to_string();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let existing = config.lights.iter().find(|item| item.id == id);
    let alias = existing.and_then(|item| item.alias.clone());
    let previous_accessory = existing.and_then(|item| item.accessory_info.clone());
    let enabled = existing.map(|item| item.enabled).unwrap_or(true);
    let addresses = info
        .get_addresses()
        .iter()
        .map(|addr| addr.to_string())
        .collect::<Vec<_>>();
    let primary_ip = select_address_from_list(&addresses);
    let accessory_info = primary_ip
        .as_deref()
        .and_then(|ip| fetch_accessory_info(client, ip))
        .or(previous_accessory);
    let record = LightRecord {
        id: id.clone(),
        alias,
        name: info.get_fullname().to_string(),
        hostname: info.get_hostname().to_string(),
        port: info.get_port(),
        addresses,
        last_seen_unix: now,
        enabled,
        accessory_info,
    };

    match config.lights.iter_mut().find(|item| item.id == id) {
        Some(existing) => *existing = record,
        None => config.lights.push(record),
    }
}

fn config_path() -> Result<PathBuf, Box<dyn Error>> {
    let base = if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        PathBuf::from(xdg)
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".config")
    } else {
        return Err("Unable to determine config directory".into());
    };

    Ok(base.join("limekit-keylight").join("config.json"))
}

fn load_config() -> Result<Config, Box<dyn Error>> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(Config::default());
    }
    let bytes = fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn save_config(config: &Config) -> Result<(), Box<dyn Error>> {
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(config)?;
    fs::write(path, bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kelvin_to_mired_clamps_and_rounds() {
        assert_eq!(kelvin_to_mired(7000), 143);
        assert_eq!(kelvin_to_mired(2900), 344);
        assert_eq!(kelvin_to_mired(1000), 344);
    }

    #[test]
    fn mired_to_kelvin_clamps_and_rounds() {
        assert_eq!(mired_to_kelvin(143), 6993);
        assert_eq!(mired_to_kelvin(344), 2907);
        assert_eq!(mired_to_kelvin(999), 2907);
    }
}
