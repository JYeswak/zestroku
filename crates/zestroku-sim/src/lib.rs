//! zestroku-sim — virtual Roku ECP devices on real loopback sockets, so e2e
//! tests need no LAN (bead zestroku-m7f-sim-9bt, slice 1).
//!
//! One [`SimDevice`] is one `Http1Listener` on `127.0.0.1:0` serving the route
//! table in `docs/SIM-DESIGN.md` §2. Wire facts come from the scrubbed ECP wire
//! doc and fixtures; anything the wire doc does not specify is marked `FILL`
//! in a comment and must be sampled from a live device before it is asserted
//! as fact.
//!
//! The Limited tier is a *wrapper*, not a filter: every route that answers 403
//! under [`SimTier::Limited`] answers 200 or 404 (never 403) under
//! [`SimTier::Permissive`].
#![forbid(unsafe_code)]

use std::io;
use std::net::SocketAddr;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use asupersync::http::h1::server::HostPolicy;
use asupersync::http::h1::types::{Method, Request, Response};
use asupersync::http::h1::{Http1Config, Http1Listener, Http1ListenerConfig};
use asupersync::runtime::{Runtime, RuntimeBuilder, reactor::create_reactor};
use asupersync::server::ShutdownSignal;

/// Firmware string used in the `Server` header and `software-version`.
pub const FIRMWARE: &str = "15.2.4";

/// The byte-exact Limited-mode 403 body (40 bytes), from the committed
/// `ecp_fixture_limited_response.txt`.
pub const LIMITED_403_BODY: &str = "ECP command not allowed in Limited mode.";

/// Device class fingerprint: `tv-active-channel` exists only on [`DeviceKind::Tv`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Express,
    Tv,
}

/// ECP access tier of the virtual device (live-switchable).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimTier {
    Limited,
    Permissive,
}

/// Configuration for [`SimDevice::start`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SimConfig {
    pub kind: DeviceKind,
    pub tier: SimTier,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Home,
    InApp,
}

struct App {
    id: &'static str,
    kind: &'static str,
    subtype: Option<&'static str>,
    name: &'static str,
}

/// Installed list, same shape as `ecp_fixture_permissive_apps.xml`.
const APPS: &[App] = &[
    App {
        id: "1001",
        kind: "appl",
        subtype: Some("svod"),
        name: "Streaming One",
    },
    App {
        id: "1002",
        kind: "appl",
        subtype: Some("svod"),
        name: "Streaming Two",
    },
    App {
        id: "1003",
        kind: "appl",
        subtype: None,
        name: "Example Channel",
    },
    App {
        id: "tvinput.hdmi1",
        kind: "tvin",
        subtype: None,
        name: "HDMI Input One",
    },
    App {
        id: "tvinput.dtv",
        kind: "tvin",
        subtype: None,
        name: "Live TV Input",
    },
];

struct DeviceState {
    kind: DeviceKind,
    tier: SimTier,
    /// Per-device serial placeholder (FIXTURESERIAL-class, never a live
    /// value). Lets tests give two same-model sims different identities.
    serial: String,
    active: Option<&'static App>,
    malformed_apps: bool,
    focus: Focus,
    launch_count: usize,
    requests: Vec<(String, String)>,
}

struct Shared {
    state: Mutex<DeviceState>,
}

impl Shared {
    /// The critical sections never panic while holding the lock; if a handler
    /// thread ever did, the state is still a valid `DeviceState`, so recover
    /// the guard instead of propagating the poison.
    fn lock(&self) -> MutexGuard<'_, DeviceState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

struct Reply {
    status: u16,
    reason: &'static str,
    content_type: &'static str,
    body: Vec<u8>,
}

impl Reply {
    fn xml(body: String) -> Self {
        // FILL: the live Content-Type of non-403 ECP responses is not in the
        // wire doc; `text/xml; charset="utf-8"` is the conventional ECP value.
        Self {
            status: 200,
            reason: "OK",
            content_type: "text/xml; charset=\"utf-8\"",
            body: body.into_bytes(),
        }
    }

    fn ok_empty() -> Self {
        Self {
            status: 200,
            reason: "OK",
            content_type: "text/plain; charset=\"utf-8\"",
            body: Vec::new(),
        }
    }

    fn not_found() -> Self {
        Self {
            status: 404,
            reason: "Not Found",
            content_type: "text/plain; charset=\"utf-8\"",
            body: Vec::new(),
        }
    }

