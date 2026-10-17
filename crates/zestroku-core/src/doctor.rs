//! Doctor: read-only health checks over inventory, store, and LAN posture.
//!
//! Every check is a pure function of injected inputs: the [`Store`] is only
//! ever read, device probes go through [`Transport`], the SSDP reply and the
//! discovered set are passed in, and the `lan-policy` platform control is an
//! injected function. [`run_checked`] snapshots the store before and after
//! and refuses to return findings when any check mutated it.

use std::path::Path;
use std::time::Instant;

use zestroku_proto::{self, Transport};
use zestroku_types::Result;

use crate::discovery;
use crate::store::Store;
use crate::store::migrations::MIGRATIONS;
use crate::tier;

/// Outcome of one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Warn,
    Fail,
    Skip,
}

/// One check result: machine-readable id plus human detail, evidence, remedy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub id: &'static str,
    pub verdict: Verdict,
    pub detail: String,
    pub evidence: String,
    pub remedy: String,
}

/// Injected outcome of a direct TCP connect, for `lan-policy` classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectOutcome {
    Ok,
    Refused,
    HostUnreachable,
    Timeout,
}

/// Refusal of [`run_checked`] when a check mutated the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadOnlyViolation {
    pub before: String,
    pub after: String,
}

/// Everything a doctor run may read. Function pointers (never closures)
/// keep every seam fixture-injectable without trait objects.
pub struct Ctx<'a, T: Transport> {
    pub store: &'a dyn Store,
    pub store_path: &'a Path,
    pub targets_toml: Option<&'a str>,
    pub transport: &'a T,
    /// One raw SSDP reply datagram to validate (`None` skips the check).
    pub ssdp_reply: Option<&'a [u8]>,
    /// Device ids (`host:port`) seen by the last sweep (`&[]` skips).
    pub discovered_ids: &'a [&'a str],
    /// Direct-connect probe, per host:port.
    pub connect: fn(&str, u16) -> ConnectOutcome,
    /// Platform-tool control: does an OS-native tool reach the host?
    pub platform_ok: fn(&str) -> bool,
}

/// Extra checks run after the built-ins (test seam for the read-only gate).
pub type ExtraCheck<T> = fn(&Ctx<T>) -> Finding;

fn finding(
    id: &'static str,
    verdict: Verdict,
    detail: String,
    evidence: String,
    remedy: &str,
) -> Finding {
    Finding {
        id,
        verdict,
        detail,
        evidence,
        remedy: remedy.to_string(),
    }
}

fn split_host_port(id: &str) -> Option<(&str, u16)> {
    let (host, port) = id.rsplit_once(':')?;
    Some((host, port.parse().ok()?))
}

/// `store-readable`: the database opens and lists devices.
pub fn check_store(store: &dyn Store, path: &Path) -> Finding {
    match store.list_devices() {
        Ok(devices) => finding(
            "store-readable",
            Verdict::Pass,
            format!("store lists {} device(s)", devices.len()),
            format!("path={}", path.display()),
            "",
        ),
        Err(error) => finding(
            "store-readable",
            Verdict::Fail,
            format!("store unreadable: {error}"),
            format!("path={}", path.display()),
            "point ZESTROKU_DB at a readable store or re-run device discovery",
        ),
    }
}

/// `migrations-current`: applied schema versions match the compiled set.
pub fn check_migrations(store: &dyn Store) -> Finding {
    let expected: Vec<i64> = MIGRATIONS.iter().map(|m| m.version).collect();
    match store.schema_versions() {
        Ok(applied) => {
            let missing: Vec<i64> = expected
                .iter()
                .filter(|v| !applied.contains(v))
                .copied()
                .collect();
            if missing.is_empty() {
                finding(
                    "migrations-current",
                    Verdict::Pass,
                    format!("{} migration(s) applied", expected.len()),
                    format!("versions={applied:?}"),
                    "",
                )
            } else {
                finding(
                    "migrations-current",
                    Verdict::Fail,
                    format!("missing migrations {missing:?}"),
                    format!("applied={applied:?}"),
                    "reopen the store with the current binary so it migrates",
                )
            }
        }
        Err(error) => finding(
            "migrations-current",
            Verdict::Fail,
            format!("schema unreadable: {error}"),
            String::new(),
            "reopen the store with the current binary so it migrates",
        ),
    }
}

