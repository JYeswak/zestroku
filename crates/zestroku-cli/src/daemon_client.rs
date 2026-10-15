//! Bounded HTTP/1.1 client for daemon-backed reads and explicit direct reads.
#![forbid(unsafe_code)]

use std::fmt;
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use zestroku_proto::{Command, EcpResponse, Method};

const DEFAULT_DAEMON_URL: &str = "http://127.0.0.1:8070";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_RESPONSE_BYTES: usize = 1_048_576;

#[derive(Debug)]
pub enum ReadError {
    DaemonUnavailable(String),
    LanBlockedByOsPolicy(String),
    DirectConnectionFailed(String),
    DaemonRequestFailed { status: u16, error: String },
    InvalidDaemonUrl(String),
    InvalidDeviceTarget(String),
    InvalidResponse(String),
}

impl ReadError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::DaemonUnavailable(_) => "DAEMON_UNAVAILABLE",
            Self::LanBlockedByOsPolicy(_) => "LAN_BLOCKED_BY_OS_POLICY",
            Self::DirectConnectionFailed(_) => "DIRECT_CONNECTION_FAILED",
            Self::DaemonRequestFailed { .. } => "DAEMON_REQUEST_FAILED",
            Self::InvalidDaemonUrl(_) => "INVALID_DAEMON_URL",
            Self::InvalidDeviceTarget(_) => "INVALID_DEVICE_TARGET",
            Self::InvalidResponse(_) => "PROTOCOL_ERROR",
        }
    }
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DaemonUnavailable(detail) => write!(f, "daemon unavailable: {detail}"),
            Self::LanBlockedByOsPolicy(detail) => {
                write!(f, "direct LAN connection blocked by OS policy: {detail}")
            }
            Self::DirectConnectionFailed(detail) => write!(f, "direct connection failed: {detail}"),
            Self::DaemonRequestFailed { status, error } => {
                write!(f, "daemon returned HTTP {status}: {error}")
            }
            Self::InvalidDaemonUrl(detail) => write!(f, "invalid daemon URL: {detail}"),
            Self::InvalidDeviceTarget(detail) => write!(f, "invalid device target: {detail}"),
            Self::InvalidResponse(detail) => write!(f, "invalid HTTP response: {detail}"),
        }
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct DeviceInfo {
    pub model_name: String,
    pub model_number: String,
    pub software_version: String,
    pub is_tv: bool,
    pub access_tier: String,
    pub tv_active_channel: Option<bool>,
}

#[derive(Debug, serde::Deserialize)]
pub struct ActiveApp {
    pub app_id: Option<u32>,
    pub name: String,
}

#[derive(serde::Deserialize)]
struct InstalledAppsResponse {
    apps: Vec<InstalledAppResponse>,
}

#[derive(serde::Deserialize)]
struct InstalledAppResponse {
    id: String,
    name: String,
    #[serde(rename = "type")]
    app_type: String,
    subtype: Option<String>,
    version: Option<String>,
}

#[derive(Debug)]
struct Endpoint {
    host: String,
    port: u16,
    authority: String,
}

#[derive(Debug)]
struct HttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

#[derive(Debug)]
enum HttpFailure {
    Connect(io::Error),
    Io(io::Error),
    TimedOut,
    Invalid(String),
}

pub fn info(target: &str) -> Result<DeviceInfo, ReadError> {
    let path = device_path(target, "info")?;
    let response = daemon_get(&path)?;
    serde_json::from_str(&response.body)
        .map_err(|error| ReadError::InvalidResponse(format!("daemon JSON: {error}")))
}

pub fn active_app(target: &str) -> Result<ActiveApp, ReadError> {
    let path = device_path(target, "active-app")?;
    let response = daemon_get(&path)?;
    serde_json::from_str(&response.body)
        .map_err(|error| ReadError::InvalidResponse(format!("daemon JSON: {error}")))
}

pub fn apps(target: &str) -> Result<Vec<zestroku_proto::InstalledApp>, ReadError> {
    let path = device_path(target, "apps")?;
    let response = daemon_get(&path)?;
    let response: InstalledAppsResponse = serde_json::from_str(&response.body)
        .map_err(|error| ReadError::InvalidResponse(format!("daemon JSON: {error}")))?;
    Ok(response
        .apps
        .into_iter()
        .map(|app| zestroku_proto::InstalledApp {
            id: app.id,
            name: app.name,
            app_type: app.app_type,
            subtype: app.subtype,
            version: app.version,
        })
        .collect())
}

