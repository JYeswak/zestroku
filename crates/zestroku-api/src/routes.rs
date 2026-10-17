#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use fastapi::OpenApiConfig;
use fastapi::core::app::RouteEntry;
use fastapi::core::{
    App, BoxFuture, FromRequest, Json, JsonExtractError, Method, Request, RequestContext, Response,
    StatusCode,
};
use fastapi::fastapi_openapi;
use fastapi::router::Route;
use serde::{Deserialize, Serialize};
use zestroku_core::mutation::{
    ClientPrincipal, MutationContext, MutationError, authorize_mutation, finish_mutation,
};
use zestroku_core::store::{ActionRow, Store};
use zestroku_core::tier::{before_app_identity, marked_before_app_state, recorded_tier_for};
use zestroku_proto::{
    EcpGate, Transport, keypress_raw, launch, parse_active_app, parse_apps, parse_ecp_response,
    query, query_apps,
};
use zestroku_types::{AccessTier, Error};

use super::{
    ActiveAppView, ApiState, DeviceInfoView, DeviceSummary, ErrorView, HealthView, active_app,
    device_info, error_response, json_response, list_devices, path_target,
};

const HEALTH_PATH: &str = "/health";
const DEVICES_PATH: &str = "/v1/devices";
const INFO_PATH: &str = "/v1/devices/{host}/info";
const ACTIVE_APP_PATH: &str = "/v1/devices/{host}/active-app";
const APPS_PATH: &str = "/v1/devices/{host}/apps";
const KEY_PATH: &str = "/v1/devices/{host}/key";
const LAUNCH_PATH: &str = "/v1/devices/{host}/launch";
const UNDO_PLAN_PATH: &str = "/v1/devices/{host}/undo-plan";
const UNDO_PATH: &str = "/v1/devices/{host}/undo";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyMutationRequest {
    key: String,
    #[serde(default = "default_http_client")]
    client: String,
    confirm: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LaunchMutationRequest {
    app_id: u32,
    #[serde(default = "default_http_client")]
    client: String,
    confirm: Option<String>,
    requested_app: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UndoMutationRequest {
    action_id: i64,
    #[serde(default = "default_http_client")]
    client: String,
    confirm: Option<String>,
}

#[derive(Debug, Serialize, fastapi::JsonSchema)]
struct UndoPlanView {
    action_id: i64,
    app_id: u32,
    app_name: String,
}

impl fastapi_openapi::JsonSchema for UndoMutationRequest {
    fn schema() -> fastapi_openapi::Schema {
        closed_request_schema(
            "UndoMutationRequest",
            HashMap::from([
                (
                    "action_id".to_owned(),
                    <i64 as fastapi_openapi::JsonSchema>::schema(),
                ),
                ("client".to_owned(), client_schema()),
                (
                    "confirm".to_owned(),
                    <Option<String> as fastapi_openapi::JsonSchema>::schema(),
                ),
            ]),
            vec!["action_id".to_owned()],
        )
    }

    fn schema_name() -> Option<&'static str> {
        Some("UndoMutationRequest")
    }
}
fn closed_request_schema(
    title: &str,
    properties: HashMap<String, fastapi_openapi::Schema>,
    required: Vec<String>,
) -> fastapi_openapi::Schema {
    let mut schema = fastapi_openapi::Schema::object(properties, required);
    if let fastapi_openapi::Schema::Object(object) = &mut schema {
        object.title = Some(title.to_owned());
        object.additional_properties = Some(Box::new(fastapi_openapi::Schema::Boolean(false)));
    }
    schema
}
fn client_schema() -> fastapi_openapi::Schema {
    fastapi_openapi::Schema::string_enum(vec![
        "cli".to_owned(),
        "mcp-stdio".to_owned(),
        "loopback-http".to_owned(),
    ])
}

fn default_http_client() -> String {
    ClientPrincipal::LoopbackHttp.as_str().to_owned()
}

impl fastapi_openapi::JsonSchema for KeyMutationRequest {
    fn schema() -> fastapi_openapi::Schema {
        closed_request_schema(
            "KeyMutationRequest",
            HashMap::from([
                (
                    "key".to_owned(),
                    <String as fastapi_openapi::JsonSchema>::schema(),
                ),
                ("client".to_owned(), client_schema()),
                (
                    "confirm".to_owned(),
                    <Option<String> as fastapi_openapi::JsonSchema>::schema(),
                ),
            ]),
            vec!["key".to_owned()],
        )
    }

    fn schema_name() -> Option<&'static str> {
        Some("KeyMutationRequest")
    }
}

