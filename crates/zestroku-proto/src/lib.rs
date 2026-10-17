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

/// An HTTP response from an ECP endpoint, before protocol classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EcpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EcpResponseClass {
    Introspection,
    Gated,
    GatedAnomaly,
    Absent,
    Unclassified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EcpGate {
    Open,
    Limited403,
    Unknown403,
    Absent404,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EcpClassification {
    pub class: EcpResponseClass,
    pub gate: EcpGate,
}

const LIMITED_403_BODY: &str = "ECP command not allowed in Limited mode.";

/// Classifies an HTTP response using the verified ECP response reference.
/// The exact body is required before treating HTTP 403 as the Limited gate.
pub fn parse_ecp_response(response: &EcpResponse) -> EcpClassification {
    let (class, gate) = match response.status {
        200 => (EcpResponseClass::Introspection, EcpGate::Open),
        403 if response.body.trim() == LIMITED_403_BODY => {
            (EcpResponseClass::Gated, EcpGate::Limited403)
        }
        403 => (EcpResponseClass::GatedAnomaly, EcpGate::Unknown403),
        404 => (EcpResponseClass::Absent, EcpGate::Absent404),
        _ => (EcpResponseClass::Unclassified, EcpGate::Other),
    };
    EcpClassification { class, gate }
}

/// Async ECP I/O boundary. Implementations own runtime-specific networking;
/// callers only provide a host, port, and protocol command.
pub trait Transport: Send + Sync {
    /// Sends one command and returns raw response fields for pure parsing.
    ///
    /// RPITIT keeps this async boundary dependency-free while requiring the
    /// returned future to be movable across executor threads.
    fn send(
        &self,
        host: &str,
        port: u16,
        cmd: &Command,
    ) -> impl Future<Output = Result<EcpResponse>> + Send;
}

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
/// Build the installed-app query command (Permissive tier).
#[must_use]
pub fn query_apps() -> Command {
    Command {
        method: Method::Get,
        path: "/query/apps".to_string(),
        tier: AccessTier::Permissive,
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

/// Build an app-launch command (Permissive tier).
pub fn launch(app_id: u32) -> Command {
    Command {
        method: Method::Post,
        path: format!("/launch/{app_id}"),
        tier: AccessTier::Permissive,
    }
}

/// Build a literal keypress from a raw ECP key name not in the `Key` enum
/// (e.g. "Lit_<char>", "PlayOnly"). Permissive tier. Prefer `keypress`
/// for the modeled keys.
pub fn keypress_raw(ecp_key: &str) -> Command {
    Command {
        method: Method::Post,
        path: format!("/keypress/{ecp_key}"),
        tier: AccessTier::Permissive,
    }
}

/// The currently focused app from `<active-app>` (Open tier).
#[derive(Debug, Clone, PartialEq)]
pub struct ActiveApp {
    pub app_id: Option<u32>,
    pub name: String,
}

/// Parse an `<active-app>` XML body. Two real forms (verified live 2026-10-10):
/// home screen: `<app>Roku</app>` (short form — no attributes, bare text name);
/// launched app: `<app id="…" type="…" …>Name</app>` (attribute form).
/// Scrubbed-fixture tested for both.
pub fn parse_active_app(xml: &str) -> Result<ActiveApp> {
    let block = if let Some(i) = xml.find("<app ") {
        // Attribute form: <app id="…" …>Name</app>
        let end = xml[i..]
            .find("</app>")
            .map(|e| i + e)
            .ok_or_else(|| Error::Malformed("missing </app> in active-app".to_string()))?;
        &xml[i..end]
    } else if let Some(i) = xml.find("<app>") {
        // Short form: <app>Name</app> (home screen; no id)
        let end = xml[i..]
            .find("</app>")
            .map(|e| i + e)
            .ok_or_else(|| Error::Malformed("missing </app> in active-app".to_string()))?;
        &xml[i..end]
    } else {
        return Err(Error::Malformed("missing <app> in active-app".to_string()));
    };

    let attr = |name: &str| -> Option<String> {
        let needle = format!("{name}=\"");
        let s = block.find(&needle)? + needle.len();
        let e = block[s..].find('"')? + s;
        Some(block[s..e].to_string())
    };
    let app_id = attr("id").and_then(|v| v.parse().ok());
    let name = if block.starts_with("<app ") {
        // attribute form: name is the text after the opening tag's '>'
        let name_end = block
            .find('>')
            .ok_or_else(|| Error::Malformed("malformed <app> tag in active-app".to_string()))?
            + 1;
        block[name_end..].trim().to_string()
    } else {
        // short form: name is the text between <app> and </app>
        block["<app>".len()..].trim().to_string()
    };
    Ok(ActiveApp { app_id, name })
}
/// One installed application returned by `GET /query/apps`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledApp {
    pub id: String,
    pub name: String,
    pub app_type: String,
    pub subtype: Option<String>,
    pub version: Option<String>,
}
/// The modeled playback state in the committed simulator fixture.
///
/// The simulator's `<player state="..."/>` body is a FILL contract, not a
/// claim about every Roku firmware field. Unknown XML fields are intentionally
/// ignored until their wire semantics are sampled and added to a fixture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaPlayerState {
    pub state: String,
}

/// Parse the installed-app XML response. The exact Limited-mode denial body
/// is a typed tier refusal, not an empty app list.
pub fn parse_apps(xml: &str) -> Result<Vec<InstalledApp>> {
    if xml == LIMITED_403_BODY {
        return Err(Error::TierRefused {
            cmd: "query/apps".to_string(),
            mode: AccessTier::Open,
        });
    }

    let document = roxmltree::Document::parse(xml)
        .map_err(|error| Error::Malformed(format!("invalid query/apps XML: {error}")))?;
    let root = document.root_element();
    if root.tag_name().name() != "apps" {
        return Err(Error::Malformed(
            "expected <apps> root in query/apps".to_string(),
        ));
    }

    root.children()
        .filter(|node| node.is_element() && node.tag_name().name() == "app")
        .map(|node| {
            let id = node
                .attribute("id")
                .filter(|value| !value.is_empty())
                .ok_or_else(|| Error::Malformed("missing app id in query/apps".to_string()))?;
            let app_type = node
                .attribute("type")
                .filter(|value| !value.is_empty())
                .ok_or_else(|| Error::Malformed("missing app type in query/apps".to_string()))?;
            let name = node.text().unwrap_or_default().trim();
            if name.is_empty() {
                return Err(Error::Malformed(
                    "missing app name in query/apps".to_string(),
                ));
            }
            Ok(InstalledApp {
                id: id.to_string(),
                name: name.to_string(),
                app_type: app_type.to_string(),
                subtype: node
                    .attribute("subtype")
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned),
                version: node
                    .attribute("version")
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned),
            })
        })
        .collect()
}
/// Parse the `query/media-player` response without treating an arbitrary 403
/// body as the Limited-tier refusal.
pub fn parse_media_player(response: &EcpResponse) -> Result<MediaPlayerState> {
    match parse_ecp_response(response).gate {
        EcpGate::Open => {}
        EcpGate::Limited403 => {
            return Err(Error::TierRefused {
                cmd: "query/media-player".to_string(),
                mode: AccessTier::Open,
            });
        }
        EcpGate::Unknown403 => {
            return Err(Error::Malformed(
                "unrecognized HTTP 403 body for query/media-player".to_string(),
            ));
        }
        EcpGate::Absent404 => {
            return Err(Error::Malformed(
                "query/media-player endpoint is absent".to_string(),
            ));
        }
        EcpGate::Other => {
            return Err(Error::Unreachable(format!(
                "query/media-player returned HTTP {}",
                response.status
            )));
        }
    }
    let document = roxmltree::Document::parse(&response.body)
        .map_err(|error| Error::Malformed(format!("invalid query/media-player XML: {error}")))?;
    let root = document.root_element();
    if root.tag_name().name() != "player" {
        return Err(Error::Malformed(
            "expected <player> root in query/media-player".to_string(),
        ));
    }
    let state = root
        .attribute("state")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| Error::Malformed("missing player state".to_string()))?;
    Ok(MediaPlayerState {
        state: state.to_owned(),
    })
}

