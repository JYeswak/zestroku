#![forbid(unsafe_code)]

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use md5::{Digest, Md5};

const ROUTE: &str = "/plugin_install";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RESPONSE_BYTES: u64 = 1_048_576;

#[derive(Debug)]
pub struct InstallResponse {
    pub status: u16,
    pub body: String,
}

fn md5_hex(input: &[u8]) -> String {
    let digest = Md5::digest(input);
    let mut output = String::with_capacity(32);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn parse_digest_challenge(
    value: &str,
) -> Result<std::collections::HashMap<String, String>, String> {
    let value = value
        .strip_prefix("Digest ")
        .ok_or_else(|| "installer did not offer Digest authentication".to_string())?;
    let mut fields = std::collections::HashMap::new();
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        while index < bytes.len() && (bytes[index] == b' ' || bytes[index] == b',') {
            index += 1;
        }
        if index == bytes.len() {
            break;
        }
        let key_start = index;
        while index < bytes.len() && bytes[index] != b'=' && bytes[index] != b',' {
            index += 1;
        }
        if index == bytes.len() || bytes[index] != b'=' {
            return Err("malformed Digest challenge".to_string());
        }
        let key = value[key_start..index].trim().to_ascii_lowercase();
        index += 1;
        let field = if index < bytes.len() && bytes[index] == b'"' {
            index += 1;
            let start = index;
            while index < bytes.len() && bytes[index] != b'"' {
                if bytes[index] == b'\\' {
                    return Err("escaped Digest challenge fields are unsupported".to_string());
                }
                index += 1;
            }
            if index == bytes.len() {
                return Err("unterminated Digest challenge field".to_string());
            }
            let field = value[start..index].to_string();
            index += 1;
            field
        } else {
            let start = index;
            while index < bytes.len() && bytes[index] != b',' {
                index += 1;
            }
            value[start..index].trim().to_string()
        };
        if fields.insert(key, field).is_some() {
            return Err("duplicate Digest challenge field".to_string());
        }
    }
    Ok(fields)
}

fn digest_authorization(challenge: &str, username: &str, password: &str) -> Result<String, String> {
    if username
        .bytes()
        .any(|byte| byte.is_ascii_control() || matches!(byte, b'"' | b'\\'))
    {
        return Err("installer username contains invalid Digest characters".to_string());
    }
    let fields = parse_digest_challenge(challenge)?;
    let realm = fields.get("realm").ok_or("Digest challenge has no realm")?;
    let nonce = fields.get("nonce").ok_or("Digest challenge has no nonce")?;
    let qop = fields
        .get("qop")
        .filter(|qop| qop.split(',').any(|item| item.trim() == "auth"))
        .map(|_| "auth")
        .ok_or("Digest challenge does not support qop=auth")?;
    if fields
        .get("algorithm")
        .is_some_and(|algorithm| !algorithm.eq_ignore_ascii_case("MD5"))
    {
        return Err("installer Digest algorithm is not MD5".to_string());
    }
    let mut random = [0_u8; 16];
    getrandom::fill(&mut random).map_err(|error| format!("Digest cnonce generation: {error}"))?;
    let mut cnonce = String::with_capacity(32);
    for byte in random {
        use std::fmt::Write as _;
        let _ = write!(cnonce, "{byte:02x}");
    }
    let nc = "00000001";
    let ha1 = md5_hex(format!("{username}:{realm}:{password}").as_bytes());
    let ha2 = md5_hex(format!("POST:{ROUTE}").as_bytes());
    let response = md5_hex(format!("{ha1}:{nonce}:{nc}:{cnonce}:{qop}:{ha2}").as_bytes());
    Ok(format!(
        "Digest username=\"{username}\", realm=\"{realm}\", nonce=\"{nonce}\", uri=\"{ROUTE}\", algorithm=MD5, response=\"{response}\", qop={qop}, nc={nc}, cnonce=\"{cnonce}\""
    ))
}

fn response_parts(response: &[u8]) -> Result<(u16, Vec<(String, String)>, String), String> {
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| "malformed installer HTTP response".to_string())?;
    let head = std::str::from_utf8(&response[..split])
        .map_err(|_| "non-UTF-8 installer response headers".to_string())?;
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| "malformed installer status line".to_string())?;
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    let body = String::from_utf8_lossy(&response[split + 4..]).into_owned();
    Ok((status, headers, body))
}

