//! Blocking client for the Elgato local HTTP API (`http://<light>:9123/elgato/...`).
//!
//! All Elgato lights (Key Light, Key Light Air, Key Light Mini, Ring Light,
//! Light Strip, Light Strip Pro) share these endpoints. Colour-capable lights
//! report `hue`/`saturation` instead of `temperature` while in colour mode.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::net::IpAddr;
use std::time::Duration;

/// Port every Elgato light listens on.
pub const DEVICE_PORT: u16 = 9123;

/// Maximum bytes accepted from a device response (they are all tiny).
const MAX_RESPONSE_BYTES: u64 = 64 * 1024;

#[derive(Debug)]
pub enum DeviceError {
    /// TCP connect / timeout / reset. The light is not reachable right now.
    Unreachable(String),
    /// The light answered with a non-2xx status.
    Status(u16),
    /// The light answered but the body was not what we expected.
    Decode(String),
}

impl fmt::Display for DeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeviceError::Unreachable(msg) => write!(f, "unreachable: {msg}"),
            DeviceError::Status(code) => write!(f, "device returned HTTP {code}"),
            DeviceError::Decode(msg) => write!(f, "bad device response: {msg}"),
        }
    }
}

impl std::error::Error for DeviceError {}

impl From<ureq::Error> for DeviceError {
    fn from(err: ureq::Error) -> Self {
        match err {
            ureq::Error::StatusCode(code) => DeviceError::Status(code),
            ureq::Error::Io(e) => DeviceError::Unreachable(e.to_string()),
            ureq::Error::Timeout(t) => DeviceError::Unreachable(format!("timeout ({t})")),
            ureq::Error::ConnectionFailed => DeviceError::Unreachable("connection failed".into()),
            ureq::Error::HostNotFound => DeviceError::Unreachable("host not found".into()),
            other => DeviceError::Decode(other.to_string()),
        }
    }
}

/// Envelope used by `GET/PUT /elgato/lights`.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LightsPayload<T> {
    pub number_of_lights: u8,
    pub lights: Vec<T>,
}

/// State as reported by the device. `temperature` is in mired.
/// Colour lights (Light Strip) report `hue` (0..360) and `saturation` (0..100)
/// instead of `temperature` when in colour mode.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct LightState {
    #[serde(default)]
    pub on: u8,
    #[serde(default)]
    pub brightness: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hue: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saturation: Option<f32>,
}

impl LightState {
    /// True when the device is currently in colour (hue/saturation) mode.
    pub fn is_color_mode(&self) -> bool {
        self.hue.is_some() && self.saturation.is_some()
    }
}

/// Partial update sent to the device. Only `Some` fields are transmitted.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct LightUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brightness: Option<u8>,
    /// Mired.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hue: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saturation: Option<f32>,
}

impl LightUpdate {
    pub fn is_empty(&self) -> bool {
        self.on.is_none()
            && self.brightness.is_none()
            && self.temperature.is_none()
            && self.hue.is_none()
            && self.saturation.is_none()
    }
}

/// `/elgato/lights/settings`. Every field is optional so the same type serves
/// GET (full) and PUT (partial). Temperature is in mired.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DeviceSettings {
    /// 1 = restore last state on power-up, 2 = use the defaults below.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power_on_behavior: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power_on_brightness: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power_on_temperature: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power_on_hue: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power_on_saturation: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub switch_on_duration_ms: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub switch_off_duration_ms: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_change_duration_ms: Option<u32>,
    /// Key Light Mini only (energy saving / bypass); passed through untouched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub battery: Option<Value>,
}

/// Typed view of the interesting parts of `/elgato/accessory-info`.
#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct AccessoryInfo {
    #[serde(default)]
    pub product_name: String,
    #[serde(default)]
    pub serial_number: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub firmware_version: String,
    #[serde(default)]
    pub firmware_build_number: u32,
    #[serde(default)]
    pub hardware_board_type: u32,
    #[serde(default)]
    pub features: Vec<String>,
}

impl AccessoryInfo {
    pub fn from_value(value: &Value) -> Option<Self> {
        serde_json::from_value(value.clone()).ok()
    }
}

/// Build `http://<ip>:9123/elgato` for an IPv4 or IPv6 literal.
/// IPv6 gets bracketed and any `%zone` suffix is dropped (not valid in URLs).
pub fn base_url(ip: &str) -> String {
    let ip = ip.trim();
    let bare = ip.split('%').next().unwrap_or(ip);
    match bare.parse::<IpAddr>() {
        Ok(IpAddr::V6(v6)) => format!("http://[{v6}]:{DEVICE_PORT}/elgato"),
        _ => format!("http://{bare}:{DEVICE_PORT}/elgato"),
    }
}

