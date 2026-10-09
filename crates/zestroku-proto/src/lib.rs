//! zestroku-proto — LANE A: ECP protocol encoding/decoding. Pure functions.
//!
//! Every request builder and response parser here is unit-tested against
//! scrubbed fixtures (no network). The Transport trait sits at the edge;
//! the async runtime is wired in zestroku-cli, not here.
//!
//! Ground truth (verified live 2026-10-09):
//! - `GET /query/device-info`   → 200 `<device-info>…` (Open tier)
//! - `GET /query/active-app`    → 200 `<active-app>…`  (Open tier)
//! - `GET /query/apps`          → 403 in Limited mode   (Permissive tier)
//! - `POST /keypress/<Key>`     → 403 in Limited mode   (Permissive tier)
//! - `POST /launch/<appID>`     → 403 in Limited mode   (Permissive tier)
#![forbid(unsafe_code)]

use std::fmt;

use zestroku_types::{AccessTier, Error, Key, Result};

/// One ECP command: everything the Transport needs.
#[derive(Debug, Clone, PartialEq)]
pub struct Command {
    /// HTTP method (ECP uses GET for query/*, POST for the rest).
    pub method: Method,
    /// Path with query, e.g. "/query/device-info".
    pub path: String,
    /// Tier this command requires — the core checks BEFORE dispatch.
    pub tier: AccessTier,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Method::Get => f.write_str("GET"),
            Method::Post => f.write_str("POST"),
        }
    }
}

/// Build a query command (Open tier).
pub fn query(resource: &str) -> Command {
    Command {
        method: Method::Get,
        path: format!("/query/{resource}"),
        tier: AccessTier::Open,
    }
}

/// Build a keypress command (Permissive tier).
pub fn keypress(key: Key) -> Command {
    Command {
        method: Method::Post,
        path: format!("/keypress/{key:?}"),
        tier: AccessTier::Permissive,
    }
}

/// The subset of `<device-info>` fields the core models.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceInfo {
    pub model_name: String,
    pub model_number: String,
    pub software_version: String,
    pub is_tv: bool,
}

/// Parse a `<device-info>` XML body (scrubbed-fixture tested).
pub fn parse_device_info(xml: &str) -> Result<DeviceInfo> {
    let field = |tag: &str| -> Result<String> {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        let start = xml.find(&open).map(|i| i + open.len()).ok_or_else(|| {
            Error::Malformed(format!("missing <{tag}> in device-info"))
        })?;
        let end = xml[start..].find(&close).map(|i| start + i).ok_or_else(|| {
            Error::Malformed(format!("missing </{tag}> in device-info"))
        })?;
        Ok(xml[start..end].to_string())
    };
    Ok(DeviceInfo {
        model_name: field("model-name")?,
        model_number: field("model-number")?,
        software_version: field("software-version")?,
        is_tv: field("is-tv")? == "true",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE_DEVICE_INFO: &str = r#"<?xml version="1.0" encoding="UTF-8" ?>
<device-info>
    <udn>SCRUBBED</udn>
    <serial-number>SCRUBBED</serial-number>
    <vendor-name>Roku</vendor-name>
    <model-name>Roku Express 4K+</model-name>
    <model-number>3941X2</model-number>
    <is-tv>false</is-tv>
    <software-version>15.2.4</software-version>
</device-info>"#;

    #[test]
    fn parse_device_info_extracts_modeled_fields() {
        let info = parse_device_info(FIXTURE_DEVICE_INFO).unwrap();
        assert_eq!(info.model_name, "Roku Express 4K+");
        assert_eq!(info.model_number, "3941X2");
        assert_eq!(info.software_version, "15.2.4");
        assert!(!info.is_tv);
    }

    #[test]
    fn parse_device_info_fails_closed_on_missing_field() {
        let e = parse_device_info("<device-info></device-info>").unwrap_err();
        assert!(matches!(e, Error::Malformed(_)));
    }

    #[test]
    fn query_commands_are_open_tier() {
        assert_eq!(query("device-info").tier, AccessTier::Open);
        assert_eq!(query("device-info").method, Method::Get);
    }

    #[test]
    fn keypress_commands_are_permissive_tier() {
        let c = keypress(Key::Home);
        assert_eq!(c.tier, AccessTier::Permissive);
        assert_eq!(c.method, Method::Post);
        assert_eq!(c.path, "/keypress/Home");
    }
}
