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

use crate::discovery;
use crate::store::migrations::MIGRATIONS;
use crate::store::{
    AccessTierRow, ActionRow, AliasRow, AppRow, ConfirmationTokenRow, DeviceRow, DiffRow, Store,
    StoreError,
};
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

/// Read-only view of a [`Store`]: every read delegates, every write is
/// refused. Doctor checks only ever observe this view, so a check cannot
/// mutate authorization posture no matter what it attempts — concurrent
/// daemon activity is never misattributed to doctor.
pub struct ReadOnly<'a>(pub &'a dyn Store);

fn denied<T>(op: &str) -> Result<T, StoreError> {
    Err(StoreError::Backend(format!(
        "doctor is read-only: {op} refused"
    )))
}

impl Store for ReadOnly<'_> {
    fn schema_versions(&self) -> Result<Vec<i64>, StoreError> {
        self.0.schema_versions()
    }
    fn upsert_device(&self, _d: &DeviceRow) -> Result<(), StoreError> {
        denied("upsert_device")
    }
    fn get_device(&self, id: &str) -> Result<Option<DeviceRow>, StoreError> {
        self.0.get_device(id)
    }
    fn list_devices(&self) -> Result<Vec<DeviceRow>, StoreError> {
        self.0.list_devices()
    }
    fn delete_device(&self, _id: &str) -> Result<(), StoreError> {
        denied("delete_device")
    }
    fn upsert_app(&self, _a: &AppRow) -> Result<(), StoreError> {
        denied("upsert_app")
    }
    fn list_apps(&self, device_id: &str) -> Result<Vec<AppRow>, StoreError> {
        self.0.list_apps(device_id)
    }
    fn delete_app(&self, _device_id: &str, _app_id: &str) -> Result<(), StoreError> {
        denied("delete_app")
    }
    fn add_capability(&self, _device_id: &str, _cap: &str) -> Result<(), StoreError> {
        denied("add_capability")
    }
    fn list_capabilities(&self, device_id: &str) -> Result<Vec<String>, StoreError> {
        self.0.list_capabilities(device_id)
    }
    fn remove_capability(&self, _device_id: &str, _cap: &str) -> Result<(), StoreError> {
        denied("remove_capability")
    }
    fn set_alias(&self, _alias: &AliasRow) -> Result<(), StoreError> {
        denied("set_alias")
    }
    fn list_aliases(&self) -> Result<Vec<AliasRow>, StoreError> {
        self.0.list_aliases()
    }
    fn delete_alias(&self, _alias: &str) -> Result<(), StoreError> {
        denied("delete_alias")
    }
    fn record_device_identity(&self, _device_id: &str, _identity: &str) -> Result<(), StoreError> {
        denied("record_device_identity")
    }
    fn find_device_id_by_identity(&self, identity: &str) -> Result<Option<String>, StoreError> {
        self.0.find_device_id_by_identity(identity)
    }
    fn delete_device_identity(&self, _device_id: &str) -> Result<(), StoreError> {
        denied("delete_device_identity")
    }
    fn rekey_aliases(&self, _old_id: &str, _new_id: &str) -> Result<(), StoreError> {
        denied("rekey_aliases")
    }
    fn move_device_identity(
        &self,
        _old_id: &str,
        _new_device: &DeviceRow,
        _identity: &str,
    ) -> Result<(), StoreError> {
        denied("move_device_identity")
    }
    fn insert_confirmation_token(&self, _token: &ConfirmationTokenRow) -> Result<(), StoreError> {
        denied("insert_confirmation_token")
    }
    fn consume_confirmation_token(
        &self,
        _token_hash: &str,
        _device_id: &str,
        _action: &str,
        _now: i64,
    ) -> Result<bool, StoreError> {
        denied("consume_confirmation_token")
    }
    fn upsert_access_tier(&self, _t: &AccessTierRow) -> Result<(), StoreError> {
        denied("upsert_access_tier")
    }
    fn get_access_tier(&self, device_id: &str) -> Result<Option<AccessTierRow>, StoreError> {
        self.0.get_access_tier(device_id)
    }
    fn delete_access_tier(&self, _device_id: &str) -> Result<(), StoreError> {
        denied("delete_access_tier")
    }
    fn insert_action(&self, _a: &ActionRow) -> Result<i64, StoreError> {
        denied("insert_action")
    }
    fn get_action(&self, id: i64) -> Result<Option<ActionRow>, StoreError> {
        self.0.get_action(id)
    }
    fn list_actions(&self) -> Result<Vec<ActionRow>, StoreError> {
        self.0.list_actions()
    }
    fn update_action_result(&self, _id: i64, _result: &str) -> Result<(), StoreError> {
        denied("update_action_result")
    }
    fn delete_action(&self, _id: i64) -> Result<(), StoreError> {
        denied("delete_action")
    }
    fn insert_diff(&self, _d: &DiffRow) -> Result<i64, StoreError> {
        denied("insert_diff")
    }
    fn list_diffs(&self) -> Result<Vec<DiffRow>, StoreError> {
        self.0.list_diffs()
    }
    fn delete_diff(&self, _id: i64) -> Result<(), StoreError> {
        denied("delete_diff")
    }
    fn try_record_policy_hit(
        &self,
        _client: &str,
        _verb: &str,
        _since: i64,
        _budget: i64,
        _now: i64,
    ) -> Result<bool, StoreError> {
        denied("try_record_policy_hit")
    }
    fn peek_confirmation_token_expiry(&self, token_hash: &str) -> Result<Option<i64>, StoreError> {
        self.0.peek_confirmation_token_expiry(token_hash)
    }
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
    /// Device ids (`host:port`) seen by the last sweep. `None` means no
    /// sweep ran (skip); `Some` — even empty — is a completed sweep whose
    /// absences are evidence.
    pub discovered_ids: Option<&'a [&'a str]>,
    /// Direct-connect probe, per host:port.
    pub connect: fn(&str, u16) -> ConnectOutcome,
    /// Platform-tool control: does an OS-native tool reach the host?
    pub platform_ok: fn(&str) -> bool,
}