/// Thread-safe, cheap-to-clone HTTP client for talking to lights on the LAN.
#[derive(Clone)]
pub struct DeviceClient {
    agent: ureq::Agent,
}

impl Default for DeviceClient {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceClient {
    /// Tight timeouts: a LAN light that has not accepted TCP in 750 ms is not there.
    pub fn new() -> Self {
        Self::with_timeouts(Duration::from_millis(750), Duration::from_millis(2500))
    }

    pub fn with_timeouts(connect: Duration, total: Duration) -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_connect(Some(connect))
            .timeout_global(Some(total))
            .http_status_as_error(true)
            .max_response_header_size(16 * 1024)
            .user_agent("limelight")
            .build();
        Self {
            agent: config.into(),
        }
    }

    fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T, DeviceError> {
        let mut resp = self.agent.get(url).call()?;
        resp.body_mut()
            .with_config()
            .limit(MAX_RESPONSE_BYTES)
            .read_json::<T>()
            .map_err(|e| DeviceError::Decode(e.to_string()))
    }

    fn put_json<B: Serialize, T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        body: &B,
    ) -> Result<T, DeviceError> {
        let mut resp = self.agent.put(url).send_json(body)?;
        resp.body_mut()
            .with_config()
            .limit(MAX_RESPONSE_BYTES)
            .read_json::<T>()
            .map_err(|e| DeviceError::Decode(e.to_string()))
    }

    /// `GET /elgato/lights` — the first (and in practice only) light entry.
    pub fn get_state(&self, ip: &str) -> Result<LightState, DeviceError> {
        let payload: LightsPayload<LightState> =
            self.get_json(&format!("{}/lights", base_url(ip)))?;
        payload
            .lights
            .into_iter()
            .next()
            .ok_or_else(|| DeviceError::Decode("empty lights array".into()))
    }

    /// `PUT /elgato/lights` — apply a partial update, returns the resulting state.
    pub fn set_state(&self, ip: &str, update: &LightUpdate) -> Result<LightState, DeviceError> {
        let body = LightsPayload {
            number_of_lights: 1,
            lights: vec![update.clone()],
        };
        let payload: LightsPayload<LightState> =
            self.put_json(&format!("{}/lights", base_url(ip)), &body)?;
        payload
            .lights
            .into_iter()
            .next()
            .ok_or_else(|| DeviceError::Decode("empty lights array".into()))
    }

    /// `GET /elgato/accessory-info` as raw JSON (kept verbatim in the config).
    pub fn accessory_info(&self, ip: &str) -> Result<Value, DeviceError> {
        self.get_json(&format!("{}/accessory-info", base_url(ip)))
    }

    /// `PUT /elgato/accessory-info` — rename the device itself (what Control Center does).
    pub fn set_display_name(&self, ip: &str, name: &str) -> Result<(), DeviceError> {
        let body = serde_json::json!({ "displayName": name });
        self.agent
            .put(&format!("{}/accessory-info", base_url(ip)))
            .send_json(&body)?;
        Ok(())
    }

    /// `POST /elgato/identify` — make the light blink so the user can find it.
    pub fn identify(&self, ip: &str) -> Result<(), DeviceError> {
        self.agent
            .post(&format!("{}/identify", base_url(ip)))
            .send_empty()?;
        Ok(())
    }

    /// `GET /elgato/battery-info` (Key Light Mini only; others return 404).
    pub fn battery_info(&self, ip: &str) -> Result<Value, DeviceError> {
        self.get_json(&format!("{}/battery-info", base_url(ip)))
    }

    /// `GET /elgato/lights/settings` (power-on behaviour, transition durations, ...).
    pub fn light_settings(&self, ip: &str) -> Result<DeviceSettings, DeviceError> {
        self.get_json(&format!("{}/lights/settings", base_url(ip)))
    }

    /// `PUT /elgato/lights/settings` with a partial update; returns the
    /// settings as the device reports them afterwards.
    pub fn set_light_settings(
        &self,
        ip: &str,
        settings: &DeviceSettings,
    ) -> Result<DeviceSettings, DeviceError> {
        self.agent
            .put(&format!("{}/lights/settings", base_url(ip)))
            .send_json(settings)?;
        self.light_settings(ip)
    }
}
