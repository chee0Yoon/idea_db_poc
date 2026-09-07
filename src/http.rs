//! HTTP surface. Routing, auth, body limits and readiness gating; the domain
//! work lives in `mutation`, `backup` and `query`.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use tower_http::services::{ServeDir, ServeFile};

use crate::config::Config;
use crate::error::{ApiError, ApiResult};
use crate::limits;
use crate::model::PROTOCOL_VERSION;
use crate::neo4j::Neo4j;
use crate::{backup, query, store};

pub struct AppState {
    pub config: Config,
    pub neo4j: Neo4j,
}

pub type Shared = Arc<AppState>;

pub fn router(state: Shared) -> Router {
    let static_dir = state.config.static_dir.clone();
    let index = format!("{}/index.html", static_dir.trim_end_matches('/'));

    let api = Router::new()
        .route("/api/state", get(get_state))
        .route("/api/records/{id}", get(get_record))
        .route("/api/captures", get(get_captures))
        .route("/api/occurrences", get(get_occurrences))
        .route("/api/snapshot", get(get_snapshot))
        .route("/api/search", post(post_search))
        .route("/api/goals", get(get_goals))
        .route("/api/export", get(get_export))
        .route("/api/packages/validate", axum::routing::any(mcp_only))
        .route("/api/packages/apply", axum::routing::any(mcp_only))
        .route("/api/occurrences/replace", axum::routing::any(mcp_only))
        .route("/api/import", axum::routing::any(mcp_only))
        .route("/api/atomization/preview", axum::routing::any(mcp_only))
        .route("/api/atomization/prepare", axum::routing::any(mcp_only))
        .layer(DefaultBodyLimit::max(limits::MAX_BODY_BYTES));

    Router::new()
        .route("/health/live", get(health_live))
        .route("/health/ready", get(health_ready))
        .merge(api)
        // Static files are served from disk at runtime so the UI can be
        // rewritten without rebuilding the binary.
        .fallback_service(ServeDir::new(&static_dir).fallback(ServeFile::new(index)))
        .with_state(state)
}

async fn mcp_only() -> (StatusCode, Json<Value>) {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        Json(
            json!({"code":"mcp_required","message":"Changes are available only through the local stdio MCP server."}),
        ),
    )
}

// ---------------------------------------------------------------- plumbing

type Params = BTreeMap<String, String>;

fn params(raw: Query<BTreeMap<String, String>>) -> Params {
    raw.0
}

fn authorize(state: &AppState, headers: &HeaderMap) -> ApiResult<()> {
    let Some(expected) = &state.config.token else {
        return Ok(());
    };
    let presented = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    // Length-independent comparison is not required here: the token is a local
    // deployment secret, not a per-user credential.
    if presented == expected {
        Ok(())
    } else {
        Err(ApiError::unauthorized())
    }
}

/// Every `/api/*` request needs an authenticated caller and a live database.
async fn enter(state: &AppState, headers: &HeaderMap) -> ApiResult<()> {
    authorize(state, headers)?;
    store::ping(&state.neo4j)
        .await
        .map_err(|e| ApiError::not_ready(Some(e.message)))?;
    Ok(())
}

/// A write additionally has to arrive as JSON.
///
/// `text/plain`, `application/x-www-form-urlencoded` and `multipart/form-data`
/// are the three types a browser will POST cross-origin without a preflight. A
/// page on any origin could otherwise aim a JSON body at a local deployment and
/// have it applied, because with no `IDEA_DB_TOKEN` set there is nothing else to
/// stop it. Demanding a JSON media type forces a preflight, which we never
/// answer: no CORS headers are sent, so the browser blocks the request. This is
/// a content-type check, not an origin check — a non-browser client is
/// unaffected.
async fn enter_write(state: &AppState, headers: &HeaderMap) -> ApiResult<()> {
    authorize(state, headers)?;
    require_json(headers)?;
    store::ping(&state.neo4j)
        .await
        .map_err(|e| ApiError::not_ready(Some(e.message)))?;
    Ok(())
}

fn require_json(headers: &HeaderMap) -> ApiResult<()> {
    let raw = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    // Compare the media type only; parameters such as `charset=utf-8` are fine.
    let media_type = raw
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if media_type == "application/json" || media_type.ends_with("+json") {
        return Ok(());
    }
    Err(ApiError {
        status: StatusCode::UNSUPPORTED_MEDIA_TYPE,
        code: "unsupported_media_type",
        message: format!(
            "this endpoint takes application/json; received '{}'",
            if raw.is_empty() { "(none)" } else { raw }
        ),
        details: None,
    })
}

fn parse_json(body: &Bytes) -> ApiResult<Value> {
    serde_json::from_slice(body).map_err(|e| ApiError::bad_request(format!("invalid JSON: {e}")))
}

/// Read endpoints all share one consistent snapshot of state.
async fn read_ctx(state: &AppState) -> ApiResult<crate::validate::Context> {
    crate::retry::transient(|| store::read_context(&state.neo4j)).await
}

// ---------------------------------------------------------------- health

