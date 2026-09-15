//! In-memory state of the daemon: the config (persisted atomically on every
//! change) plus a last-known-state cache per light. All light I/O fans out
//! over scoped threads so every target is contacted at the same time.

use limelight_core::api::{LightStateResponse, TargetResult, UpdateRequest, UpdateResponse};
use limelight_core::config::{self, Config, LightRecord, Settings};
use limelight_core::convert::{clamp_mired, kelvin_to_mired, mired_to_kelvin};
use limelight_core::elgato::{
    AccessoryInfo, DeviceClient, DeviceError, DeviceSettings, LightState, LightUpdate,
};
use limelight_core::now_unix;
use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Instant, SystemTime};

/// How long a discovered light's record stays "fresh" before an mDNS
/// re-announcement triggers another accessory-info fetch and config write.
const REANNOUNCE_REFRESH_SECS: u64 = 300;

#[derive(Clone, Debug)]
pub struct CachedState {
    pub state: LightState,
    pub reachable: bool,
    pub updated: Instant,
}

/// Which lights an update applies to.
pub enum Target {
    Light(String),
    Group(String),
    All,
}

pub struct AppState {
    config: Mutex<Config>,
    /// mtime of the file when we last read/wrote it; used to notice edits by
    /// the CLI while the daemon is running.
    mtime: Mutex<Option<SystemTime>>,
    cache: Mutex<HashMap<String, CachedState>>,
    pub client: DeviceClient,
    pub shutdown: AtomicBool,
}

impl AppState {
    /// Load config from disk (migrating / recovering as needed).
    pub fn load() -> io::Result<Arc<Self>> {
        let loaded = config::load()?;
        if loaded.dirty {
            config::save(&loaded.config)?;
        }
        Ok(Arc::new(Self {
            config: Mutex::new(loaded.config),
            mtime: Mutex::new(config::config_mtime()),
            cache: Mutex::new(HashMap::new()),
            client: DeviceClient::new(),
            shutdown: AtomicBool::new(false),
        }))
    }

