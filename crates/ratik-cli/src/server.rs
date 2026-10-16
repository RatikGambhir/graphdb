//! `ratik serve`: the HTTP API and web console.
//!
//! | Method | Path | |
//! |---|---|---|
//! | GET  | `/` | web console |
//! | GET  | `/v1/health` | liveness (never needs a token) |
//! | POST | `/v1/query` | `{"query": "...", "params": {...}}` → `{columns, rows, stats, time_ms}` |
//! | POST | `/v1/call/{name}` | `{"params": {...}}` → same, for a stored query |
//! | GET  | `/v1/schema` | types, properties, stored queries, DDL |
//! | GET  | `/v1/stats` | counts and uptime |
//! | POST | `/v1/admin/snapshot` | compact the log now |
//! | POST | `/v1/admin/shutdown` | graceful stop (used by `ratik stop`) |

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Context as _;
use axum::body::Bytes;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use ratikdb::{Database, Options, Params, SyncMode, Value};
use serde::{Deserialize, Serialize};
use serde_json::{Value as J, json};
use tokio::sync::Notify;
use tower_http::cors::{AllowOrigin, CorsLayer};

use crate::{brand, paths};

const CONSOLE: &str = include_str!("../assets/console.html");

pub struct AppState {
    pub db: Arc<Database>,
    pub token: Option<String>,
    pub started: Instant,
    pub shutdown: Notify,
}

/// Uniform JSON error body: `{"error": {"code", "message"}}`.
#[derive(Debug)]
pub enum ApiError {
    Db(ratikdb::Error),
    BadRequest(String),
    Unauthorized,
    Internal(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::Db(e) => {
                let status = match &e {
                    ratikdb::Error::Constraint(_) => StatusCode::CONFLICT,
                    ratikdb::Error::NotFound(_) => StatusCode::NOT_FOUND,
                    ratikdb::Error::Io(_)
                    | ratikdb::Error::Corrupt(_)
                    | ratikdb::Error::Locked(_) => {
                        tracing::error!(error = %e, "storage failure");
                        StatusCode::INTERNAL_SERVER_ERROR
                    }
                    _ => StatusCode::BAD_REQUEST,
                };
                (status, e.code().to_owned(), e.to_string())
            }
            Self::BadRequest(m) => (StatusCode::BAD_REQUEST, "bad_request".into(), m),
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "unauthorized".into(),
                "missing or wrong token (send `Authorization: Bearer <token>`)".into(),
            ),
            Self::Internal(m) => {
                tracing::error!(error = %m, "internal error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal".into(),
                    "internal server error".into(),
                )
            }
        };
        (
            status,
            Json(json!({ "error": { "code": code, "message": message } })),
        )
            .into_response()
    }
}

impl From<ratikdb::Error> for ApiError {
    fn from(e: ratikdb::Error) -> Self {
        Self::Db(e)
    }
}

#[derive(Deserialize, Default)]
struct QueryBody {
    #[serde(default)]
    query: String,
    #[serde(default)]
    params: serde_json::Map<String, J>,
}

fn parse_body(bytes: &Bytes) -> Result<QueryBody, ApiError> {
    if bytes.is_empty() {
        return Ok(QueryBody::default());
    }
    serde_json::from_slice(bytes)
        .map_err(|e| ApiError::BadRequest(format!("invalid JSON body: {e}")))
}

fn to_params(map: serde_json::Map<String, J>) -> Params {
    map.into_iter()
        .map(|(k, v)| (k, Value::from_json(v)))
        .collect()
}

async fn run_blocking<T: Send + 'static>(
    f: impl FnOnce() -> ratikdb::Result<T> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .map_err(ApiError::Db)
}

fn timed(result: &ratikdb::QueryResult, started: Instant) -> Json<J> {
    let mut body = result.to_json();
    body["time_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
    Json(body)
}

async fn query(State(s): State<Arc<AppState>>, body: Bytes) -> Result<Json<J>, ApiError> {
    let req = parse_body(&body)?;
    if req.query.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "body must include a non-empty \"query\"".into(),
        ));
    }
    let (db, started) = (s.db.clone(), Instant::now());
    let params = to_params(req.params);
    let result = run_blocking(move || db.execute_with(&req.query, &params)).await?;
    Ok(timed(&result, started))
}

async fn call(
    State(s): State<Arc<AppState>>,
    Path(name): Path<String>,
    body: Bytes,
) -> Result<Json<J>, ApiError> {
    let req = parse_body(&body)?;
    let (db, started) = (s.db.clone(), Instant::now());
    let params = to_params(req.params);
    let result = run_blocking(move || db.call(&name, &params)).await?;
    Ok(timed(&result, started))
}

async fn health() -> Json<J> {
    Json(json!({ "status": "ok", "name": "RatikDB", "version": env!("CARGO_PKG_VERSION") }))
}

async fn schema(State(s): State<Arc<AppState>>) -> Result<Json<J>, ApiError> {
    let db = s.db.clone();
    Ok(Json(run_blocking(move || db.schema_json()).await?))
}

