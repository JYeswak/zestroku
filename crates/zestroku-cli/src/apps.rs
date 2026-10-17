use std::process::ExitCode;

use asupersync::runtime::{RuntimeBuilder, reactor::create_reactor};
use zestroku_core::check_tier;
use zestroku_core::store::Store;
use zestroku_proto::{EcpGate, InstalledApp, parse_apps, parse_ecp_response, query_apps};
use zestroku_types::{AccessTier, Device, Error};

use crate::transport::{AsupersyncTransport, send_selected};

#[derive(Debug)]
struct AppsOpts {
    host: String,
    json: bool,
    mode: crate::ReadMode,
}

fn parse_opts(args: &[String]) -> std::result::Result<AppsOpts, String> {
    let mut host = None;
    let mut json = false;
    let mut direct = false;
    let mut daemon: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--json" => json = true,
            "--direct" => direct = true,
            "--daemon" => {
                index += 1;
                let Some(url) = args.get(index) else {
                    return Err("apps: --daemon needs a URL".into());
                };
                daemon = Some(url.clone());
            }
            value if value.starts_with('-') => return Err(format!("unknown option {value}")),
            value if host.is_none() => host = Some(value.to_string()),
            _ => return Err("apps accepts one <host>".to_string()),
        }
        index += 1;
    }
    let host = host.ok_or_else(|| "apps needs <host>".to_string())?;
    let mode = match (direct, daemon) {
        (true, Some(_)) => return Err("apps: --direct conflicts with --daemon".into()),
        (true, None) => crate::ReadMode::Direct,
        (false, Some(url)) => crate::ReadMode::Daemon(url),
        (false, None) => crate::ReadMode::Default,
    };
    Ok(AppsOpts { host, json, mode })
}

/// Refresh the installed-app cache after a successful fetch so shell
/// completion can offer app names offline. Failures are swallowed: a stale
/// cache must never break the read itself or pollute machine output.
fn refresh_app_cache(host: &str, port: u16, apps: &[InstalledApp]) {
    let Ok(store) = crate::store_db::open() else {
        return;
    };
    let device_id = zestroku_core::tier::device_id(host, port);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .min(i64::MAX as u64) as i64;
    let cached = match store.list_apps(&device_id) {
        Ok(cached) => cached,
        Err(_) => return,
    };
    for row in &cached {
        if !apps.iter().any(|app| app.id == row.app_id) {
            let _ = store.delete_app(&device_id, &row.app_id);
        }
    }
    for app in apps {
        let _ = store.upsert_app(&zestroku_core::store::AppRow {
            device_id: device_id.clone(),
            app_id: app.id.clone(),
            name: app.name.clone(),
            version: app.version.clone().unwrap_or_default(),
            fetched_at: now,
        });
    }
}

enum TierLookupError {
    Store(String),
    NotObserved,
}

fn access_tier_for(host: &str, port: u16) -> std::result::Result<AccessTier, TierLookupError> {
    match std::env::var("ZESTROKU_TIER").as_deref() {
        Ok("permissive") | Ok("Permissive") => return Ok(AccessTier::Permissive),
        Ok(other) if !other.trim().is_empty() && !other.eq_ignore_ascii_case("open") => {
            eprintln!("zestroku: ignoring unrecognized ZESTROKU_TIER={other:?}");
        }
        _ => {}
    }

    let store =
        crate::store_db::open().map_err(|error| TierLookupError::Store(error.to_string()))?;
    let recorded = zestroku_core::tier::recorded_tier_for(&store, host, port)
        .map_err(|error| TierLookupError::Store(error.to_string()))?;
    recorded
        .map(|tier| tier.access_tier())
        .ok_or(TierLookupError::NotObserved)
}

