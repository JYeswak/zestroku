//! Existing daemon-client boundary and decoding regressions.
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
    let fixed =
        parse_http_response(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").expect("fixed body");
    assert_eq!(fixed.body, "ok");
    let chunked = parse_http_response(
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nok\r\n0\r\n\r\n",
    )
    .expect("chunked body");
    assert_eq!(chunked.body, "ok");
}

#[test]
fn invalid_semantic_metadata_preserves_the_operation_cause() {
    for (status, cause, metadata) in [
        (
            404,
            "device_not_found",
            serde_json::json!({
                "code": "UNREGISTERED_FAILURE",
                "detail": "missing device",
                "hint": "list devices",
                "suggestions": [],
            }),
        ),
        (
            403,
            "confirmation_required",
            serde_json::json!({
                "code": "CONFIRMATION_REQUIRED",
                "detail": "approval absent",
                "hint": "obtain owner approval",
                "suggestions": 17,
            }),
        ),
    ] {
        let mut body = metadata;
        body["error"] = serde_json::Value::String(cause.to_owned());
        let failure = super::daemon_http_error(super::HttpResponse {
            status,
            headers: Vec::new(),
            body: body.to_string(),
        });
        assert_eq!(failure.code(), "PROTOCOL_ERROR");
        match failure {
            super::ReadError::DaemonRequestFailed {
                status: observed_status,
                error: observed_cause,
                ..
            } => {
                assert_eq!(observed_status, status);
                assert_eq!(observed_cause, cause);
            }
            other => panic!("HTTP failure classification was lost: {other:?}"),
        }
    }
}
