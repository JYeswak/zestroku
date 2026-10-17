//! Shared real-process mutation fixture; never touches a live device or approval store.

use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use zestroku_core::store::{AccessTierRow, DeviceRow, SqliteStore, Store};

pub(super) struct HttpApiDaemon(Option<Child>);

impl Drop for HttpApiDaemon {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl HttpApiDaemon {
    pub(super) fn stop(mut self) -> Output {
        let mut child = self.0.take().expect("daemon child");
        let _ = child.kill();
        child.wait_with_output().expect("collect daemon output")
    }
}

pub(super) fn http_free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("reserve HTTP port")
        .local_addr()
        .expect("HTTP port address")
        .port()
}

pub(super) fn start_http_daemon(db: &Path, port: u16) -> HttpApiDaemon {
    let child = Command::new(env!("CARGO_BIN_EXE_zestroku"))
        .env("ZESTROKU_DB", db)
        .args(["serve", "--bind", &format!("127.0.0.1:{port}")])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start real daemon");
    let daemon = HttpApiDaemon(Some(child));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return daemon;
        }
        assert!(
            Instant::now() < deadline,
            "serve did not bind 127.0.0.1:{port}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

pub(super) fn seed_http_device(db: &Path, port: u16) -> String {
    let device_id = zestroku_core::tier::device_id("127.0.0.1", port);
    let store = SqliteStore::open(db).expect("open seed store");
    store
        .upsert_device(&DeviceRow {
            id: device_id.clone(),
            name: "Sim Express".into(),
            ip: "127.0.0.1".into(),
            model: "Sim Express".into(),
            model_num: "SIMX1".into(),
            firmware: "15.2.4".into(),
            wifi_driver: "loopback".into(),
            access_tier: "permissive".into(),
            last_seen: 1,
        })
        .expect("seed inventory device");
    store
        .upsert_access_tier(&AccessTierRow {
            device_id: device_id.clone(),
            tier: "permissive".into(),
            probe_at: 1,
            probe_map: r#"{"query/apps":"open"}"#.into(),
        })
        .expect("seed recorded tier");
    device_id
}

pub(super) fn mint_http_approval(
    db: &Path,
    port: u16,
    command: &zestroku_proto::Command,
) -> String {
    use zestroku_core::mutation::issue_approval;
    use zestroku_types::{AccessTier, Device};

    let store = SqliteStore::open(db).expect("open store to mint HTTP approval");
    issue_approval(
        &store,
        &Device {
            host: "127.0.0.1".into(),
            port,
            model_name: "Sim Express".into(),
            model_number: "SIMX1".into(),
            software_version: "15.2.4".into(),
            is_tv: false,
            access_tier: AccessTier::Permissive,
        },
        command,
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_secs() as i64,
    )
    .expect("mint one-use HTTP approval")
}