fn send_request(
    host: &str,
    port: u16,
    body: &[u8],
    boundary: &str,
    authorization: Option<&str>,
) -> Result<(u16, Vec<(String, String)>, String), String> {
    if host
        .bytes()
        .any(|byte| byte.is_ascii_control() || matches!(byte, b' ' | b'/' | b'\\'))
    {
        return Err("installer host contains invalid HTTP authority characters".to_string());
    }
    let address = if host.parse::<std::net::Ipv6Addr>().is_ok() {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let socket = address
        .to_socket_addrs()
        .map_err(|error| format!("installer address: {error}"))?
        .next()
        .ok_or_else(|| "installer address resolved to no sockets".to_string())?;
    let mut stream = TcpStream::connect_timeout(&socket, REQUEST_TIMEOUT)
        .map_err(|error| format!("installer connect: {error}"))?;
    stream
        .set_read_timeout(Some(REQUEST_TIMEOUT))
        .map_err(|error| format!("installer read timeout: {error}"))?;
    stream
        .set_write_timeout(Some(REQUEST_TIMEOUT))
        .map_err(|error| format!("installer write timeout: {error}"))?;
    let authority = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let mut headers = format!(
        "POST {ROUTE} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\nAccept: */*\r\nContent-Type: multipart/form-data; boundary={boundary}\r\nContent-Length: {}\r\n",
        body.len()
    );
    if let Some(authorization) = authorization {
        headers.push_str("Authorization: ");
        headers.push_str(authorization);
        headers.push_str("\r\n");
    }
    headers.push_str("\r\n");
    stream
        .write_all(headers.as_bytes())
        .and_then(|()| stream.write_all(body))
        .map_err(|error| format!("installer request write: {error}"))?;
    let mut response = Vec::new();
    stream
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut response)
        .map_err(|error| format!("installer response read: {error}"))?;
    if response.len() as u64 > MAX_RESPONSE_BYTES {
        return Err("installer response exceeds 1 MiB limit".to_string());
    }
    response_parts(&response)
}

fn multipart(zip: &[u8], filename: &str, boundary: &str) -> Result<Vec<u8>, String> {
    if filename.is_empty()
        || filename
            .bytes()
            .any(|byte| byte.is_ascii_control() || matches!(byte, b'"' | b'\\'))
    {
        return Err("archive filename contains invalid multipart characters".to_string());
    }
    let mut body = Vec::with_capacity(zip.len() + 512);
    body.extend_from_slice(format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"mysubmit\"\r\n\r\nInstall\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"archive\"; filename=\"{filename}\"\r\nContent-Type: application/zip\r\n\r\n"
    ).as_bytes());
    body.extend_from_slice(zip);
    body.extend_from_slice(format!("\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"passwd\"\r\n\r\n\r\n--{boundary}--\r\n").as_bytes());
    Ok(body)
}

/// Upload one owner-provided ZIP through Roku's developer installer.
/// The Digest password and archive bytes are call-scoped and not logged.
pub fn install(
    host: &str,
    port: u16,
    zip: &[u8],
    filename: &str,
    username: &str,
    password: &str,
) -> Result<InstallResponse, String> {
    let mut nonce = [0_u8; 16];
    getrandom::fill(&mut nonce)
        .map_err(|error| format!("multipart boundary generation: {error}"))?;
    let mut boundary = String::with_capacity(41);
    boundary.push_str("zestroku-");
    for byte in nonce {
        use std::fmt::Write as _;
        let _ = write!(boundary, "{byte:02x}");
    }
    let body = multipart(zip, filename, &boundary)?;
    let (status, headers, _) = send_request(host, port, &body, &boundary, None)?;
    if status != 401 {
        return Err(format!(
            "installer expected Digest challenge, received HTTP {status}"
        ));
    }
    let challenge = headers
        .iter()
        .find(|(name, value)| name == "www-authenticate" && value.starts_with("Digest "))
        .map(|(_, value)| value.as_str())
        .ok_or_else(|| "installer returned 401 without Digest challenge".to_string())?;
    let authorization = digest_authorization(challenge, username, password)?;
    let (status, _, response_body) =
        send_request(host, port, &body, &boundary, Some(&authorization))?;
    if status != 200 {
        return Err(format!("installer returned HTTP {status}"));
    }
    let lower = response_body.to_ascii_lowercase();
    if lower.contains("<font color=\"red\">") || lower.contains("<font color='red'>") {
        return Err("installer reported a channel installation error".to_string());
    }
    Ok(InstallResponse {
        status,
        body: response_body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_digest_matches_rfc_1321_known_vector() {
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
    }

    #[test]
    fn digest_challenge_parses_quoted_commas_and_rejects_unsupported_qop() {
        let fields =
            parse_digest_challenge(r#"Digest realm="roku,dev", nonce="n", qop="auth,auth-int""#)
                .unwrap();
        assert_eq!(fields.get("realm").map(String::as_str), Some("roku,dev"));
        assert!(
            digest_authorization(
                r#"Digest realm="r", nonce="n", qop="auth-int""#,
                "rokudev",
                "pw"
            )
            .is_err()
        );
    }

    #[test]
    fn multipart_carries_required_fields_and_raw_zip_bytes() {
        let body = multipart(b"PK\x03\x04ZIP", "channel.zip", "boundary").unwrap();
        assert!(
            body.windows(b"name=\"mysubmit\"".len())
                .any(|part| part == b"name=\"mysubmit\"")
        );
        assert!(
            body.windows(b"name=\"archive\"".len())
                .any(|part| part == b"name=\"archive\"")
        );
        assert!(
            body.windows(b"PK\x03\x04ZIP".len())
                .any(|part| part == b"PK\x03\x04ZIP")
        );
        assert!(
            body.windows(b"name=\"passwd\"".len())
                .any(|part| part == b"name=\"passwd\"")
        );
    }
}