    fn lock_config(&self) -> MutexGuard<'_, Config> {
        let mut guard = self.config.lock().unwrap_or_else(|p| p.into_inner());
        self.reload_if_changed(&mut guard);
        guard
    }

    /// If another process rewrote the file since we last touched it, pick up
    /// its content. Cheap: one `stat` per call.
    fn reload_if_changed(&self, cfg: &mut Config) {
        let on_disk = config::config_mtime();
        let mut known = self.mtime.lock().unwrap_or_else(|p| p.into_inner());
        if on_disk.is_some() && on_disk != *known {
            if let Ok(loaded) = config::load() {
                *cfg = loaded.config;
                if loaded.dirty {
                    let _ = config::save(cfg);
                }
                *known = config::config_mtime();
            }
        }
    }

    /// Read-only access to the config.
    pub fn read<R>(&self, f: impl FnOnce(&Config) -> R) -> R {
        let guard = self.lock_config();
        f(&guard)
    }

    /// Mutate the config and persist it atomically.
    pub fn write<R>(&self, f: impl FnOnce(&mut Config) -> R) -> R {
        let mut guard = self.lock_config();
        let result = f(&mut guard);
        match config::save(&guard) {
            Ok(()) => {
                let mut known = self.mtime.lock().unwrap_or_else(|p| p.into_inner());
                *known = config::config_mtime();
            }
            Err(err) => eprintln!("[keylightd] failed to save config: {err}"),
        }
        result
    }

    pub fn settings(&self) -> Settings {
        self.read(|c| c.settings.clone())
    }

    // ------------------------------------------------------------------
    // Cache
    // ------------------------------------------------------------------

    fn cache(&self) -> MutexGuard<'_, HashMap<String, CachedState>> {
        self.cache.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn cache_ok(&self, id: &str, state: LightState) {
        self.cache().insert(
            id.to_string(),
            CachedState {
                state,
                reachable: true,
                updated: Instant::now(),
            },
        );
    }

    pub fn cache_fail(&self, id: &str) {
        let mut cache = self.cache();
        match cache.get_mut(id) {
            Some(entry) => {
                entry.reachable = false;
                entry.updated = Instant::now();
            }
            None => {
                cache.insert(
                    id.to_string(),
                    CachedState {
                        state: LightState::default(),
                        reachable: false,
                        updated: Instant::now(),
                    },
                );
            }
        }
    }

    pub fn cache_touch_reachable(&self, id: &str) {
        if let Some(entry) = self.cache().get_mut(id) {
            entry.reachable = true;
            entry.updated = Instant::now();
        }
    }

    pub fn cached(&self, id: &str) -> Option<CachedState> {
        self.cache().get(id).cloned()
    }

    pub fn reachable_count(&self) -> usize {
        self.cache().values().filter(|c| c.reachable).count()
    }

    /// mDNS told us a service went away.
    pub fn mark_unreachable_by_mdns(&self, fullname: &str) {
        let id = self.read(|c| {
            c.lights
                .iter()
                .find(|l| l.mdns_fullname.as_deref() == Some(fullname))
                .map(|l| l.id.clone())
        });
        if let Some(id) = id {
            self.cache_fail(&id);
        }
    }

    // ------------------------------------------------------------------
    // Discovery / add
    // ------------------------------------------------------------------

    /// Merge an mDNS resolution into the config. Returns the record, or `None`
    /// if nothing changed (fresh re-announcement of a known light).
    pub fn upsert_discovered(
        &self,
        fullname: &str,
        hostname: &str,
        port: u16,
        addresses: Vec<String>,
    ) -> Option<LightRecord> {
        let primary = config::select_address(&addresses).map(str::to_string);

        // Fast path: same light, same addresses, seen recently → just refresh liveness.
        let fresh = self.read(|c| {
            c.lights
                .iter()
                .find(|l| l.mdns_fullname.as_deref() == Some(fullname))
                .filter(|l| l.addresses == addresses)
                .filter(|l| now_unix().saturating_sub(l.last_seen_unix) < REANNOUNCE_REFRESH_SECS)
                .map(|l| l.id.clone())
        });
        if let Some(id) = fresh {
            self.cache_touch_reachable(&id);
            return None;
        }

        let fetched = primary
            .as_deref()
            .and_then(|ip| self.client.accessory_info(ip).ok());
        let info = fetched.as_ref().and_then(AccessoryInfo::from_value);
        let serial = info
            .as_ref()
            .map(|i| i.serial_number.trim().to_string())
            .filter(|s| !s.is_empty());

        let record = self.write(|c| {
            // Identity resolution order: serial, then current mDNS name.
            let existing = serial
                .as_deref()
                .and_then(|s| c.lights.iter().find(|l| l.serial.as_deref() == Some(s)))
                .or_else(|| {
                    c.lights
                        .iter()
                        .find(|l| l.mdns_fullname.as_deref() == Some(fullname))
                })
                .cloned();

            let old_id = existing.as_ref().map(|e| e.id.clone());
            let new_id = config::make_id(serial.as_deref(), Some(fullname), primary.as_deref());

            let display = info
                .as_ref()
                .map(|i| i.display_name.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| config::instance_name(fullname).to_string());

            let record = LightRecord {
                id: new_id.clone(),
                alias: existing.as_ref().and_then(|e| e.alias.clone()),
                name: display,
                hostname: hostname.to_string(),
                port,
                addresses,
                last_seen_unix: now_unix(),
                enabled: existing
                    .as_ref()
                    .map(|e| e.enabled)
                    .unwrap_or(c.settings.auto_enable_discovered),
                accessory_info: fetched
                    .clone()
                    .or_else(|| existing.as_ref().and_then(|e| e.accessory_info.clone())),
                mdns_fullname: Some(fullname.to_string()),
                serial: serial
                    .clone()
                    .or_else(|| existing.as_ref().and_then(|e| e.serial.clone())),
                product: info
                    .as_ref()
                    .map(|i| i.product_name.clone())
                    .filter(|p| !p.trim().is_empty())
                    .or_else(|| existing.as_ref().and_then(|e| e.product.clone())),
            };

            if let Some(old) = old_id.filter(|o| *o != new_id) {
                // Identity upgraded (e.g. serial now known): re-key groups.
                c.lights.retain(|l| l.id != old);
                for g in &mut c.groups {
                    for m in &mut g.members {
                        if *m == old {
                            *m = new_id.clone();
                        }
                    }
                    g.members.dedup();
                }
            }
            c.upsert_light(record.clone());
            record
        });

        if fetched.is_some() {
            self.cache_touch_reachable(&record.id);
        }
        Some(record)
    }

    /// Manual add by IP (fallback when mDNS is blocked).
    pub fn add_by_ip(&self, ip: String) -> Result<LightRecord, String> {
        let value = self
            .client
            .accessory_info(&ip)
            .map_err(|e| format!("Unable to fetch accessory-info from {ip}: {e}"))?;
        let info = AccessoryInfo::from_value(&value).unwrap_or_default();
        let serial = Some(info.serial_number.trim().to_string()).filter(|s| !s.is_empty());
        let name = if info.display_name.trim().is_empty() {
            if info.product_name.trim().is_empty() {
                "Elgato Light".to_string()
            } else {
                info.product_name.clone()
            }
        } else {
            info.display_name.clone()
        };

        let record = self.write(|c| {
            let existing = serial
                .as_deref()
                .and_then(|s| c.lights.iter().find(|l| l.serial.as_deref() == Some(s)))
                .cloned();
            let id = config::make_id(serial.as_deref(), None, Some(&ip));
            let mut addresses = existing
                .as_ref()
                .map(|e| e.addresses.clone())
                .unwrap_or_default();
            if !addresses.contains(&ip) {
                addresses.insert(0, ip.clone());
            }
            let record = LightRecord {
                id,
                alias: existing.as_ref().and_then(|e| e.alias.clone()),
                name,
                hostname: existing
                    .as_ref()
                    .map(|e| e.hostname.clone())
                    .filter(|h| !h.is_empty())
                    .unwrap_or_else(|| ip.clone()),
                port: limelight_core::elgato::DEVICE_PORT,
                addresses,
                last_seen_unix: now_unix(),
                enabled: true,
                accessory_info: Some(value.clone()),
                mdns_fullname: existing.as_ref().and_then(|e| e.mdns_fullname.clone()),
                serial: serial.clone(),
                product: Some(info.product_name.clone()).filter(|p| !p.trim().is_empty()),
            };
            c.upsert_light(record.clone());
            record
        });
        self.cache_touch_reachable(&record.id);
        Ok(record)
    }

    // ------------------------------------------------------------------
    // State + updates (parallel fan-out)
    // ------------------------------------------------------------------

    fn to_response(
        &self,
        record: &LightRecord,
        state: Option<&LightState>,
        reachable: bool,
    ) -> LightStateResponse {
        let state = state.cloned().unwrap_or_default();
        LightStateResponse {
            id: record.id.clone(),
            name: record.name.clone(),
            alias: record.alias.clone(),
            enabled: record.enabled,
            reachable,
            on: state.on == 1,
            brightness: state.brightness.min(100),
            kelvin: state
                .temperature
                .map(mired_to_kelvin)
                .unwrap_or(limelight_core::convert::KELVIN_MAX.min(5600)),
            hue: state.hue,
            saturation: state.saturation,
            color_capable: record.is_color_capable() || state.is_color_mode(),
            product: record.product.clone(),
        }
    }

    /// Current state of every enabled light, fetched in parallel.
    /// Unreachable lights are included with their last known values.
    pub fn states(&self) -> Vec<LightStateResponse> {
        let lights: Vec<LightRecord> =
            self.read(|c| c.lights.iter().filter(|l| l.enabled).cloned().collect());
        let results = fan_out(&lights, |record| {
            let ip = record
                .primary_address()
                .ok_or_else(|| DeviceError::Unreachable("no address".into()))?;
            self.client.get_state(ip)
        });
        lights
            .iter()
            .zip(results)
            .map(|(record, result)| match result {
                Ok(state) => {
                    self.cache_ok(&record.id, state.clone());
                    self.to_response(record, Some(&state), true)
                }
                Err(_) => {
                    self.cache_fail(&record.id);
                    let cached = self.cached(&record.id);
                    self.to_response(record, cached.as_ref().map(|c| &c.state), false)
                }
            })
            .collect()
    }

    /// Resolve an update target to concrete light records (enabled only).
    pub fn resolve_targets(&self, target: &Target) -> Result<Vec<LightRecord>, String> {
        self.read(|c| match target {
            Target::Light(ident) => {
                let record = c
                    .find_light(ident)
                    .ok_or_else(|| format!("No light found with id '{ident}'"))?;
                if !record.enabled {
                    return Err(format!("Light '{}' is disabled", record.display_name()));
                }
                Ok(vec![record.clone()])
            }
            Target::Group(name) => {
                let group = c
                    .find_group(name)
                    .ok_or_else(|| format!("No group named '{name}'"))?;
                let members: Vec<LightRecord> = group
                    .members
                    .iter()
                    .filter_map(|m| c.lights.iter().find(|l| l.id == *m && l.enabled))
                    .cloned()
                    .collect();
                if members.is_empty() {
                    return Err(format!("Group '{name}' has no enabled lights"));
                }
                Ok(members)
            }
            Target::All => {
                let all: Vec<LightRecord> =
                    c.lights.iter().filter(|l| l.enabled).cloned().collect();
                if all.is_empty() {
                    return Err("No enabled lights. Run a scan first.".into());
                }
                Ok(all)
            }
        })
    }

    /// Convert an API update into the device payload.
    pub fn to_device_update(update: &UpdateRequest) -> LightUpdate {
        LightUpdate {
            on: update.on.map(|v| v.min(1)),
            brightness: update.brightness.map(|v| v.min(100)),
            temperature: update
                .mired
                .map(clamp_mired)
                .or_else(|| update.kelvin.map(kelvin_to_mired)),
            hue: update.hue.map(|h| h.clamp(0.0, 360.0)),
            saturation: update.saturation.map(|s| s.clamp(0.0, 100.0)),
        }
    }

    /// Send one update to many lights at the same time.
    pub fn apply(&self, targets: &[LightRecord], update: &LightUpdate) -> UpdateResponse {
        let started = Instant::now();
        let results = fan_out(targets, |record| {
            let result = record
                .primary_address()
                .ok_or_else(|| DeviceError::Unreachable("no address".into()))
                .and_then(|ip| self.client.set_state(ip, update));
            (result, started.elapsed().as_millis() as u64)
        });
        let mut response = UpdateResponse::default();
        for (record, (result, elapsed_ms)) in targets.iter().zip(results) {
            match result {
                Ok(state) => {
                    self.cache_ok(&record.id, state.clone());
                    response.ok = true;
                    response.results.push(TargetResult {
                        id: record.id.clone(),
                        ok: true,
                        error: None,
                        state: Some(self.to_response(record, Some(&state), true)),
                        elapsed_ms,
                    });
                }
                Err(err) => {
                    self.cache_fail(&record.id);
                    response.results.push(TargetResult {
                        id: record.id.clone(),
                        ok: false,
                        error: Some(err.to_string()),
                        state: None,
                        elapsed_ms,
                    });
                }
            }
        }
        response
    }

    /// Rename the device itself (Control Center semantics) and record it.
    pub fn rename_device(&self, ident: &str, name: &str) -> Result<LightRecord, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("Name must not be empty".into());
        }
        let record = self
            .read(|c| c.find_light(ident).cloned())
            .ok_or_else(|| format!("No light found with id '{ident}'"))?;
        let ip = record
            .primary_address()
            .ok_or_else(|| "Light has no known address".to_string())?;
        self.client
            .set_display_name(ip, name)
            .map_err(|e| format!("Light did not accept the new name: {e}"))?;
        // The mDNS instance name will change; the persistent browser re-links it by serial.
        Ok(self.write(|c| {
            if let Some(l) = c.find_light_mut(&record.id) {
                l.name = name.to_string();
                if let Some(info) = l.accessory_info.as_mut() {
                    if let Some(obj) = info.as_object_mut() {
                        obj.insert(
                            "displayName".into(),
                            serde_json::Value::String(name.to_string()),
                        );
                    }
                }
                l.clone()
            } else {
                record.clone()
            }
        }))
    }

    /// Full persisted record of one light.
    pub fn record(&self, ident: &str) -> Option<LightRecord> {
        self.read(|c| c.find_light(ident).cloned())
    }

    fn address_of(&self, ident: &str) -> Result<(LightRecord, String), String> {
        let record = self
            .record(ident)
            .ok_or_else(|| format!("No light found with id '{ident}'"))?;
        let ip = record
            .primary_address()
            .ok_or_else(|| "Light has no known address".to_string())?
            .to_string();
        Ok((record, ip))
    }

    /// Device-side settings (power-on behaviour, fade durations).
    pub fn device_settings(&self, ident: &str) -> Result<DeviceSettings, String> {
        let (_, ip) = self.address_of(ident)?;
        self.client.light_settings(&ip).map_err(|e| e.to_string())
    }

    /// Apply a partial settings update on the device; returns the new settings.
    /// The lights reject partial bodies, so the current settings are fetched,
    /// overlaid with the requested fields, and written back whole.
    pub fn set_device_settings(
        &self,
        ident: &str,
        settings: &DeviceSettings,
    ) -> Result<DeviceSettings, String> {
        let (_, ip) = self.address_of(ident)?;
        let mut merged = self
            .client
            .light_settings(&ip)
            .map_err(|e| format!("could not read current settings: {e}"))?;
        if settings.power_on_behavior.is_some() {
            merged.power_on_behavior = settings.power_on_behavior;
        }
        if settings.power_on_brightness.is_some() {
            merged.power_on_brightness = settings.power_on_brightness;
        }
        if settings.power_on_temperature.is_some() {
            merged.power_on_temperature = settings.power_on_temperature;
        }
        if settings.power_on_hue.is_some() {
            merged.power_on_hue = settings.power_on_hue;
        }
        if settings.power_on_saturation.is_some() {
            merged.power_on_saturation = settings.power_on_saturation;
        }
        if settings.switch_on_duration_ms.is_some() {
            merged.switch_on_duration_ms = settings.switch_on_duration_ms;
        }
        if settings.switch_off_duration_ms.is_some() {
            merged.switch_off_duration_ms = settings.switch_off_duration_ms;
        }
        if settings.color_change_duration_ms.is_some() {
            merged.color_change_duration_ms = settings.color_change_duration_ms;
        }
        // Battery settings (Key Light Mini) are only written when explicitly given.
        merged.battery = settings.battery.clone();
        self.client
            .set_light_settings(&ip, &merged)
            .map_err(|e| e.to_string())
    }

    pub fn identify(&self, ident: &str) -> Result<(), String> {
        let record = self
            .read(|c| c.find_light(ident).cloned())
            .ok_or_else(|| format!("No light found with id '{ident}'"))?;
        let ip = record
            .primary_address()
            .ok_or_else(|| "Light has no known address".to_string())?;
        self.client.identify(ip).map_err(|e| e.to_string())
    }

    pub fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }

    pub fn shutdown_requested(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }
}

/// Run `f` for every item on its own scoped thread and collect results in order.
/// Zero allocations beyond the result vector; threads end before returning.
pub fn fan_out<T, R, F>(items: &[T], f: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(&T) -> R + Sync,
{
    if items.len() <= 1 {
        return items.iter().map(&f).collect();
    }
    std::thread::scope(|scope| {
        let handles: Vec<_> = items.iter().map(|item| scope.spawn(|| f(item))).collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("fan-out worker panicked"))
            .collect()
    })
}