/// Direct ECP GET is only selected by the CLI's explicit ZESTROKU_DIRECT=1 mode.
pub fn direct_ecp_get(host: &str, port: u16, command: &Command) -> Result<EcpResponse, ReadError> {
    if command.method != Method::Get {
        return Err(ReadError::InvalidResponse(
            "direct read client accepts GET commands only".into(),
        ));
    }
    let host = normalize_direct_host(host)?;
    let endpoint = Endpoint {
        authority: authority(&host, port),
        host,
        port,
    };
    let response = http_get(&endpoint, &command.path, "*/*").map_err(map_direct_failure)?;
    Ok(EcpResponse {
        status: response.status,
        headers: response.headers,
        body: response.body,
    })
}
fn normalize_direct_host(host: &str) -> Result<String, ReadError> {
    let host = match host.strip_prefix('[') {
        Some(bracketed) => bracketed
            .strip_suffix(']')
            .ok_or_else(|| ReadError::InvalidDeviceTarget(host.to_owned()))?,
        None => host,
    };
    if host.is_empty()
        || host.bytes().any(|byte| {
            byte.is_ascii_control()
                || byte.is_ascii_whitespace()
                || matches!(byte, b'/' | b'\\' | b'?' | b'#' | b'@' | b'[' | b']')
        })
    {
        return Err(ReadError::InvalidDeviceTarget(host.to_owned()));
    }
    Ok(host.to_owned())
}

fn daemon_get(path: &str) -> Result<HttpResponse, ReadError> {
    let raw_url =
        std::env::var("ZESTROKU_DAEMON_URL").unwrap_or_else(|_| DEFAULT_DAEMON_URL.to_owned());
    let endpoint = parse_daemon_url(&raw_url)?;
    let response = http_get(&endpoint, path, "application/json").map_err(map_daemon_failure)?;
    if response.status != 200 {
        let error = serde_json::from_str::<serde_json::Value>(&response.body)
            .ok()
            .and_then(|value| {
                value
                    .get("error")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| "unrecognized error response".to_owned());
        return Err(ReadError::DaemonRequestFailed {
            status: response.status,
            error,
        });
    }
    Ok(response)
}

fn device_path(target: &str, verb: &str) -> Result<String, ReadError> {
    if target.is_empty()
        || target
            .bytes()
            .any(|byte| byte.is_ascii_control() || matches!(byte, b'/' | b'\\' | b'?' | b'#'))
    {
        return Err(ReadError::InvalidDeviceTarget(target.to_owned()));
    }
    let mut segment = String::with_capacity(target.len());
    for byte in target.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~:[]".contains(&byte) {
            segment.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(segment, "%{byte:02X}");
        }
    }
    Ok(format!("/v1/devices/{segment}/{verb}"))
}

fn parse_daemon_url(url: &str) -> Result<Endpoint, ReadError> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| ReadError::InvalidDaemonUrl("only http:// URLs are supported".into()))?;
    let authority = rest.strip_suffix('/').unwrap_or(rest);
    if authority.is_empty()
        || authority.bytes().any(|byte| {
            byte.is_ascii_control()
                || byte.is_ascii_whitespace()
                || matches!(byte, b'/' | b'?' | b'#' | b'@')
        })
    {
        return Err(ReadError::InvalidDaemonUrl(
            "expected http://host[:port]".into(),
        ));
    }

    let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
        let close = bracketed
            .find(']')
            .ok_or_else(|| ReadError::InvalidDaemonUrl("unterminated IPv6 host".into()))?;
        let host = &bracketed[..close];
        let suffix = &bracketed[close + 1..];
        let port = if suffix.is_empty() {
            80
        } else {
            parse_port(suffix.strip_prefix(':').ok_or_else(|| {
                ReadError::InvalidDaemonUrl("invalid bracketed host authority".into())
            })?)?
        };
        (host.to_owned(), port)
    } else if let Some((host, port)) = authority.rsplit_once(':') {
        if host.contains(':') {
            return Err(ReadError::InvalidDaemonUrl(
                "IPv6 hosts must be bracketed".into(),
            ));
        }
        (host.to_owned(), parse_port(port)?)
    } else {
        (authority.to_owned(), 80)
    };
    if host.is_empty() {
        return Err(ReadError::InvalidDaemonUrl("host is empty".into()));
    }
    Ok(Endpoint {
        host,
        port,
        authority: authority.to_owned(),
    })
}

fn parse_port(value: &str) -> Result<u16, ReadError> {
    value
        .parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .ok_or_else(|| ReadError::InvalidDaemonUrl("port must be in 1..=65535".into()))
}