/// `targets-parse`: operator seeds still parse.
pub fn check_targets(toml: Option<&str>) -> Finding {
    match toml {
        None => finding(
            "targets-parse",
            Verdict::Skip,
            "no targets.toml configured".to_string(),
            String::new(),
            "",
        ),
        Some(text) => match discovery::parse_targets(text) {
            Ok(endpoints) => finding(
                "targets-parse",
                Verdict::Pass,
                format!("{} seed endpoint(s)", endpoints.len()),
                String::new(),
                "",
            ),
            Err(error) => finding(
                "targets-parse",
                Verdict::Fail,
                format!("targets.toml unparsable: {error}"),
                String::new(),
                "fix targets.toml [[target]] host/port entries",
            ),
        },
    }
}

/// `tier-posture`: every inventoried device has a recorded tier.
pub fn check_tier_posture(store: &dyn Store) -> Finding {
    let devices = match store.list_devices() {
        Ok(devices) => devices,
        Err(error) => {
            return finding(
                "tier-posture",
                Verdict::Fail,
                format!("device list unreadable: {error}"),
                String::new(),
                "see store-readable",
            );
        }
    };
    let mut untiered = Vec::new();
    for device in &devices {
        let (host, port) = match split_host_port(&device.id) {
            Some(pair) => pair,
            None => {
                untiered.push(device.id.clone());
                continue;
            }
        };
        match tier::recorded_tier_for(store, host, port) {
            Ok(Some(_)) => {}
            Ok(None) => untiered.push(device.id.clone()),
            Err(error) => {
                return finding(
                    "tier-posture",
                    Verdict::Fail,
                    format!("tier read failed: {error}"),
                    String::new(),
                    "see store-readable",
                );
            }
        }
    }
    if untiered.is_empty() {
        finding(
            "tier-posture",
            Verdict::Pass,
            format!("{} device(s) carry a recorded tier", devices.len()),
            String::new(),
            "",
        )
    } else {
        finding(
            "tier-posture",
            Verdict::Warn,
            format!("{} device(s) lack a recorded tier", untiered.len()),
            format!("untiered={untiered:?}"),
            "run info once per device so its tier is recorded",
        )
    }
}

/// `device-info`: each inventoried device answers `query/device-info`.
pub async fn check_device_info<T: Transport>(store: &dyn Store, transport: &T) -> Finding {
    let devices = match store.list_devices() {
        Ok(devices) => devices,
        Err(error) => {
            return finding(
                "device-info",
                Verdict::Fail,
                format!("device list unreadable: {error}"),
                String::new(),
                "see store-readable",
            );
        }
    };
    if devices.is_empty() {
        return finding(
            "device-info",
            Verdict::Skip,
            "no inventoried devices to probe".to_string(),
            String::new(),
            "",
        );
    }
    let mut down = Vec::new();
    let mut slow = Vec::new();
    for device in &devices {
        let (host, port) = match split_host_port(&device.id) {
            Some(pair) => pair,
            None => {
                down.push(device.id.clone());
                continue;
            }
        };
        let started = Instant::now();
        let cmd = zestroku_proto::query("device-info");
        match transport.send(host, port, &cmd).await {
            Ok(response) if response.status == 200 => {
                if started.elapsed().as_secs() >= 2 {
                    slow.push(device.id.clone());
                }
            }
            Ok(response) => down.push(format!("{}:http{}", device.id, response.status)),
            Err(error) => down.push(format!("{}:{error}", device.id)),
        }
    }
    if !down.is_empty() {
        finding(
            "device-info",
            Verdict::Fail,
            format!("{} device(s) unreachable", down.len()),
            format!("down={down:?}"),
            "check the device is on the LAN and its ECP port answers",
        )
    } else if !slow.is_empty() {
        finding(
            "device-info",
            Verdict::Warn,
            format!("{} device(s) answer slowly", slow.len()),
            format!("slow={slow:?}"),
            "investigate LAN latency before relying on live probes",
        )
    } else {
        finding(
            "device-info",
            Verdict::Pass,
            format!("{} device(s) answer device-info", devices.len()),
            String::new(),
            "",
        )
    }
}

/// `ssdp-responder`: an injected reply datagram parses as `roku:ecp`.
pub fn check_ssdp_reply(datagram: Option<&[u8]>) -> Finding {
    match datagram {
        None => finding(
            "ssdp-responder",
            Verdict::Skip,
            "no SSDP reply captured".to_string(),
            String::new(),
            "",
        ),
        Some(bytes) => match discovery::parse_ssdp_response(bytes) {
            Ok(endpoint) => finding(
                "ssdp-responder",
                Verdict::Pass,
                format!("reply advertises {}:{}", endpoint.host, endpoint.port),
                String::new(),
                "",
            ),
            Err(error) => finding(
                "ssdp-responder",
                Verdict::Fail,
                format!("reply refused: {error}"),
                String::new(),
                "check the responder answers M-SEARCH with ST roku:ecp and a LOCATION",
            ),
        },
    }
}