pub fn run(args: &[String]) -> ExitCode {
    let opts = match parse_opts(args) {
        Ok(opts) => opts,
        Err(error) => {
            eprintln!(
                "zestroku apps: {error}; usage: zestroku apps <host> [--json] [--daemon <url>] [--direct]"
            );
            return ExitCode::from(2);
        }
    };
    let (host, port) = crate::split_host_port(&opts.host);
    let direct_env = std::env::var("ZESTROKU_DIRECT").as_deref() == Ok("1");
    let direct = opts.mode == crate::ReadMode::Direct
        || (opts.mode == crate::ReadMode::Default && direct_env);
    if !direct {
        return execute_daemon(&host, port, opts.json, &opts.mode);
    }
    let access_tier = match access_tier_for(&host, port) {
        Ok(access_tier) => access_tier,
        Err(TierLookupError::Store(error)) => {
            eprintln!("zestroku: cannot read recorded tier: {error}");
            return ExitCode::FAILURE;
        }
        Err(TierLookupError::NotObserved) => {
            eprintln!(
                "zestroku: no recorded tier for {} — run `zestroku info {}` first (or set ZESTROKU_TIER=permissive explicitly)",
                host, host
            );
            return ExitCode::from(3);
        }
    };
    execute_direct(&host, port, access_tier, opts.json)
}