fn authority(host: &str, port: u16) -> String {
    if host.starts_with('[') {
        format!("{host}:{port}")
    } else if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn http_get(endpoint: &Endpoint, path: &str, accept: &str) -> Result<HttpResponse, HttpFailure> {
    let deadline = Instant::now() + REQUEST_TIMEOUT;
    let mut stream = connect(endpoint, deadline)?;
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nAccept: {accept}\r\n\r\n",
        endpoint.authority
    );
    set_write_timeout(&stream, deadline)?;
    stream
        .write_all(request.as_bytes())
        .map_err(HttpFailure::Io)?;

    let mut raw = Vec::with_capacity(4096);
    let mut buffer = [0_u8; 4096];
    loop {
        set_read_timeout(&stream, deadline)?;
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                if raw.len().saturating_add(count) > MAX_RESPONSE_BYTES {
                    return Err(HttpFailure::Invalid("response exceeded 1 MiB".into()));
                }
                raw.extend_from_slice(&buffer[..count]);
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                return Err(HttpFailure::TimedOut);
            }
            Err(error) => return Err(HttpFailure::Io(error)),
        }
    }
    parse_http_response(&raw)
}

fn connect(endpoint: &Endpoint, deadline: Instant) -> Result<TcpStream, HttpFailure> {
    let addresses = (endpoint.host.as_str(), endpoint.port)
        .to_socket_addrs()
        .map_err(HttpFailure::Connect)?;
    let mut last_error = None;
    for address in addresses {
        let timeout = deadline.saturating_duration_since(Instant::now());
        if timeout.is_zero() {
            return Err(HttpFailure::TimedOut);
        }
        match TcpStream::connect_timeout(&address, timeout) {
            Ok(stream) => return Ok(stream),
            Err(error) => last_error = Some(error),
        }
    }
    Err(HttpFailure::Connect(last_error.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            "host resolved to no addresses",
        )
    })))
}

fn set_write_timeout(stream: &TcpStream, deadline: Instant) -> Result<(), HttpFailure> {
    let timeout = deadline.saturating_duration_since(Instant::now());
    if timeout.is_zero() {
        return Err(HttpFailure::TimedOut);
    }
    stream
        .set_write_timeout(Some(timeout))
        .map_err(HttpFailure::Io)
}

fn set_read_timeout(stream: &TcpStream, deadline: Instant) -> Result<(), HttpFailure> {
    let timeout = deadline.saturating_duration_since(Instant::now());
    if timeout.is_zero() {
        return Err(HttpFailure::TimedOut);
    }
    stream
        .set_read_timeout(Some(timeout))
        .map_err(HttpFailure::Io)
}

fn parse_http_response(raw: &[u8]) -> Result<HttpResponse, HttpFailure> {
    let separator = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| HttpFailure::Invalid("missing HTTP header terminator".into()))?;
    let head = std::str::from_utf8(&raw[..separator])
        .map_err(|error| HttpFailure::Invalid(format!("HTTP headers are not UTF-8: {error}")))?;
    let mut lines = head.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| HttpFailure::Invalid("missing HTTP status line".into()))?;
    let mut status_fields = status_line.split_whitespace();
    let version = status_fields.next().unwrap_or_default();
    if version != "HTTP/1.1" && version != "HTTP/1.0" {
        return Err(HttpFailure::Invalid("unsupported HTTP version".into()));
    }
    let status = status_fields
        .next()
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| HttpFailure::Invalid("malformed HTTP status".into()))?;
    let mut headers = Vec::new();
    for line in lines {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| HttpFailure::Invalid("malformed HTTP header".into()))?;
        headers.push((name.trim().to_owned(), value.trim().to_owned()));
    }

    let body = &raw[separator + 4..];
    let chunked = headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("transfer-encoding") && value.eq_ignore_ascii_case("chunked")
    });
    let body = if chunked {
        decode_chunked(body)?
    } else {
        if let Some(length) = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, value)| value.parse::<usize>().ok())
            && length != body.len()
        {
            return Err(HttpFailure::Invalid(
                "Content-Length does not match body".into(),
            ));
        }
        body.to_vec()
    };
    let body = String::from_utf8(body)
        .map_err(|error| HttpFailure::Invalid(format!("response body is not UTF-8: {error}")))?;
    Ok(HttpResponse {
        status,
        headers,
        body,
    })
}