/// Extra checks run after the built-ins (test seam for the read-only gate).
/// Higher-ranked: any borrowing lifetime unifies at the call site.
pub type ExtraCheck<T> = for<'x> fn(&'x Ctx<'x, T>) -> Finding;

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

/// `tier-posture`: every inventoried device has a recorded tier that still
/// matches a live re-probe of the probe map. Recorded-but-unprobed,
/// untiered, and mismatched devices warn; only full agreement passes.
pub async fn check_tier_posture<T: Transport>(store: &dyn Store, transport: &T) -> Finding {
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
    let mut stale = Vec::new();
    let mut mismatch = Vec::new();
    for device in &devices {
        let (host, port) = match split_host_port(&device.id) {
            Some(pair) => pair,
            None => {
                untiered.push(device.id.clone());
                continue;
            }
        };
        let recorded = match tier::recorded_tier_for(store, host, port) {
            Ok(recorded) => recorded,
            Err(error) => {
                return finding(
                    "tier-posture",
                    Verdict::Fail,
                    format!("tier read failed: {error}"),
                    String::new(),
                    "see store-readable",
                );
            }
        };
        // Tier discrimination lives on query/apps: introspection routes
        // stay tier-blind by design and cannot discriminate.
        let live = {
            let cmd = zestroku_proto::query("apps");
            match transport.send(host, port, &cmd).await {
                Ok(response) => Some(tier::classify_tier_from_response(&response)),
                Err(_) => None,
            }
        };
        match (recorded, live) {
            (Some(expected), Some(actual)) if expected == actual => {}
            (Some(expected), Some(actual)) => mismatch.push(format!(
                "{}: recorded {} but live {}",
                device.id,
                expected.as_str(),
                actual.as_str()
            )),
            (Some(_), None) => stale.push(device.id.clone()),
            (None, _) => untiered.push(device.id.clone()),
        }
    }
    let mut problems = Vec::new();
    if !untiered.is_empty() {
        problems.push(format!("{} untiered", untiered.len()));
    }
    if !stale.is_empty() {
        problems.push(format!("{} unprobed", stale.len()));
    }
    if !mismatch.is_empty() {
        problems.push(format!("{} drifted", mismatch.len()));
    }
    if problems.is_empty() {
        finding(
            "tier-posture",
            Verdict::Pass,
            format!("{} device(s) agree with a live probe", devices.len()),
            String::new(),
            "",
        )
    } else {
        finding(
            "tier-posture",
            Verdict::Warn,
            format!("tier posture: {}", problems.join(", ")),
            format!("untiered={untiered:?} stale={stale:?} mismatch={mismatch:?}"),
            "run info once per untiered device; re-run info where live and recorded disagree",
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

/// `policy-file`: the sibling `policy.toml` of the store validates through
/// the canonical [`Policy`](crate::policy::Policy) loader. Missing means
/// documented defaults (Pass); any syntactic or semantic rejection is Fail.
/// Read-only: the file is read, never written.
pub fn check_policy_file(store_path: &Path) -> Finding {
    let path = crate::policy::path_for_db(store_path);
    let present = path.exists();
    match crate::policy::Policy::load(&path) {
        Ok(_) if !present => finding(
            "policy-file",
            Verdict::Pass,
            "no policy.toml: documented defaults apply".to_string(),
            format!("path={}", path.display()),
            "",
        ),
        Ok(_) => finding(
            "policy-file",
            Verdict::Pass,
            "policy.toml parses and validates".to_string(),
            format!("path={}", path.display()),
            "",
        ),
        Err(error) => finding(
            "policy-file",
            Verdict::Fail,
            format!("policy.toml rejected: {error}"),
            format!("path={}", path.display()),
            "fix policy.toml tables, keys, and ranges",
        ),
    }
}

/// `suspend-vs-absent`: inventoried devices missing from a completed sweep
/// warn. `None` means no sweep ran (skip); `Some`, even empty, is a
/// completed sweep whose absences are evidence — so every device
/// disappearing at once still warns instead of vanishing into a skip.
pub fn check_suspend(store: &dyn Store, discovered: Option<&[&str]>) -> Finding {
    let discovered = match discovered {
        Some(ids) => ids,
        None => {
            return finding(
                "suspend-vs-absent",
                Verdict::Skip,
                "no sweep ran".to_string(),
                String::new(),
                "",
            );
        }
    };
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

/// Run every built-in check plus extras through the [`ReadOnly`] view: no
/// check can mutate, so concurrent daemon activity is never misattributed
/// to doctor. When the store itself is unreadable, the root finding carries
/// the failure and store-dependent checks Skip (prerequisites skip, not fail).
pub async fn run_checked<T: Transport>(ctx: &Ctx<'_, T>, extra: &[ExtraCheck<T>]) -> Vec<Finding> {
    let view = ReadOnly(ctx.store);
    let store = &view as &dyn Store;
    let inner = Ctx {
        store,
        store_path: ctx.store_path,
        targets_toml: ctx.targets_toml,
        transport: ctx.transport,
        ssdp_reply: ctx.ssdp_reply,
        discovered_ids: ctx.discovered_ids,
        connect: ctx.connect,
        platform_ok: ctx.platform_ok,
    };
    let store_finding = check_store(inner.store, inner.store_path);
    let store_ok = store_finding.verdict != Verdict::Fail;
    let mut findings = vec![
        store_finding,
        check_targets(inner.targets_toml),
        check_ssdp_reply(inner.ssdp_reply),
        check_policy_file(inner.store_path),
    ];
    if store_ok {
        findings.push(check_migrations(inner.store));
        findings.push(check_tier_posture(inner.store, inner.transport).await);
        findings.push(check_device_info(inner.store, inner.transport).await);
        findings.push(check_suspend(inner.store, inner.discovered_ids));
        for device in inner
            .store
            .list_devices()
            .unwrap_or_default()
            .iter()
            .filter_map(|row| split_host_port(&row.id))
        {
            findings.push(check_lan_policy(
                device.0,
                device.1,
                inner.connect,
                inner.platform_ok,
            ));
        }
    } else {
        for id in [
            "migrations-current",
            "tier-posture",
            "device-info",
            "suspend-vs-absent",
            "lan-policy",
        ] {
            findings.push(finding(
                id,
                Verdict::Skip,
                "prerequisite store-readable failed".to_string(),
                String::new(),
                "see store-readable",
            ));
        }
    }
    for check in extra {
        findings.push(check(&inner));
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::ECP_DEFAULT_PORT;
    use crate::store::{AccessTierRow, AppRow, DeviceRow, SqliteStore};
    use zestroku_proto::{Command, EcpResponse};
    use zestroku_types::Result;

    struct FixtureTransport {
        bodies: std::collections::BTreeMap<(String, u16), (u16, String)>,
        apps: std::collections::BTreeMap<(String, u16), (u16, String)>,
    }

    impl Transport for FixtureTransport {
        fn send(
            &self,
            host: &str,
            port: u16,
            cmd: &Command,
        ) -> impl Future<Output = Result<EcpResponse>> + Send {
            // Tier discrimination lives on query/apps; introspection routes
            // stay tier-blind, so the fixture answers per command path.
            let table = if cmd.path.contains("/query/apps") {
                &self.apps
            } else {
                &self.bodies
            };
            let out = table
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

    fn connect_ok(_host: &str, _port: u16) -> ConnectOutcome {
        ConnectOutcome::Ok
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
            discovered_ids: Some(&["192.0.2.10:8060"]),
            connect: connect_ok,
            platform_ok: platform_down,
        }
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
            apps: std::collections::BTreeMap::from([(
                ("192.0.2.10".to_string(), ECP_DEFAULT_PORT),
                (200, "<apps><app id=\"1001\" /></apps>".to_string()),
            )]),
        };
        let ctx = ctx_for(&store, &db, &transport);
        let findings = runtime().block_on(run_checked(&ctx, &[]));
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
    fn mutating_check_is_denied_by_the_read_only_view() {
        // Enforcement by construction: the write never reaches the store,
        // so no byte comparison is needed and concurrent daemon activity
        // can never be misattributed to doctor.
        fn writer(ctx: &Ctx<FixtureTransport>) -> Finding {
            assert!(
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
                    .is_err(),
                "doctor view must refuse writes"
            );
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
            apps: std::collections::BTreeMap::new(),
        };
        let ctx = ctx_for(&store, &db, &transport);
        let findings = runtime().block_on(run_checked(&ctx, &[writer]));
        assert!(
            findings.iter().any(|f| f.id == "mutating-probe"),
            "{findings:?}"
        );
    }

    #[test]
    fn every_write_method_is_denied() {
        // Planted-negative suite: if any write below starts delegating
        // instead of refusing, this test goes red.
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("doctor.db");
        let store = seeded(&db);
        let view = ReadOnly(&store);
        let denied = [
            view.upsert_device(&device_row("192.0.2.99:8060")).is_err(),
            view.delete_device("192.0.2.10:8060").is_err(),
            view.upsert_app(&crate::store::AppRow {
                device_id: "192.0.2.10:8060".to_string(),
                app_id: "9".to_string(),
                name: "X".to_string(),
                version: "1".to_string(),
                fetched_at: 1,
            })
            .is_err(),
            view.set_alias(&crate::store::AliasRow {
                alias: "x".to_string(),
                device_id: "192.0.2.10:8060".to_string(),
            })
            .is_err(),
            view.upsert_access_tier(&crate::store::AccessTierRow {
                device_id: "192.0.2.10:8060".to_string(),
                tier: "limited".to_string(),
                probe_at: 1,
                probe_map: "{}".to_string(),
            })
            .is_err(),
            view.insert_action(&crate::store::ActionRow {
                id: 0,
                at: 1,
                client: "c".to_string(),
                surface: "s".to_string(),
                intent: "i".to_string(),
                decision: "d".to_string(),
                result: "r".to_string(),
                before_state: "{}".to_string(),
            })
            .is_err(),
            view.try_record_policy_hit("c", "v", 1, 1, 2).is_err(),
            view.consume_confirmation_token("h", "d", "a", 1).is_err(),
        ];
        assert!(denied.iter().all(|&d| d), "{denied:?}");
        // Reads still delegate.
        assert_eq!(view.list_devices().expect("read").len(), 1);
    }

    #[test]
    fn unreachable_device_fails_device_info() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("doctor.db");
        let store = seeded(&db);
        let transport = FixtureTransport {
            bodies: std::collections::BTreeMap::new(),
            apps: std::collections::BTreeMap::new(),
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
        let transport = FixtureTransport {
            bodies: std::collections::BTreeMap::new(),
            apps: std::collections::BTreeMap::new(),
        };
        let finding = runtime().block_on(check_tier_posture(&store, &transport));
        assert_eq!(finding.verdict, Verdict::Warn);
        assert!(finding.evidence.contains("192.0.2.10:8060"), "{finding:?}");
    }

    #[test]
    fn drifted_tier_warns_with_recorded_and_live() {
        // Recorded permissive but live apps answers exact Limited-403
        // while introspection stays 200 (tier-blind): mismatch names both.
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
            apps: std::collections::BTreeMap::from([(
                ("192.0.2.10".to_string(), ECP_DEFAULT_PORT),
                (403, "ECP command not allowed in Limited mode.".to_string()),
            )]),
        };
        let finding = runtime().block_on(check_tier_posture(&store, &transport));
        assert_eq!(finding.verdict, Verdict::Warn);
        assert!(finding.detail.contains("drifted"), "{finding:?}");
        assert!(finding.evidence.contains("permissive"), "{finding:?}");
        assert!(finding.evidence.contains("limited"), "{finding:?}");
    }

    #[test]
    fn absent_device_warns_suspend() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("doctor.db");
        let store = seeded(&db);
        let finding = check_suspend(&store, Some(&["192.0.2.99:8060"]));
        assert_eq!(finding.verdict, Verdict::Warn);
        assert!(finding.remedy.contains("suspended"), "{finding:?}");
    }

    #[test]
    fn completed_empty_sweep_warns_unrun_sweep_skips() {
        // A completed sweep with zero responders still reports every
        // inventoried device missing; only an unrun sweep skips.
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("doctor.db");
        let store = seeded(&db);
        let empty = check_suspend(&store, Some(&[]));
        assert_eq!(empty.verdict, Verdict::Warn);
        assert!(empty.evidence.contains("192.0.2.10:8060"), "{empty:?}");
        let unrun = check_suspend(&store, None);
        assert_eq!(unrun.verdict, Verdict::Skip);
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
    #[test]
    fn policy_file_missing_means_defaults_invalid_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("doctor.db");
        let missing = check_policy_file(&db);
        assert_eq!(missing.verdict, Verdict::Pass);
        assert!(missing.detail.contains("defaults"), "{missing:?}");
        std::fs::write(dir.path().join("policy.toml"), "[[[broken\n")
            .expect("write invalid policy");
        let invalid = check_policy_file(&db);
        assert_eq!(invalid.verdict, Verdict::Fail);
        // Syntactically valid but semantically rejected values fail too.
        std::fs::write(dir.path().join("policy.toml"), "keypress_per_sec = -1\n")
            .expect("write semantic-invalid policy");
        let semantic = check_policy_file(&db);
        assert_eq!(semantic.verdict, Verdict::Fail);
        assert!(semantic.detail.contains("keypress_per_sec"), "{semantic:?}");
        std::fs::write(
            dir.path().join("policy.toml"),
            "[quiet_hours]\nenabled = true\n",
        )
        .expect("write valid policy");
        assert_eq!(check_policy_file(&db).verdict, Verdict::Pass);
    }

    #[test]
    fn policy_hit_gate_is_atomic_and_peek_preserves_token() {
        use crate::store::ConfirmationTokenRow;
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("doctor.db");
        let store = SqliteStore::open(&db).expect("open store");
        // Budget of 2: first two record, third refuses without inserting.
        assert!(
            store
                .try_record_policy_hit("cli", "key", 1_700_000_000, 2, 1_700_000_100)
                .expect("record 1")
        );
        assert!(
            store
                .try_record_policy_hit("cli", "key", 1_700_000_000, 2, 1_700_000_101)
                .expect("record 2")
        );
        assert!(
            !store
                .try_record_policy_hit("cli", "key", 1_700_000_000, 2, 1_700_000_102)
                .expect("record 3 refuses")
        );
        // Other verbs/clients keep their own budget.
        assert!(
            store
                .try_record_policy_hit("cli", "launch", 1_700_000_000, 2, 1_700_000_102)
                .expect("other verb")
        );
        // Peek observes expiry without consuming.
        store
            .insert_confirmation_token(&ConfirmationTokenRow {
                token_hash: "abc".to_string(),
                device_id: "192.0.2.10:8060".to_string(),
                action: "key".to_string(),
                expires_at: 1_700_000_400,
            })
            .expect("mint token");
        assert_eq!(
            store.peek_confirmation_token_expiry("abc").expect("peek"),
            Some(1_700_000_400)
        );
        assert_eq!(
            store
                .peek_confirmation_token_expiry("missing")
                .expect("peek missing"),
            None
        );
        assert!(
            store
                .consume_confirmation_token("abc", "192.0.2.10:8060", "key", 1_700_000_100)
                .expect("consume still works")
        );
    }
    struct BrokenStore;

    macro_rules! broken_method {
        ($($name:ident($($arg:ident : $ty:ty),*) -> $ret:ty;)*) => {
            impl crate::store::Store for BrokenStore {
                $(fn $name(&self, $($arg: $ty),*) -> $ret {
                    $(let _ = &$arg;)*
                    Err(crate::store::StoreError::Backend("broken fixture".into()))
                })*
            }
        };
    }

    broken_method! {
        schema_versions() -> Result<Vec<i64>, crate::store::StoreError>;
        upsert_device(d: &crate::store::DeviceRow) -> Result<(), crate::store::StoreError>;
        get_device(id: &str) -> Result<Option<crate::store::DeviceRow>, crate::store::StoreError>;
        list_devices() -> Result<Vec<crate::store::DeviceRow>, crate::store::StoreError>;
        delete_device(id: &str) -> Result<(), crate::store::StoreError>;
        upsert_app(a: &crate::store::AppRow) -> Result<(), crate::store::StoreError>;
        list_apps(device_id: &str) -> Result<Vec<crate::store::AppRow>, crate::store::StoreError>;
        delete_app(device_id: &str, app_id: &str) -> Result<(), crate::store::StoreError>;
        add_capability(device_id: &str, cap: &str) -> Result<(), crate::store::StoreError>;
        list_capabilities(device_id: &str) -> Result<Vec<String>, crate::store::StoreError>;
        remove_capability(device_id: &str, cap: &str) -> Result<(), crate::store::StoreError>;
        set_alias(alias: &crate::store::AliasRow) -> Result<(), crate::store::StoreError>;
        list_aliases() -> Result<Vec<crate::store::AliasRow>, crate::store::StoreError>;
        delete_alias(alias: &str) -> Result<(), crate::store::StoreError>;
        record_device_identity(device_id: &str, identity: &str) -> Result<(), crate::store::StoreError>;
        find_device_id_by_identity(identity: &str) -> Result<Option<String>, crate::store::StoreError>;
        delete_device_identity(device_id: &str) -> Result<(), crate::store::StoreError>;
        rekey_aliases(old_id: &str, new_id: &str) -> Result<(), crate::store::StoreError>;
        move_device_identity(old_id: &str, new_device: &crate::store::DeviceRow, identity: &str) -> Result<(), crate::store::StoreError>;
        insert_confirmation_token(token: &crate::store::ConfirmationTokenRow) -> Result<(), crate::store::StoreError>;
        consume_confirmation_token(token_hash: &str, device_id: &str, action: &str, now: i64) -> Result<bool, crate::store::StoreError>;
        upsert_access_tier(t: &crate::store::AccessTierRow) -> Result<(), crate::store::StoreError>;
        get_access_tier(device_id: &str) -> Result<Option<crate::store::AccessTierRow>, crate::store::StoreError>;
        delete_access_tier(device_id: &str) -> Result<(), crate::store::StoreError>;
        insert_action(a: &crate::store::ActionRow) -> Result<i64, crate::store::StoreError>;
        get_action(id: i64) -> Result<Option<crate::store::ActionRow>, crate::store::StoreError>;
        list_actions() -> Result<Vec<crate::store::ActionRow>, crate::store::StoreError>;
        update_action_result(id: i64, result: &str) -> Result<(), crate::store::StoreError>;
        delete_action(id: i64) -> Result<(), crate::store::StoreError>;
        insert_diff(d: &crate::store::DiffRow) -> Result<i64, crate::store::StoreError>;
        list_diffs() -> Result<Vec<crate::store::DiffRow>, crate::store::StoreError>;
        delete_diff(id: i64) -> Result<(), crate::store::StoreError>;
        try_record_policy_hit(client: &str, verb: &str, since: i64, budget: i64, now: i64) -> Result<bool, crate::store::StoreError>;
        peek_confirmation_token_expiry(token_hash: &str) -> Result<Option<i64>, crate::store::StoreError>;
    }

    #[test]
    fn broken_store_yields_root_fail_and_skipped_dependents() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("missing.db");
        let store = BrokenStore;
        let transport = FixtureTransport {
            bodies: std::collections::BTreeMap::new(),
            apps: std::collections::BTreeMap::new(),
        };
        let ctx = Ctx {
            store: &store,
            store_path: &db,
            targets_toml: None,
            transport: &transport,
            ssdp_reply: None,
            discovered_ids: None,
            connect: refuse_connect,
            platform_ok: platform_down,
        };
        let findings = runtime().block_on(run_checked(&ctx, &[]));
        let verdict_of = |id: &str| {
            findings
                .iter()
                .find(|finding| finding.id == id)
                .unwrap_or_else(|| panic!("missing finding {id}"))
                .verdict
        };
        assert_eq!(verdict_of("store-readable"), Verdict::Fail);
        for id in [
            "migrations-current",
            "tier-posture",
            "device-info",
            "suspend-vs-absent",
            "lan-policy",
        ] {
            assert_eq!(verdict_of(id), Verdict::Skip, "{id}");
        }
    }
}
