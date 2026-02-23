use serde::{Deserialize, Serialize};

const BASE_URL: &str = "http://127.0.0.1:9124";

#[derive(Deserialize, Debug, Clone)]
pub struct LightRecord {
    pub id: String,
    pub alias: Option<String>,
    pub name: String,
    pub enabled: bool,
}

#[derive(Deserialize, Debug, Clone)]
pub struct LightStateResponse {
    pub id: String,
    pub on: bool,
    pub brightness: u8,
    pub kelvin: u16,
}

#[derive(Deserialize, Debug, Clone)]
pub struct GroupRecord {
    pub name: String,
    pub members: Vec<String>,
}

#[derive(Serialize, Debug, Clone)]
pub struct UpdatePayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brightness: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kelvin: Option<u16>,
}

pub const KELVIN_MIN: f32 = 2900.0;
pub const KELVIN_MAX: f32 = 7000.0;
pub const KELVIN_RANGE: f32 = KELVIN_MAX - KELVIN_MIN;

/// warmth 0.0 (cool/left) → 7000K, warmth 1.0 (warm/right) → 2900K
pub fn warmth_to_kelvin(warmth: f32) -> u16 {
    (KELVIN_MAX - warmth * KELVIN_RANGE)
        .round()
        .clamp(KELVIN_MIN, KELVIN_MAX) as u16
}

pub fn kelvin_to_warmth(kelvin: u16) -> f32 {
    ((KELVIN_MAX - kelvin as f32) / KELVIN_RANGE).clamp(0.0, 1.0)
}

pub fn brightness_to_api(slider: f32) -> u8 {
    (slider * 100.0).round().clamp(0.0, 100.0) as u8
}

pub fn api_to_brightness(api: u8) -> f32 {
    api as f32 / 100.0
}

#[derive(Clone)]
pub struct ApiClient {
    client: reqwest::blocking::Client,
}

impl ApiClient {
    pub fn new() -> Self {
        Self {
            client: reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(3))
                .build()
                .expect("failed to build reqwest client"),
        }
    }

    pub fn get_lights(&self) -> Result<Vec<LightRecord>, reqwest::Error> {
        self.client
            .get(format!("{BASE_URL}/v1/lights"))
            .send()?
            .error_for_status()?
            .json()
    }

    pub fn get_light_states(&self) -> Result<Vec<LightStateResponse>, reqwest::Error> {
        self.client
            .get(format!("{BASE_URL}/v1/lights/states"))
            .send()?
            .error_for_status()?
            .json()
    }

    pub fn update_light(&self, id: &str, payload: &UpdatePayload) -> Result<(), reqwest::Error> {
        self.client
            .put(format!("{BASE_URL}/v1/lights/{id}"))
            .json(payload)
            .send()?
            .error_for_status()?;
        Ok(())
    }

    pub fn update_all(&self, payload: &UpdatePayload) -> Result<(), reqwest::Error> {
        self.client
            .put(format!("{BASE_URL}/v1/all"))
            .json(payload)
            .send()?
            .error_for_status()?;
        Ok(())
    }

    pub fn update_group(&self, name: &str, payload: &UpdatePayload) -> Result<(), reqwest::Error> {
        self.client
            .put(format!("{BASE_URL}/v1/groups/{name}"))
            .json(payload)
            .send()?
            .error_for_status()?;
        Ok(())
    }

    pub fn get_groups(&self) -> Result<Vec<GroupRecord>, reqwest::Error> {
        self.client
            .get(format!("{BASE_URL}/v1/groups"))
            .send()?
            .error_for_status()?
            .json()
    }

    pub fn refresh_lights(&self) -> Result<(), reqwest::Error> {
        self.client
            .post(format!("{BASE_URL}/v1/lights/refresh"))
            .send()?
            .error_for_status()?;
        Ok(())
    }

    pub fn set_light_enabled(&self, id: &str, enabled: bool) -> Result<(), reqwest::Error> {
        self.client
            .put(format!("{BASE_URL}/v1/lights/{id}/enabled"))
            .json(&serde_json::json!({ "enabled": enabled }))
            .send()?
            .error_for_status()?;
        Ok(())
    }

    pub fn set_light_alias(&self, id: &str, alias: &str) -> Result<(), reqwest::Error> {
        let value = if alias.trim().is_empty() {
            serde_json::json!({ "alias": null })
        } else {
            serde_json::json!({ "alias": alias })
        };
        self.client
            .put(format!("{BASE_URL}/v1/lights/{id}/alias"))
            .json(&value)
            .send()?
            .error_for_status()?;
        Ok(())
    }

    pub fn delete_light(&self, id: &str) -> Result<(), reqwest::Error> {
        self.client
            .delete(format!("{BASE_URL}/v1/lights/{id}"))
            .send()?
            .error_for_status()?;
        Ok(())
    }

    pub fn create_group(&self, name: &str, members: &[String]) -> Result<(), reqwest::Error> {
        self.client
            .post(format!("{BASE_URL}/v1/groups"))
            .json(&serde_json::json!({ "name": name, "members": members }))
            .send()?
            .error_for_status()?;
        Ok(())
    }

    pub fn delete_group(&self, name: &str) -> Result<(), reqwest::Error> {
        self.client
            .delete(format!("{BASE_URL}/v1/groups/{name}"))
            .send()?
            .error_for_status()?;
        Ok(())
    }
}