fn decode_chunked(mut input: &[u8]) -> Result<Vec<u8>, HttpFailure> {
    let mut output = Vec::new();
    loop {
        let line_end = input
            .windows(2)
            .position(|window| window == b"\r\n")
            .ok_or_else(|| HttpFailure::Invalid("malformed chunk size".into()))?;
        let size_line = std::str::from_utf8(&input[..line_end])
            .map_err(|error| HttpFailure::Invalid(format!("chunk size is not ASCII: {error}")))?;
        let size = usize::from_str_radix(size_line.split(';').next().unwrap_or_default(), 16)
            .map_err(|_| HttpFailure::Invalid("invalid chunk size".into()))?;
        input = &input[line_end + 2..];
        if size == 0 {
            return Ok(output);
        }
        let end = size
            .checked_add(2)
            .filter(|end| *end <= input.len())
            .ok_or_else(|| HttpFailure::Invalid("truncated HTTP chunk".into()))?;
        if &input[end - 2..end] != b"\r\n" {
            return Err(HttpFailure::Invalid("chunk missing CRLF".into()));
        }
        output.extend_from_slice(&input[..end - 2]);
        input = &input[end..];
    }
}

fn map_daemon_failure(error: HttpFailure) -> ReadError {
    match error {
        HttpFailure::Connect(error) => ReadError::DaemonUnavailable(error.to_string()),
        HttpFailure::TimedOut => ReadError::DaemonUnavailable("request exceeded 2 s".into()),
        HttpFailure::Io(error) => ReadError::DaemonUnavailable(error.to_string()),
        HttpFailure::Invalid(detail) => ReadError::InvalidResponse(detail),
    }
}

fn map_direct_failure(error: HttpFailure) -> ReadError {
    match error {
        HttpFailure::Connect(error) => classify_direct_connect_error(error),
        HttpFailure::TimedOut => ReadError::DirectConnectionFailed("request exceeded 2 s".into()),
        HttpFailure::Io(error) => ReadError::DirectConnectionFailed(error.to_string()),
        HttpFailure::Invalid(detail) => ReadError::InvalidResponse(detail),
    }
}

fn classify_direct_connect_error(error: io::Error) -> ReadError {
    if error.raw_os_error() == Some(65) {
        ReadError::LanBlockedByOsPolicy(error.to_string())
    } else {
        ReadError::DirectConnectionFailed(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::{
        classify_direct_connect_error, normalize_direct_host, parse_daemon_url, parse_http_response,
    };

    #[test]
    fn classifies_errno_65_as_policy_and_connection_refused_as_ordinary_failure() {
        let blocked = classify_direct_connect_error(io::Error::from_raw_os_error(65));
        assert_eq!(blocked.code(), "LAN_BLOCKED_BY_OS_POLICY");

        let refused = classify_direct_connect_error(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            "connection refused",
        ));
        assert_eq!(refused.code(), "DIRECT_CONNECTION_FAILED");
    }
    #[test]
    fn direct_host_validation_blocks_http_header_injection() {
        assert_eq!(normalize_direct_host("[::1]").unwrap(), "::1");
        let error = normalize_direct_host("127.0.0.1\r\nHost: evil").unwrap_err();
        assert_eq!(error.code(), "INVALID_DEVICE_TARGET");
    }

    #[test]
    fn channel_json_preserves_unknown_and_absent_values() {
        let unknown: super::DeviceInfo = serde_json::from_str(
            r#"{"model_name":"Test","model_number":"X","software_version":"1","is_tv":false,"access_tier":"permissive","tv_active_channel":null}"#,
        )
        .expect("unknown channel JSON");
        assert_eq!(unknown.tv_active_channel, None);

        let absent: super::DeviceInfo = serde_json::from_str(
            r#"{"model_name":"Test","model_number":"X","software_version":"1","is_tv":false,"access_tier":"permissive","tv_active_channel":false}"#,
        )
        .expect("absent channel JSON");
        assert_eq!(absent.tv_active_channel, Some(false));
    }

    #[test]
    fn daemon_url_parser_accepts_default_and_bracketed_ipv6_authorities() {
        let default = parse_daemon_url("http://127.0.0.1:8070").expect("default URL");
        assert_eq!((default.host.as_str(), default.port), ("127.0.0.1", 8070));
        let ipv6 = parse_daemon_url("http://[::1]:8071/").expect("IPv6 URL");
        assert_eq!((ipv6.host.as_str(), ipv6.port), ("::1", 8071));
        assert!(parse_daemon_url("https://127.0.0.1:8070").is_err());
    }

    #[test]
    fn parses_content_length_and_chunked_http_bodies() {
        let fixed = parse_http_response(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
            .expect("fixed body");
        assert_eq!(fixed.body, "ok");
        let chunked = parse_http_response(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nok\r\n0\r\n\r\n",
        )
        .expect("chunked body");
        assert_eq!(chunked.body, "ok");
    }
}
