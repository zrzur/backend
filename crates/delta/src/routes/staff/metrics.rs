//! API metrics for the admin console: requests, errors and response
//! times per route, counted in memory by this API process since it
//! started (each process counts its own; nothing personal is kept, only
//! the route pattern such as `/users/<target>`).
//!
//! - `platform.operations.read`: read them

use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
    time::Instant,
};

use revolt_database::{Database, StaffPermission};
use revolt_result::Result;
use rocket::{
    fairing::{Fairing, Info, Kind},
    serde::json::Json,
    Data, Request, Response, State,
};
use serde::Serialize;

use super::StaffContext;

#[derive(Default, Clone)]
struct Counter {
    requests: u64,
    client_errors: u64,
    server_errors: u64,
    total_ms: u64,
    max_ms: u64,
}

static COUNTERS: LazyLock<Mutex<HashMap<String, Counter>>> = LazyLock::new(Default::default);
static STARTED: LazyLock<i64> = LazyLock::new(super::super::support::now_ms);

/// Counts every request by its route pattern
pub struct Metrics;

#[derive(Clone, Copy)]
struct Started(Instant);

#[rocket::async_trait]
impl Fairing for Metrics {
    fn info(&self) -> Info {
        Info {
            name: "API metrics",
            kind: Kind::Request | Kind::Response,
        }
    }

    async fn on_request(&self, request: &mut Request<'_>, _: &mut Data<'_>) {
        LazyLock::force(&STARTED);
        request.local_cache(|| Started(Instant::now()));
    }

    async fn on_response<'r>(&self, request: &'r Request<'_>, response: &mut Response<'r>) {
        let Some(route) = request.route() else {
            // unknown paths are not a route; don't let them grow the map
            return;
        };
        let key = format!("{} {}", request.method(), route.uri.as_str());
        let elapsed = request.local_cache(|| Started(Instant::now())).0.elapsed().as_millis() as u64;
        let status = response.status().code;
        if let Ok(mut counters) = COUNTERS.lock() {
            let c = counters.entry(key).or_default();
            c.requests += 1;
            c.total_ms += elapsed;
            c.max_ms = c.max_ms.max(elapsed);
            if (400..500).contains(&status) {
                c.client_errors += 1;
            } else if status >= 500 {
                c.server_errors += 1;
            }
        }
    }
}

#[derive(Serialize)]
pub struct RouteMetrics {
    pub route: String,
    pub requests: u64,
    pub client_errors: u64,
    pub server_errors: u64,
    pub average_ms: u64,
    pub max_ms: u64,
}

#[derive(Serialize)]
pub struct ApiMetrics {
    /// When this process started counting (milliseconds since the epoch)
    pub since: i64,
    pub requests: u64,
    pub client_errors: u64,
    pub server_errors: u64,
    /// Busiest routes first
    pub routes: Vec<RouteMetrics>,
}

/// # API metrics
#[get("/operations/metrics")]
pub async fn metrics(db: &State<Database>, staff: StaffContext) -> Result<Json<ApiMetrics>> {
    staff.require(db, StaffPermission::OperationsRead).await?;
    let snapshot: Vec<(String, Counter)> = COUNTERS
        .lock()
        .map(|c| c.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();
    let mut routes: Vec<RouteMetrics> = snapshot
        .into_iter()
        .map(|(route, c)| RouteMetrics {
            route,
            requests: c.requests,
            client_errors: c.client_errors,
            server_errors: c.server_errors,
            average_ms: c.total_ms.checked_div(c.requests).unwrap_or(0),
            max_ms: c.max_ms,
        })
        .collect();
    routes.sort_by(|a, b| b.requests.cmp(&a.requests).then(a.route.cmp(&b.route)));
    Ok(Json(ApiMetrics {
        since: *STARTED,
        requests: routes.iter().map(|r| r.requests).sum(),
        client_errors: routes.iter().map(|r| r.client_errors).sum(),
        server_errors: routes.iter().map(|r| r.server_errors).sum(),
        routes,
    }))
}
