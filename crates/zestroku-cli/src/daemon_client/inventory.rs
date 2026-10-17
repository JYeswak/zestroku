//! Cached inventory from the daemon; this endpoint performs no discovery.
#![forbid(unsafe_code)]

use super::ReadError;

#[derive(serde::Deserialize, serde::Serialize)]
pub(crate) struct InventoryDevice {
    pub(crate) name: String,
    pub(crate) host: String,
    pub(crate) model: String,
    pub(crate) model_number: String,
    pub(crate) software_version: String,
    pub(crate) access_tier: String,
    pub(crate) last_seen: i64,
}

pub(crate) fn devices() -> Result<Vec<InventoryDevice>, ReadError> {
    let response = super::daemon_get("/v1/devices")?;
    serde_json::from_str(&response.body)
        .map_err(|error| ReadError::InvalidResponse(format!("daemon JSON: {error}")))
}