/// `suspend-vs-absent`: inventoried devices missing from the last sweep warn.
pub fn check_suspend(store: &dyn Store, discovered: &[&str]) -> Finding {
    if discovered.is_empty() {
        return finding(
            "suspend-vs-absent",
            Verdict::Skip,
            "no sweep results supplied".to_string(),
            String::new(),
            "",
        );
    }
    let devices = match store.list_devices() {
        Ok(devices) => devices,
        Err(error) => {
            return finding(
                "suspend-vs-absent",
                Verdict::Fail,
                format!("device list unreadable: {error}"),
                String::new(),
                "see store-readable",
            );
        }
    };
    let missing: Vec<String> = devices
        .iter()
        .map(|row| row.id.clone())
        .filter(|id| !discovered.contains(&id.as_str()))
        .collect();
    if missing.is_empty() {
        finding(
            "suspend-vs-absent",
            Verdict::Pass,
            "every inventoried device was swept".to_string(),
            String::new(),
            "",
        )
    } else {
        finding(
            "suspend-vs-absent",
            Verdict::Warn,
            format!("{} inventoried device(s) absent from sweep", missing.len()),
            format!("missing={missing:?}"),
            "suspended (powered off) or moved: re-sweep before pruning",
        )
    }
}

/// `lan-policy`: classify one failed direct connect with the platform control.
pub fn check_lan_policy(
    host: &str,
    port: u16,
    connect: fn(&str, u16) -> ConnectOutcome,
    platform_ok: fn(&str) -> bool,
) -> Finding {
    match connect(host, port) {
        ConnectOutcome::Ok => finding(
            "lan-policy",
            Verdict::Skip,
            format!("{host}:{port} connects; no policy question"),
            String::new(),
            "",
        ),
        ConnectOutcome::Refused => finding(
            "lan-policy",
            Verdict::Skip,
            format!("{host}:{port} refused: neither policy nor unreachable"),
            String::new(),
            "",
        ),
        ConnectOutcome::HostUnreachable | ConnectOutcome::Timeout => {
            if platform_ok(host) {
                finding(
                    "lan-policy",
                    Verdict::Fail,
                    format!("{host}:{port} unreachable to zestroku but reachable to the OS"),
                    "control=platform-tool-ok".to_string(),
                    "run through the resident daemon (agents) or an ssh localhost hop; \
                     the launching process tree's Local Network permission is the suspected gate",
                )
            } else {
                finding(
                    "lan-policy",
                    Verdict::Fail,
                    format!("{host}:{port} unreachable to both zestroku and the OS"),
                    "control=platform-tool-failed".to_string(),
                    "device down or off-LAN: power, network, and address first",
                )
            }
        }
    }
}

/// Debug rendering of every read the checks may observe.
pub fn store_fingerprint(store: &dyn Store) -> Result<String> {
    let mut parts = vec![format!("versions={:?}", store.schema_versions())];
    let mut devices = store.list_devices()?;
    devices.sort_by(|a, b| a.id.cmp(&b.id));
    for device in &devices {
        parts.push(format!("device={device:?}"));
        let mut apps = store.list_apps(&device.id)?;
        apps.sort_by(|a, b| a.app_id.cmp(&b.app_id));
        parts.push(format!("apps={apps:?}"));
    }
    let mut aliases = store.list_aliases()?;
    aliases.sort_by(|a, b| a.alias.cmp(&b.alias));
    parts.push(format!("aliases={aliases:?}"));
    parts.push(format!("actions={:?}", store.list_actions()?));
    parts.push(format!("diffs={:?}", store.list_diffs()?));
    Ok(parts.join("\n"))
}