async fn stats(State(s): State<Arc<AppState>>) -> Result<Json<J>, ApiError> {
    let db = s.db.clone();
    let st = run_blocking(move || db.stats()).await?;
    Ok(Json(json!({
        "nodes": st.nodes,
        "edges": st.edges,
        "node_types": st.node_types,
        "edge_types": st.edge_types,
        "stored_queries": st.stored_queries,
        "disk_bytes": st.disk_bytes,
        "uptime_secs": s.started.elapsed().as_secs(),
        "pid": std::process::id(),
        "data": s.db.path().map(|p| p.display().to_string()),
        "version": env!("CARGO_PKG_VERSION"),
    })))
}

async fn snapshot(State(s): State<Arc<AppState>>) -> Result<Json<J>, ApiError> {
    let db = s.db.clone();
    run_blocking(move || db.snapshot()).await?;
    Ok(Json(json!({ "status": "ok" })))
}

async fn shutdown(State(s): State<Arc<AppState>>) -> Json<J> {
    tracing::info!("shutdown requested over HTTP");
    s.shutdown.notify_one();
    Json(json!({ "status": "stopping" }))
}

async fn console() -> impl IntoResponse {
    (
        [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        Html(CONSOLE),
    )
}

async fn auth(
    State(s): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    if let Some(token) = &s.token {
        let given = req
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));
        if given != Some(token.as_str()) {
            return Err(ApiError::Unauthorized);
        }
    }
    Ok(next.run(req).await)
}

pub fn router(state: Arc<AppState>, cors: &[String]) -> Router {
    let api = Router::new()
        .route("/query", post(query))
        .route("/call/{name}", post(call))
        .route("/schema", get(schema))
        .route("/stats", get(stats))
        .route("/admin/snapshot", post(snapshot))
        .route("/admin/shutdown", post(shutdown))
        .layer(middleware::from_fn_with_state(state.clone(), auth))
        .route("/health", get(health));
    let mut app = Router::new()
        .route("/", get(console))
        .nest("/v1", api)
        .layer(axum::extract::DefaultBodyLimit::max(256 * 1024 * 1024))
        .with_state(state);
    // No CORS unless asked: a browser page on another origin must not be able
    // to read a local database by default.
    if !cors.is_empty() {
        let origins: Vec<HeaderValue> = cors.iter().filter_map(|o| o.parse().ok()).collect();
        app = app.layer(
            CorsLayer::new()
                .allow_origin(AllowOrigin::list(origins))
                .allow_methods([axum::http::Method::GET, axum::http::Method::POST])
                .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION]),
        );
    }
    app
}

#[derive(Debug, Clone)]
pub struct ServeOpts {
    pub data: PathBuf,
    pub memory: bool,
    pub host: String,
    pub port: u16,
    pub token: Option<String>,
    pub sync: SyncMode,
    pub apply: Vec<PathBuf>,
    pub cors: Vec<String>,
    pub cache_mb: usize,
    pub quiet: bool,
}

/// Recorded in `<data>/server.json` while the server runs.
#[derive(Debug, Serialize, Deserialize)]
pub struct ServerState {
    pub pid: u32,
    pub url: String,
    pub data: String,
    pub memory: bool,
    pub version: String,
    pub started_at_ms: u128,
}

async fn shutdown_signal(state: Arc<AppState>) {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.ok();
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
        () = state.shutdown.notified() => {}
    }
}

