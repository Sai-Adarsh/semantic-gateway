mod cache;
mod flight;
mod upstream;

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use cache::Cache;
use flight::{Admission, Flights};
use serde::{Deserialize, Serialize};
use std::{
    error::Error,
    net::SocketAddr,
    sync::{Arc, Mutex, atomic::{AtomicU64, Ordering::Relaxed}},
    time::Duration,
};
use upstream::Backend;

#[cfg(test)]
#[path = "../tests/unit/generate.rs"]
mod tests;

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
type FlightResult = std::result::Result<Completion, ()>;

struct App {
    backend: Backend,
    cache: Mutex<Cache>,
    flights: Arc<Flights<FlightResult>>,
    expected_auth: Option<String>,
    hits: AtomicU64,
    misses: AtomicU64,
    embedding_calls: AtomicU64,
    generation_calls: AtomicU64,
    joined: AtomicU64,
    rejected: AtomicU64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    prompt: String,
    #[serde(default = "cache_default")]
    cacheable: bool,
    #[serde(default)]
    exact_only: bool,
}

fn cache_default() -> bool {
    true
}

#[derive(Serialize)]
struct Completion {
    answer: String,
    cache_hit: bool,
    similarity: Option<f64>,
}

#[derive(Serialize)]
struct ClientCompletion<'a> {
    #[serde(flatten)]
    completion: &'a Completion,
    coalesced: bool,
}

type Shared = Arc<App>;

#[tokio::main]
async fn main() -> Result<()> {
    let demo = match std::env::args().nth(1).as_deref() {
        Some("--demo") => true,
        None => false,
        _ => return Err("usage: semantic-gateway [--demo]".into()),
    };
    let listen: SocketAddr = setting("LISTEN", "127.0.0.1:8090").parse()?;
    let capacity: usize = setting("CACHE_CAPACITY", "512").parse()?;
    let ttl: u64 = setting("CACHE_TTL_SECONDS", "600").parse()?;
    let threshold: f64 = setting("CACHE_SIMILARITY", "0.98").parse()?;
    let concurrent: usize = setting("CACHE_MAX_INFLIGHT", "64").parse()?;
    let waiters: usize = setting("CACHE_MAX_WAITERS_PER_KEY", "64").parse()?;
    if !(1..=4096).contains(&capacity) || !(1..=86_400).contains(&ttl)
        || !(0.9..1.0).contains(&threshold)
    {
        return Err("invalid cache capacity, TTL, or similarity threshold".into());
    }
    if !(1..=1024).contains(&concurrent) || !(1..=256).contains(&waiters) {
        return Err("CACHE_MAX_INFLIGHT must be 1 to 1024 and CACHE_MAX_WAITERS_PER_KEY must be 1 to 256".into());
    }

    let (backend, expected_auth) = if demo {
        (Backend::Demo, None)
    } else {
        let key = std::env::var("GATEWAY_API_KEY")?;
        if key.len() < 16 || !key.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err("GATEWAY_API_KEY must be at least 16 non-space ASCII characters".into());
        }
        (Backend::from_env()?, Some(format!("Bearer {key}")))
    };

    let state = Arc::new(App {
        backend,
        expected_auth,
        cache: Mutex::new(Cache::new(capacity, Duration::from_secs(ttl), threshold)),
        flights: Arc::new(Flights::new(concurrent, waiters)),
        hits: AtomicU64::new(0),
        misses: AtomicU64::new(0),
        embedding_calls: AtomicU64::new(0),
        generation_calls: AtomicU64::new(0),
        joined: AtomicU64::new(0),
        rejected: AtomicU64::new(0),
    });
    let app = Router::new()
        .route("/healthz", get(|| async { StatusCode::OK }))
        .route("/metrics", get(metrics))
        .route("/generate", post(generate))
        .layer(DefaultBodyLimit::max(16 * 1024))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(listen).await?;
    eprintln!("semantic gateway listening on {} (demo={demo})", listener.local_addr()?);
    axum::serve(listener, app).with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    }).await?;
    Ok(())
}

fn setting(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}

fn authorized(app: &App, headers: &HeaderMap) -> bool {
    let Some(expected) = &app.expected_auth else {
        return true;
    };
    let Some(candidate) = headers.get(header::AUTHORIZATION) else {
        return false;
    };
    let candidate = candidate.as_bytes();
    candidate.len() == expected.len() && candidate.iter().zip(expected.bytes())
        .fold(0, |difference, (candidate, expected)| difference | (candidate ^ expected)) == 0
}

