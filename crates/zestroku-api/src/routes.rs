#![forbid(unsafe_code)]

use std::sync::{Arc, Mutex};

use fastapi::OpenApiConfig;
use fastapi::core::app::RouteEntry;
use fastapi::core::{App, BoxFuture, Method, Request, RequestContext, Response, StatusCode};
use fastapi::fastapi_openapi;
use fastapi::router::Route;
use serde::Serialize;
use zestroku_core::store::Store;
use zestroku_proto::{EcpGate, Transport, parse_apps, parse_ecp_response, query_apps};
use zestroku_types::Error;

use super::{
    ActiveAppView, ApiState, DeviceInfoView, DeviceSummary, ErrorView, HealthView, active_app,
    device_info, error_response, json_response, list_devices, path_target,
};

const HEALTH_PATH: &str = "/health";
const DEVICES_PATH: &str = "/v1/devices";
const INFO_PATH: &str = "/v1/devices/{host}/info";
const ACTIVE_APP_PATH: &str = "/v1/devices/{host}/active-app";
const APPS_PATH: &str = "/v1/devices/{host}/apps";

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

fn with_error_schemas(mut route: RouteEntry, errors: &[(u16, &'static str)]) -> RouteEntry {
    for &(status, description) in errors {
        route = route.response_schema::<ErrorView>(status, description);
    }
    route
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

    let health = RouteEntry::from_route(
        documented_route(
            HEALTH_PATH,
            "health",
            "Health status",
            "Returns whether the HTTP service is responding.",
        ),
        |_ctx: &RequestContext, _req: &mut Request| -> BoxFuture<'_, Response> {
            Box::pin(async { json_response(StatusCode::OK, &HealthView { status: "ok" }) })
        },
    )
    .response_schema::<HealthView>(200, "The service is healthy.");

    let devices = with_error_schemas(
        RouteEntry::from_route(
            documented_route(
                DEVICES_PATH,
                "list_devices",
                "List known devices",
                "Returns the device rows currently stored in the inventory.",
            ),
            move |_ctx: &RequestContext, _req: &mut Request| -> BoxFuture<'_, Response> {
                let state = Arc::clone(&devices_state);
                Box::pin(async move { list_devices(&state) })
            },
        )
        .response_schema::<Vec<DeviceSummary>>(200, "The stored device list."),
        &[(500, "The inventory store could not be read.")],
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
                let target = path_target(req);
                let state = Arc::clone(&info_state);
                Box::pin(async move {
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
            (400, "The device path parameter is invalid."),
            (403, "The device access tier refuses this query."),
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
                let target = path_target(req);
                let state = Arc::clone(&active_app_state);
                Box::pin(async move {
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
            (400, "The device path parameter is invalid."),
            (403, "The device access tier refuses this query."),
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
                let target = path_target(req);
                let state = Arc::clone(&apps_state);
                Box::pin(async move {
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
            (400, "The device path parameter is invalid."),
            (403, "The device's access tier refuses the query."),
            (404, "The device or its apps query endpoint is absent."),
            (500, "The inventory store failed."),
            (502, "The device response was unavailable or malformed."),
        ],
    );

    App::builder()
        .route_entry(health)
        .route_entry(devices)
        .route_entry(info)
        .route_entry(active_app_route)
        .route_entry(apps)
        .openapi(
            OpenApiConfig::new()
                .title("zestroku HTTP API")
                .version("v1")
                .description("Read-only HTTP routes for the local Roku device inventory."),
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
