//! Thin blocking client for the keylightd localhost API.

use limelight_core::api::{
    encode_path_segment, Group, HealthResponse, LightRecord, LightStateResponse, Preset, Settings,
    UpdateRequest, UpdateResponse,
};
use limelight_core::elgato::DeviceSettings;
use serde::de::DeserializeOwned;
use std::fmt;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct ApiError(pub String);

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<ureq::Error> for ApiError {
    fn from(err: ureq::Error) -> Self {
        ApiError(err.to_string())
    }
}

#[derive(Clone)]
pub struct ApiClient {
    agent: ureq::Agent,
    base: String,
}

impl ApiClient {
    pub fn new() -> Self {
        Self::with_timeout(Duration::from_secs(20))
    }

    /// `total` must cover the daemon's own fan-out (a scan is up to 15 s).
    pub fn with_timeout(total: Duration) -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_millis(500)))
            .timeout_global(Some(total))
            .http_status_as_error(false)
            .user_agent("limelight-gui")
            .build();
        Self {
            agent: config.into(),
            base: limelight_core::daemon_base_url(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    fn decode<T: DeserializeOwned>(
        mut resp: ureq::http::Response<ureq::Body>,
    ) -> Result<T, ApiError> {
        let status = resp.status().as_u16();
        let text = resp
            .body_mut()
            .read_to_string()
            .map_err(|e| ApiError(e.to_string()))?;
        if !(200..300).contains(&status) {
            let msg = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
                .unwrap_or_else(|| format!("HTTP {status}"));
            return Err(ApiError(msg));
        }
        serde_json::from_str::<T>(&text).map_err(|e| ApiError(format!("bad response: {e}")))
    }

    fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, ApiError> {
        Self::decode(self.agent.get(&self.url(path)).call()?)
    }

    fn put<B: serde::Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, ApiError> {
        Self::decode(self.agent.put(&self.url(path)).send_json(body)?)
    }

    fn post<B: serde::Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, ApiError> {
        Self::decode(self.agent.post(&self.url(path)).send_json(body)?)
    }

    fn post_empty<T: DeserializeOwned>(&self, path: &str) -> Result<T, ApiError> {
        Self::decode(self.agent.post(&self.url(path)).send_empty()?)
    }

    fn delete<T: DeserializeOwned>(&self, path: &str) -> Result<T, ApiError> {
        Self::decode(self.agent.delete(&self.url(path)).call()?)
    }

    // ---- endpoints ----

    pub fn health(&self) -> Result<HealthResponse, ApiError> {
        self.get("/v1/health")
    }

    pub fn shutdown(&self) -> Result<(), ApiError> {
        self.post_empty::<serde_json::Value>("/v1/shutdown")
            .map(|_| ())
    }

    pub fn get_lights(&self) -> Result<Vec<LightRecord>, ApiError> {
        self.get("/v1/lights")
    }

    pub fn get_states(&self) -> Result<Vec<LightStateResponse>, ApiError> {
        self.get("/v1/lights/states")
    }

    pub fn get_groups(&self) -> Result<Vec<Group>, ApiError> {
        self.get("/v1/groups")
    }

    pub fn update_light(
        &self,
        id: &str,
        update: &UpdateRequest,
    ) -> Result<UpdateResponse, ApiError> {
        self.put(&format!("/v1/lights/{}", encode_path_segment(id)), update)
    }

    pub fn update_group(
        &self,
        name: &str,
        update: &UpdateRequest,
    ) -> Result<UpdateResponse, ApiError> {
        self.put(&format!("/v1/groups/{}", encode_path_segment(name)), update)
    }

    pub fn update_all(&self, update: &UpdateRequest) -> Result<UpdateResponse, ApiError> {
        self.put("/v1/all", update)
    }

    pub fn refresh(&self) -> Result<(), ApiError> {
        self.post::<_, serde_json::Value>(
            "/v1/lights/refresh",
            &serde_json::json!({ "timeout": 3 }),
        )
        .map(|_| ())
    }

    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<LightRecord, ApiError> {
        self.put(
            &format!("/v1/lights/{}/enabled", encode_path_segment(id)),
            &serde_json::json!({ "enabled": enabled }),
        )
    }

    pub fn set_alias(&self, id: &str, alias: &str) -> Result<LightRecord, ApiError> {
        let value = if alias.trim().is_empty() {
            serde_json::json!({ "alias": null })
        } else {
            serde_json::json!({ "alias": alias.trim() })
        };
        self.put(
            &format!("/v1/lights/{}/alias", encode_path_segment(id)),
            &value,
        )
    }

    pub fn identify(&self, id: &str) -> Result<(), ApiError> {
        self.post_empty::<serde_json::Value>(&format!(
            "/v1/lights/{}/identify",
            encode_path_segment(id)
        ))
        .map(|_| ())
    }

    pub fn delete_light(&self, id: &str) -> Result<(), ApiError> {
        self.delete::<serde_json::Value>(&format!("/v1/lights/{}", encode_path_segment(id)))
            .map(|_| ())
    }

    pub fn create_group(&self, name: &str, members: &[String]) -> Result<Group, ApiError> {
        self.post(
            "/v1/groups",
            &serde_json::json!({ "name": name, "members": members }),
        )
    }

    pub fn delete_group(&self, name: &str) -> Result<(), ApiError> {
        self.delete::<serde_json::Value>(&format!("/v1/groups/{}", encode_path_segment(name)))
            .map(|_| ())
    }

    pub fn get_record(&self, id: &str) -> Result<LightRecord, ApiError> {
        self.get(&format!("/v1/lights/{}", encode_path_segment(id)))
    }

    pub fn get_device_settings(&self, id: &str) -> Result<DeviceSettings, ApiError> {
        self.get(&format!("/v1/lights/{}/settings", encode_path_segment(id)))
    }

    pub fn set_device_settings(
        &self,
        id: &str,
        settings: &DeviceSettings,
    ) -> Result<DeviceSettings, ApiError> {
        self.put(
            &format!("/v1/lights/{}/settings", encode_path_segment(id)),
            settings,
        )
    }

    /// Renames the device itself (what Control Center does).
    pub fn rename_device(&self, id: &str, name: &str) -> Result<LightRecord, ApiError> {
        self.put(
            &format!("/v1/lights/{}/name", encode_path_segment(id)),
            &serde_json::json!({ "name": name }),
        )
    }

    pub fn get_presets(&self) -> Result<Vec<Preset>, ApiError> {
        self.get("/v1/presets")
    }

    /// Insert or replace one preset (matched by name).
    pub fn save_preset(&self, preset: &Preset) -> Result<Preset, ApiError> {
        self.post("/v1/presets", preset)
    }

    /// Replace the whole ordered list (rename / reorder).
    pub fn set_presets(&self, presets: &[Preset]) -> Result<Vec<Preset>, ApiError> {
        self.put("/v1/presets", &presets)
    }

    pub fn delete_preset(&self, name: &str) -> Result<(), ApiError> {
        self.delete::<serde_json::Value>(&format!("/v1/presets/{}", encode_path_segment(name)))
            .map(|_| ())
    }

    pub fn get_settings(&self) -> Result<Settings, ApiError> {
        self.get("/v1/settings")
    }

    pub fn set_settings(&self, settings: &Settings) -> Result<Settings, ApiError> {
        self.put("/v1/settings", settings)
    }
}