/// Parse the `query/tv-active-channel` surface fingerprint.
///
/// The wire contract establishes presence by status (200/limited-403) versus
/// absence (404); channel XML contents are not yet modeled.
pub fn parse_tv_active_channel(response: &EcpResponse) -> Result<bool> {
    match parse_ecp_response(response).gate {
        EcpGate::Open => {
            roxmltree::Document::parse(&response.body).map_err(|error| {
                Error::Malformed(format!("invalid query/tv-active-channel XML: {error}"))
            })?;
            Ok(true)
        }
        EcpGate::Limited403 => Ok(true),
        EcpGate::Absent404 => Ok(false),
        EcpGate::Unknown403 => Err(Error::Malformed(
            "unrecognized HTTP 403 body for query/tv-active-channel".to_string(),
        )),
        EcpGate::Other => Err(Error::Unreachable(format!(
            "query/tv-active-channel returned HTTP {}",
            response.status
        ))),
    }
}

/// The subset of `<device-info>` fields the core models.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceInfo {
    pub model_name: String,
    pub model_number: String,
    pub software_version: String,
    pub is_tv: bool,
    /// Raw serial-number/UDN identity fields. Never printed, logged, or
    /// stored verbatim outside the runtime DB; see `identity_key` in
    /// zestroku-core. Absent tags parse as empty (fail-open read, the
    /// required modeled fields still fail closed below).
    pub serial_number: String,
    pub udn: String,
}

