//! A mutation client cannot select a LAN backend or turn an incomplete outcome into success.

use serde::{Deserialize, Serialize};
use zestroku_proto::Method;

use super::http::Endpoint;
use super::{DEFAULT_DAEMON_URL, ReadError, daemon_http_error, device_path, parse_daemon_url};

use zestroku_core::mutation::ClientPrincipal;
#[derive(Debug, Deserialize)]
enum MutationCompletion {
    #[serde(rename = "completed")]
    Completed,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CompletedMutation {
    pub(crate) action_id: i64,
    pub(crate) target: String,
    pub(crate) surface: String,
    #[serde(rename = "outcome")]
    _outcome: MutationCompletion,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UndoPlan {
    pub(crate) action_id: i64,
    pub(crate) app_id: u32,
    pub(crate) app_name: String,
}

#[derive(Serialize)]
struct UndoRequest<'a> {
    client: &'static str,
    action_id: i64,
    confirm: Option<&'a str>,
}

#[derive(Serialize)]
struct KeyRequest<'a> {
    client: &'static str,
    key: &'a str,
    confirm: Option<&'a str>,
}

#[derive(Serialize)]
struct LaunchRequest<'a> {
    client: &'static str,
    app_id: u32,
    confirm: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    requested_app: Option<&'a str>,
}

pub(crate) struct MutationClient {
    endpoint: Endpoint,
}

impl MutationClient {
    pub(crate) fn from_env() -> Result<Self, ReadError> {
        let configured = std::env::var("ZESTROKU_DAEMON_URL");
        let raw_url = match &configured {
            Ok(url) => url.as_str(),
            Err(std::env::VarError::NotPresent) => DEFAULT_DAEMON_URL,
            Err(std::env::VarError::NotUnicode(_)) => {
                return Err(ReadError::InvalidDaemonUrl("URL must be UTF-8".into()));
            }
        };
        let endpoint = parse_daemon_url(raw_url)?;
        if !endpoint
            .host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
        {
            return Err(ReadError::InvalidDaemonUrl(
                "mutation daemon must use a loopback IP literal".into(),
            ));
        }
        Ok(Self { endpoint })
    }

    pub(crate) fn apps(
        &self,
        target: &str,
    ) -> Result<Vec<zestroku_proto::InstalledApp>, ReadError> {
        let path = device_path(target, "apps")?;
        let response = super::daemon_get_at_endpoint(&self.endpoint, &path)?;
        super::decode_installed_apps(&response.body)
    }
    pub(crate) fn undo_plan(&self, target: &str) -> Result<UndoPlan, ReadError> {
        let path = device_path(target, "undo-plan")?;
        let response = super::daemon_get_at_endpoint(&self.endpoint, &path)?;
        if !(200..300).contains(&response.status) {
            return Err(daemon_http_error(response));
        }
        serde_json::from_str(&response.body)
            .map_err(|_| ReadError::InvalidResponse("daemon returned an invalid undo plan".into()))
    }

    pub(crate) fn key(
        &self,
        target: &str,
        key: &str,
        confirm: Option<&str>,
    ) -> Result<CompletedMutation, ReadError> {
        self.post(
            target,
            "key",
            &KeyRequest {
                client: ClientPrincipal::Cli.as_str(),
                key,
                confirm,
            },
        )
    }

    pub(crate) fn launch(
        &self,
        target: &str,
        app_id: u32,
        requested_app: Option<&str>,
        confirm: Option<&str>,
    ) -> Result<CompletedMutation, ReadError> {
        self.post(
            target,
            "launch",
            &LaunchRequest {
                client: ClientPrincipal::Cli.as_str(),
                app_id,
                confirm,
                requested_app,
            },
        )
    }
    pub(crate) fn undo_last(
        &self,
        target: &str,
        action_id: i64,
        confirm: Option<&str>,
    ) -> Result<CompletedMutation, ReadError> {
        self.post(
            target,
            "undo",
            &UndoRequest {
                client: ClientPrincipal::Cli.as_str(),
                action_id,
                confirm,
            },
        )
    }

    fn post(
        &self,
        target: &str,
        operation: &str,
        payload: &impl Serialize,
    ) -> Result<CompletedMutation, ReadError> {
        let path = device_path(target, operation)?;
        let body = serde_json::to_vec(payload)
            .map_err(|_| ReadError::InvalidResponse("cannot encode mutation request".into()))?;
        let response = super::http::request(
            &self.endpoint,
            Method::Post,
            &path,
            "application/json",
            Some(&body),
        )
        .map_err(super::map_daemon_failure)?;
        if !(200..300).contains(&response.status) {
            return Err(daemon_http_error(response));
        }
        serde_json::from_str(&response.body).map_err(|_| {
            ReadError::InvalidResponse("daemon did not return a completed mutation outcome".into())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::CompletedMutation;

    #[test]
    fn failed_outcome_cannot_be_deserialized_as_completed() {
        let body = r#"{"action_id":7,"target":"127.0.0.1:8060","surface":"key","outcome":"dispatch_failed"}"#;
        let error = serde_json::from_str::<CompletedMutation>(body)
            .expect_err("failed execution must never become completed success");
        assert_eq!(error.classify(), serde_json::error::Category::Data);
    }
}
