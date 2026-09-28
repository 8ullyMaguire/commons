//! The server: library, index, and peer roles (§3.1).
//!
//! One binary, three modes. The mode decides the storage engine, the default
//! bind address, and whether federation is enabled — nothing else. Phase 0
//! serves health and metrics; the browse and query surface arrives in Phase 2.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use commons_store::{Mode, Store};
use tower_http::trace::TraceLayer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

pub mod config;
// T-P6-005. `pub` and not `pub(crate)` because the ticket's accept criterion
// is an integration test in `tests/`, and an integration test is a separate
// crate: it can only see `pub` items. A `pub(crate)` module would compile
// cleanly and leave the acceptance test unable to reach the one function it
// exists to check.
pub mod external_player;
// T-P6-003. This is the first route that opens a path read out of the
// database, so `contained_in` is load-bearing here -- see the module doc.
pub mod funscript;
pub mod health;
pub mod interview;
pub mod media;
pub mod playback;
pub mod proxy;
pub mod range;
// T-P5-007 part 2B. Share links (§9.5, #5612). The policy is in
// `commons-consent::share`; this file is the wire shape and nothing else.
pub mod share;
pub mod subtitles;

pub use config::{Config, RunMode};
pub use health::HealthState;

/// The application state shared by every route.
pub struct AppState {
    pub store: Store,
    pub config: Config,
}

impl AppState {
    /// Open the store for the configured mode. A library and a peer use SQLite
    /// in the data directory; an index requires `DATABASE_URL`.
    pub async fn open(config: Config) -> Result<Self, StartError> {
        let store = match config.mode {
            RunMode::Library | RunMode::Peer => Store::open_library(&config.data_dir).await?,
            RunMode::Index => Store::open_index().await?,
        };
        Ok(Self { store, config })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error(transparent)]
    Store(#[from] commons_store::StoreError),
    #[error("cannot bind {addr}: {source}")]
    Bind {
        addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Config(#[from] config::ConfigError),
}

/// Build the router.
///
/// `/healthz` and `/livez` never require auth. `/metrics` exists only when
/// `--metrics` is passed, and answers 404 otherwise rather than 403: the
/// endpoint is not part of this deployment's surface, and a 403 invites a
/// support question about a credential that does not exist.
pub fn router(state: Arc<AppState>) -> Router {
    let health = HealthState {
        mode: state.config.mode,
        db_engine: match state.store.mode() {
            Mode::Library => "sqlite",
            Mode::Index => "postgres",
        },
        metrics_enabled: state.config.metrics,
        ready: true,
    };

    // Two states, so two routers nested rather than one. `HealthState` is
    // `Clone + Send + Sync` with no store behind it and is the whole of the
    // Phase 0 surface; `AppState` carries the `Store` and is what `/media`
    // needs. Axum's `with_state` fixes the state type for the router it is
    // called on, so merging them would mean putting the store in the health
    // state -- and then `HealthState` would have to construct a store to be
    // tested, which is the wrong direction for a health check.
    let health_routes = Router::new()
        .route("/healthz", get(health::healthz))
        .route("/livez", get(health::livez))
        .route("/metrics", get(metrics))
        .with_state(health);

    Router::new()
        .merge(health_routes)
        .route("/media/:object_id", get(media::get_media))
        .route("/media/:object_id/caps", get(proxy::get_caps))
        .route("/media/:object_id/proxy.m3u8", get(proxy::get_proxy))
        .route(
            "/media/:object_id/playback",
            get(playback::get_playback_route).put(playback::put_playback_route),
        )
        .route("/media/:object_id/subtitles", get(subtitles::list_tracks))
        .route(
            "/media/:object_id/transcript",
            get(interview::get_transcript),
        )
        .route(
            "/media/:object_id/transcript/words",
            get(interview::get_words),
        )
        .route("/media/:object_id/chapters", get(interview::get_chapters))
        // T-P6-004b. Both are gated on the same 404 `GET /media/:id` uses --
        // absent, off-disk and denied stay indistinguishable, or these two
        // routes become the cheapest way to probe a library.
        .route("/media/:object_id/quotes", get(interview::get_quotes))
        .route("/media/:object_id/topics", get(interview::get_topics))
        .route(
            "/media/:object_id/subtitles/:document_id.vtt",
            get(subtitles::get_vtt),
        )
        // T-P6-003. The list is cheap and changes on a re-scan; the timeline is
        // the parsed action list a player seeks in. Two routes because a list
        // can succeed while every script on it is unreadable, and a client that
        // cannot tell those apart shows an empty player and the user concludes
        // their funscript is broken.
        .route("/media/:object_id/funscripts", get(funscript::list))
        .route(
            "/media/:object_id/funscripts/:funscript_id",
            get(funscript::timeline),
        )
        // T-P5-007 part 2B. The owner's routes and the recipient's routes are
        // kept apart even though both live under /api: a recipient's token is a
        // capability and must never be presented to a route that assumes the
        // caller owns the library, so the two sets have different handlers and
        // no shared prefix beyond /api.
        .route(
            "/api/share",
            post(share::create_share).get(share::list_share),
        )
        .route("/api/share/:id", axum::routing::delete(share::revoke_share))
        .route("/api/s/:token", get(share::resolve_share))
        .route("/api/s/:token/access", get(share::share_access))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
        .fallback(not_found)
}

async fn metrics(State(st): State<HealthState>) -> Response {
    if !st.metrics_enabled {
        return (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "application/json")],
            r#"{"error":"metrics_disabled","hint":"start with --metrics to expose it"}"#,
        )
            .into_response();
    }
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        health::render_metrics(&st),
    )
        .into_response()
}

async fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        [(header::CONTENT_TYPE, "application/json")],
        r#"{"error":"not_found","phase":"0","note":"browse and query arrive in phase 2; /healthz and /livez are live"}"#,
    )
        .into_response()
}

/// Run until the process is signalled.
pub async fn run(config: Config) -> Result<(), StartError> {
    init_tracing();

    let addr: SocketAddr = config
        .bind
        .parse()
        .unwrap_or_else(|_| "127.0.0.1:9999".parse().expect("valid default"));

    // Captured before `config` moves into AppState.
    let mode = config.mode;
    let data_dir = config.data_dir.clone();

    let state = Arc::new(AppState::open(config).await?);

    // Record what the store actually reports, so /healthz states the truth
    // rather than a constant.
    let version = state.store.schema_version().await.unwrap_or(0);
    let pending = state.store.pending_jobs().await.unwrap_or(0);
    health::record_schema_version(version);
    health::record_pending_migrations(0);
    health::record_pending_jobs(pending);

    let app = router(state);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|source| StartError::Bind { addr, source })?;

    tracing::info!(
        mode = %mode.as_str(),
        data_dir = %data_dir.display(),
        bind = %addr,
        schema_version = version,
        "commons listening"
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|source| StartError::Bind { addr, source })?;

    tracing::info!("commons stopped");
    Ok(())
}

async fn shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("cannot install SIGTERM handler: {e}");
            return std::future::pending().await;
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => tracing::info!("interrupt received"),
        _ = term.recv() => tracing::info!("terminate received"),
    }
}

/// JSON when a log file is configured, human-readable on a terminal
/// (stash#2463 asked for exactly this split): a log meant to be read is not a
/// log meant to be parsed.
fn init_tracing() {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,sqlx=warn"));
    // `Box<dyn Layer>` erases the formatter type so both branches are one value.
    // `boxed()` on a concrete Layer would need the same effect via a different
    // route; boxing directly is clearer about what is happening.
    let layer: Box<dyn tracing_subscriber::Layer<_> + Send + Sync> =
        if std::io::IsTerminal::is_terminal(&std::io::stderr()) {
            Box::new(tracing_subscriber::fmt::layer().with_target(false))
        } else {
            Box::new(tracing_subscriber::fmt::layer().json().with_target(false))
        };
    let _ = tracing_subscriber::registry()
        .with(env_filter)
        .with(layer)
        .try_init();
}