/// Parse a `<device-info>` XML body (scrubbed-fixture tested).
pub fn parse_device_info(xml: &str) -> Result<DeviceInfo> {
    let field = |tag: &str| -> Result<String> {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        let start = xml
            .find(&open)
            .map(|i| i + open.len())
            .ok_or_else(|| Error::Malformed(format!("missing <{tag}> in device-info")))?;
        let end = xml[start..]
            .find(&close)
            .map(|i| start + i)
            .ok_or_else(|| Error::Malformed(format!("missing </{tag}> in device-info")))?;
        Ok(xml[start..end].to_string())
    };
    let optional = |tag: &str| -> String {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        let start = match xml.find(&open).map(|i| i + open.len()) {
            Some(start) => start,
            None => return String::new(),
        };
        match xml[start..].find(&close).map(|i| start + i) {
            Some(end) => xml[start..end].to_string(),
            None => String::new(),
        }
    };
    Ok(DeviceInfo {
        model_name: field("model-name")?,
        model_number: field("model-number")?,
        software_version: field("software-version")?,
        is_tv: field("is-tv")? == "true",
        serial_number: optional("serial-number"),
        udn: optional("udn"),
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
        assert_eq!(info.serial_number, "SCRUBBED");
        assert_eq!(info.udn, "SCRUBBED");
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

#[cfg(test)]
mod active_app_tests {
    use super::*;

    const FIXTURE_ACTIVE_APP: &str = r#"<?xml version="1.0" encoding="UTF-8" ?>
<active-app>
    <app id="562859" type="home" version="14.10.5" ui-location="home">Roku Dynamic Menu</app>
</active-app>"#;

    const FIXTURE_NO_APP: &str = "<active-app>\n</active-app>";

    #[test]
    fn parse_active_app_extracts_id_and_name() {
        let app = parse_active_app(FIXTURE_ACTIVE_APP).unwrap();
        assert_eq!(app.app_id, Some(562859));
        assert_eq!(app.name, "Roku Dynamic Menu");
    }

    #[test]
    fn parse_active_app_fails_closed_when_empty() {
        assert!(parse_active_app(FIXTURE_NO_APP).is_err());
    }

    #[test]
    fn parse_active_app_short_form_home_screen() {
        // Real home-screen body, captured live 2026-10-10 (curl of the Express):
        // short form, no attributes, bare text name. Planted negative: the
        // attribute-form-only parser (pre-fix) fails this.
        let xml = "<?xml version=\"1.0\" encoding=\"UTF-8\" ?>\n<active-app>\n\t<app>Roku</app>\n</active-app>";
        let app = parse_active_app(xml).unwrap();
        assert_eq!(app.app_id, None);
        assert_eq!(app.name, "Roku");
    }

    #[test]
    fn launch_targets_permissive_tier() {
        let c = launch(562859);
        assert_eq!(c.tier, AccessTier::Permissive);
        assert_eq!(c.path, "/launch/562859");
    }

    #[test]
    fn keypress_raw_targets_permissive_tier() {
        let c = keypress_raw("Lit_a");
        assert_eq!(c.tier, AccessTier::Permissive);
        assert_eq!(c.path, "/keypress/Lit_a");
    }
}

#[cfg(test)]
mod response_tests {
    use super::*;

    fn response(status: u16, body: &str) -> EcpResponse {
        EcpResponse {
            status,
            headers: Vec::new(),
            body: body.to_string(),
        }
    }

    #[test]
    fn classifies_open_limited_absent_and_other_responses() {
        assert_eq!(
            parse_ecp_response(&response(200, "<device-info/>")),
            EcpClassification {
                class: EcpResponseClass::Introspection,
                gate: EcpGate::Open,
            }
        );
        assert_eq!(
            parse_ecp_response(&response(
                403,
                " ECP command not allowed in Limited mode. \n",
            )),
            EcpClassification {
                class: EcpResponseClass::Gated,
                gate: EcpGate::Limited403,
            }
        );
        assert_eq!(
            parse_ecp_response(&response(403, "Different response.")),
            EcpClassification {
                class: EcpResponseClass::GatedAnomaly,
                gate: EcpGate::Unknown403,
            }
        );
        assert_eq!(
            parse_ecp_response(&response(404, "")),
            EcpClassification {
                class: EcpResponseClass::Absent,
                gate: EcpGate::Absent404,
            }
        );
        assert_eq!(
            parse_ecp_response(&response(503, "ECP command not allowed in Limited mode.")),
            EcpClassification {
                class: EcpResponseClass::Unclassified,
                gate: EcpGate::Other,
            }
        );
    }
}

#[cfg(test)]
mod apps_tests {
    use super::*;

    // Based on edge-library's scrubbed ecp_fixture_permissive_apps.xml
    // (6bec640); the version attribute is synthetic to exercise that field.
    const FIXTURE_APPS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<apps>
<app id="1001" subtype="svod" type="appl" version="1.0">Streaming One</app>
<app id="1002" subtype="svod" type="appl">Streaming Two</app>
<app id="1003" type="appl">Example Channel</app>
<app id="tvinput.hdmi1" type="tvin">HDMI Input One</app>
<app id="tvinput.dtv" type="tvin">Live TV Input</app>
</apps>"#;

    #[test]
    fn query_apps_requires_permissive_tier() {
        let command = query_apps();
        assert_eq!(command.method, Method::Get);
        assert_eq!(command.path, "/query/apps");
        assert_eq!(command.tier, AccessTier::Permissive);
    }

    #[test]
    fn parse_apps_extracts_five_entries_including_tv_inputs() {
        assert_eq!(
            parse_apps(FIXTURE_APPS).unwrap(),
            vec![
                InstalledApp {
                    id: "1001".to_string(),
                    name: "Streaming One".to_string(),
                    app_type: "appl".to_string(),
                    subtype: Some("svod".to_string()),
                    version: Some("1.0".to_string()),
                },
                InstalledApp {
                    id: "1002".to_string(),
                    name: "Streaming Two".to_string(),
                    app_type: "appl".to_string(),
                    subtype: Some("svod".to_string()),
                    version: None,
                },
                InstalledApp {
                    id: "1003".to_string(),
                    name: "Example Channel".to_string(),
                    app_type: "appl".to_string(),
                    subtype: None,
                    version: None,
                },
                InstalledApp {
                    id: "tvinput.hdmi1".to_string(),
                    name: "HDMI Input One".to_string(),
                    app_type: "tvin".to_string(),
                    subtype: None,
                    version: None,
                },
                InstalledApp {
                    id: "tvinput.dtv".to_string(),
                    name: "Live TV Input".to_string(),
                    app_type: "tvin".to_string(),
                    subtype: None,
                    version: None,
                },
            ]
        );
    }

    #[test]
    fn parse_apps_classifies_the_limited_body_as_typed_refusal() {
        assert!(matches!(
            parse_apps(LIMITED_403_BODY),
            Err(Error::TierRefused { cmd, mode: AccessTier::Open }) if cmd == "query/apps"
        ));
    }

    #[test]
    fn parse_apps_rejects_malformed_xml() {
        assert!(matches!(parse_apps("<apps><app"), Err(Error::Malformed(_))));
    }
}
#[cfg(test)]
mod state_surface_tests {
    use super::*;

    const FIXTURE_MEDIA_PLAYER: &str = r#"<player state="close"/>"#;
    const FIXTURE_TV_ACTIVE_CHANNEL: &str = "<tv-channel/>";

    fn response(status: u16, body: &str) -> EcpResponse {
        EcpResponse {
            status,
            headers: Vec::new(),
            body: body.to_owned(),
        }
    }

    #[test]
    fn media_player_parser_reads_the_scrubbed_sim_state_fixture() {
        let parsed = parse_media_player(&response(200, FIXTURE_MEDIA_PLAYER)).unwrap();
        assert_eq!(parsed.state, "close");
    }

    #[test]
    fn media_player_parser_distinguishes_limited_from_unknown_403() {
        assert!(matches!(
            parse_media_player(&response(403, LIMITED_403_BODY)),
            Err(Error::TierRefused { cmd, mode: AccessTier::Open }) if cmd == "query/media-player"
        ));
        assert!(matches!(
            parse_media_player(&response(403, "unrecognized denial")),
            Err(Error::Malformed(_))
        ));
        assert!(matches!(
            parse_media_player(&response(404, "")),
            Err(Error::Malformed(_))
        ));
        assert!(matches!(
            parse_media_player(&response(500, "")),
            Err(Error::Unreachable(_))
        ));
        assert!(matches!(
            parse_media_player(&response(200, "<player")),
            Err(Error::Malformed(_))
        ));
    }

    #[test]
    fn tv_active_channel_parser_classifies_surface_and_refuses_unknown_403() {
        assert_eq!(
            parse_tv_active_channel(&response(200, FIXTURE_TV_ACTIVE_CHANNEL)),
            Ok(true)
        );
        assert_eq!(
            parse_tv_active_channel(&response(403, LIMITED_403_BODY)),
            Ok(true)
        );
        assert_eq!(parse_tv_active_channel(&response(404, "")), Ok(false));
        assert!(matches!(
            parse_tv_active_channel(&response(403, "unrecognized denial")),
            Err(Error::Malformed(_))
        ));
        assert!(matches!(
            parse_tv_active_channel(&response(200, "<tv-channel")),
            Err(Error::Malformed(_))
        ));
    }
}
