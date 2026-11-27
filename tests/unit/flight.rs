use super::*;

#[tokio::test]
async fn abandoned_leader_wakes_waiters_and_releases_admission() {
    let flights = Arc::new(Flights::<u8>::new(1, 1));
    let Admission::Lead(leader, original) = flights.admit("question", false) else {
        panic!("expected leader for an unknown key");
    };
    let Admission::Join(follower) = flights.admit("question", false) else {
        panic!("expected an exact match to join");
    };
    assert!(matches!(
        flights.admit("question", false),
        Admission::Saturated
    ));
    assert!(matches!(
        flights.admit("question", true),
        Admission::Saturated
    ));
    assert!(matches!(
        flights.admit("different", false),
        Admission::Saturated
    ));
    drop(leader);
    assert!(original.receive().await.is_none());
    assert!(follower.receive().await.is_none());
    assert_eq!(flights.len(), 0);
    assert!(matches!(flights.admit("next", false), Admission::Lead(..)));
}
