use std::sync::Arc;

use crate::handlers::admin::request::{AdminAppState, AdminRequestContext};
use crate::state::{TurnStateConfig, TurnStateScope};
use crate::GatewayError;
use axum::{
    body::{Body, Bytes},
    http,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Default, Deserialize)]
struct ProbeRequest {
    key_ids: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct DryRunRequest {
    dry_run: bool,
}

fn bad_request(detail: impl Into<String>) -> Response<Body> {
    (
        http::StatusCode::BAD_REQUEST,
        Json(json!({ "detail": detail.into() })),
    )
        .into_response()
}

pub(super) async fn maybe_build_local_admin_turn_state_response(
    state: &AdminAppState<'_>,
    request_context: &AdminRequestContext<'_>,
    request_body: Option<&Bytes>,
) -> Result<Option<Response<Body>>, GatewayError> {
    let Some(decision) = request_context.decision() else {
        return Ok(None);
    };
    if decision.route_family.as_deref() != Some("codex_turn_state_manage") {
        return Ok(None);
    }

    let app = state.app();
    let runtime = &app.codex_turn_state;
    let route_kind = decision.route_kind.as_deref().unwrap_or_default();

    match route_kind {
        "status" if request_context.method() == http::Method::GET => {
            return Ok(Some(Json(runtime.status_json(app).await?).into_response()));
        }
        "scope_get" if request_context.method() == http::Method::GET => {
            return Ok(Some(Json(runtime.scope(app).await?).into_response()));
        }
        "scope_set" if request_context.method() == http::Method::PUT => {
            let Some(body) = request_body else {
                return Ok(Some(bad_request("请求体不能为空")));
            };
            let Ok(scope) = serde_json::from_slice::<TurnStateScope>(body) else {
                return Ok(Some(bad_request("探测范围格式无效")));
            };
            if let Err(detail) = scope.validate() {
                return Ok(Some(bad_request(detail)));
            }
            return Ok(Some(
                Json(runtime.update_scope(app, scope).await?).into_response(),
            ));
        }
        "config_get" if request_context.method() == http::Method::GET => {
            return Ok(Some(Json(runtime.config(app).await?).into_response()));
        }
        "config_set" if request_context.method() == http::Method::PUT => {
            let Some(body) = request_body else {
                return Ok(Some(bad_request("请求体不能为空")));
            };
            let Ok(config) = serde_json::from_slice::<TurnStateConfig>(body) else {
                return Ok(Some(bad_request("模块配置格式无效")));
            };
            if let Err(detail) = config.validate() {
                return Ok(Some(bad_request(detail)));
            }
            return Ok(Some(
                Json(runtime.update_config(app, config).await?).into_response(),
            ));
        }
        "dry_run" if request_context.method() == http::Method::PUT => {
            let Some(body) = request_body else {
                return Ok(Some(bad_request("请求体不能为空")));
            };
            let Ok(payload) = serde_json::from_slice::<DryRunRequest>(body) else {
                return Ok(Some(bad_request("dry_run 必须是布尔值")));
            };
            runtime.set_dry_run(app, payload.dry_run).await?;
            return Ok(Some(
                Json(json!({ "dry_run": payload.dry_run })).into_response(),
            ));
        }
        "clear" if request_context.method() == http::Method::POST => {
            let cleared = runtime.clear(app).await?;
            return Ok(Some(Json(json!({ "cleared": cleared })).into_response()));
        }
        "probe_cancel" if request_context.method() == http::Method::POST => {
            return Ok(Some(Json(runtime.cancel_probe(app).await?).into_response()));
        }
        "probe_start" if request_context.method() == http::Method::POST => {
            let requested = request_body
                .and_then(|body| serde_json::from_slice::<ProbeRequest>(body).ok())
                .and_then(|payload| payload.key_ids);
            let (run, claimed) = runtime.start_probe_claim(app, requested.clone()).await?;
            if claimed {
                let worker_runtime = Arc::clone(runtime);
                let worker_app = app.clone();
                tokio::spawn(async move {
                    worker_runtime.run_probe(worker_app, requested).await;
                });
            }
            return Ok(Some(Json(run).into_response()));
        }
        "proxy_check" if request_context.method() == http::Method::POST => {
            return Ok(Some(Json(runtime.proxy_check(app).await?).into_response()));
        }
        _ => {}
    }

    Ok(None)
}