fn execute_daemon(host: &str, port: u16, json: bool, mode: &crate::ReadMode) -> ExitCode {
    let target = zestroku_core::tier::device_id(host, port);
    let fetched = match mode {
        crate::ReadMode::Daemon(url) => crate::daemon_client::apps_via(&target, url),
        _ => crate::daemon_client::apps(&target),
    };
    match fetched {
        Ok(apps) => {
            refresh_app_cache(host, port, &apps);
            print_apps(apps, json)
        }
        Err(error) => {
            eprintln!("zestroku: [{}] {error}", error.code());
            if matches!(
                error,
                crate::daemon_client::ReadError::DaemonRequestFailed { status: 403, .. }
            ) {
                ExitCode::from(3)
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

fn execute_direct(host: &str, port: u16, access_tier: AccessTier, json: bool) -> ExitCode {
    let device = Device {
        host: host.to_string(),
        port,
        model_name: String::new(),
        model_number: String::new(),
        software_version: String::new(),
        is_tv: false,
        access_tier,
    };
    let command = query_apps();
    if let Err(error) = check_tier(&command, &device) {
        eprintln!("zestroku: {error}");
        return ExitCode::from(3);
    }

    let runtime = match create_reactor()
        .map_err(|error| error.to_string())
        .and_then(|reactor| {
            RuntimeBuilder::current_thread()
                .with_reactor(reactor)
                .build()
                .map_err(|error| error.to_string())
        }) {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("zestroku: could not start asupersync runtime: {error}");
            return ExitCode::FAILURE;
        }
    };

    let result = runtime.block_on(async {
        let response =
            send_selected(AsupersyncTransport, &device.host, device.port, &command).await?;
        let classification = parse_ecp_response(&response);
        if classification.gate == EcpGate::Limited403 {
            return parse_apps(&response.body);
        }
        if classification.gate != EcpGate::Open {
            return Err(Error::Unreachable(format!(
                "{} {} returned an unclassified response: {:?}",
                command.method, command.path, classification
            )));
        }
        parse_apps(&response.body)
    });

    match result {
        Ok(apps) => {
            refresh_app_cache(host, port, &apps);
            print_apps(apps, json)
        }
        Err(error) => {
            eprintln!("zestroku: {error}");
            if matches!(error, Error::TierRefused { .. }) {
                ExitCode::from(3)
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

fn print_apps(apps: Vec<InstalledApp>, json: bool) -> ExitCode {
    match render_apps(apps, json) {
        Ok(output) => {
            println!("{output}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("zestroku: could not encode apps as JSON: {error}");
            ExitCode::FAILURE
        }
    }
}

fn render_apps(
    mut apps: Vec<InstalledApp>,
    json: bool,
) -> std::result::Result<String, serde_json::Error> {
    apps.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.id.cmp(&right.id))
    });
    if json {
        let document = serde_json::json!({
            "apps": apps.iter().map(|app| serde_json::json!({
                "id": app.id,
                "name": app.name,
                "type": app.app_type,
                "subtype": app.subtype,
                "version": app.version,
            })).collect::<Vec<_>>(),
        });
        serde_json::to_string_pretty(&document)
    } else {
        Ok(apps
            .iter()
            .map(|app| {
                format!(
                    "{}\t{}\ttype={}\tsubtype={}\tversion={}",
                    app.id,
                    app.name,
                    app.app_type,
                    app.subtype.as_deref().unwrap_or("-"),
                    app.version.as_deref().unwrap_or("-"),
                )
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{
        Arc, Mutex,
        mpsc::{self, RecvTimeoutError},
    };
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    struct LoopbackEcp {
        port: u16,
        request_log: Arc<Mutex<Vec<String>>>,
        stop: mpsc::Sender<()>,
        thread: Option<JoinHandle<()>>,
    }

    impl LoopbackEcp {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback ECP");
            listener
                .set_nonblocking(true)
                .expect("nonblocking listener");
            let port = listener.local_addr().expect("loopback address").port();
            let request_log = Arc::new(Mutex::new(Vec::new()));
            let thread_log = Arc::clone(&request_log);
            let (stop, stop_rx) = mpsc::channel();
            let thread = thread::spawn(move || {
                loop {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
                            let mut request = [0_u8; 2048];
                            let read = stream.read(&mut request).unwrap_or(0);
                            thread_log
                                .lock()
                                .expect("request log lock")
                                .push(String::from_utf8_lossy(&request[..read]).into_owned());
                            let body = b"<apps/>";
                            let _ = write!(
                                stream,
                                "HTTP/1.1 200 OK\r\nContent-Type: text/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            );
                            let _ = stream.write_all(body);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            match stop_rx.recv_timeout(Duration::from_millis(10)) {
                                Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                                Err(RecvTimeoutError::Timeout) => {}
                            }
                        }
                        Err(_) => break,
                    }
                }
            });
            Self {
                port,
                request_log,
                stop,
                thread: Some(thread),
            }
        }
    }

    impl Drop for LoopbackEcp {
        fn drop(&mut self) {
            let _ = self.stop.send(());
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    #[test]
    fn parse_opts_accepts_host_and_json() {
        let args = vec!["roku.local".to_string(), "--json".to_string()];
        let opts = parse_opts(&args).expect("valid apps arguments");
        assert_eq!(opts.host, "roku.local");
        assert!(opts.json);
    }

    #[test]
    fn limited_tier_refuses_before_any_loopback_request() {
        let server = LoopbackEcp::start();
        let code = execute_direct("127.0.0.1", server.port, AccessTier::Open, true);
        assert_eq!(code, ExitCode::from(3));
        assert!(
            server
                .request_log
                .lock()
                .expect("request log lock")
                .is_empty()
        );
    }

    #[test]
    fn parse_opts_accepts_explicit_mode_flags() {
        let args = vec![
            "roku.local".to_string(),
            "--daemon".to_string(),
            "http://127.0.0.1:9".to_string(),
        ];
        let opts = parse_opts(&args).expect("daemon flag");
        assert_eq!(
            opts.mode,
            crate::ReadMode::Daemon("http://127.0.0.1:9".into())
        );
        let args = vec!["roku.local".to_string(), "--direct".to_string()];
        let opts = parse_opts(&args).expect("direct flag");
        assert_eq!(opts.mode, crate::ReadMode::Direct);
        let args = vec![
            "roku.local".to_string(),
            "--direct".to_string(),
            "--daemon".to_string(),
            "http://127.0.0.1:9".to_string(),
        ];
        parse_opts(&args).expect_err("conflicting flags must fail");
    }

    #[test]
    fn json_golden_is_sorted_and_preserves_all_app_fields() {
        let rendered = render_apps(
            vec![
                InstalledApp {
                    id: "z9".to_string(),
                    name: "Zulu".to_string(),
                    app_type: "appl".to_string(),
                    subtype: Some("svod".to_string()),
                    version: Some("2.1".to_string()),
                },
                InstalledApp {
                    id: "a1".to_string(),
                    name: "Alpha".to_string(),
                    app_type: "tvin".to_string(),
                    subtype: None,
                    version: None,
                },
            ],
            true,
        )
        .expect("serialize apps JSON");
        let actual: serde_json::Value = serde_json::from_str(&rendered).expect("valid JSON");
        let expected = serde_json::json!({
            "apps": [
                {
                    "id": "a1",
                    "name": "Alpha",
                    "type": "tvin",
                    "subtype": null,
                    "version": null,
                },
                {
                    "id": "z9",
                    "name": "Zulu",
                    "type": "appl",
                    "subtype": "svod",
                    "version": "2.1",
                },
            ]
        });
        assert_eq!(actual, expected);
    }
}
