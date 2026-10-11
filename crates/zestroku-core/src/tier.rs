//! Observed access tier (zestroku-tier-discovery-jt8): classify a device's
//! tier from wire evidence, record it, and gate mutations on the record.
//!
//! NO-CLAIM: the tier is OBSERVED, not authenticated. Nothing here defends
//! against a hostile device that lies about its mode; the recorded tier is
//! an operational fact ("what the device last answered"), never a security
//! boundary.
//!
//! Classification (acceptance [a]): `query/apps` 200 => Permissive; 403
//! whose body is byte-exactly the verified Limited 403 => Limited; ANY other
//! outcome (different 403 body, 404, 5xx, unreachable, malformed) => Unknown
//! — an unknown device is never assumed Limited, because assuming a tier
//! from a hostile or broken answer is exactly the NO-CLAIM line.
//!
//! Persistence: the classification plus a probe_map (command class ->
//! open/403/404/untested) land in the m7d `access_tier` table and
//! `devices.access_tier`; `devices` is the inventory writer the store was
//! missing.
//!
//! Probing uses `query/apps` because it is the documented discriminator:
//! Open refuses it with the exact 403 (Limited), Permissive answers with
//! the full install list (200). Introspection routes stay tier-blind by
//! design and cannot discriminate.
#![forbid(unsafe_code)]

use std::time::{SystemTime, UNIX_EPOCH};

use zestroku_proto::{EcpGate, EcpResponse, parse_ecp_response};
use zestroku_types::AccessTier;

use crate::store::{AccessTierRow, Store, StoreError};

/// The probed command class recorded in the probe_map.
pub const PROBE_COMMAND: &str = "query/apps";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservedTier {
    Permissive,
    Limited,
    Unknown,
}

impl ObservedTier {
    /// The exact string stored in `devices.access_tier` / `access_tier.tier`.
    pub fn as_str(self) -> &'static str {
        match self {
            ObservedTier::Permissive => "permissive",
            ObservedTier::Limited => "limited",
            ObservedTier::Unknown => "unknown",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "permissive" => Some(ObservedTier::Permissive),
            "limited" => Some(ObservedTier::Limited),
            "unknown" => Some(ObservedTier::Unknown),
            _ => None,
        }
    }

    /// The gate-side tier the rest of the core consumes.
    pub fn access_tier(self) -> AccessTier {
        match self {
            ObservedTier::Permissive => AccessTier::Permissive,
            ObservedTier::Limited | ObservedTier::Unknown => AccessTier::Open,
        }
    }
}

/// Pure classification (acceptance [a]). Takes the already-classified gate
/// so the byte-exact 403 rule lives in exactly one place (proto), and adds
/// the tier decision: Open 200 => Permissive, byte-exact Limited403 =>
/// Limited, ANYTHING else => Unknown.
pub fn classify_tier(gate: EcpGate) -> ObservedTier {
    match gate {
        // query/apps answered with data: enumeration is Permissive-only.
        EcpGate::Open => ObservedTier::Permissive,
        // Byte-exact Limited 403 (verified body checked in proto).
        EcpGate::Limited403 => ObservedTier::Limited,
        // 403 with a different body, 404, 5xx, malformed: Unknown. Never
        // Limited — a hostile/broken answer must not downgrade the gate.
        _ => ObservedTier::Unknown,
    }
}