impl fastapi_openapi::JsonSchema for LaunchMutationRequest {
    fn schema() -> fastapi_openapi::Schema {
        closed_request_schema(
            "LaunchMutationRequest",
            HashMap::from([
                (
                    "app_id".to_owned(),
                    <u32 as fastapi_openapi::JsonSchema>::schema(),
                ),
                ("client".to_owned(), client_schema()),
                (
                    "confirm".to_owned(),
                    <Option<String> as fastapi_openapi::JsonSchema>::schema(),
                ),
                (
                    "requested_app".to_owned(),
                    <Option<String> as fastapi_openapi::JsonSchema>::schema(),
                ),
            ]),
            vec!["app_id".to_owned()],
        )
    }

    fn schema_name() -> Option<&'static str> {
        Some("LaunchMutationRequest")
    }
}

#[derive(Debug, Serialize, fastapi::JsonSchema)]
struct MutationResponse {
    action_id: i64,
    target: String,
    surface: &'static str,
    outcome: &'static str,
}

#[derive(Debug, Serialize, fastapi::JsonSchema)]
struct InstalledAppView {
    id: String,
    name: String,
    #[serde(rename = "type")]
    app_type: String,
    subtype: Option<String>,
    version: Option<String>,
}

#[derive(Debug, Serialize, fastapi::JsonSchema)]
struct InstalledAppsView {
    host: String,
    apps: Vec<InstalledAppView>,
}

fn documented_route(
    path: &'static str,
    operation_id: &'static str,
    summary: &'static str,
    description: &'static str,
) -> Route {
    Route::new(Method::Get, path)
        .operation_id(operation_id)
        .summary(summary)
        .description(description)
        .tag("read")
}
fn mutation_route(
    path: &'static str,
    operation_id: &'static str,
    summary: &'static str,
    description: &'static str,
) -> Route {
    Route::new(Method::Post, path)
        .operation_id(operation_id)
        .summary(summary)
        .description(description)
        .tag("mutation")
}

