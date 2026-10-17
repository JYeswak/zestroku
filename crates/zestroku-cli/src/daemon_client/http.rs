//! One bounded transport for daemon reads, mutation requests, and explicit ECP reads.

use std::fmt::Write as _;
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Instant;

use zestroku_proto::Method;

use super::{MAX_RESPONSE_BYTES, REQUEST_TIMEOUT};

#[derive(Debug)]
pub(super) struct Endpoint {
    pub(super) host: String,
    pub(super) port: u16,
    pub(super) authority: String,
}

#[derive(Debug)]
pub(super) struct HttpResponse {
    pub(super) status: u16,
    pub(super) headers: Vec<(String, String)>,
    pub(super) body: String,
}

#[derive(Debug)]
pub(super) enum HttpFailure {
    Connect(io::Error),
    Io(io::Error),
    TimedOut,
    Invalid(String),
}

pub(super) fn request(
    endpoint: &Endpoint,
    method: Method,
    path: &str,
    accept: &str,
    body: Option<&[u8]>,
) -> Result<HttpResponse, HttpFailure> {
    let deadline = Instant::now() + REQUEST_TIMEOUT;
    let mut stream = connect(endpoint, deadline)?;
    let method = match method {
        Method::Get => "GET",
        Method::Post => "POST",
    };
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nAccept: {accept}\r\n",
        endpoint.authority
    );
    if let Some(body) = body {
        write!(
            request,
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        )
        .expect("formatting a byte count into String cannot fail");
    }
    request.push_str("\r\n");
    set_write_timeout(&stream, deadline)?;
    stream
        .write_all(request.as_bytes())
        .map_err(HttpFailure::Io)?;
    if let Some(body) = body {
        set_write_timeout(&stream, deadline)?;
        stream.write_all(body).map_err(HttpFailure::Io)?;
    }

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
    super::parse_http_response(&raw)
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
