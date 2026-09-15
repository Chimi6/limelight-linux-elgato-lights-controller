//! Persisted configuration: lights, groups, settings.
//!
//! File: `$XDG_CONFIG_HOME/limelight-keylight/config.json` (default `~/.config/...`).
//! Writes are atomic (temp file + rename). A corrupt file is moved aside and
//! a fresh config is started so the app never dead-ends on a bad file.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::SystemTime;

use crate::elgato::AccessoryInfo;

/// Current schema version. Bump when the on-disk shape changes and add a
/// migration step in [`migrate`].
pub const CONFIG_VERSION: u32 = 2;

const APP_DIR: &str = "limelight-keylight";
const LEGACY_APP_DIR: &str = "limekit-keylight";
const FILE_NAME: &str = "config.json";

fn default_true() -> bool {
    true
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Settings {
    /// Newly discovered lights are enabled (shown) immediately.
    #[serde(default = "default_true")]
    pub auto_enable_discovered: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            auto_enable_discovered: true,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Config {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub lights: Vec<LightRecord>,
    #[serde(default)]
    pub groups: Vec<Group>,
    #[serde(default)]
    pub settings: Settings,
}

/// One known light. `id` is stable across renames and IP changes:
/// `serial:<serialNumber>` when the serial is known, otherwise
/// `mdns:<instance>` or `manual:<ip>`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LightRecord {
    pub id: String,
    /// Local nickname chosen in LimeLight (overrides `name` in the UI).
    #[serde(default)]
    pub alias: Option<String>,
    /// The device's own display name (what Control Center shows).
    pub name: String,
    #[serde(default)]
    pub hostname: String,
    #[serde(default = "default_device_port")]
    pub port: u16,
    #[serde(default)]
    pub addresses: Vec<String>,
    #[serde(default)]
    pub last_seen_unix: u64,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub accessory_info: Option<Value>,
    /// Current mDNS instance fullname (changes when the device is renamed).
    #[serde(default)]
    pub mdns_fullname: Option<String>,
    #[serde(default)]
    pub serial: Option<String>,
    #[serde(default)]
    pub product: Option<String>,
}

fn default_device_port() -> u16 {
    crate::elgato::DEVICE_PORT
}

impl LightRecord {
    /// Name shown in UIs: alias if set, else the device name.
    pub fn display_name(&self) -> &str {
        self.alias
            .as_deref()
            .filter(|a| !a.trim().is_empty())
            .unwrap_or(&self.name)
    }

    /// Preferred address for HTTP: first IPv4, else first IPv6, else none.
    pub fn primary_address(&self) -> Option<&str> {
        select_address(&self.addresses)
    }

    /// True if `ident` matches this record by id, alias, device name or mDNS name.
    pub fn matches(&self, ident: &str) -> bool {
        self.id == ident
            || self.name == ident
            || self.alias.as_deref() == Some(ident)
            || self.mdns_fullname.as_deref() == Some(ident)
    }

    /// True if the stored accessory info says this is a colour-capable light.
    pub fn is_color_capable(&self) -> bool {
        let product = self
            .product
            .as_deref()
            .or_else(|| {
                self.accessory_info
                    .as_ref()
                    .and_then(|v| v.get("productName"))
                    .and_then(|v| v.as_str())
            })
            .unwrap_or("");
        product.to_ascii_lowercase().contains("strip")
    }
}

/// Pick the address to talk to: IPv4 first, then IPv6.
pub fn select_address(addresses: &[String]) -> Option<&str> {
    addresses
        .iter()
        .find(|a| a.parse::<std::net::Ipv4Addr>().is_ok())
        .or_else(|| addresses.first())
        .map(String::as_str)
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Group {
    pub name: String,
    pub members: Vec<String>,
}

/// Build the canonical id for a light from what we know about it.
pub fn make_id(serial: Option<&str>, mdns_fullname: Option<&str>, ip: Option<&str>) -> String {
    if let Some(serial) = serial.map(str::trim).filter(|s| !s.is_empty()) {
        return format!("serial:{serial}");
    }
    if let Some(name) = mdns_fullname.map(str::trim).filter(|s| !s.is_empty()) {
        return format!("mdns:{name}");
    }
    format!("manual:{}", ip.unwrap_or("unknown"))
}

/// Strip the `._elg._tcp.local.` suffix from an mDNS instance fullname.
pub fn instance_name(fullname: &str) -> &str {
    fullname
        .split("._elg._tcp")
        .next()
        .unwrap_or(fullname)
        .trim_end_matches('.')
}

impl Config {
    pub fn find_light(&self, ident: &str) -> Option<&LightRecord> {
        self.lights.iter().find(|l| l.matches(ident))
    }

    pub fn find_light_mut(&mut self, ident: &str) -> Option<&mut LightRecord> {
        self.lights.iter_mut().find(|l| l.matches(ident))
    }

    pub fn find_group(&self, name: &str) -> Option<&Group> {
        self.groups.iter().find(|g| g.name == name)
    }

    /// Insert or replace a light by id.
    pub fn upsert_light(&mut self, record: LightRecord) {
        match self.lights.iter_mut().find(|l| l.id == record.id) {
            Some(existing) => *existing = record,
            None => self.lights.push(record),
        }
    }

    /// Remove a light and drop it from every group. Returns false if not found.
    pub fn remove_light(&mut self, ident: &str) -> bool {
        let Some(id) = self.find_light(ident).map(|l| l.id.clone()) else {
            return false;
        };
        self.lights.retain(|l| l.id != id);
        for group in &mut self.groups {
            group.members.retain(|m| *m != id);
        }
        true
    }

    /// Replace group `name` (or add it). Members are canonicalised to ids and
    /// unknown members are rejected.
    pub fn save_group(&mut self, name: String, members: Vec<String>) -> Result<Group, String> {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err("Group name must not be empty".into());
        }
        let mut ids = Vec::new();
        for member in members {
            let record = self
                .find_light(&member)
                .ok_or_else(|| format!("Unknown light '{member}'"))?;
            if !ids.contains(&record.id) {
                ids.push(record.id.clone());
            }
        }
        if ids.is_empty() {
            return Err("Group needs at least one light".into());
        }
        let group = Group { name, members: ids };
        match self.groups.iter_mut().find(|g| g.name == group.name) {
            Some(existing) => *existing = group.clone(),
            None => self.groups.push(group.clone()),
        }
        Ok(group)
    }

    pub fn remove_group(&mut self, name: &str) -> bool {
        let before = self.groups.len();
        self.groups.retain(|g| g.name != name);
        self.groups.len() != before
    }
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

fn config_base() -> io::Result<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.trim().is_empty() {
            return Ok(PathBuf::from(xdg));
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.trim().is_empty() {
            return Ok(PathBuf::from(home).join(".config"));
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "Unable to determine config directory (HOME/XDG_CONFIG_HOME unset)",
    ))
}

pub fn config_path() -> io::Result<PathBuf> {
    Ok(config_base()?.join(APP_DIR).join(FILE_NAME))
}

fn legacy_config_path() -> io::Result<PathBuf> {
    Ok(config_base()?.join(LEGACY_APP_DIR).join(FILE_NAME))
}

/// Modification time of the config file, if it exists.
pub fn config_mtime() -> Option<SystemTime> {
    fs::metadata(config_path().ok()?).ok()?.modified().ok()
}

// ---------------------------------------------------------------------------
// Load / save
// ---------------------------------------------------------------------------

/// Outcome of loading, so callers can decide whether to persist right away.
pub struct Loaded {
    pub config: Config,
    /// True when the in-memory config differs from what is on disk
    /// (migrated, recovered, or freshly created) and should be saved.
    pub dirty: bool,
}

/// Load the config, migrating older schemas and recovering from corruption.
/// Never fails on content problems; only on an unusable environment.
pub fn load() -> io::Result<Loaded> {
    let path = config_path()?;
    if path.exists() {
        return match read_file(&path) {
            Ok(config) => Ok(finish_load(config)),
            Err(err) => {
                let backup =
                    path.with_file_name(format!("config.json.corrupt-{}", crate::now_unix()));
                eprintln!(
                    "[limelight] config unreadable ({err}); moving it to {}",
                    backup.display()
                );
                let _ = fs::rename(&path, &backup);
                Ok(Loaded {
                    config: fresh(),
                    dirty: true,
                })
            }
        };
    }

    // First run on this machine: pick up the pre-rename config if present.
    let legacy = legacy_config_path()?;
    if legacy.exists() {
        if let Ok(config) = read_file(&legacy) {
            let mut loaded = finish_load(config);
            loaded.dirty = true;
            return Ok(loaded);
        }
    }

    Ok(Loaded {
        config: fresh(),
        dirty: true,
    })
}

fn fresh() -> Config {
    Config {
        version: CONFIG_VERSION,
        ..Default::default()
    }
}

fn read_file(path: &PathBuf) -> Result<Config, String> {
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    serde_json::from_slice::<Config>(&bytes).map_err(|e| e.to_string())
}

fn finish_load(mut config: Config) -> Loaded {
    let dirty = migrate(&mut config);
    Loaded { config, dirty }
}

/// Atomic save: write `config.json.tmp`, fsync, rename over the target.
pub fn save(config: &Config) -> io::Result<()> {
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(config).map_err(io::Error::other)?;
    {
        use std::io::Write as _;
        let mut file = fs::File::create(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, &path)
}

// ---------------------------------------------------------------------------
// Migrations
// ---------------------------------------------------------------------------

/// Bring `config` up to `CONFIG_VERSION`. Returns true if anything changed.
pub fn migrate(config: &mut Config) -> bool {
    let mut changed = false;

    if config.version < 2 {
        migrate_v1_to_v2(config);
        config.version = 2;
        changed = true;
    }

    // Always-on hygiene: derive serial/product from stored accessory info if missing.
    for light in &mut config.lights {
        if let Some(info) = light
            .accessory_info
            .as_ref()
            .and_then(AccessoryInfo::from_value)
        {
            if light.serial.is_none() && !info.serial_number.trim().is_empty() {
                light.serial = Some(info.serial_number.clone());
                changed = true;
            }
            if light.product.is_none() && !info.product_name.trim().is_empty() {
                light.product = Some(info.product_name.clone());
                changed = true;
            }
        }
    }

    // Drop dangling group members.
    let ids: Vec<String> = config.lights.iter().map(|l| l.id.clone()).collect();
    for group in &mut config.groups {
        let before = group.members.len();
        group.members.retain(|m| ids.contains(m));
        if group.members.len() != before {
            changed = true;
        }
    }

    if config.version != CONFIG_VERSION {
        config.version = CONFIG_VERSION;
        changed = true;
    }
    changed
}

/// v1 → v2: ids were the raw mDNS fullname (which is the device's display
/// name, so renaming a light produced a new identity). Re-key on serial.
fn migrate_v1_to_v2(config: &mut Config) -> bool {
    let mut id_map: HashMap<String, String> = HashMap::new();
    let mut changed = false;

    for light in &mut config.lights {
        let info = light
            .accessory_info
            .as_ref()
            .and_then(AccessoryInfo::from_value);
        let serial = info
            .as_ref()
            .map(|i| i.serial_number.trim().to_string())
            .filter(|s| !s.is_empty());

        // Old ids: "<Instance>._elg._tcp.local." (mDNS) or "manual-<serial>".
        let old_id = light.id.clone();
        let looks_mdns = old_id.contains("._elg._tcp");
        let mdns_fullname = if looks_mdns {
            Some(old_id.clone())
        } else {
            light.mdns_fullname.clone()
        };
        let ip = light.addresses.first().cloned();

        let new_id = make_id(serial.as_deref(), mdns_fullname.as_deref(), ip.as_deref());
        if new_id != old_id {
            id_map.insert(old_id, new_id.clone());
            light.id = new_id;
            changed = true;
        }
        if light.mdns_fullname.is_none() {
            light.mdns_fullname = mdns_fullname;
        }
        light.serial = serial;
        if let Some(info) = &info {
            if !info.product_name.trim().is_empty() {
                light.product = Some(info.product_name.clone());
            }
            let display = info.display_name.trim();
            if !display.is_empty() {
                light.name = display.to_string();
            } else if looks_mdns {
                light.name =
                    instance_name(&light.mdns_fullname.clone().unwrap_or_default()).to_string();
            }
        } else if looks_mdns {
            light.name = instance_name(&light.name).to_string();
        }
    }

    // Collapse duplicates that now share an id (e.g. a light added both by
    // mDNS and manually): keep the first, merge alias/enabled.
    let mut seen: Vec<String> = Vec::new();
    let mut deduped: Vec<LightRecord> = Vec::new();
    for light in config.lights.drain(..) {
        if seen.contains(&light.id) {
            if let Some(existing) = deduped.iter_mut().find(|l| l.id == light.id) {
                if existing.alias.is_none() {
                    existing.alias = light.alias.clone();
                }
                existing.enabled |= light.enabled;
                for addr in light.addresses {
                    if !existing.addresses.contains(&addr) {
                        existing.addresses.push(addr);
                    }
                }
            }
            changed = true;
            continue;
        }
        seen.push(light.id.clone());
        deduped.push(light);
    }
    config.lights = deduped;

    for group in &mut config.groups {
        for member in &mut group.members {
            if let Some(new) = id_map.get(member) {
                *member = new.clone();
                changed = true;
            }
        }
        group.members.dedup();
    }

    changed
}