fn with_error_schemas(mut route: RouteEntry, errors: &[(u16, &'static str)]) -> RouteEntry {
    for &(status, description) in errors {
        route = route.response_schema::<ErrorView>(status, description);
    }
    route
}
fn same_origin_or_absent(req: &Request) -> bool {
    let Some(origin) = req.headers().get("origin") else {
        return true;
    };
    let Some(host) = req.headers().get("host") else {
        return false;
    };
    let (Ok(origin), Ok(host)) = (std::str::from_utf8(origin), std::str::from_utf8(host)) else {
        return false;
    };
    origin
        .strip_prefix("http://")
        .is_some_and(|authority| !authority.is_empty() && authority.eq_ignore_ascii_case(host))
}
fn request_host_is_ip_literal(req: &Request) -> bool {
    let Some(host) = req.headers().get("host") else {
        // The HTTP/1.1 server rejects a missing Host; direct TestClient calls
        // have no listener-level Host validation.
        return true;
    };
    let Ok(host) = std::str::from_utf8(host) else {
        return false;
    };
    host.parse::<std::net::SocketAddr>().is_ok()
        || host.parse::<std::net::IpAddr>().is_ok()
        || host
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
            .is_some_and(|value| value.parse::<std::net::Ipv6Addr>().is_ok())
}
fn browser_safety_error(req: &Request) -> Option<Response> {
    if !request_host_is_ip_literal(req) {
        Some(error_response(StatusCode::BAD_REQUEST, "invalid_host"))
    } else if !same_origin_or_absent(req) {
        Some(error_response(StatusCode::FORBIDDEN, "UNTRUSTED_ORIGIN"))
    } else {
        None
    }
}

pub(super) fn build_app<S, T>(store: Arc<Mutex<S>>, transport: Arc<T>) -> App
where
    S: Store + Send + 'static,
    T: Transport + 'static,
{
    let state = Arc::new(ApiState { store, transport });

    let devices_state = Arc::clone(&state);
    let info_state = Arc::clone(&state);
    let active_app_state = Arc::clone(&state);
    let apps_state = Arc::clone(&state);
    let key_state = Arc::clone(&state);
    let launch_state = Arc::clone(&state);
    let undo_plan_state = Arc::clone(&state);
    let undo_state = Arc::clone(&state);

    let health = RouteEntry::from_route(
        documented_route(
            HEALTH_PATH,
            "health",
            "Health status",
            "Returns whether the HTTP service is responding.",
        ),
        |_ctx: &RequestContext, req: &mut Request| -> BoxFuture<'_, Response> {
            let safety_error = browser_safety_error(req);
            Box::pin(async move {
                if let Some(response) = safety_error {
                    return response;
                }
                json_response(StatusCode::OK, &HealthView { status: "ok" })
            })
        },
    )
    .response_schema::<HealthView>(200, "The service is healthy.")
    .response_schema::<ErrorView>(400, "The Host header is invalid.")
    .response_schema::<ErrorView>(403, "The request Origin is not trusted.");

    let devices = with_error_schemas(
        RouteEntry::from_route(
            documented_route(
                DEVICES_PATH,
                "list_devices",
                "List known devices",
                "Returns the device rows currently stored in the inventory.",
            ),
            move |_ctx: &RequestContext, req: &mut Request| -> BoxFuture<'_, Response> {
                let safety_error = browser_safety_error(req);
                let state = Arc::clone(&devices_state);
                Box::pin(async move {
                    if let Some(response) = safety_error {
                        return response;
                    }
                    list_devices(&state)
                })
            },
        )
        .response_schema::<Vec<DeviceSummary>>(200, "The stored device list."),
        &[
            (400, "The Host header is invalid."),
            (403, "The request Origin is not trusted."),
            (500, "The inventory store could not be read."),
        ],
    );

    let info = with_error_schemas(
        RouteEntry::from_route(
            documented_route(
                INFO_PATH,
                "get_device_info",
                "Read device information",
                "Reads ECP device information and refreshes the stored access-tier observation.",
            ),
            move |_ctx: &RequestContext, req: &mut Request| -> BoxFuture<'_, Response> {
                let safety_error = browser_safety_error(req);
                let target = path_target(req);
                let state = Arc::clone(&info_state);
                Box::pin(async move {
                    if let Some(response) = safety_error {
                        return response;
                    }
                    match target {
                        Some(target) => device_info(&state, &target).await,
                        None => error_response(StatusCode::BAD_REQUEST, "missing_host"),
                    }
                })
            },
        )
        .path_schema::<String>(&["host"])
        .response_schema::<DeviceInfoView>(200, "The device information."),
        &[
            (400, "The Host header or device path parameter is invalid."),
            (
                403,
                "The request Origin or device access tier refuses this query.",
            ),
            (404, "The device is not present in the inventory."),
            (500, "The inventory store failed."),
            (502, "The device response was unavailable or malformed."),
        ],
    );

    let active_app_route = with_error_schemas(
        RouteEntry::from_route(
            documented_route(
                ACTIVE_APP_PATH,
                "get_active_app",
                "Read the active app",
                "Reads the foreground app from a device in the inventory.",
            ),
            move |_ctx: &RequestContext, req: &mut Request| -> BoxFuture<'_, Response> {
                let safety_error = browser_safety_error(req);
                let target = path_target(req);
                let state = Arc::clone(&active_app_state);
                Box::pin(async move {
                    if let Some(response) = safety_error {
                        return response;
                    }
                    match target {
                        Some(target) => active_app(&state, &target).await,
                        None => error_response(StatusCode::BAD_REQUEST, "missing_host"),
                    }
                })
            },
        )
        .path_schema::<String>(&["host"])
        .response_schema::<ActiveAppView>(200, "The active app."),
        &[
            (400, "The Host header or device path parameter is invalid."),
            (
                403,
                "The request Origin or device access tier refuses this query.",
            ),
            (404, "The device is not present in the inventory."),
            (500, "The inventory store failed."),
            (502, "The device response was unavailable or malformed."),
        ],
    );

    let apps = with_error_schemas(
        RouteEntry::from_route(
            documented_route(
                APPS_PATH,
                "list_installed_apps",
                "List installed apps",
                "Reads and parses the device's installed-app list. This query requires Permissive access.",
            ),
            move |_ctx: &RequestContext, req: &mut Request| -> BoxFuture<'_, Response> {
                let safety_error = browser_safety_error(req);
                let target = path_target(req);
                let state = Arc::clone(&apps_state);
                Box::pin(async move {
                    if let Some(response) = safety_error {
                        return response;
                    }
                    match target {
                        Some(target) => installed_apps(&state, &target).await,
                        None => error_response(StatusCode::BAD_REQUEST, "missing_host"),
                    }
                })
            },
        )
        .path_schema::<String>(&["host"])
        .response_schema::<InstalledAppsView>(200, "The device's installed apps."),
        &[
            (400, "The Host header or device path parameter is invalid."),
            (
                403,
                "The request Origin or device access tier refuses this query.",
            ),
            (404, "The device or its apps query endpoint is absent."),
            (500, "The inventory store failed."),
            (502, "The device response was unavailable or malformed."),
        ],
    );
    let undo_plan_route = with_error_schemas(
        RouteEntry::from_route(
            documented_route(
                UNDO_PLAN_PATH,
                "undo_plan",
                "Inspect the most recent reversible app action",
                "Resolves the previous app for undo without mutating the device.",
            ),
            move |_ctx: &RequestContext, req: &mut Request| -> BoxFuture<'_, Response> {
                let safety_error = browser_safety_error(req);
                let target = path_target(req);
                let state = Arc::clone(&undo_plan_state);
                Box::pin(async move {
                    if let Some(response) = safety_error {
                        return response;
                    }
                    let Some(target) = target else {
                        return error_response(StatusCode::BAD_REQUEST, "missing_host");
                    };
                    match resolve_undo_plan(&state, &target).await {
                        Ok(plan) => json_response(StatusCode::OK, &plan),
                        Err(response) => response,
                    }
                })
            },
        )
        .path_schema::<String>(&["host"])
        .response_schema::<UndoPlanView>(200, "The app that undo would restore."),
        &[
            (400, "The Host header or device path parameter is invalid."),
            (
                403,
                "The request Origin or device tier refuses the apps query.",
            ),
            (
                404,
                "No completed app action or matching installed app exists.",
            ),
            (409, "The previous app state cannot be restored."),
            (500, "The inventory or action log failed."),
            (
                502,
                "The installed-app response was unavailable or malformed.",
            ),
        ],
    );
    let undo_route = with_error_schemas(
        RouteEntry::from_route(
            mutation_route(
                UNDO_PATH,
                "undo_last",
                "Restore the previous app",
                "Relaunches the previous app through the ordinary launch approval gate.",
            ),
            move |ctx: &RequestContext, req: &mut Request| -> BoxFuture<'_, Response> {
                let safety_error = browser_safety_error(req);
                let target = path_target(req);
                let state = Arc::clone(&undo_state);
                Box::pin(async move {
                    if let Some(response) = safety_error {
                        return response;
                    }
                    let Some(target) = target else {
                        return error_response(StatusCode::BAD_REQUEST, "missing_host");
                    };
                    let body = match Json::<UndoMutationRequest>::from_request(ctx, req).await {
                        Ok(Json(body)) => body,
                        Err(error) => return json_extract_error(error),
                    };
                    let plan = match resolve_undo_plan(&state, &target).await {
                        Ok(plan) => plan,
                        Err(response) => return response,
                    };
                    if plan.action_id != body.action_id {
                        return error_response(StatusCode::from_u16(409), "undo_target_changed");
                    }
                    execute_mutation(
                        &state,
                        MutationInvocation {
                            raw_target: &target,
                            surface: "undo",
                            command: launch(plan.app_id),
                            expected_client: ClientPrincipal::parse(&body.client),
                            confirmation: body.confirm.as_deref(),
                            requested_app: Some((&plan.app_name, plan.app_id)),
                            source_action_id: Some(plan.action_id),
                        },
                    )
                    .await
                })
            },
        )
        .path_schema::<String>(&["host"])
        .request_schema::<UndoMutationRequest>(true)
        .response_schema::<MutationResponse>(200, "The approved undo result."),
        &[
            (400, "The Host header or request body is invalid."),
            (
                403,
                "The request Origin, device tier, or confirmation gate refused undo.",
            ),
            (
                404,
                "No completed app action or matching installed app exists.",
            ),
            (
                409,
                "The selected action changed or its previous app cannot be restored.",
            ),
            (500, "The inventory or action log failed."),
            (502, "The device response was unavailable or malformed."),
        ],
    );
    let key = with_error_schemas(
        RouteEntry::from_route(
            mutation_route(
                KEY_PATH,
                "keypress",
                "Press a key",
                "Authorizes one keypress against the inventory-selected device.",
            ),
            move |ctx: &RequestContext, req: &mut Request| -> BoxFuture<'_, Response> {
                let safety_error = browser_safety_error(req);
                let target = path_target(req);
                let state = Arc::clone(&key_state);
                Box::pin(async move {
                    if let Some(response) = safety_error {
                        return response;
                    }
                    let Some(target) = target else {
                        return error_response(StatusCode::BAD_REQUEST, "missing_host");
                    };
                    let body = match Json::<KeyMutationRequest>::from_request(ctx, req).await {
                        Ok(Json(body)) => body,
                        Err(error) => return json_extract_error(error),
                    };
                    let Some(command) = keypress_command(&body.key) else {
                        return error_response(StatusCode::BAD_REQUEST, "invalid_key");
                    };
                    execute_mutation(
                        &state,
                        MutationInvocation {
                            raw_target: &target,
                            surface: "key",
                            command,
                            expected_client: ClientPrincipal::parse(&body.client),
                            confirmation: body.confirm.as_deref(),
                            requested_app: None,
                            source_action_id: None,
                        },
                    )
                    .await
                })
            },
        )
        .path_schema::<String>(&["host"])
        .request_schema::<KeyMutationRequest>(true)
        .response_schema::<MutationResponse>(200, "The approved keypress result."),
        &[
            (400, "The Host header, key, or request body is invalid."),
            (
                403,
                "The request Origin, device tier, or confirmation gate refused the mutation.",
            ),
            (
                404,
                "The inventory device is absent or the requested key is unknown.",
            ),
            (413, "The JSON request body exceeds the framework limit."),
            (415, "The request body is not JSON."),
            (500, "The inventory or action log failed."),
            (502, "The device transport or response failed."),
        ],
    );

    let launch_route = with_error_schemas(
        RouteEntry::from_route(
            mutation_route(
                LAUNCH_PATH,
                "launch_app",
                "Launch an app",
                "Authorizes one app launch using the inventory-selected device and app id.",
            ),
            move |ctx: &RequestContext, req: &mut Request| -> BoxFuture<'_, Response> {
                let safety_error = browser_safety_error(req);
                let target = path_target(req);
                let state = Arc::clone(&launch_state);
                Box::pin(async move {
                    if let Some(response) = safety_error {
                        return response;
                    }
                    let Some(target) = target else {
                        return error_response(StatusCode::BAD_REQUEST, "missing_host");
                    };
                    let body = match Json::<LaunchMutationRequest>::from_request(ctx, req).await {
                        Ok(Json(body)) => body,
                        Err(error) => return json_extract_error(error),
                    };
                    execute_mutation(
                        &state,
                        MutationInvocation {
                            raw_target: &target,
                            surface: "launch",
                            command: launch(body.app_id),
                            expected_client: ClientPrincipal::parse(&body.client),
                            confirmation: body.confirm.as_deref(),
                            requested_app: body
                                .requested_app
                                .as_deref()
                                .map(|name| (name, body.app_id)),
                            source_action_id: None,
                        },
                    )
                    .await
                })
            },
        )
        .path_schema::<String>(&["host"])
        .request_schema::<LaunchMutationRequest>(true)
        .response_schema::<MutationResponse>(200, "The approved app-launch result."),
        &[
            (400, "The Host header or request body is invalid."),
            (
                403,
                "The request Origin, device tier, or confirmation gate refused the mutation.",
            ),
            (
                404,
                "The inventory device is absent or the requested app is not installed.",
            ),
            (413, "The JSON request body exceeds the framework limit."),
            (415, "The request body is not JSON."),
            (500, "The inventory or action log failed."),
            (502, "The device transport or response failed."),
        ],
    );

    App::builder()
        .route_entry(health)
        .route_entry(devices)
        .route_entry(info)
        .route_entry(active_app_route)
        .route_entry(apps)
        .route_entry(undo_plan_route)
        .route_entry(undo_route)
        .route_entry(key)
        .route_entry(launch_route)
        .openapi(
            OpenApiConfig::new()
                .title("zestroku HTTP API")
                .version("v1")
                .description("HTTP control and read routes for the local Roku inventory."),
        )
        .build()
}