    fn limited_403() -> Self {
        Self {
            status: 403,
            reason: "Forbidden",
            content_type: "text/plain; charset=\"utf-8\"",
            body: LIMITED_403_BODY.as_bytes().to_vec(),
        }
    }

    fn into_response(self) -> Response {
        let server = format!("Roku/{FIRMWARE} UPnP/1.0 Roku/{FIRMWARE}");
        let length = self.body.len().to_string();
        let mut response =
            Response::new(self.status, self.reason, self.body).with_header("Server", server);
        // Header order and set follow the fixture for the 403:
        // Server, Content-Length, Cache-Control, Content-Type.
        response = response.with_header("Content-Length", length);
        if self.status == 403 {
            response = response.with_header("Cache-Control", "no-cache");
        }
        response.with_header("Content-Type", self.content_type)
    }
}

fn device_info_xml(kind: DeviceKind, serial: &str) -> String {
    let (name, number, is_tv) = match kind {
        DeviceKind::Express => ("Sim Express", "SIMX1", "false"),
        DeviceKind::Tv => ("Sim TV", "SIMT1", "true"),
    };
    // Identifier fields are FIXTURESERIAL-class placeholders, never live values.
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<device-info>\n\
<udn>FIXTUREUDN</udn>\n<serial-number>{serial}</serial-number>\n\
<device-id>FIXTUREDEVICEID</device-id>\n<model-name>{name}</model-name>\n\
<model-number>{number}</model-number>\n<software-version>{FIRMWARE}</software-version>\n\
<is-tv>{is_tv}</is-tv>\n</device-info>\n"
    )
}

fn active_app_xml(active: Option<&App>) -> String {
    match active {
        // Home screen short form (parse_active_app doc: `<app>Roku</app>`).
        None => "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<active-app>\n<app>Roku</app>\n</active-app>\n".to_string(),
        Some(app) => {
            let subtype = app.subtype.map(|s| format!(" subtype=\"{s}\"")).unwrap_or_default();
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<active-app>\n<app id=\"{}\"{subtype} type=\"{}\">{}</app>\n</active-app>\n",
                app.id, app.kind, app.name
            )
        }
    }
}

fn apps_xml() -> String {
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<apps>\n");
    for app in APPS {
        let subtype = app
            .subtype
            .map(|s| format!(" subtype=\"{s}\""))
            .unwrap_or_default();
        out.push_str(&format!(
            "<app id=\"{}\"{subtype} type=\"{}\">{}</app>\n",
            app.id, app.kind, app.name
        ));
    }
    out.push_str("</apps>\n");
    out
}

