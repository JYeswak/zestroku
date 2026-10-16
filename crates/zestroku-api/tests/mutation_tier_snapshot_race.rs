#![forbid(unsafe_code)]

#[path = "common/mod.rs"]
mod common;
use common::*;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use fastapi::TestClient;
use serde_json::Value;
use zestroku_api::build_app;
use zestroku_core::store::{AccessTierRow, Store};
use zestroku_proto::{Command, EcpResponse, Method as EcpMethod, Transport, launch};
use zestroku_types::{AccessTier, Device, Error, Result};

const DEVICE_INFO_XML: &str = "<device-info><model-name>Roku Fixture</model-name><model-number>FIXTURE1</model-number><software-version>15.2.4</software-version><is-tv>false</is-tv></device-info>";
const LIMITED_403_BODY: &str = "ECP command not allowed in Limited mode.";

struct SnapshotBarrierTransport {
    calls: Mutex<Vec<(String, u16, Command)>>,
    block_next_snapshot: AtomicBool,
    snapshot_started: mpsc::Sender<()>,
    release_snapshot: Mutex<mpsc::Receiver<()>>,
}

impl SnapshotBarrierTransport {
    fn new() -> (Self, mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (snapshot_started, started) = mpsc::channel();
        let (release, release_snapshot) = mpsc::channel();
        (
            Self {
                calls: Mutex::new(Vec::new()),
                block_next_snapshot: AtomicBool::new(true),
                snapshot_started,
                release_snapshot: Mutex::new(release_snapshot),
            },
            started,
            release,
        )
    }

    fn calls(&self) -> Vec<(String, u16, Command)> {
        self.calls.lock().expect("calls lock").clone()
    }
}

impl Transport for SnapshotBarrierTransport {
    async fn send(&self, host: &str, port: u16, command: &Command) -> Result<EcpResponse> {
        self.calls
            .lock()
            .expect("calls lock")
            .push((host.to_owned(), port, command.clone()));

        if command.method == EcpMethod::Get
            && command.path == "/query/active-app"
            && self.block_next_snapshot.swap(false, Ordering::SeqCst)
        {
            self.snapshot_started
                .send(())
                .map_err(|_| Error::Unreachable("snapshot observer disconnected".into()))?;
            self.release_snapshot
                .lock()
                .expect("snapshot release lock")
                .recv()
                .map_err(|_| Error::Unreachable("snapshot release disconnected".into()))?;
        }

        match (command.method, command.path.as_str()) {
            (EcpMethod::Get, "/query/active-app") => {
                Ok(response(200, "<active-app><app>Roku</app></active-app>"))
            }
            (EcpMethod::Get, "/query/device-info") => Ok(response(200, DEVICE_INFO_XML)),
            (EcpMethod::Get, "/query/apps") => Ok(response(403, LIMITED_403_BODY)),
            (EcpMethod::Get, "/query/tv-active-channel") => Ok(response(404, "")),
            (EcpMethod::Post, "/launch/1001") => Ok(response(200, "")),
            _ => Err(Error::Unreachable(
                format!("unexpected fixture command {}", command.path).into(),
            )),
        }
    }
}

#[test]
fn concurrent_info_downgrade_during_snapshot_denies_without_consuming_approval() {
    let (_directory, store) = store_with_device(HOST, PORT, "permissive");
    let device = Device {
        host: HOST.into(),
        port: PORT,
        model_name: "Roku Fixture".into(),
        model_number: "FIXTURE1".into(),
        software_version: "15.2.4".into(),
        is_tv: false,
        access_tier: AccessTier::Permissive,
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock")
        .as_secs() as i64;
    let token = {
        let store_guard = store.lock().expect("store lock");
        zestroku_core::mutation::issue_approval(&*store_guard, &device, &launch(1001), now)
            .expect("mint launch token")
    };

    let (transport, snapshot_started, release_snapshot) = SnapshotBarrierTransport::new();
    let transport = Arc::new(transport);
    let mutation_path = format!("/v1/devices/{HOST}:{PORT}/launch");
    let info_path = format!("/v1/devices/{HOST}:{PORT}/info");
    let mutation_store = Arc::clone(&store);
    let mutation_transport = Arc::clone(&transport);
    let thread_path = mutation_path.clone();
    let thread_token = token.clone();
    let mutation = std::thread::spawn(move || {
        let client = TestClient::new(build_app(mutation_store, mutation_transport));
        let response = client
            .post(&thread_path)
            .header_str("host", HOST)
            .json(&serde_json::json!({"app_id": 1001, "confirm": thread_token}))
            .send();
        (response.status().as_u16(), response.text().to_owned())
    });

    snapshot_started
        .recv()
        .expect("mutation reached blocked active-app snapshot");
    let client = TestClient::new(build_app(Arc::clone(&store), Arc::clone(&transport)));
    let info = client.get(&info_path).header_str("host", HOST).send();
    let info_status = info.status().as_u16();
    let info_body = info.text().to_owned();
    release_snapshot
        .send(())
        .expect("release mutation snapshot");
    let (denied_status, denied_body) = mutation.join().expect("join mutation request");

    assert_eq!(info_status, 200);
    let info_json: Value = serde_json::from_str(&info_body).expect("info JSON");
    assert_eq!(info_json["access_tier"], "limited");
    assert_eq!(denied_status, 403);
    let denied_json: Value = serde_json::from_str(&denied_body).expect("denial JSON");
    assert_eq!(denied_json["error"], "tier_refused");

    let before_retry = transport.calls();
    assert!(
        before_retry
            .iter()
            .all(|(_, _, command)| command.method == EcpMethod::Get),
        "the denied request must not dispatch a mutation POST"
    );
    assert!(
        before_retry.iter().any(|(_, _, command)| {
            command.method == EcpMethod::Get && command.path == "/query/device-info"
        }),
        "the concurrent info request must reach the device-info query"
    );
    assert!(
        before_retry
            .iter()
            .any(|(_, _, command)| command.path == "/query/apps"),
        "the concurrent info request must observe the tier discriminator"
    );

    {
        let store_guard = store.lock().expect("store lock");
        let device_id = zestroku_core::tier::device_id(HOST, PORT);
        let mut row = store_guard
            .get_device(&device_id)
            .expect("lookup device")
            .expect("seeded device");
        row.access_tier = "permissive".into();
        store_guard
            .upsert_device(&row)
            .expect("restore inventory tier");
        store_guard
            .upsert_access_tier(&AccessTierRow {
                device_id,
                tier: "permissive".into(),
                probe_at: now + 1,
                probe_map: r#"{"query/apps":"open"}"#.into(),
            })
            .expect("restore recorded tier");
    }

    let approved = client
        .post(&mutation_path)
        .header_str("host", HOST)
        .json(&serde_json::json!({"app_id": 1001, "confirm": &token}))
        .send();
    assert_eq!(approved.status().as_u16(), 200);
    let approved_json: Value = serde_json::from_str(approved.text()).expect("approval JSON");
    assert_eq!(approved_json["outcome"], "completed");

    let replay = client
        .post(&mutation_path)
        .header_str("host", HOST)
        .json(&serde_json::json!({"app_id": 1001, "confirm": &token}))
        .send();
    assert_eq!(replay.status().as_u16(), 403);
    assert_eq!(error_code(&replay), "confirmation_required");

    let posts: Vec<String> = transport
        .calls()
        .into_iter()
        .filter(|(_, _, command)| command.method == EcpMethod::Post)
        .map(|(_, _, command)| command.path)
        .collect();
    assert_eq!(posts, ["/launch/1001"]);
}