async fn installed_apps<S: Store, T: Transport>(
    state: &ApiState<S, T>,
    raw_target: &str,
) -> Response {
    let target = match super::parse_device_target(raw_target) {
        Some(target) => target,
        None => return error_response(StatusCode::BAD_REQUEST, "invalid_device_target"),
    };
    let row = match super::find_device(state, &target) {
        Ok(Some(row)) => row,
        Ok(None) => return error_response(StatusCode::NOT_FOUND, "device_not_found"),
        Err(_) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, "store_error"),
    };
    let device = super::device_for_gate(&row, &target);
    let response = match super::send_checked(state, &device, &query_apps()).await {
        Ok(response) => response,
        Err(Error::TierRefused { .. }) => {
            return error_response(StatusCode::FORBIDDEN, "tier_refused");
        }
        Err(_) => return error_response(StatusCode::from_u16(502), "transport_error"),
    };

    match parse_ecp_response(&response).gate {
        EcpGate::Open => {}
        EcpGate::Limited403 => return error_response(StatusCode::FORBIDDEN, "tier_refused"),
        EcpGate::Absent404 => {
            return error_response(StatusCode::NOT_FOUND, "apps_endpoint_not_found");
        }
        EcpGate::Unknown403 | EcpGate::Other => {
            return error_response(StatusCode::from_u16(502), "ecp_response");
        }
    }

    let apps = match parse_apps(&response.body) {
        Ok(apps) => apps,
        Err(Error::TierRefused { .. }) => {
            return error_response(StatusCode::FORBIDDEN, "tier_refused");
        }
        Err(_) => return error_response(StatusCode::from_u16(502), "malformed_apps"),
    };

    json_response(
        StatusCode::OK,
        &InstalledAppsView {
            host: device.host,
            apps: apps
                .into_iter()
                .map(|app| InstalledAppView {
                    id: app.id,
                    name: app.name,
                    app_type: app.app_type,
                    subtype: app.subtype,
                    version: app.version,
                })
                .collect(),
        },
    )
}

