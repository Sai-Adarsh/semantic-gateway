use super::*;
use std::{
    future::{Future, poll_fn},
    task::Poll,
};

#[tokio::test]
async fn follower_gets_bad_gateway_and_slot_is_released_when_leader_disappears() {
    let state = Arc::new(App {
        backend: Backend::Demo,
        cache: Mutex::new(Cache::new(1, Duration::from_secs(1), 0.98)),
        flights: Arc::new(Flights::new(1, 1)),
        expected_auth: None,
        hits: AtomicU64::new(0),
        misses: AtomicU64::new(0),
        embedding_calls: AtomicU64::new(0),
        generation_calls: AtomicU64::new(0),
        joined: AtomicU64::new(0),
        rejected: AtomicU64::new(0),
    });
    let Admission::Lead(leader, _) = state.flights.admit("prompt") else {
        panic!("expected a leader with an empty table");
    };
    let response = generate(
        State(state.clone()),
        HeaderMap::new(),
        Json(Request {
            prompt: "prompt".into(),
            cacheable: true,
        }),
    );
    tokio::pin!(response);
    assert!(
        poll_fn(|cx| Poll::Ready(response.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    drop(leader);
    assert_eq!(response.await.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(state.flights.len(), 0);
    assert_eq!(state.joined.load(Relaxed), 1);
    assert_eq!(state.generation_calls.load(Relaxed), 0);
}
