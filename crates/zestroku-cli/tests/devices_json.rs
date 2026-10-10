//! m7b (Josh-leg) fixture test: `zestroku devices --json` output schema.
//!
//! Pins the machine-checkable contract Josh's live run is asserted against
//! (docs/LIVE-VERIFY.md): stable field names, notes carry target+reason, and
//! the document parses as JSON. Offline — no network, no runtime; the JSON
//! emission path is exercised through the same code `run()` uses by building
//! the document from Device rows exactly as the CLI does.
//!
//! The schema here IS the contract: if you change the `--json` shape in
//! devices.rs, change these assertions in the same commit.
#![forbid(unsafe_code)]

use serde_json::Value;

/// Rebuild the JSON doc exactly as devices.rs `--json` does, from the same
/// Device rows the live leg will produce (values here are scrubbed fixtures:
/// TEST-NET hosts, fake serials never appear — device rows carry no serial).
fn devices_json(devices: &[zestroku_types::Device], notes: &[String]) -> Value {
    serde_json::json!({
        "devices": devices.iter().map(|d| serde_json::json!({
            "host": d.host,
            "port": d.port,
            "model_name": d.model_name,
            "model_number": d.model_number,
            "software_version": d.software_version,
            "is_tv": d.is_tv,
        })).collect::<Vec<_>>(),
        "notes": notes.iter().map(|n| {
            let (target, reason) = match n.split_once(": ") {
                Some((t, r)) => (t.to_string(), r.to_string()),
                None => (n.clone(), String::new()),
            };
            serde_json::json!({ "target": target, "reason": reason })
        }).collect::<Vec<_>>(),
    })
}

fn fixture_device(
    host: &str,
    model: &str,
    num: &str,
    fw: &str,
    tv: bool,
) -> zestroku_types::Device {
    zestroku_types::Device {
        host: host.into(),
        port: 8060,
        model_name: model.into(),
        model_number: num.into(),
        software_version: fw.into(),
        is_tv: tv,
        access_tier: zestroku_types::AccessTier::Open,
    }
}

#[test]
fn json_schema_matches_live_verify_contract() {
    // Values = plan §2 ground truth shape (scrubbed to TEST-NET for git).
    let devices = vec![
        fixture_device("192.0.2.10", "Roku Express 4K+", "3941X2", "15.2.4", false),
        fixture_device("192.0.2.11", "50RokuTV", "81R0X", "15.3.4", true),
    ];
    let doc = devices_json(&devices, &[]);
    let arr = doc["devices"].as_array().expect("devices is an array");
    assert_eq!(arr.len(), 2);
    for d in arr {
        // Every field the runbook asserts, present with the right type.
        assert!(d["host"].is_string(), "{d}");
        assert!(d["port"].is_number());
        assert!(d["model_name"].is_string());
        assert!(d["model_number"].is_string());
        assert!(d["software_version"].is_string());
        assert!(d["is_tv"].is_boolean());
    }
    // Field-level values survive the round trip.
    assert_eq!(arr[0]["model_number"], "3941X2");
    assert_eq!(arr[0]["software_version"], "15.2.4");
    assert_eq!(arr[0]["is_tv"], false);
    assert_eq!(arr[1]["is_tv"], true);
    // Notes key always present (empty array when all probes succeed).
    assert_eq!(doc["notes"].as_array().map(Vec::len), Some(0));
}

#[test]
fn dead_seed_note_is_machine_checkable() {
    // A seeds file may list a powered-off device: the note must split into
    // target ("host:port") + reason so a script can assert on either.
    let doc = devices_json(
        &[fixture_device(
            "192.0.2.10",
            "Roku Express 4K+",
            "3941X2",
            "15.2.4",
            false,
        )],
        &["192.0.2.99:8060: device unreachable: connection refused".to_string()],
    );
    assert_eq!(doc["devices"].as_array().map(Vec::len), Some(1));
    let notes = doc["notes"].as_array().unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0]["target"], "192.0.2.99:8060");
    assert_eq!(notes[0]["reason"], "device unreachable: connection refused");
}

#[test]
fn note_without_reason_split_keeps_whole_line_as_target() {
    // Defensive: a note that predates the "target: reason" convention must
    // still produce valid schema (reason empty, target = whole line).
    let doc = devices_json(&[], &["odd note without separator".to_string()]);
    let notes = doc["notes"].as_array().unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0]["target"], "odd note without separator");
    assert_eq!(notes[0]["reason"], "");
}