async fn resolve_undo_plan<S: Store, T: Transport>(
    state: &ApiState<S, T>,
    raw_target: &str,
) -> Result<UndoPlanView, Response> {
    let target = super::parse_device_target(raw_target)
        .ok_or_else(|| error_response(StatusCode::BAD_REQUEST, "invalid_device_target"))?;
    let row = super::find_device(state, &target)
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "store_error"))?
        .ok_or_else(|| error_response(StatusCode::NOT_FOUND, "device_not_found"))?;
    let prior_action = {
        let store = state
            .store
            .lock()
            .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "store_error"))?;
        let actions = store
            .list_actions()
            .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "action_log_error"))?;
        actions
            .into_iter()
            .rev()
            .find(|action| is_undo_source_action(action, &target.id))
            .ok_or_else(|| error_response(StatusCode::NOT_FOUND, "undo_action_not_found"))?
    };
    let Some((device_id, app_id, app_name)) = before_app_identity(&prior_action.before_state)
    else {
        return Err(error_response(
            StatusCode::from_u16(409),
            "undo_state_unavailable",
        ));
    };
    if device_id != target.id || app_name.trim().is_empty() {
        return Err(error_response(
            StatusCode::from_u16(409),
            "undo_state_unavailable",
        ));
    }

    let device = super::device_for_gate(&row, &target);
    let response = match super::send_checked(state, &device, &query_apps()).await {
        Ok(response) => response,
        Err(Error::TierRefused { .. }) => {
            return Err(error_response(StatusCode::FORBIDDEN, "tier_refused"));
        }
        Err(_) => return Err(error_response(StatusCode::from_u16(502), "transport_error")),
    };
    match parse_ecp_response(&response).gate {
        EcpGate::Open => {}
        EcpGate::Limited403 => {
            return Err(error_response(StatusCode::FORBIDDEN, "tier_refused"));
        }
        EcpGate::Absent404 => {
            return Err(error_response(
                StatusCode::NOT_FOUND,
                "apps_endpoint_not_found",
            ));
        }
        EcpGate::Unknown403 | EcpGate::Other => {
            return Err(error_response(StatusCode::from_u16(502), "ecp_response"));
        }
    }
    let apps = parse_apps(&response.body).map_err(|error| match error {
        Error::TierRefused { .. } => error_response(StatusCode::FORBIDDEN, "tier_refused"),
        _ => error_response(StatusCode::from_u16(502), "malformed_apps"),
    })?;
    let mut matching_apps = apps.iter().filter(|app| {
        app.id.parse::<u32>().ok() == Some(app_id) && app.name.eq_ignore_ascii_case(app_name)
    });
    if matching_apps.next().is_none() {
        return Err(error_response(
            StatusCode::NOT_FOUND,
            "undo_app_not_installed",
        ));
    }
    if matching_apps.next().is_some() {
        return Err(error_response(
            StatusCode::from_u16(409),
            "undo_app_ambiguous",
        ));
    }
    Ok(UndoPlanView {
        action_id: prior_action.id,
        app_id,
        app_name: app_name.to_owned(),
    })
}