/// A fresh data directory under the system temp dir, for tests.
pub fn temp_data_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("commons-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp data dir");
    dir
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    async fn app(config: Config) -> Router {
        let state = Arc::new(AppState::open(config).await.expect("open"));
        // Populate the health counters the way `run` does.
        let version = state.store.schema_version().await.unwrap_or(0);
        let pending = state.store.pending_jobs().await.unwrap_or(0);
        health::record_schema_version(version);
        health::record_pending_migrations(0);
        health::record_pending_jobs(pending);
        router(state)
    }

    fn config(dir: PathBuf, metrics: bool) -> Config {
        Config {
            public_base_url: "http://127.0.0.1:9999".to_string(),
            mode: RunMode::Library,
            data_dir: dir,
            bind: "127.0.0.1:0".into(),
            metrics,
            // T-P6-005: off. These tests exercise the router, and a responder
            // would bind a socket none of them read.
            dlna: crate::config::DlnaConfig {
                enabled: false,
                bind: "127.0.0.1:0".into(),
                location_base: "http://127.0.0.1:9999".into(),
                friendly_name: "commons-test".into(),
            },
        }
    }

    async fn get_json(r: Router, uri: &str) -> (StatusCode, serde_json::Value) {
        let resp = r
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .expect("response");
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .expect("body");
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    #[tokio::test]
    async fn healthz_reports_the_schema_and_zero_pending_migrations() {
        // The plan's acceptance criterion for T-P0-009, asserted in-process.
        let app = app(config(temp_data_dir("healthz"), false)).await;
        let (status, body) = get_json(app, "/healthz").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["ok"], true);
        assert_eq!(body["mode"], "library");
        assert_eq!(body["db_engine"], "sqlite");
        assert_eq!(body["migrations_pending"], 0);
        assert!(
            body["schema_version"].as_i64().unwrap() >= 1,
            "a fresh library must have applied at least migration 1: {body}"
        );
    }

    #[tokio::test]
    async fn healthz_needs_no_authentication() {
        // No Authorization header, no session, and it still succeeds. An
        // orchestrator holding no credential must be able to ask.
        let app = app(config(temp_data_dir("noauth"), false)).await;
        let (status, _) = get_json(app, "/healthz").await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn livez_answers() {
        let app = app(config(temp_data_dir("livez"), false)).await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/livez")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn metrics_are_absent_unless_the_flag_is_passed() {
        let app = app(config(temp_data_dir("metrics-off"), false)).await;
        let (status, body) = get_json(app, "/metrics").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], "metrics_disabled");
    }

    #[tokio::test]
    async fn metrics_are_served_when_the_flag_is_passed() {
        let app = app(config(temp_data_dir("metrics-on"), true)).await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(ct.starts_with("text/plain"), "got {ct}");
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("commons_up"), "{text}");
    }

    #[tokio::test]
    async fn an_unknown_route_says_what_does_exist() {
        let app = app(config(temp_data_dir("404"), false)).await;
        let (status, body) = get_json(app, "/scenes").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], "not_found");
        assert!(
            body["note"].as_str().unwrap().contains("/healthz"),
            "the 404 should point at what does work: {body}"
        );
    }

    #[tokio::test]
    async fn a_library_opens_on_a_fresh_directory_with_the_full_schema() {
        let dir = temp_data_dir("fresh");
        assert!(!dir.join("commons.sqlite").exists());
        let state = AppState::open(config(dir.clone(), false)).await.unwrap();
        assert_eq!(state.store.mode(), Mode::Library);
        assert!(dir.join("commons.sqlite").exists());
        assert!(
            state.store.schema_version().await.unwrap() >= 1,
            "the library must be migrated, not merely created"
        );
    }
}
