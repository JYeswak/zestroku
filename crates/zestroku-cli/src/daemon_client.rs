//! Bounded daemon HTTP requests and explicit diagnostic ECP reads.
#![forbid(unsafe_code)]

use std::fmt;
use std::io;
use std::time::Duration;

use zestroku_proto::{Command, EcpResponse, Method};
use zestroku_types::ErrorCode;

mod http;
pub(crate) mod inventory;
pub(crate) mod mutation;

use self::http::{Endpoint, HttpFailure, HttpResponse};

const DEFAULT_DAEMON_URL: &str = "http://127.0.0.1:8070";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_RESPONSE_BYTES: usize = 1_048_576;

#[derive(Debug)]
pub enum ReadError {
    DaemonUnavailable(String),
    LanBlockedByOsPolicy(String),
    DirectConnectionFailed(String),
    DaemonRequestFailed {
        status: u16,
        error: String,
        code: ErrorCode,
        detail: Option<String>,
        hint: Option<String>,
        suggestions: Vec<String>,
    },
    InvalidDaemonUrl(String),
    InvalidDeviceTarget(String),
    InvalidResponse(String),
}

impl ReadError {
    fn semantic_code(&self) -> ErrorCode {
        match self {
            Self::DaemonUnavailable(_) => ErrorCode::DaemonUnavailable,
            Self::LanBlockedByOsPolicy(_) => ErrorCode::LanBlockedByOsPolicy,
            Self::DirectConnectionFailed(_) => ErrorCode::DirectConnectionFailed,
            Self::DaemonRequestFailed { code, .. } => *code,
            Self::InvalidDaemonUrl(_) => ErrorCode::InvalidDaemonUrl,
            Self::InvalidDeviceTarget(_) => ErrorCode::InvalidDeviceTarget,
            Self::InvalidResponse(_) => ErrorCode::ProtocolError,
        }
    }

    pub fn code(&self) -> &'static str {
        self.semantic_code().as_str()
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
            Self::DaemonRequestFailed { status, error, .. } => {
                write!(f, "daemon returned HTTP {status}: {error}")
            }
            Self::InvalidDaemonUrl(detail) => write!(f, "invalid daemon URL: {detail}"),
            Self::InvalidDeviceTarget(detail) => write!(f, "invalid device target: {detail}"),
            Self::InvalidResponse(detail) => write!(f, "invalid HTTP response: {detail}"),
        }?;
        let (hint, suggestions) = match self {
            Self::DaemonRequestFailed {
                code,
                detail,
                hint,
                suggestions,
                ..
            } => {
                if let Some(detail) = detail {
                    write!(f, "; {detail}")?;
                } else {
                    write!(
                        f,
                        "; the daemon returned incomplete or unregistered semantic error metadata"
                    )?;
                }
                (
                    hint.as_deref().unwrap_or_else(|| code.hint()),
                    suggestions.as_slice(),
                )
            }
            _ => (self.semantic_code().hint(), &[][..]),
        };
        write!(f, "; hint: {hint}; suggestions: {suggestions:?}")
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

/// Explicit-URL variants for the CLI `--daemon <url>` flag. They behave
/// exactly like [`info`], [`active_app`] and [`apps`] but use the given
/// daemon URL instead of `ZESTROKU_DAEMON_URL`/the default.
pub fn info_via(target: &str, daemon_url: &str) -> Result<DeviceInfo, ReadError> {
    let path = device_path(target, "info")?;
    let response = daemon_get_via(daemon_url, &path)?;
    serde_json::from_str(&response.body)
        .map_err(|error| ReadError::InvalidResponse(format!("daemon JSON: {error}")))
}

/// Explicit-URL variant of [`active_app`]; see [`info_via`].
pub fn active_app_via(target: &str, daemon_url: &str) -> Result<ActiveApp, ReadError> {
    let path = device_path(target, "active-app")?;
    let response = daemon_get_via(daemon_url, &path)?;
    serde_json::from_str(&response.body)
        .map_err(|error| ReadError::InvalidResponse(format!("daemon JSON: {error}")))
}

/// Explicit-URL variant of [`apps`]; see [`info_via`].
pub fn apps_via(
    target: &str,
    daemon_url: &str,
) -> Result<Vec<zestroku_proto::InstalledApp>, ReadError> {
    let path = device_path(target, "apps")?;
    let response = daemon_get_via(daemon_url, &path)?;
    decode_installed_apps(&response.body)
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
    decode_installed_apps(&response.body)
}

fn decode_installed_apps(body: &str) -> Result<Vec<zestroku_proto::InstalledApp>, ReadError> {
    let response: InstalledAppsResponse = serde_json::from_str(body)
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
    let response = http::request(&endpoint, Method::Get, &command.path, "*/*", None)
        .map_err(map_direct_failure)?;
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
    daemon_get_via(&raw_url, path)
}

fn daemon_get_via(raw_url: &str, path: &str) -> Result<HttpResponse, ReadError> {
    let endpoint = parse_daemon_url(raw_url)?;
    daemon_get_at_endpoint(&endpoint, path)
}

fn daemon_get_at_endpoint(endpoint: &Endpoint, path: &str) -> Result<HttpResponse, ReadError> {
    let response = http::request(endpoint, Method::Get, path, "application/json", None)
        .map_err(map_daemon_failure)?;
    if response.status != 200 {
        return Err(daemon_http_error(response));
    }
    Ok(response)
}

fn daemon_http_error(response: HttpResponse) -> ReadError {
    #[derive(serde::Deserialize)]
    struct ErrorMetadata {
        code: ErrorCode,
        detail: String,
        hint: String,
        suggestions: Vec<String>,
    }

    // Numeric exit compatibility depends on the operation cause. Invalid
    // semantic metadata must not erase a valid cause from the same response.
    let (error, metadata) = match serde_json::from_str::<serde_json::Value>(&response.body) {
        Ok(serde_json::Value::Object(mut body)) => match body.remove("error") {
            Some(serde_json::Value::String(error)) => (
                error,
                serde_json::from_value::<ErrorMetadata>(serde_json::Value::Object(body)).ok(),
            ),
            _ => ("unrecognized error response".to_owned(), None),
        },
        _ => ("unrecognized error response".to_owned(), None),
    };
    let (code, detail, hint, suggestions) = match metadata {
        Some(metadata) => (
            metadata.code,
            Some(metadata.detail),
            Some(metadata.hint),
            metadata.suggestions,
        ),
        None => (ErrorCode::ProtocolError, None, None, Vec::new()),
    };
    ReadError::DaemonRequestFailed {
        status: response.status,
        error,
        code,
        detail,
        hint,
        suggestions,
    }
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
mod tests;