// Only completed actions with a device-bound active-app identity are eligible
// for undo. The legacy `http` label is read only for historical action rows;
// new HTTP and MCP calls use canonical client identities.
fn is_undo_source_action(action: &ActionRow, target_id: &str) -> bool {
    let target_matches = before_app_identity(&action.before_state).map_or_else(
        || action.intent.split_whitespace().nth(1) == Some(target_id),
        |(device_id, _, _)| device_id == target_id,
    );
    let successful = match (action.client.as_str(), action.surface.as_str()) {
        ("loopback-http" | "mcp-stdio" | "http", "launch" | "key" | "undo") => {
            action.result == "completed"
        }
        ("cli", "launch" | "key") => action
            .result
            .strip_prefix(action.surface.as_str())
            .is_some_and(|suffix| {
                suffix == " dispatched; after_state snapshot on next active-app read"
            }),
        ("cli", "install") => action.result == "installer reported success",
        _ => false,
    };
    action.decision == "approved" && successful && target_matches
}

fn keypress_command(key: &str) -> Option<zestroku_proto::Command> {
    let is_segment_byte = |byte: u8| {
        byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'-' | b'.'
                    | b'_'
                    | b'~'
                    | b'!'
                    | b'$'
                    | b'&'
                    | 0x27
                    | b'('
                    | b')'
                    | b'*'
                    | b'+'
                    | b','
                    | b';'
                    | b'='
                    | b':'
                    | b'@'
            )
    };
    if key.is_empty() || !key.bytes().all(is_segment_byte) {
        return None;
    }
    Some(keypress_raw(key))
}

