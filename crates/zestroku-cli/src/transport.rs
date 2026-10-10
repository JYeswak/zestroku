#![forbid(unsafe_code)]

use std::time::Duration;

use asupersync::Cx;
use asupersync::http::Client;
use asupersync::http::h1::types::Method;
use zestroku_proto::{Command, EcpResponse, Method as EcpMethod, Transport};
use zestroku_types::{Error, Result};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Default, Clone, Copy)]
pub struct AsupersyncTransport;

impl Transport for AsupersyncTransport {
    async fn send(&self, host: &str, port: u16, cmd: &Command) -> Result<EcpResponse> {
        let authority = if host.starts_with('[') {
            format!("{host}:{port}")
        } else if host.parse::<std::net::Ipv6Addr>().is_ok() {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };
        let url = format!("http://{authority}{}", cmd.path);
        let cx =
            Cx::current().ok_or_else(|| Error::Unreachable("missing asupersync context".into()))?;
        let method = match cmd.method {
            EcpMethod::Get => Method::Get,
            EcpMethod::Post => Method::Post,
        };
        let response = Client::default_for_runtime(&cx)
            .request_builder(method, url.clone())
            .timeout(REQUEST_TIMEOUT)
            .send(&cx)
            .await
            .map_err(|error| Error::Unreachable(format!("{url}: {error}")))?;
        Ok(EcpResponse {
            status: response.status,
            headers: response.headers,
            body: String::from_utf8_lossy(&response.body).into_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use asupersync::http::h1::server::HostPolicy;
    use asupersync::http::h1::types::{Request, Response};
    use asupersync::http::h1::{Http1Config, Http1Listener, Http1ListenerConfig};
    use asupersync::runtime::{Runtime, RuntimeBuilder, reactor::create_reactor};
    use std::sync::mpsc;
    use std::thread;

    fn runtime() -> Runtime {
        RuntimeBuilder::current_thread()
            .with_reactor(create_reactor().expect("reactor"))
            .build()
            .expect("runtime")
    }

    #[test]
    fn sends_get_and_post_and_parses_ecp_responses() {
        let server_config = Http1ListenerConfig::default().http_config(
            Http1Config::default().host_policy(HostPolicy::allow_list(vec!["127.0.0.1".into()])),
        );
        let (ready_tx, ready_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let runtime = runtime();
            let handle = runtime.handle();
            runtime.block_on(async move {
                let listener = Http1Listener::bind_with_config(
                    "127.0.0.1:0",
                    |request: Request| async move {
                        let response = match (
                            request.uri.as_str(),
                            request.method.to_string().as_str(),
                        ) {
                            ("/query/device-info", "GET") => Response::new(
                                200,
                                "OK",
                                concat!(
                                    "<device-info><model-name>Test Roku</model-name>",
                                    "<model-number>TEST1</model-number><software-version>1.2</software-version>",
                                    "<is-tv>false</is-tv></device-info>"
                                )
                                .as_bytes()
                                .to_vec(),
                            ),
                            ("/query/active-app", "GET") => Response::new(
                                200,
                                "OK",
                                b"<active-app><app id=\"42\">Test App</app></active-app>".to_vec(),
                            ),
                            ("/keypress/Home", "POST") => Response::new(
                                403,
                                "Forbidden",
                                b"ECP command not allowed in Limited mode.".to_vec(),
                            ),
                            ("/keypress/Home", _) => {
                                Response::new(405, "Method Not Allowed", Vec::new())
                            }
                            _ => Response::new(404, "Not Found", Vec::new()),
                        };
                        response.with_header("Content-Type", "application/xml")
                    },
                    server_config,
                )
                .await
                .expect("bind loopback ECP server");
                ready_tx
                    .send((
                        listener.local_addr().expect("listener address"),
                        listener.shutdown_signal(),
                    ))
                    .expect("send listener readiness");
                listener.run(&handle).await.expect("run loopback server");
            });
        });
        let (address, shutdown) = ready_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("loopback server ready");
        let runtime = runtime();
        let transport = AsupersyncTransport;
        let device_info = runtime
            .block_on(transport.send(
                "127.0.0.1",
                address.port(),
                &zestroku_proto::query("device-info"),
            ))
            .expect("device-info request");
        let active_app = runtime
            .block_on(transport.send(
                "127.0.0.1",
                address.port(),
                &zestroku_proto::query("active-app"),
            ))
            .expect("active-app request");
        let post_response = runtime
            .block_on(transport.send(
                "127.0.0.1",
                address.port(),
                &zestroku_proto::keypress_raw("Home"),
            ))
            .expect("keypress request");
        shutdown.trigger_immediate();
        server.join().expect("server thread");

        assert_eq!(device_info.status, 200);
        assert!(device_info.headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("content-type") && value == "application/xml"
        }));
        assert!(active_app.headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("content-type") && value == "application/xml"
        }));
        let parsed_info =
            zestroku_proto::parse_device_info(&device_info.body).expect("device-info fixture");
        assert_eq!(parsed_info.model_name, "Test Roku");
        assert_eq!(parsed_info.model_number, "TEST1");
        assert_eq!(parsed_info.software_version, "1.2");
        assert_eq!(active_app.status, 200);
        let parsed_app =
            zestroku_proto::parse_active_app(&active_app.body).expect("active-app fixture");
        assert_eq!(parsed_app.app_id, Some(42));
        assert_eq!(parsed_app.name, "Test App");
        assert_eq!(post_response.status, 403);
        assert_eq!(
            zestroku_proto::parse_ecp_response(&post_response).gate,
            zestroku_proto::EcpGate::Limited403
        );
    }
}