async fn generate(State(app): State<Shared>, headers: HeaderMap, Json(request): Json<Request>) -> Response {
    if !authorized(&app, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if request.prompt.trim().is_empty() || request.prompt.len() > 8192 {
        return (StatusCode::BAD_REQUEST, "prompt must be 1 to 8192 UTF-8 bytes").into_response();
    }
    if !request.cacheable {
        let Some(_slot) = app.flights.admit_uncached() else {
            return reject_overload(&app);
        };
        return respond(&load(&app, request).await, false);
    }
    if let Some(completion) = exact_match(&app, &request.prompt) {
        return respond(&Ok(completion), false);
    }
    let (waiter, coalesced) = match app.flights.admit(&request.prompt, request.exact_only) {
        Admission::Lead(leader, waiter) => {
            // The load belongs to the group, not to the first HTTP connection.
            tokio::spawn(async move {
                leader.complete(load(&app, request).await);
            });
            (waiter, false)
        }
        Admission::Join(waiter) => {
            app.joined.fetch_add(1, Relaxed);
            (waiter, true)
        }
        Admission::Saturated => return reject_overload(&app),
    };
    match waiter.receive().await {
        Some(completion) => respond(&completion, coalesced),
        None => respond(&Err(()), coalesced),
    }
}

fn reject_overload(app: &App) -> Response {
    app.rejected.fetch_add(1, Relaxed);
    (StatusCode::SERVICE_UNAVAILABLE, [(header::RETRY_AFTER, "1")],
     "upstream work or its waiters are at capacity").into_response()
}

fn respond(result: &FlightResult, coalesced: bool) -> Response {
    match result {
        Ok(completion) => Json(ClientCompletion { completion, coalesced }).into_response(),
        Err(()) => (StatusCode::BAD_GATEWAY, "upstream inference or embedding call failed").into_response(),
    }
}

async fn load(app: &App, request: Request) -> FlightResult {
    answer(app, request).await.map_err(|error| eprintln!("upstream call failed: {error}"))
}

fn exact_match(app: &App, prompt: &str) -> Option<Completion> {
    let answer = app.cache.lock().expect("cache lock").find_exact(prompt)?;
    app.hits.fetch_add(1, Relaxed);
    Some(Completion { answer, cache_hit: true, similarity: None })
}

async fn answer(app: &App, request: Request) -> Result<Completion> {
    // Another flight may have stored this prompt after the request was admitted.
    if request.cacheable {
        if let Some(completion) = exact_match(app, &request.prompt) {
            return Ok(completion);
        }
    }
    let embedding = if request.cacheable && !request.exact_only {
        app.embedding_calls.fetch_add(1, Relaxed);
        let embedding = app.backend.embed(&request.prompt).await?;
        let mut cache = app.cache.lock().expect("cache lock");
        if let Some(answer) = cache.find_exact(&request.prompt) {
            app.hits.fetch_add(1, Relaxed);
            return Ok(Completion { answer, cache_hit: true, similarity: None });
        }
        if let Some((answer, score)) = cache.find_semantic(&embedding) {
            app.hits.fetch_add(1, Relaxed);
            return Ok(Completion { answer, cache_hit: true, similarity: Some(score) });
        }
        Some(embedding)
    } else {
        None
    };

    if request.cacheable {
        app.misses.fetch_add(1, Relaxed);
    }
    app.generation_calls.fetch_add(1, Relaxed);
    let answer = app.backend.generate(&request.prompt).await?;
    let answer = if request.cacheable {
        app.cache.lock().expect("cache lock").insert(request.prompt, embedding, answer)
    } else {
        answer
    };
    Ok(Completion { answer, cache_hit: false, similarity: None })
}

async fn metrics(State(app): State<Shared>, headers: HeaderMap) -> Response {
    if !authorized(&app, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")],
        format!(
            "# TYPE semantic_cache_hits_total counter\nsemantic_cache_hits_total {}\n\
             # TYPE semantic_cache_misses_total counter\nsemantic_cache_misses_total {}\n\
             # TYPE semantic_embedding_calls_total counter\nsemantic_embedding_calls_total {}\n\
             # TYPE semantic_generation_calls_total counter\nsemantic_generation_calls_total {}\n\
             # TYPE semantic_coalesced_requests_total counter\nsemantic_coalesced_requests_total {}\n\
             # TYPE semantic_overload_rejected_total counter\nsemantic_overload_rejected_total {}\n\
             # TYPE semantic_inflight_keys gauge\nsemantic_inflight_keys {}\n\
             # TYPE semantic_inflight_loads gauge\nsemantic_inflight_loads {}\n",
            app.hits.load(Relaxed), app.misses.load(Relaxed), app.embedding_calls.load(Relaxed),
            app.generation_calls.load(Relaxed), app.joined.load(Relaxed), app.rejected.load(Relaxed),
            app.flights.len(), app.flights.active_loads()
        ),
    ).into_response()
}