/// Pure route table (docs/SIM-DESIGN.md §2). Mutations happen only on the
/// Permissive POST arms.
fn route(state: &mut DeviceState, method: &Method, uri: &str) -> Reply {
    let path = uri.split('?').next().unwrap_or(uri);
    let limited = state.tier == SimTier::Limited;
    match (method, path) {
        (Method::Get, "/query/device-info") => {
            Reply::xml(device_info_xml(state.kind, &state.serial))
        }
        (Method::Get, "/query/active-app") => Reply::xml(active_app_xml(state.active)),
        (Method::Get, "/query/apps") => {
            if limited {
                Reply::limited_403()
            } else if state.malformed_apps {
                Reply::xml("<apps><app".to_string())
            } else {
                Reply::xml(apps_xml())
            }
        }
        (Method::Get, "/query/media-player") => {
            if limited {
                Reply::limited_403()
            } else {
                // FILL: the live 232 B (Express) / 131 B (TV) media-player XML
                // has not been sampled; this placeholder is NOT wire fact.
                Reply::xml("<player state=\"close\"/>".to_string())
            }
        }
        (Method::Get, "/query/tv-active-channel") => match (state.kind, limited) {
            // Surface fingerprint: absent on the stick in both tiers.
            (DeviceKind::Express, _) => Reply::not_found(),
            (DeviceKind::Tv, true) => Reply::limited_403(),
            // FILL: the live 80 B tv-active-channel XML has not been sampled.
            (DeviceKind::Tv, false) => Reply::xml("<tv-channel/>".to_string()),
        },
        (Method::Get, p) if p.starts_with("/query/icon/") => {
            // Limited must not leak installed-vs-absent: 403 for every id.
            if limited {
                Reply::limited_403()
            } else if APPS
                .iter()
                .any(|app| Some(app.id) == p.strip_prefix("/query/icon/"))
            {
                // FILL: real icon bytes/content-type unsampled; placeholder body.
                Reply {
                    status: 200,
                    reason: "OK",
                    content_type: "image/png",
                    body: b"FILL-icon-placeholder".to_vec(),
                }
            } else {
                Reply::not_found()
            }
        }
        // Absent on both tiers (wire doc §2).
        (Method::Get, "/query/rid" | "/query/dev-mode") => Reply::not_found(),
        // GET on mutation verbs never works: method routing 404 in both tiers.
        (Method::Get, p) if p.starts_with("/keypress/") || p.starts_with("/launch/") => {
            Reply::not_found()
        }
        (Method::Post, p) if p.starts_with("/keypress/") => {
            if limited {
                return Reply::limited_403();
            }
            // FILL: error codes for unknown/invalid keys are unsampled; every
            // key is accepted with 200 here.
            match p.strip_prefix("/keypress/") {
                Some("Home") => {
                    state.active = None;
                    state.focus = Focus::Home;
                }
                // Minimal machine: Back from an app (stack depth 1) returns Home.
                Some("Back") if state.focus == Focus::InApp => {
                    state.active = None;
                    state.focus = Focus::Home;
                }
                _ => {}
            }
            Reply::ok_empty()
        }
        (Method::Post, p) if p.starts_with("/launch/") => {
            if limited {
                return Reply::limited_403();
            }
            let id = p.strip_prefix("/launch/").unwrap_or_default();
            match APPS.iter().find(|app| app.id == id) {
                Some(app) => {
                    state.active = Some(app);
                    state.focus = Focus::InApp;
                    state.launch_count += 1;
                    Reply::ok_empty()
                }
                None => Reply::not_found(),
            }
        }
        _ => Reply::not_found(),
    }
}
fn installer_route(method: &Method, headers: &[(String, String)], body: &[u8]) -> Option<Response> {
    if method != &Method::Post {
        return None;
    }
    let has_digest = headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("authorization") && value.starts_with("Digest ")
    });
    if !has_digest {
        return Some(
            Response::new(401, "Unauthorized", Vec::new())
                .with_header("WWW-Authenticate", "Digest realm=\"zestroku-sim\", nonce=\"sim-nonce\", qop=\"auth\", algorithm=MD5")
                .with_header("Content-Length", "0"),
        );
    }
    let valid = [
        b"name=\"mysubmit\"".as_slice(),
        b"Install",
        b"name=\"archive\"",
        b"name=\"passwd\"",
    ]
    .iter()
    .all(|needle| body.windows(needle.len()).any(|window| window == *needle));
    if valid {
        Some(Response::new(200, "OK", b"<html>Installed</html>".to_vec()))
    } else {
        Some(Response::new(400, "Bad Request", Vec::new()))
    }
}

fn runtime() -> io::Result<Runtime> {
    let reactor = create_reactor()?;
    RuntimeBuilder::current_thread()
        .with_reactor(reactor)
        .build()
        .map_err(|error| io::Error::other(format!("runtime: {error}")))
}

struct Server {
    shutdown: ShutdownSignal,
    thread: JoinHandle<()>,
}

impl Server {
    fn spawn(shared: Arc<Shared>, bind: String) -> io::Result<(Self, SocketAddr)> {
        let (ready_tx, ready_rx) = mpsc::channel::<io::Result<(SocketAddr, ShutdownSignal)>>();
        let thread = thread::spawn(move || {
            let runtime = match runtime() {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                    return;
                }
            };
            let handle = runtime.handle();
            // FND-DEPS hazard: the default host policy rejects loopback Host
            // headers that carry a port; allow the literal loopback host.
            let config = Http1ListenerConfig::default().http_config(
                Http1Config::default()
                    .host_policy(HostPolicy::allow_list(vec!["127.0.0.1".into()])),
            );
            runtime.block_on(async move {
                let handler = move |request: Request| {
                    let shared = Arc::clone(&shared);
                    async move {
                        let mut state = shared.lock();
                        state
                            .requests
                            .push((request.method.to_string(), request.uri.clone()));
                        if request.uri.split('?').next() == Some("/plugin_install") {
                            installer_route(&request.method, &request.headers, &request.body)
                                .unwrap_or_else(|| Response::new(404, "Not Found", Vec::new()))
                        } else {
                            route(&mut state, &request.method, &request.uri).into_response()
                        }
                    }
                };
                let listener = match Http1Listener::bind_with_config(bind, handler, config).await {
                    Ok(listener) => listener,
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                        return;
                    }
                };
                let local = match listener.local_addr() {
                    Ok(local) => local,
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                        return;
                    }
                };
                let _ = ready_tx.send(Ok((local, listener.shutdown_signal())));
                let _ = listener.run(&handle).await;
            });
        });
        match ready_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok((addr, shutdown))) => Ok((Self { shutdown, thread }, addr)),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            Err(error) => Err(io::Error::other(format!("sim server not ready: {error}"))),
        }
    }

    /// Stop accepting and wait until the listening socket is closed.
    fn stop(self) {
        self.shutdown.trigger_immediate();
        let _ = self.thread.join();
    }
}