/// Convenience wrapper: classify straight from a raw response.
pub fn classify_tier_from_response(response: &EcpResponse) -> ObservedTier {
    classify_tier(parse_ecp_response(response).gate)
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// What decided the tier that gated a mutation. The m7d actions schema has
/// no tier-source column and closed migrations are append-only, so the
/// source rides as a documented marker prefix on `before_state`:
/// `tier_source=<…>; <snapshot>`. Marker values: `recorded` (from the
/// access_tier table), `probe` (fresh probe this run), `env-override`
/// (ZESTROKU_TIER), `unknown-unprobed`. Asserted in tests.
pub const TIER_SOURCE_PREFIX: &str = "tier_source=";

/// Builds the before_state string with the tier-source marker prepended.
/// Marker values: `recorded` (from the access_tier table), `probe` (fresh
/// probe this run), `env-override` (ZESTROKU_TIER), `unknown-unprobed`.
pub fn marked_before_state(source: &str, snapshot: &str) -> String {
    format!("{TIER_SOURCE_PREFIX}{source}; {snapshot}")
}

/// Persist one probe: access_tier row + devices.access_tier refresh.
/// `probe_map` is the JSON the acceptance names (command class -> outcome).
pub fn record_probe<S: Store + ?Sized>(
    store: &S,
    device_id: &str,
    observed: ObservedTier,
    apps_outcome: &str,
) -> Result<(), StoreError> {
    let at = now_unix();
    let probe_map = format!(r#"{{"{PROBE_COMMAND}":"{apps_outcome}"}}"#);
    store.upsert_access_tier(&AccessTierRow {
        device_id: device_id.to_string(),
        tier: observed.as_str().to_string(),
        probe_at: at,
        probe_map,
    })?;
    // devices.access_tier: only refresh when a device row exists (info/launch
    // may run before any `devices` inventory pass created one).
    if let Some(mut row) = store.get_device(device_id)? {
        row.access_tier = observed.as_str().to_string();
        row.last_seen = at;
        store.upsert_device(&row)?;
    }
    Ok(())
}

/// Reads the recorded tier for a device, if any.
pub fn recorded_tier<S: Store + ?Sized>(
    store: &S,
    device_id: &str,
) -> Result<Option<ObservedTier>, StoreError> {
    Ok(store
        .get_access_tier(device_id)?
        .and_then(|row| ObservedTier::parse(&row.tier)))
}

/// The one canonical key for a device across `info`, `devices`, `apps`
/// and the mutation gate: `host:port`.
pub fn device_id(host: &str, port: u16) -> String {
    format!("{host}:{port}")
}

/// Recorded tier under the canonical `host:port` key, falling back to the
/// bare-host key that earlier builds wrote (so an existing recorded tier is
/// not lost to the unification). The canonical row wins when both exist.
pub fn recorded_tier_for<S: Store + ?Sized>(
    store: &S,
    host: &str,
    port: u16,
) -> Result<Option<ObservedTier>, StoreError> {
    if let Some(t) = recorded_tier(store, &device_id(host, port))? {
        return Ok(Some(t));
    }
    recorded_tier(store, host)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::SqliteStore;

    fn class_of(status: u16, body: &str) -> EcpGate {
        parse_ecp_response(&EcpResponse {
            status,
            headers: vec![],
            body: body.to_string(),
        })
        .gate
    }

    const LIMITED_BODY: &str = "ECP command not allowed in Limited mode.";

    #[test]
    fn permissive_from_open_200() {
        assert_eq!(
            classify_tier(class_of(200, "<apps></apps>")),
            ObservedTier::Permissive
        );
    }

    #[test]
    fn limited_only_from_byte_exact_403() {
        assert_eq!(
            classify_tier(class_of(403, LIMITED_BODY)),
            ObservedTier::Limited
        );
    }

    // -- planted negatives (acceptance [a]) --------------------------------

    #[test]
    fn planted_negative_different_403_body_is_unknown_never_limited() {
        for body in [
            "ECP command not allowed in Limited mode", // truncated: no final '.'
            "ECP command not allowed in LIMITED mode.", // case shift
            "forbidden",
            "",
        ] {
            let gate = class_of(403, body);
            assert_ne!(
                gate,
                EcpGate::Limited403,
                "body {body:?} must not classify as Limited403"
            );
            assert_eq!(classify_tier(gate), ObservedTier::Unknown, "body {body:?}");
        }
    }

    #[test]
    fn planted_negative_200_with_garbage_is_still_permissive_probe_but_unknown_paths_safe() {
        // A 200 with garbage XML still classifies the TIER as Permissive
        // (the tier discriminator is the status on query/apps, not the body
        // shape; body parsing is parse_apps' job and its failure is a
        // separate, later error). The Unknown path covers non-200/403
        // outcomes: 404, 5xx, Other.
        assert_eq!(
            classify_tier(class_of(200, "<not-xml")),
            ObservedTier::Permissive
        );
        for status in [404, 500, 503] {
            assert_eq!(classify_tier(class_of(status, "x")), ObservedTier::Unknown);
        }
    }

    #[test]
    fn unknown_maps_to_fail_safe_open_gate() {
        // The gate-side mapping is the security-relevant property: Unknown
        // and Limited both refuse mutations (AccessTier::Open device).
        assert_eq!(ObservedTier::Unknown.access_tier(), AccessTier::Open);
        assert_eq!(ObservedTier::Limited.access_tier(), AccessTier::Open);
        assert_eq!(
            ObservedTier::Permissive.access_tier(),
            AccessTier::Permissive
        );
    }

    #[test]
    fn tier_strings_round_trip() {
        for t in [
            ObservedTier::Permissive,
            ObservedTier::Limited,
            ObservedTier::Unknown,
        ] {
            assert_eq!(ObservedTier::parse(t.as_str()), Some(t));
        }
        assert_eq!(ObservedTier::parse("open"), None);
        assert_eq!(ObservedTier::parse(""), None);
    }

    #[test]
    fn marker_prefix_is_stable() {
        assert_eq!(TIER_SOURCE_PREFIX, "tier_source=");
        assert_eq!(
            marked_before_state("env-override", r#"{"app":"home"}"#),
            "tier_source=env-override; {\"app\":\"home\"}"
        );
    }

    // -- store persistence (acceptance [a] record + [d] reopen) ------------

    #[test]
    fn record_probe_persists_tier_and_device_row_and_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let device_id = "192.0.2.10";
        {
            let store = SqliteStore::open(&db).expect("open");
            // A device row must exist for the devices.access_tier refresh.
            store
                .upsert_device(&crate::store::DeviceRow {
                    id: device_id.into(),
                    name: "probe-test".into(),
                    ip: device_id.into(),
                    model: "Sim Express".into(),
                    model_num: "SIMX1".into(),
                    firmware: "15.2.4".into(),
                    wifi_driver: "wifi".into(),
                    access_tier: "unknown".into(),
                    last_seen: 0,
                })
                .unwrap();
            record_probe(&store, device_id, ObservedTier::Permissive, "open").unwrap();
            // Read path.
            assert_eq!(
                recorded_tier(&store, device_id).unwrap(),
                Some(ObservedTier::Permissive)
            );
            let row = store.get_access_tier(device_id).unwrap().unwrap();
            assert_eq!(row.tier, "permissive");
            assert_eq!(row.probe_map, r#"{"query/apps":"open"}"#);
            let dev = store.get_device(device_id).unwrap().unwrap();
            assert_eq!(dev.access_tier, "permissive");
            assert!(dev.last_seen > 0);
            // store closes here (drop) — reopen below proves persistence.
        }
        {
            let store = SqliteStore::open(&db).expect("reopen");
            assert_eq!(
                recorded_tier(&store, device_id).unwrap(),
                Some(ObservedTier::Permissive)
            );
            assert_eq!(
                store.get_device(device_id).unwrap().unwrap().access_tier,
                "permissive"
            );
        }
    }

    #[test]
    fn record_probe_without_device_row_still_records_tier() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(&dir.path().join("t.db")).expect("open");
        // info/launch may run before any inventory pass: no error, tier row
        // still recorded, no device row conjured.
        record_probe(&store, "192.0.2.99", ObservedTier::Limited, "limited403").unwrap();
        assert_eq!(
            recorded_tier(&store, "192.0.2.99").unwrap(),
            Some(ObservedTier::Limited)
        );
        assert!(store.get_device("192.0.2.99").unwrap().is_none());
    }

    #[test]
    fn device_id_is_host_colon_port() {
        assert_eq!(device_id("192.0.2.7", 8060), "192.0.2.7:8060");
    }

    #[test]
    fn info_and_devices_agree_on_one_key() {
        // `info` records under device_id(host, port); `devices` reads and
        // writes the inventory row under "host:port". Same key, so the
        // inventory row's tier follows the probe (was `unknown` forever
        // when info keyed by bare host).
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(&dir.path().join("k.db")).expect("open");
        let id = device_id("192.0.2.8", 8060);
        store
            .upsert_device(&crate::store::DeviceRow {
                id: id.clone(),
                name: String::new(),
                ip: "192.0.2.8".into(),
                model: String::new(),
                model_num: "X".into(),
                firmware: "1".into(),
                wifi_driver: String::new(),
                access_tier: "unknown".into(),
                last_seen: 1,
            })
            .unwrap();
        record_probe(&store, &id, ObservedTier::Permissive, "200").unwrap();
        assert_eq!(
            store.get_device(&id).unwrap().unwrap().access_tier,
            "permissive"
        );
        assert_eq!(
            recorded_tier_for(&store, "192.0.2.8", 8060).unwrap(),
            Some(ObservedTier::Permissive)
        );
    }

    #[test]
    fn recorded_tier_for_reads_legacy_bare_host_rows() {
        // Rows written before the unification used the bare host.
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(&dir.path().join("l.db")).expect("open");
        record_probe(&store, "192.0.2.9", ObservedTier::Permissive, "200").unwrap();
        assert_eq!(
            recorded_tier_for(&store, "192.0.2.9", 8060).unwrap(),
            Some(ObservedTier::Permissive)
        );
        // The canonical row wins when both exist.
        record_probe(&store, "192.0.2.9:8060", ObservedTier::Limited, "403").unwrap();
        assert_eq!(
            recorded_tier_for(&store, "192.0.2.9", 8060).unwrap(),
            Some(ObservedTier::Limited)
        );
        // Unrelated host: nothing.
        assert_eq!(recorded_tier_for(&store, "192.0.2.10", 8060).unwrap(), None);
    }
}