/// Run every built-in check plus extras, refusing to return when the store moved.
pub async fn run_checked<T: Transport>(
    ctx: &Ctx<T>,
    extra: &[ExtraCheck<T>],
) -> std::result::Result<Vec<Finding>, ReadOnlyViolation> {
    let before = store_fingerprint(ctx.store).map_err(|error| ReadOnlyViolation {
        before: format!("unreadable: {error}"),
        after: String::new(),
    })?;
    let mut findings = vec![
        check_store(ctx.store, ctx.store_path),
        check_migrations(ctx.store),
        check_targets(ctx.targets_toml),
        check_tier_posture(ctx.store),
        check_device_info(ctx.store, ctx.transport).await,
        check_ssdp_reply(ctx.ssdp_reply),
        check_suspend(ctx.store, ctx.discovered_ids),
    ];
    for device in ctx
        .store
        .list_devices()
        .unwrap_or_default()
        .iter()
        .filter_map(|row| split_host_port(&row.id))
    {
        findings.push(check_lan_policy(
            device.0,
            device.1,
            ctx.connect,
            ctx.platform_ok,
        ));
    }
    for check in extra {
        findings.push(check(ctx));
    }
    let after = store_fingerprint(ctx.store).map_err(|error| ReadOnlyViolation {
        before: before.clone(),
        after: format!("unreadable: {error}"),
    })?;
    if before != after {
        return Err(ReadOnlyViolation { before, after });
    }
    Ok(findings)
}

