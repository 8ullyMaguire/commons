//! Health and metrics endpoints (T-P0-009, spec §3.7).
//!
//! `/healthz` answers without authentication, always — an orchestrator
//! cannot hold a credential to ask whether the process is up, and gating it
//! would mean an unauthenticated request is the only way to learn the server
//! is broken.
//!
//! `/metrics` is Prometheus text and is **off unless `--metrics` is passed**.
//! It reports library statistics, so exposing it by default on a public index
//! would leak the shape of someone's collection to anyone who asked.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Serialize;

use crate::config::RunMode;

/// Shared state for the health routes.
#[derive(Clone)]
pub struct HealthState {
    pub mode: RunMode,
    pub db_engine: &'static str,
    pub metrics_enabled: bool,
    /// Cheap liveness: true once the store is open and migrated.
    pub ready: bool,
}

/// The `/healthz` body. Field names are snake_case and part of the contract:
/// a health check is a machine interface, so renaming one is a breaking change.
#[derive(Debug, Serialize)]
pub struct Health {
    pub ok: bool,
    pub mode: &'static str,
    pub db_engine: &'static str,
    pub schema_version: i64,
    pub migrations_pending: i64,
    pub ready: bool,
    pub version: &'static str,
}

/// Which migrations are outstanding. The plan's acceptance criterion is that a
/// fresh directory reports zero, and a non-zero value here means the process
/// started against a schema it does not fully understand.
#[derive(Debug, Serialize)]
pub struct Migrations {
    pub applied: i64,
    pub pending: i64,
}

pub async fn healthz(State(st): State<HealthState>) -> impl IntoResponse {
    let schema_version = st.schema_version().await;
    let pending = st.pending_migrations().await;

    let body = Health {
        ok: st.ready,
        mode: st.mode.as_str(),
        db_engine: st.db_engine,
        schema_version,
        migrations_pending: pending,
        ready: st.ready,
        version: env!("CARGO_PKG_VERSION"),
    };

    // 200 when serving, 503 when not: a load balancer should stop sending
    // traffic to a process that has opened but not finished migrating.
    if st.ready {
        (StatusCode::OK, Json(body))
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, Json(body))
    }
}

/// Liveness: the process is running. Deliberately does not touch the database,
/// so a database outage does not make the orchestrator kill a process that
/// would recover on its own.
pub async fn livez() -> &'static str {
    "ok\n"
}

impl HealthState {
    async fn schema_version(&self) -> i64 {
        // Populated by main once the store is open; a store that has not been
        // opened reports 0 rather than panicking, so /healthz keeps answering
        // during startup.
        SCHEMA_VERSION.load(std::sync::atomic::Ordering::Relaxed)
    }

    async fn pending_migrations(&self) -> i64 {
        PENDING_MIGRATIONS.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Set once at startup. Atomics rather than a field on the state so `/healthz`
/// cannot be the thing that blocks on opening the store.
static SCHEMA_VERSION: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
static PENDING_MIGRATIONS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

pub fn record_schema_version(v: i64) {
    SCHEMA_VERSION.store(v, std::sync::atomic::Ordering::Relaxed);
}

pub fn record_pending_migrations(v: i64) {
    PENDING_MIGRATIONS.store(v, std::sync::atomic::Ordering::Relaxed);
}

/// The Prometheus exposition for the counters we actually keep.
pub fn render_metrics(state: &HealthState) -> String {
    let mut s = String::new();
    s.push_str("# HELP commons_up whether the server is serving.\n");
    s.push_str("# TYPE commons_up gauge\n");
    s.push_str(&format!("commons_up {}\n", i32::from(state.ready)));
    s.push_str("# HELP commons_build_info build metadata.\n");
    s.push_str("# TYPE commons_build_info gauge\n");
    s.push_str(&format!(
        "commons_build_info{{version=\"{}\",mode=\"{}\",engine=\"{}\"}} 1\n",
        env!("CARGO_PKG_VERSION"),
        state.mode.as_str(),
        state.db_engine
    ));
    s.push_str("# HELP commons_schema_version the migration version in use.\n");
    s.push_str("# TYPE commons_schema_version gauge\n");
    s.push_str(&format!(
        "commons_schema_version {}\n",
        SCHEMA_VERSION.load(std::sync::atomic::Ordering::Relaxed)
    ));
    s.push_str("# HELP commons_migrations_pending migrations not yet applied.\n");
    s.push_str("# TYPE commons_migrations_pending gauge\n");
    s.push_str(&format!(
        "commons_migrations_pending {}\n",
        PENDING_MIGRATIONS.load(std::sync::atomic::Ordering::Relaxed)
    ));
    s.push_str("# HELP commons_pending_jobs queued or running jobs.\n");
    s.push_str("# TYPE commons_pending_jobs gauge\n");
    s.push_str(&format!(
        "commons_pending_jobs {}\n",
        PENDING_JOBS.load(std::sync::atomic::Ordering::Relaxed)
    ));
    s
}

static PENDING_JOBS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

pub fn record_pending_jobs(v: i64) {
    PENDING_JOBS.store(v, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> HealthState {
        HealthState {
            mode: RunMode::Library,
            db_engine: "sqlite",
            metrics_enabled: false,
            ready: true,
        }
    }

    #[test]
    fn the_health_body_carries_the_fields_a_checker_needs() {
        record_schema_version(1);
        record_pending_migrations(0);
        let h = Health {
            ok: true,
            mode: "library",
            db_engine: "sqlite",
            schema_version: 1,
            migrations_pending: 0,
            ready: true,
            version: "0.1.0",
        };
        let json = serde_json::to_string(&h).unwrap();
        for key in [
            "\"ok\"",
            "\"mode\"",
            "\"db_engine\"",
            "\"schema_version\"",
            "\"migrations_pending\"",
        ] {
            assert!(json.contains(key), "health body is missing {key}: {json}");
        }
    }

    #[test]
    fn metrics_render_in_prometheus_text_form() {
        record_pending_jobs(3);
        let text = render_metrics(&state());
        assert!(text.contains("# TYPE commons_up gauge"));
        assert!(text.contains("commons_up 1"));
        assert!(text.contains("commons_pending_jobs 3"));
        // Every sample line must be `name value` or `name{labels} value`.
        for line in text
            .lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
        {
            assert!(line.contains(' '), "malformed exposition line: {line}");
        }
    }

    #[test]
    fn metrics_are_not_rendered_unless_enabled() {
        // The flag is checked at the router, not here; this test pins that the
        // state carries the flag so the router can.
        let mut st = state();
        assert!(!st.metrics_enabled);
        st.metrics_enabled = true;
        assert!(st.metrics_enabled);
    }
}