async fn health_live() -> Json<Value> {
    Json(json!({ "status": "live" }))
}

/// Readiness runs a real query every time. A flag set once at startup would go
/// on reporting ready after the database disappeared.
async fn health_ready(State(state): State<Shared>) -> Response {
    match store::ping(&state.neo4j).await {
        Ok(seq) => Json(json!({
            "status": "ready",
            "neo4j": {
                "uri": state.neo4j.uri(),
                "database": state.neo4j.database(),
                "reachable": true,
            },
            "seq": seq,
            "protocol_version": PROTOCOL_VERSION,
        }))
        .into_response(),
        Err(e) => ApiError::not_ready(Some(e.message)).into_response(),
    }
}

// ---------------------------------------------------------------- reads

async fn get_state(
    State(state): State<Shared>,
    headers: HeaderMap,
    raw: Query<BTreeMap<String, String>>,
) -> ApiResult<Json<Value>> {
    enter(&state, &headers).await?;
    let ctx = read_ctx(&state).await?;
    query::state(&ctx, &params(raw)).map(Json)
}

async fn get_record(
    State(state): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
    raw: Query<BTreeMap<String, String>>,
) -> ApiResult<Json<Value>> {
    enter(&state, &headers).await?;
    let ctx = read_ctx(&state).await?;
    query::record(&ctx, &id, &params(raw)).map(Json)
}

async fn get_captures(
    State(state): State<Shared>,
    headers: HeaderMap,
    raw: Query<BTreeMap<String, String>>,
) -> ApiResult<Json<Value>> {
    enter(&state, &headers).await?;
    let ctx = read_ctx(&state).await?;
    query::captures(&ctx, &params(raw)).map(Json)
}

async fn get_snapshot(
    State(state): State<Shared>,
    headers: HeaderMap,
    raw: Query<BTreeMap<String, String>>,
) -> ApiResult<Json<Value>> {
    enter(&state, &headers).await?;
    let ctx = read_ctx(&state).await?;
    query::snapshot(&ctx, &params(raw)).map(Json)
}

async fn get_goals(
    State(state): State<Shared>,
    headers: HeaderMap,
    raw: Query<BTreeMap<String, String>>,
) -> ApiResult<Json<Value>> {
    enter(&state, &headers).await?;
    let ctx = read_ctx(&state).await?;
    query::goals(&ctx, &params(raw)).map(Json)
}

async fn get_occurrences(
    State(state): State<Shared>,
    headers: HeaderMap,
    raw: Query<BTreeMap<String, String>>,
) -> ApiResult<Json<Value>> {
    enter(&state, &headers).await?;
    let ctx = read_ctx(&state).await?;
    query::occurrences(&ctx, &params(raw)).map(Json)
}

async fn post_search(
    State(state): State<Shared>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    enter_write(&state, &headers).await?;
    let ctx = read_ctx(&state).await?;
    query::search(&ctx, parse_json(&body)?).map(Json)
}