pub async fn serve(opts: ServeOpts) -> anyhow::Result<()> {
    std::fs::create_dir_all(&opts.data)
        .with_context(|| format!("cannot create {}", opts.data.display()))?;
    let db = if opts.memory {
        Database::in_memory()
    } else {
        let options = Options {
            sync: opts.sync,
            cache_mb: opts.cache_mb,
        };
        Database::open_with(&opts.data, options).map_err(|e| match e {
            ratikdb::Error::Locked(_) => anyhow::anyhow!(
                "{} is already in use — is another RatikDB running? Try `ratik status`",
                opts.data.display()
            ),
            e => anyhow::Error::new(e).context("could not open the database"),
        })?
    };
    for file in &opts.apply {
        let script = std::fs::read_to_string(file)
            .with_context(|| format!("cannot read {}", file.display()))?;
        let r = db
            .execute(&script)
            .with_context(|| format!("applying {}", file.display()))?;
        tracing::info!(file = %file.display(), schema_changes = r.stats.schema_changes, "applied script");
    }

    let addr: SocketAddr = format!("{}:{}", opts.host, opts.port)
        .parse()
        .with_context(|| format!("invalid address {}:{}", opts.host, opts.port))?;
    let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::AddrInUse {
            anyhow::anyhow!("port {} is already in use — is RatikDB already running? Try `ratik status` or pick another --port", opts.port)
        } else {
            anyhow::Error::new(e).context(format!("cannot listen on {addr}"))
        }
    })?;
    let url = format!("http://{}", listener.local_addr()?);

    let state = Arc::new(AppState {
        db: Arc::new(db),
        token: opts.token.clone(),
        started: Instant::now(),
        shutdown: Notify::new(),
    });
    let state_path = paths::state_file(&opts.data);
    let record = ServerState {
        pid: std::process::id(),
        url: url.clone(),
        data: opts.data.display().to_string(),
        memory: opts.memory,
        version: env!("CARGO_PKG_VERSION").into(),
        started_at_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis()),
    };
    std::fs::write(&state_path, serde_json::to_vec_pretty(&record)?)?;

    if !opts.quiet {
        let st = state.db.stats()?;
        println!("{}", brand::banner());
        println!("{}", brand::kv("api", &brand::rgb(&url, brand::CYAN)));
        println!(
            "{}",
            brand::kv("console", &brand::rgb(&format!("{url}/"), brand::CYAN))
        );
        let storage = if opts.memory {
            "in-memory (nothing is saved)".to_owned()
        } else {
            opts.data.display().to_string()
        };
        println!("{}", brand::kv("data", &storage));
        println!(
            "{}",
            brand::kv(
                "graph",
                &format!(
                    "{} nodes · {} edges · {} types",
                    st.nodes,
                    st.edges,
                    st.node_types + st.edge_types
                )
            )
        );
        println!(
            "{}",
            brand::kv(
                "auth",
                if opts.token.is_some() {
                    "token required"
                } else {
                    "open (local only)"
                }
            )
        );
        println!(
            "\n  {}\n",
            brand::dim("Ctrl-C or `ratik stop` to shut down gracefully")
        );
    }
    tracing::info!(%url, pid = std::process::id(), "RatikDB is ready");

    let app = router(state.clone(), &opts.cors);
    let served = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(state.clone()))
        .await;
    let _ = std::fs::remove_file(&state_path);
    tracing::info!("shutting down");
    // Dropping the last handle closes the database file cleanly.
    let db = state.db.clone();
    drop(state);
    tokio::task::spawn_blocking(move || drop(db)).await?;
    served?;
    if !opts.quiet {
        println!("{}", brand::ok("RatikDB stopped cleanly"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use tower::ServiceExt as _;

    use super::*;

    fn app(token: Option<&str>) -> Router {
        let state = Arc::new(AppState {
            db: Arc::new(Database::in_memory()),
            token: token.map(str::to_owned),
            started: Instant::now(),
            shutdown: Notify::new(),
        });
        router(state, &[])
    }

    async fn post(app: &Router, path: &str, body: &str, token: Option<&str>) -> (StatusCode, J) {
        let mut req = Request::post(path).header("content-type", "application/json");
        if let Some(t) = token {
            req = req.header("authorization", format!("Bearer {t}"));
        }
        let res = app
            .clone()
            .oneshot(req.body(Body::from(body.to_owned())).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(J::Null))
    }

    #[tokio::test]
    async fn query_round_trip_with_params() {
        let app = app(None);
        let (s, _) = post(
            &app,
            "/v1/query",
            r#"{"query": "CREATE NODE T (n INT UNIQUE)"}"#,
            None,
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        let (s, body) = post(
            &app,
            "/v1/query",
            r#"{"query": "INSERT INTO T (n) VALUES ($v) RETURNING n", "params": {"v": 7}}"#,
            None,
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(body["rows"], json!([[7]]));
        assert_eq!(body["stats"]["nodes_created"], 1);
        assert!(body["time_ms"].is_number());
    }

    #[tokio::test]
    async fn errors_are_structured_json_with_http_status() {
        let app = app(None);
        let (s, body) = post(
            &app,
            "/v1/query",
            r#"{"query": "SELECT * FROM Nope"}"#,
            None,
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "schema_error");
        post(
            &app,
            "/v1/query",
            r#"{"query": "CREATE NODE T (n INT UNIQUE); INSERT INTO T (n) VALUES (1)"}"#,
            None,
        )
        .await;
        let (s, body) = post(
            &app,
            "/v1/query",
            r#"{"query": "INSERT INTO T (n) VALUES (1)"}"#,
            None,
        )
        .await;
        assert_eq!(
            (s, body["error"]["code"].as_str()),
            (StatusCode::CONFLICT, Some("constraint_violation"))
        );
        let (s, body) = post(&app, "/v1/query", "{not json", None).await;
        assert_eq!(
            (s, body["error"]["code"].as_str()),
            (StatusCode::BAD_REQUEST, Some("bad_request"))
        );
        let (s, _) = post(&app, "/v1/call/missing", "", None).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn token_is_enforced_except_for_health() {
        let app = app(Some("k"));
        let (s, _) = post(&app, "/v1/query", r#"{"query": "SHOW STATS"}"#, None).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        let (s, _) = post(
            &app,
            "/v1/query",
            r#"{"query": "SHOW STATS"}"#,
            Some("wrong"),
        )
        .await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        let (s, _) = post(&app, "/v1/query", r#"{"query": "SHOW STATS"}"#, Some("k")).await;
        assert_eq!(s, StatusCode::OK);
        let res = app
            .oneshot(Request::get("/v1/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }
}
