//! Shared discovery inventory producer (a0h.11.1): the identity-aware
//! persist loop behind `zestroku devices`, reusable by the daemon instead of
//! a second scan/DB convention. `devices.rs` keeps options, CIDR
//! orchestration, stdout, exits and send_selected behavior.
//!
//! A discovered device plus its stable identity key (`identity_key`; empty
//! when unknown, which never merges). The persist loop moves a known identity
//! to its new endpoint (aliases follow) and records unknown identities
//! per-endpoint without merging.

#![forbid(unsafe_code)]

use std::path::PathBuf;

use zestroku_core::discovery::{identity_key, parse_targets, reconcile};
use zestroku_core::store::{DeviceRow, SqliteStore, Store};
use zestroku_proto::{parse_device_info, query};
use zestroku_types::{Device, Error, Result};

use crate::transport::{AsupersyncTransport, send_selected};

/// A discovered device plus its stable identity key (`identity_key`; empty
/// when unknown, which never merges). Named alias keeps signatures under
/// Clippy's type-complexity threshold.
pub type FoundDevice = (Device, String);

/// Record the first store failure only; later errors must not hide it.
pub fn note_first_error(store_err: &mut Option<String>, message: String) {
    if store_err.is_none() {
        *store_err = Some(message);
    }
}

/// Seeds file text: explicit path (must exist), else the default path if present.
pub fn read_seeds(explicit: Option<&PathBuf>) -> std::result::Result<Option<String>, String> {
    if let Some(p) = explicit {
        return std::fs::read_to_string(p)
            .map(Some)
            .map_err(|e| format!("{}: {e}", p.display()));
    }
    let Some(home) = std::env::var_os("HOME") else {
        return Ok(None);
    };
    let p = PathBuf::from(home).join(".config/zestroku/targets.toml");
    Ok(std::fs::read_to_string(p).ok())
}

pub async fn seeds_only(seeds: &str) -> Result<(Vec<FoundDevice>, Vec<String>)> {
    let (mut devices, mut notes) = (Vec::new(), Vec::new());
    for ep in parse_targets(seeds)? {
        let probed = async {
            let resp = send_selected(
                AsupersyncTransport,
                &ep.host,
                ep.port,
                &query("device-info"),
            )
            .await?;
            if resp.status != 200 {
                return Err(Error::Unreachable(format!("HTTP {}", resp.status)));
            }
            parse_device_info(&resp.body)
        }
        .await;
        match probed {
            Ok(info) => devices.push((reconcile(&ep, &info), identity_key(&info))),
            Err(e) => notes.push(format!("{}:{}: {e}", ep.host, ep.port)),
        }
    }
    Ok((devices, notes))
}

/// Persist inventory (zestroku-tier-discovery-jt8 [d]): one DeviceRow per
/// discovered device, id = "host:port". Privacy: model-name/number/version
/// only; never serial, UDN, MAC, device-id, advertising-id or the friendly
/// name; the raw device-info body is not retained. Returns the first store
/// note, if any; callers still print the devices regardless.
pub fn persist_inventory(
    store: Option<&SqliteStore>,
    devices: &[(Device, String)],
) -> Option<String> {
    let mut store_err: Option<String> = None;
    let mut rows = Vec::with_capacity(devices.len());
    for (d, identity) in devices {
        let id = zestroku_core::tier::device_id(&d.host, d.port);
        let access_tier = match store {
            Some(s) => match zestroku_core::tier::recorded_tier_for(s, &d.host, d.port) {
                Ok(Some(t)) => t.as_str().to_string(),
                Ok(None) => "unknown".to_string(),
                Err(e) => {
                    if store_err.is_none() {
                        store_err = Some(format!("tier lookup {id}: {e}"));
                    }
                    "unknown".to_string()
                }
            },
            None => "unknown".to_string(),
        };
        rows.push((
            DeviceRow {
                id: id.clone(),
                name: d.model_name.clone(),
                ip: d.host.clone(),
                model: d.model_name.clone(),
                model_num: d.model_number.clone(),
                firmware: d.software_version.clone(),
                wifi_driver: String::new(),
                access_tier,
                last_seen: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|t| t.as_secs() as i64)
                    .unwrap_or(0),
            },
            identity.clone(),
        ));
    }
    if let Some(s) = store {
        // Self-healing identity (zestroku-self-healing-identity-7ac):
        // when a known identity key reappears at a new endpoint, the
        // existing row moves (aliases follow it) instead of leaving
        // a stale duplicate. Unknown/empty identity keeps the old
        // per-endpoint behavior. Probe caches (apps, capabilities,
        // tiers) stay endpoint-keyed and are re-observed on demand.
        for (row, identity) in &rows {
            if !identity.is_empty() {
                match s.find_device_id_by_identity(identity) {
                    Ok(Some(owner)) if owner != row.id => {
                        if let Err(error) = s.move_device_identity(&owner, row, identity) {
                            note_first_error(
                                &mut store_err,
                                format!("move identity {owner} to {}: {error}", row.id),
                            );
                            break;
                        }
                        continue;
                    }
                    Err(error) => {
                        note_first_error(
                            &mut store_err,
                            format!("identity lookup {}: {error}", row.id),
                        );
                        break;
                    }
                    Ok(_) => {}
                }
            }
            if let Err(e) = s.upsert_device(row) {
                note_first_error(&mut store_err, format!("upsert {}: {e}", row.id));
                break;
            }
            if identity.is_empty() {
                // Nothing to record; unknown identity never merges.
            } else if let Err(e) = s.record_device_identity(&row.id, identity) {
                note_first_error(&mut store_err, format!("record identity {}: {e}", row.id));
                break;
            }
        }
    }
    store_err
}