fn json_extract_error(error: JsonExtractError) -> Response {
    match error {
        JsonExtractError::UnsupportedMediaType { .. } => {
            error_response(StatusCode::from_u16(415), "unsupported_media_type")
        }
        JsonExtractError::PayloadTooLarge { .. } => {
            error_response(StatusCode::from_u16(413), "request_too_large")
        }
        JsonExtractError::ReadError { .. } | JsonExtractError::DeserializeError { .. } => {
            error_response(StatusCode::BAD_REQUEST, "invalid_json")
        }
    }
}

/// Bundled per-request mutation inputs: keeps the handler arity within the
/// Clippy limit with no behavior change. All fields are plain caller data;
/// the trusted principal still derives inside from the stored grant.
struct MutationInvocation<'a> {
    raw_target: &'a str,
    surface: &'static str,
    command: zestroku_proto::Command,
    expected_client: Option<ClientPrincipal>,
    confirmation: Option<&'a str>,
    requested_app: Option<(&'a str, u32)>,
    source_action_id: Option<i64>,
}

async fn execute_mutation<S: Store, T: Transport>(
    state: &ApiState<S, T>,
    call: MutationInvocation<'_>,
) -> Response {
    let MutationInvocation {
        raw_target,
        surface,
        command,
        expected_client,
        confirmation,
        requested_app,
        source_action_id,
    } = call;
    // This is the caller's requested label, not an authorization identity.
    // Never persist an approval token supplied again through that label.
    if requested_app.is_some_and(|(name, _)| {
        name.trim().is_empty()
            || confirmation
                .filter(|token| !token.is_empty())
                .is_some_and(|token| {
                    let token = token.as_bytes();
                    name.as_bytes().windows(token.len()).any(|candidate| {
                        candidate
                            .iter()
                            .zip(token)
                            .all(|(left, right)| left.eq_ignore_ascii_case(right))
                    })
                })
    }) {
        return error_response(StatusCode::BAD_REQUEST, "invalid_requested_app");
    }
    let target = match super::parse_device_target(raw_target) {
        Some(target) => target,
        None => return error_response(StatusCode::BAD_REQUEST, "invalid_device_target"),
    };
    let row = match super::find_device(state, &target) {
        Ok(Some(row)) => row,
        Ok(None) => return error_response(StatusCode::NOT_FOUND, "device_not_found"),
        Err(_) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, "store_error"),
    };
    let mut device = super::device_for_gate(&row, &target);
    let policy = match zestroku_core::policy::Policy::load_default() {
        Ok(policy) => policy,
        Err(_) => {
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, "policy_unavailable");
        }
    };

    // Match the CLI contract: snapshot before the shared authorization gate.
    // A failed read is represented honestly and never treated as a real state.
    let before = match super::send_checked(state, &device, &query("active-app")).await {
        Ok(response) => parse_active_app(&response.body)
            .map(|app| (app.app_id, app.name))
            .unwrap_or((None, "unparseable".to_owned())),
        Err(_) => (None, "unreachable".to_owned()),
    };
    let action_id = {
        let store = match state.store.lock() {
            Ok(store) => store,
            Err(_) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, "store_error"),
        };
        if let Some(source_action_id) = source_action_id {
            let latest_source_action = match store.list_actions() {
                Ok(actions) => actions
                    .into_iter()
                    .rev()
                    .find(|action| is_undo_source_action(action, &target.id))
                    .map(|action| action.id),
                Err(_) => {
                    return error_response(StatusCode::INTERNAL_SERVER_ERROR, "action_log_error");
                }
            };
            if latest_source_action != Some(source_action_id) {
                return error_response(StatusCode::from_u16(409), "undo_target_changed");
            }
        }
        // Re-read the recorded tier at the authorization linearization point:
        // a concurrent tier update during the snapshot await above must not
        // be bypassed by the pre-snapshot value. No await is held under this
        // guard; the tier and the gate/token consumption below are atomic.
        let (tier_source, recorded_tier) = match recorded_tier_for(&*store, &row.ip, target.port) {
            Ok(Some(tier)) => ("recorded", tier.access_tier()),
            Ok(None) => ("unknown-unprobed", AccessTier::Open),
            Err(_) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, "store_error"),
        };
        device.access_tier = recorded_tier;
        let before_state = marked_before_app_state(tier_source, &target.id, before.0, &before.1);
        let intent = match (source_action_id, requested_app) {
            (Some(source_id), Some((name, app_id))) => format!(
                "{surface} {} {}; source_action=#{source_id}; requested_app={name:?}; resolved_app_id={app_id}",
                target.id, command.path,
            ),
            (Some(source_id), None) => {
                format!(
                    "{surface} {} {}; source_action=#{source_id}",
                    target.id, command.path
                )
            }
            (None, Some((name, app_id))) => format!(
                "{surface} {} {}; requested_app={name:?}; resolved_app_id={app_id}",
                target.id, command.path,
            ),
            (None, None) => format!("{surface} {} {}", target.id, command.path),
        };
        match authorize_mutation(
            &*store,
            &device,
            &command,
            MutationContext {
                client: expected_client.map_or("unknown", ClientPrincipal::as_str),
                surface,
                intent: &intent,
                before_state: &before_state,
            },
            confirmation,
            &policy,
        ) {
            Ok(action_id) => action_id,
            Err(MutationError::ConfirmationRequired { .. }) => {
                return error_response(StatusCode::FORBIDDEN, "confirmation_required");
            }
            Err(MutationError::PolicyDenied { .. }) => {
                return error_response(StatusCode::FORBIDDEN, "policy_denied");
            }
            Err(MutationError::TierRefused(_)) => {
                return error_response(StatusCode::FORBIDDEN, "tier_refused");
            }
            Err(_) => {
                return error_response(StatusCode::INTERNAL_SERVER_ERROR, "mutation_gate_error");
            }
        }
    };

    let (outcome, failure) = match state
        .transport
        .send(&device.host, device.port, &command)
        .await
    {
        Err(_) => (
            "dispatch_failed",
            Some((StatusCode::from_u16(502), "transport_error")),
        ),
        Ok(response) => match parse_ecp_response(&response).gate {
            EcpGate::Open => ("completed", None),
            EcpGate::Limited403 => (
                "device_tier_refused",
                Some((StatusCode::FORBIDDEN, "tier_refused")),
            ),
            EcpGate::Absent404 => {
                let code = if surface == "launch" {
                    "app_not_installed"
                } else {
                    "invalid_key"
                };
                (code, Some((StatusCode::NOT_FOUND, code)))
            }
            EcpGate::Unknown403 | EcpGate::Other => (
                "device_response_unclassified",
                Some((StatusCode::from_u16(502), "ecp_response")),
            ),
        },
    };
    {
        let store = match state.store.lock() {
            Ok(store) => store,
            Err(_) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, "action_log_error"),
        };
        if finish_mutation(&*store, action_id, outcome).is_err() {
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, "action_log_error");
        }
    }
    if let Some((status, code)) = failure {
        return error_response(status, code);
    }
    json_response(
        StatusCode::OK,
        &MutationResponse {
            action_id,
            target: target.id,
            surface,
            outcome,
        },
    )
}