/// Default port helper for tests and callers.
pub fn default_port() -> u16 {
    ECP_DEFAULT_PORT
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::ECP_DEFAULT_PORT;
    use crate::store::{AccessTierRow, AppRow, DeviceRow, SqliteStore};
    use zestroku_proto::{Command, EcpResponse};

    struct FixtureTransport {
        bodies: std::collections::BTreeMap<(String, u16), (u16, String)>,
    }

    impl Transport for FixtureTransport {
        fn send(
            &self,
            host: &str,
            port: u16,
            _cmd: &Command,
        ) -> impl Future<Output = Result<EcpResponse>> + Send {
            let out = self
                .bodies
                .get(&(host.to_owned(), port))
                .cloned()
                .unwrap_or((404, String::new()));
            async move {
                Ok(EcpResponse {
                    status: out.0,
                    headers: Vec::new(),
                    body: out.1,
                })
            }
        }
    }

    fn device_row(id: &str) -> DeviceRow {
        DeviceRow {
            id: id.to_string(),
            name: "Fixture".to_string(),
            ip: id.split(':').next().unwrap_or("").to_string(),
            model: "Fixture".to_string(),
            model_num: "F1".to_string(),
            firmware: "0".to_string(),
            wifi_driver: "fixture".to_string(),
            access_tier: "permissive".to_string(),
            last_seen: 1_700_000_000,
        }
    }

    fn runtime() -> asupersync::runtime::Runtime {
        let reactor = asupersync::runtime::reactor::create_reactor().expect("reactor");
        asupersync::runtime::RuntimeBuilder::current_thread()
            .with_reactor(reactor)
            .build()
            .expect("runtime")
    }

    fn seeded(path: &std::path::Path) -> SqliteStore {
        let store = SqliteStore::open(path).expect("open store");
        store
            .upsert_device(&device_row("192.0.2.10:8060"))
            .expect("seed device");
        store
            .upsert_access_tier(&AccessTierRow {
                device_id: "192.0.2.10:8060".to_string(),
                tier: "permissive".to_string(),
                probe_at: 1_700_000_000,
                probe_map: "{}".to_string(),
            })
            .expect("seed tier");
        store
            .upsert_app(&AppRow {
                device_id: "192.0.2.10:8060".to_string(),
                app_id: "1001".to_string(),
                name: "Streaming One".to_string(),
                version: "1.0".to_string(),
                fetched_at: 1_700_000_000,
            })
            .expect("seed app");
        store
    }

    fn refuse_connect(_host: &str, _port: u16) -> ConnectOutcome {
        ConnectOutcome::HostUnreachable
    }

    fn platform_down(_host: &str) -> bool {
        false
    }

    fn ctx_for<'a>(
        store: &'a SqliteStore,
        path: &'a Path,
        transport: &'a FixtureTransport,
    ) -> Ctx<'a, FixtureTransport> {
        Ctx {
            store,
            store_path: path,
            targets_toml: Some("[[target]]\nhost = \"192.0.2.10\"\n"),
            transport,
            ssdp_reply: Some(
                b"HTTP/1.1 200 OK\r\nLOCATION: http://192.0.2.10:8060/\r\nST: roku:ecp\r\nUSN: uuid:roku:ecp:fx\r\n\r\n",
            ),
            discovered_ids: &["192.0.2.10:8060"],
            connect: refuse_connect,
            platform_ok: platform_down,
        }
    }

    fn runtime() -> asupersync::Runtime {
        asupersync::Runtime::new().expect("test runtime")
    }

    #[test]
    fn registry_passes_on_a_healthy_fixture() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("doctor.db");
        let store = seeded(&db);
        let transport = FixtureTransport {
            bodies: std::collections::BTreeMap::from([(
                ("192.0.2.10".to_string(), ECP_DEFAULT_PORT),
                (
                    200,
                    "<device-info><model-name>Fixture</model-name></device-info>".to_string(),
                ),
            )]),
        };
        let ctx = ctx_for(&store, &db, &transport);
        let findings = runtime()
            .block_on(run_checked(&ctx, &[]))
            .expect("read-only run");
        assert!(
            findings.iter().all(|f| f.verdict != Verdict::Fail),
            "{findings:?}"
        );
        assert!(
            findings
                .iter()
                .any(|f| f.id == "device-info" && f.verdict == Verdict::Pass),
            "{findings:?}"
        );
    }

    #[test]
    fn mutating_check_is_refused_by_the_read_only_gate() {
        fn writer(ctx: &Ctx<FixtureTransport>) -> Finding {
            ctx.store
                .insert_action(&crate::store::ActionRow {
                    id: 0,
                    at: 1_700_000_000,
                    client: "doctor-probe".to_string(),
                    surface: "test".to_string(),
                    intent: "mutate".to_string(),
                    decision: "allowed".to_string(),
                    result: "done".to_string(),
                    before_state: "{}".to_string(),
                })
                .expect("write succeeds; the gate must catch it");
            finding(
                "mutating-probe",
                Verdict::Pass,
                String::new(),
                String::new(),
                "",
            )
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("doctor.db");
        let store = seeded(&db);
        let transport = FixtureTransport {
            bodies: std::collections::BTreeMap::new(),
        };
        let ctx = ctx_for(&store, &db, &transport);
        let err = runtime()
            .block_on(run_checked(&ctx, &[writer]))
            .expect_err("mutating check must be refused");
        assert!(err.after.contains("doctor-probe"), "{err:?}");
    }

    #[test]
    fn unreachable_device_fails_device_info() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("doctor.db");
        let store = seeded(&db);
        let transport = FixtureTransport {
            bodies: std::collections::BTreeMap::new(),
        };
        let finding = runtime().block_on(check_device_info(&store, &transport));
        assert_eq!(finding.verdict, Verdict::Fail);
        assert!(finding.detail.contains("unreachable"), "{finding:?}");
    }

    #[test]
    fn bad_targets_fail_and_missing_targets_skip() {
        assert_eq!(check_targets(Some("[[target]\n")).verdict, Verdict::Fail);
        assert_eq!(check_targets(None).verdict, Verdict::Skip);
        assert_eq!(
            check_targets(Some("[[target]]\nhost = \"192.0.2.10\"\n")).verdict,
            Verdict::Pass
        );
    }

    #[test]
    fn missing_tier_warns() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("doctor.db");
        let store = SqliteStore::open(&db).expect("open store");
        store
            .upsert_device(&device_row("192.0.2.10:8060"))
            .expect("seed device");
        let finding = check_tier_posture(&store);
        assert_eq!(finding.verdict, Verdict::Warn);
        assert!(finding.evidence.contains("192.0.2.10:8060"), "{finding:?}");
    }

    #[test]
    fn absent_device_warns_suspend() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("doctor.db");
        let store = seeded(&db);
        let finding = check_suspend(&store, &["192.0.2.99:8060"]);
        assert_eq!(finding.verdict, Verdict::Warn);
        assert!(finding.remedy.contains("suspended"), "{finding:?}");
    }

    #[test]
    fn lan_policy_classifies_with_the_platform_control() {
        fn unreachable(_host: &str, _port: u16) -> ConnectOutcome {
            ConnectOutcome::HostUnreachable
        }
        fn platform_up(_host: &str) -> bool {
            true
        }
        let policy = check_lan_policy("192.0.2.10", 8060, unreachable, platform_up);
        assert_eq!(policy.verdict, Verdict::Fail);
        assert!(policy.detail.contains("reachable to the OS"), "{policy:?}");
        let down = check_lan_policy("192.0.2.10", 8060, unreachable, platform_down);
        assert!(down.detail.contains("both zestroku and the OS"), "{down:?}");
        // Planted negative: without the platform control both cases read as
        // policy; ECONNREFUSED is neither.
        assert_ne!(policy.detail, down.detail);
        fn refused(_host: &str, _port: u16) -> ConnectOutcome {
            ConnectOutcome::Refused
        }
        assert_eq!(
            check_lan_policy("192.0.2.10", 8060, refused, platform_up).verdict,
            Verdict::Skip
        );
    }
}
