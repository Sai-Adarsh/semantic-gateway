#![cfg(unix)]

use axum::{Json, Router, http::StatusCode, routing::post};
use serde_json::{Value, json};
use std::{
    net::TcpListener as StdListener,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    net::TcpListener,
    sync::Notify,
    time::{sleep, timeout},
};

const KEY: &str = "gateway-fixture-key";
const BIN: &str = env!("CARGO_BIN_EXE_semantic-gateway");

struct Gateway {
    child: Child,
    base: String,
    client: reqwest::Client,
}

impl Gateway {
    async fn start(config: &[(&str, &str)], demo: bool) -> Self {
        let reservation = StdListener::bind("127.0.0.1:0").unwrap();
        let addr = reservation.local_addr().unwrap().to_string();
        drop(reservation);
        let mut command = Command::new(BIN);
        command
            .envs(config.iter().copied())
            .env("LISTEN", &addr)
            .stderr(Stdio::inherit());
        if demo {
            command.arg("--demo");
        }
        let gateway = Self {
            child: command.spawn().unwrap(),
            base: format!("http://{addr}"),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
        };
        timeout(Duration::from_secs(5), async {
            loop {
                if gateway
                    .client
                    .get(format!("{}/healthz", gateway.base))
                    .send()
                    .await
                    .is_ok_and(|response| response.status() == StatusCode::OK)
                {
                    break;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("gateway failed to start");
        gateway
    }

    async fn generate(&self, body: Value) -> reqwest::Response {
        self.client
            .post(format!("{}/generate", self.base))
            .bearer_auth(KEY)
            .json(&body)
            .send()
            .await
            .unwrap()
    }

    async fn metrics(&self) -> String {
        self.client
            .get(format!("{}/metrics", self.base))
            .bearer_auth(KEY)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .text()
            .await
            .unwrap()
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        let _ = Command::new("kill")
            .args(["-INT", &self.child.id().to_string()])
            .status();
        for _ in 0..100 {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[tokio::test]
async fn demo_caches_equivalent_prompts_and_bypasses_explicitly_uncacheable_requests() {
    let gateway = Gateway::start(&[("CACHE_CAPACITY", "1")], true).await;
    let prompts = [
        ("Summarize the project status.", true, false),
        ("Give me a summary of the project status.", true, true),
        ("Summarize the project status.", false, false),
        ("How does cell division work?", true, false),
        ("Summarize the project status.", true, false),
        ("!!!", true, false),
    ];
    for (prompt, cacheable, hit) in prompts {
        let result: Value = gateway
            .generate(json!({"prompt":prompt, "cacheable":cacheable}))
            .await
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(result["cache_hit"], hit, "{prompt}");
        assert_eq!(result["coalesced"], false);
    }
    assert!(
        gateway
            .metrics()
            .await
            .contains("semantic_generation_calls_total 5")
    );
    assert_eq!(
        gateway.generate(json!({"prompt":" "})).await.status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        gateway
            .generate(json!({"prompt":"x".repeat(8193)}))
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn exact_only_skips_embeddings_and_never_reuses_a_semantic_match() {
    let embeds = Arc::new(AtomicUsize::new(0));
    let generations = Arc::new(AtomicUsize::new(0));
    let embed_count = embeds.clone();
    let generation_count = generations.clone();
    let upstream = Router::new()
        .route(
            "/embed",
            post(move || {
                let count = embed_count.clone();
                async move {
                    count.fetch_add(1, Ordering::Relaxed);
                    Json(json!({"data":[{"embedding":[1.0,0.0]}]}))
                }
            }),
        )
        .route(
            "/generate",
            post(move |Json(body): Json<Value>| {
                let count = generation_count.clone();
                async move {
                    count.fetch_add(1, Ordering::Relaxed);
                    let prompt = body["messages"][0]["content"].as_str().unwrap();
                    Json(
                        json!({"choices":[{"message":{"content":format!("answer for {prompt}")}}]}),
                    )
                }
            }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let upstream = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
    let gateway = Gateway::start(
        &[
            ("GATEWAY_API_KEY", KEY),
            ("EMBEDDING_URL", &format!("http://{addr}/embed")),
            ("GENERATION_URL", &format!("http://{addr}/generate")),
            ("EMBEDDING_MODEL", "fixture"),
            ("GENERATION_MODEL", "fixture"),
        ],
        false,
    )
    .await;
    let cases = [
        ("private query", true, false, "private query", 0, 1),
        ("private query", true, true, "private query", 0, 1),
        ("semantic source", false, false, "semantic source", 1, 2),
        ("same vector", false, true, "semantic source", 2, 2),
        ("same vector", true, false, "same vector", 2, 3),
        ("same vector", false, true, "same vector", 2, 3),
    ];
    for (prompt, exact_only, cache_hit, expected, embedding_calls, generation_calls) in cases {
        let result: Value = gateway
            .generate(json!({"prompt":prompt,"exact_only":exact_only}))
            .await
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(result["answer"], format!("answer for {expected}"));
        assert_eq!(result["cache_hit"], cache_hit);
        assert_eq!(embeds.load(Ordering::Relaxed), embedding_calls);
        assert_eq!(generations.load(Ordering::Relaxed), generation_calls);
    }
    upstream.abort();
}

#[tokio::test]
async fn exact_duplicates_share_upstream_work_and_both_admission_limits_apply() {
    let upstream_calls = Arc::new(AtomicUsize::new(0));
    let generation_entered = Arc::new(Notify::new());
    let release_generation = Arc::new(Notify::new());
    let embed_calls = upstream_calls.clone();
    let generation_calls = upstream_calls.clone();
    let entered = generation_entered.clone();
    let release = release_generation.clone();
    let server = Router::new()
        .route(
            "/embed",
            post(move |Json(body): Json<Value>| {
                let calls = embed_calls.clone();
                async move {
                    calls.fetch_add(1, Ordering::Relaxed);
                    let vector = if body["input"] == "illegal embedding" {
                        vec![0.0, 0.0]
                    } else {
                        vec![1.0, 0.0]
                    };
                    Json(json!({"data": [{"embedding": vector}]}))
                }
            }),
        )
        .route(
            "/generate",
            post(move |Json(body): Json<Value>| {
                let calls = generation_calls.clone();
                let entered = entered.clone();
                let release = release.clone();
                async move {
                    calls.fetch_add(1, Ordering::Relaxed);
                    match body["messages"][0]["content"].as_str().unwrap() {
                        "burst" => {
                            entered.notify_one();
                            release.notified().await;
                            Json(json!({"choices":[{"message":{"content":"shared answer"}}]}))
                        }
                        "empty provider result" => Json(json!({"choices":[]})),
                        _ => Json(json!({"choices":[{"message":{"content":"x".repeat(65_537)}}]})),
                    }
                }
            }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let embed_url = format!("http://{}/embed", listener.local_addr().unwrap());
    let generate_url = format!("http://{}/generate", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, server).await.unwrap() });
    let gateway = Arc::new(
        Gateway::start(
            &[
                ("GATEWAY_API_KEY", KEY),
                ("EMBEDDING_URL", &embed_url),
                ("GENERATION_URL", &generate_url),
                ("EMBEDDING_MODEL", "fixture"),
                ("GENERATION_MODEL", "fixture"),
                ("GENERATION_API_KEY", "test"),
                ("CACHE_MAX_INFLIGHT", "1"),
                ("CACHE_MAX_WAITERS_PER_KEY", "1"),
            ],
            false,
        )
        .await,
    );
    let unauthorized = gateway
        .client
        .post(format!("{}/generate", gateway.base))
        .json(&json!({"prompt":"unauthorized"}))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        gateway
            .client
            .get(format!("{}/metrics", gateway.base))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );

    let origin = gateway.clone();
    let leader = tokio::spawn(async move { origin.generate(json!({"prompt":"burst"})).await });
    timeout(Duration::from_secs(5), generation_entered.notified())
        .await
        .unwrap();
    // The semantic flight occupies the only slot; an exact-only
    // caller must not inherit the semantic result.
    assert_eq!(
        gateway
            .generate(json!({"prompt":"burst", "exact_only":true}))
            .await
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    let joiner = gateway.clone();
    let follower = tokio::spawn(async move { joiner.generate(json!({"prompt":"burst"})).await });
    timeout(Duration::from_secs(2), async {
        while !gateway
            .metrics()
            .await
            .contains("semantic_coalesced_requests_total 1")
        {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    for prompt in ["burst", "separate"] {
        let response = gateway.generate(json!({"prompt":prompt})).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers()["retry-after"], "1");
    }
    leader.abort();
    release_generation.notify_one();
    let response = follower
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(response["answer"], "shared answer");
    assert_eq!(response["coalesced"], true);
    assert_eq!(upstream_calls.load(Ordering::Relaxed), 2);
    let semantic = gateway
        .generate(json!({"prompt":"paraphrase"}))
        .await
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(semantic["cache_hit"], true);
    assert_eq!(semantic["coalesced"], false);
    for prompt in ["empty provider result", "too big"] {
        assert_eq!(
            gateway
                .generate(json!({"prompt": prompt, "cacheable": false}))
                .await
                .status(),
            StatusCode::BAD_GATEWAY
        );
    }
    assert_eq!(
        gateway
            .generate(json!({"prompt":"illegal embedding"}))
            .await
            .status(),
        StatusCode::BAD_GATEWAY
    );
    assert!(gateway.metrics().await.contains("semantic_inflight_keys 0"));
    server.abort();
}

#[test]
fn rejects_out_of_range_gateway_settings_before_listening() {
    for (setting, value, reason) in [
        ("CACHE_CAPACITY", "0", "invalid cache capacity"),
        ("CACHE_TTL_SECONDS", "86401", "invalid cache capacity"),
        ("CACHE_SIMILARITY", "1.0", "invalid cache capacity"),
        ("CACHE_MAX_INFLIGHT", "0", "CACHE_MAX_INFLIGHT must"),
        ("CACHE_MAX_WAITERS_PER_KEY", "0", "CACHE_MAX_INFLIGHT must"),
        ("GATEWAY_API_KEY", "short", "GATEWAY_API_KEY must"),
    ] {
        let mut command = Command::new(BIN);
        command.env(setting, value);
        if setting != "GATEWAY_API_KEY" {
            command.arg("--demo");
        }
        let output = command.output().unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(reason),
            "{output:?}"
        );
    }
    for (url, model, reason) in [
        ("ftp://localhost", "fixture", "without URL credentials"),
        (
            "http://user:pass@localhost",
            "fixture",
            "without URL credentials",
        ),
        ("http://localhost", " ", "cannot be empty"),
    ] {
        let output = Command::new(BIN)
            .env("GATEWAY_API_KEY", KEY)
            .env("EMBEDDING_URL", url)
            .env("EMBEDDING_MODEL", model)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(reason),
            "{output:?}"
        );
    }
    let output = Command::new(BIN).arg("--unknown").output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("usage:"));
}

#[tokio::test]
async fn rejects_oversized_upstream_json_and_recovers() {
    let server = Router::new()
        .route(
            "/embed",
            post(|| async {
                Json(json!({"data": [{"embedding": [1.0, 0.0]}], "extra": "x".repeat(1024 * 1024)}))
            }),
        )
        .route(
            "/generate",
            post(|Json(body): Json<Value>| async move {
                let extra = if body["messages"][0]["content"] == "oversized completion" {
                    "x".repeat(1024 * 1024)
                } else {
                    String::new()
                };
                (
                    [("transfer-encoding", "chunked")],
                    Json(json!({"choices": [{"message": {"content": "valid answer"}}], "extra": extra})),
                )
            }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, server).await.unwrap() });
    let gateway = Gateway::start(
        &[
            ("GATEWAY_API_KEY", KEY),
            ("EMBEDDING_URL", &format!("http://{addr}/embed")),
            ("GENERATION_URL", &format!("http://{addr}/generate")),
            ("EMBEDDING_MODEL", "fixture"),
            ("GENERATION_MODEL", "fixture"),
        ],
        false,
    )
    .await;
    let mut statuses = Vec::new();
    for body in [
        json!({"prompt": "oversized embedding"}),
        json!({"prompt": "oversized completion", "cacheable": false}),
    ] {
        statuses.push(gateway.generate(body).await.status());
    }
    assert_eq!(statuses, [StatusCode::BAD_GATEWAY; 2]);
    let reply = gateway
        .generate(json!({"prompt": "ordinary completion", "exact_only": true}))
        .await
        .error_for_status()
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(reply["answer"], "valid answer");
    server.abort();
}
