//! Request / response types of the `keylightd` localhost HTTP API.
//! Shared by the daemon (producer) and the GUI (consumer). See `docs/API.md`.

use serde::{Deserialize, Serialize};

pub use crate::config::{Group, LightRecord, Preset, Settings};

/// Fields other than `status` default so a pre-0.2 daemon (which only sent
/// `{"status":"ok"}`) is recognised as "running but too old" rather than absent.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct HealthResponse {
    pub status: String,
    #[serde(default)]
    pub version: String,
    /// Number of lights currently believed reachable.
    #[serde(default)]
    pub reachable: usize,
    /// Number of enabled lights.
    #[serde(default)]
    pub enabled: usize,
}

/// Current state of one enabled light. Always returned for every enabled
/// light; when `reachable` is false the value fields are the last known ones
/// (or defaults if the light was never seen).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LightStateResponse {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub alias: Option<String>,
    pub enabled: bool,
    pub reachable: bool,
    pub on: bool,
    pub brightness: u8,
    pub kelvin: u16,
    #[serde(default)]
    pub hue: Option<f32>,
    #[serde(default)]
    pub saturation: Option<f32>,
    #[serde(default)]
    pub color_capable: bool,
    #[serde(default)]
    pub product: Option<String>,
}

impl LightStateResponse {
    pub fn display_name(&self) -> &str {
        self.alias
            .as_deref()
            .filter(|a| !a.trim().is_empty())
            .unwrap_or(&self.name)
    }
}

/// Body of `PUT /v1/lights/{id}`, `PUT /v1/groups/{name}`, `PUT /v1/all`.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct UpdateRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brightness: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kelvin: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mired: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hue: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saturation: Option<f32>,
}

impl From<&Preset> for UpdateRequest {
    fn from(p: &Preset) -> Self {
        UpdateRequest {
            on: Some(u8::from(p.on)),
            brightness: Some(p.brightness),
            kelvin: if p.hue.is_some() {
                None
            } else {
                Some(p.kelvin)
            },
            mired: None,
            hue: p.hue,
            saturation: p.saturation,
        }
    }
}

impl UpdateRequest {
    pub fn is_empty(&self) -> bool {
        self.on.is_none()
            && self.brightness.is_none()
            && self.kelvin.is_none()
            && self.mired.is_none()
            && self.hue.is_none()
            && self.saturation.is_none()
    }
}

/// Result for one light inside an update fan-out.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TargetResult {
    pub id: String,
    pub ok: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub state: Option<LightStateResponse>,
    /// Milliseconds from fan-out start until this light acknowledged.
    #[serde(default)]
    pub elapsed_ms: u64,
}

/// Response of every update endpoint. HTTP 200 when at least one target
/// succeeded, 502 when every target failed, 404 when there were no targets.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct UpdateResponse {
    pub ok: bool,
    pub results: Vec<TargetResult>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RefreshRequest {
    #[serde(default)]
    pub timeout: Option<u64>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RefreshResponse {
    pub refreshed: bool,
    pub found: usize,
    pub lights: Vec<LightRecord>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AddLightRequest {
    pub ip: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GroupRequest {
    pub name: String,
    pub members: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct EnabledRequest {
    pub enabled: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AliasRequest {
    #[serde(default)]
    pub alias: Option<String>,
}

/// `PUT /v1/lights/{id}/name` — renames the device itself.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NameRequest {
    pub name: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ErrorResponse {
    pub error: String,
}

/// Percent-encode a single path segment (ids contain spaces, dots, colons).
pub fn encode_path_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Decode a percent-encoded path segment. Invalid sequences are kept verbatim.
pub fn decode_path_segment(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = &segment[i + 1..i + 3];
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