/// A virtual Roku ECP device on `127.0.0.1:<ephemeral>`.
pub struct SimDevice {
    shared: Arc<Shared>,
    port: u16,
    server: Option<Server>,
}

impl SimDevice {
    /// Bind `127.0.0.1:0` and start serving.
    ///
    /// # Panics
    /// If the loopback socket cannot be bound; use [`SimDevice::try_start`]
    /// to handle that.
    pub fn start(config: SimConfig) -> SimDevice {
        Self::try_start(config).expect("bind loopback sim device")
    }

    /// Fallible form of [`SimDevice::start`].
    pub fn try_start(config: SimConfig) -> io::Result<SimDevice> {
        Self::try_start_with_serial(config, "FIXTURESERIAL")
    }

    /// Start serving with an explicit serial placeholder (FIXTURESERIAL-class,
    /// never a live value). Lets tests distinguish same-model devices.
    pub fn start_with_serial(config: SimConfig, serial: &str) -> SimDevice {
        Self::try_start_with_serial(config, serial).expect("bind loopback sim device")
    }

    /// Fallible form of [`SimDevice::start_with_serial`].
    pub fn try_start_with_serial(config: SimConfig, serial: &str) -> io::Result<SimDevice> {
        let shared = Arc::new(Shared {
            state: Mutex::new(DeviceState {
                kind: config.kind,
                tier: config.tier,
                serial: serial.to_owned(),
                active: None,
                focus: Focus::Home,
                malformed_apps: false,
                launch_count: 0,
                requests: Vec::new(),
            }),
        });
        let (server, addr) = Server::spawn(Arc::clone(&shared), "127.0.0.1:0".to_string())?;
        Ok(SimDevice {
            shared,
            port: addr.port(),
            server: Some(server),
        })
    }

    /// The bound loopback port (stable across suspend/resume).
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Flip the access tier at runtime (the M3 event); takes effect on the
    /// next request, on the same server.
    pub fn set_tier(&self, tier: SimTier) {
        self.shared.lock().tier = tier;
    }

    /// Return malformed installed-app XML for read-path error integration tests.
    pub fn set_malformed_apps(&self, malformed: bool) {
        self.shared.lock().malformed_apps = malformed;
    }

    /// Every request that reached the handler, in arrival order, as
    /// `(method, request-uri)`. Lets tests assert which POSTs arrived.
    pub fn received_requests(&self) -> Vec<(String, String)> {
        self.shared.lock().requests.clone()
    }

    /// Number of successful launches (Permissive POST /launch of an installed id).
    pub fn launch_count(&self) -> usize {
        self.shared.lock().launch_count
    }

    /// Fault: drop the listener so new connections are refused. Idempotent.
    pub fn suspend(&mut self) {
        if let Some(server) = self.server.take() {
            server.stop();
        }
    }

    /// Rebind the SAME port after [`SimDevice::suspend`]. Idempotent. Retries
    /// briefly in case the OS has not released the socket yet.
    ///
    /// # Panics
    /// If the port cannot be re-bound within five seconds.
    pub fn resume(&mut self) {
        if self.server.is_some() {
            return;
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match Server::spawn(Arc::clone(&self.shared), format!("127.0.0.1:{}", self.port)) {
                Ok((server, _)) => {
                    self.server = Some(server);
                    return;
                }
                Err(error) if Instant::now() < deadline => {
                    let _ = error;
                    thread::sleep(Duration::from_millis(50));
                }
                Err(error) => panic!("resume: rebind 127.0.0.1:{}: {error}", self.port),
            }
        }
    }
}

impl Drop for SimDevice {
    fn drop(&mut self) {
        self.suspend();
    }
}