async fn get_export(
    State(state): State<Shared>,
    headers: HeaderMap,
    raw: Query<BTreeMap<String, String>>,
) -> ApiResult<Json<Value>> {
    enter(&state, &headers).await?;
    let params = params(raw);
    let project_id = params.get("project_id").cloned();
    let include_receipts = params
        .get("include_receipts")
        .map(|v| v != "false")
        .unwrap_or(true);
    backup::export(&state.neo4j, project_id, include_receipts)
        .await
        .map(Json)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Points at a port nothing listens on, so any handler that reaches the
    /// database fails fast and visibly with `not_ready`. A test that expects a
    /// rejection therefore also proves the rejection happened *before* the
    /// database was consulted.
    fn unreachable_state(token: Option<&str>) -> Shared {
        let config = Config {
            bind: "127.0.0.1:0".to_string(),
            neo4j_uri: "http://127.0.0.1:1".to_string(),
            neo4j_user: "neo4j".to_string(),
            neo4j_password: None,
            neo4j_database: "neo4j".to_string(),
            token: token.map(str::to_string),
            static_dir: "./static".to_string(),
        };
        let neo4j = Neo4j::new(&config).expect("client builds");
        Arc::new(AppState { config, neo4j })
    }

    /// Serve the real router on an ephemeral port and speak HTTP to it, so the
    /// header handling under test is the one requests actually meet.
    async fn serve(state: Shared) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("ephemeral port");
        let addr = listener.local_addr().expect("bound address");
        let app = router(state);
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}"), handle)
    }

    struct Reply {
        status: u16,
        code: Option<String>,
    }

    async fn post(
        base: &str,
        path: &str,
        content_type: Option<&str>,
        token: Option<&str>,
    ) -> Reply {
        let client = reqwest::Client::new();
        let mut req = client.post(format!("{base}{path}")).body("{}");
        if let Some(ct) = content_type {
            req = req.header(reqwest::header::CONTENT_TYPE, ct);
        }
        if let Some(t) = token {
            req = req.header(reqwest::header::AUTHORIZATION, format!("Bearer {t}"));
        }
        let resp = req.send().await.expect("request reaches the test server");
        let status = resp.status().as_u16();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        Reply {
            status,
            code: body.get("code").and_then(Value::as_str).map(str::to_string),
        }
    }

    const WRITE_ROUTES: [&str; 1] = ["/api/search"];

    #[tokio::test]
    async fn a_post_without_a_json_media_type_is_refused_before_the_database() {
        let (base, server) = serve(unreachable_state(None)).await;
        // The three types a browser sends cross-origin without a preflight.
        for content_type in [
            "text/plain;charset=UTF-8",
            "application/x-www-form-urlencoded",
            "multipart/form-data; boundary=x",
        ] {
            for route in WRITE_ROUTES {
                let reply = post(&base, route, Some(content_type), None).await;
                assert_eq!(
                    reply.status, 415,
                    "{route} accepted {content_type} (got {})",
                    reply.status
                );
                assert_eq!(reply.code.as_deref(), Some("unsupported_media_type"));
            }
        }
        // A missing header is refused the same way.
        let reply = post(&base, "/api/search", None, None).await;
        assert_eq!(reply.status, 415);
        server.abort();
    }

    #[tokio::test]
    async fn a_json_media_type_passes_the_gate_and_reaches_the_database() {
        let (base, server) = serve(unreachable_state(None)).await;
        // 503 is the point: the gate let these through, and the unreachable
        // database is what stopped them.
        for content_type in [
            "application/json",
            "application/json; charset=utf-8",
            "APPLICATION/JSON",
            "application/vnd.idea-db+json",
        ] {
            let reply = post(&base, "/api/search", Some(content_type), None).await;
            assert_eq!(
                reply.status, 503,
                "{content_type} was rejected by the media-type gate"
            );
            assert_eq!(reply.code.as_deref(), Some("not_ready"));
        }
        server.abort();
    }

    #[tokio::test]
    async fn a_token_is_demanded_before_the_media_type_and_the_database() {
        let (base, server) = serve(unreachable_state(Some("s3cret"))).await;

        let anonymous = post(&base, "/api/search", Some("text/plain"), None).await;
        assert_eq!(
            anonymous.status, 401,
            "an unauthenticated write must not leak"
        );
        assert_eq!(anonymous.code.as_deref(), Some("unauthorized"));

        let wrong = post(&base, "/api/search", Some("application/json"), Some("nope")).await;
        assert_eq!(wrong.status, 401);

        // Authenticated but still the wrong media type.
        let typed = post(&base, "/api/search", Some("text/plain"), Some("s3cret")).await;
        assert_eq!(typed.status, 415);

        // Authenticated and correctly typed: only the database is missing.
        let allowed = post(
            &base,
            "/api/search",
            Some("application/json"),
            Some("s3cret"),
        )
        .await;
        assert_eq!(allowed.status, 503);
        server.abort();
    }

    #[tokio::test]
    async fn liveness_stays_open_and_reads_stay_authenticated() {
        let (base, server) = serve(unreachable_state(Some("s3cret"))).await;
        let client = reqwest::Client::new();

        let live = client
            .get(format!("{base}/health/live"))
            .send()
            .await
            .expect("liveness answers");
        assert_eq!(
            live.status().as_u16(),
            200,
            "liveness must not need a token"
        );

        let read = client
            .get(format!("{base}/api/state"))
            .send()
            .await
            .expect("read answers");
        assert_eq!(read.status().as_u16(), 401, "reads still need the token");
        server.abort();
    }

    /// The gate is a media-type check, not an origin check: no CORS headers are
    /// sent, so a browser cannot use the preflight it now needs.
    #[tokio::test]
    async fn no_cors_headers_are_offered() {
        let (base, server) = serve(unreachable_state(None)).await;
        let resp = reqwest::Client::new()
            .post(format!("{base}/api/search"))
            .header(reqwest::header::CONTENT_TYPE, "text/plain")
            .header(reqwest::header::ORIGIN, "https://evil.example")
            .body("{}")
            .send()
            .await
            .expect("request answers");
        assert_eq!(resp.status().as_u16(), 415);
        for header in [
            "access-control-allow-origin",
            "access-control-allow-credentials",
            "access-control-allow-headers",
        ] {
            assert!(
                resp.headers().get(header).is_none(),
                "{header} must not be sent"
            );
        }
        server.abort();
    }
    #[tokio::test]
    async fn writes_are_unavailable_even_with_valid_credentials() {
        let (base, server) = serve(unreachable_state(Some("s3cret"))).await;
        for route in [
            "/api/packages/apply",
            "/api/packages/validate",
            "/api/occurrences/replace",
            "/api/import",
            "/api/captures",
            "/api/atomization/preview",
            "/api/atomization/prepare",
        ] {
            for method in [
                reqwest::Method::POST,
                reqwest::Method::PUT,
                reqwest::Method::PATCH,
                reqwest::Method::DELETE,
            ] {
                let response = reqwest::Client::new()
                    .request(method, format!("{base}{route}"))
                    .bearer_auth("s3cret")
                    .json(&json!({}))
                    .send()
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED, "{route}");
            }
        }
        server.abort();
    }
}
