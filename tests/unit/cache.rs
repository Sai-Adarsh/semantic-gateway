use super::*;

#[test]
fn rejects_unusable_vectors_and_normalizes_large_values() {
    for vector in [vec![], vec![0.0], vec![f64::INFINITY], vec![1.0; 16_385]] {
        assert!(normalize(vector).is_none());
    }
    let vector = normalize(vec![f64::MAX, f64::MAX]).unwrap();
    assert!((vector.iter().map(|v| v * v).sum::<f64>() - 1.0).abs() < 1e-12);
}

#[test]
fn finds_best_match_and_evicts_the_least_recently_used_entry() {
    let mut cache = Cache::new(2, Duration::from_secs(30), 0.98);
    cache.insert("precise".into(), Some(vec![1.0, 0.0]), "precise".into());
    cache.insert("close".into(), Some(vec![0.99, 0.1]), "close".into());
    assert_eq!(cache.find_semantic(&[1.0, 0.0]).unwrap().0, "precise");
    cache.insert("other".into(), Some(vec![0.0, 1.0]), "other".into());
    assert!(cache.find_semantic(&[0.98, 0.0]).is_none());
    assert!(cache.find_semantic(&[1.0]).is_none());
    assert_eq!(cache.find_semantic(&[1.0, 0.0]).unwrap().0, "precise");
    assert!(cache.find_exact("close").is_none());
    cache.insert("precise".into(), Some(vec![1.0, 0.0]), "replacement".into());
    assert_eq!(cache.find_exact("precise").unwrap(), "replacement");
    let mut expires = Cache::new(1, Duration::ZERO, 0.98);
    expires.insert("expired".into(), Some(vec![1.0]), "expired".into());
    assert!(expires.find_exact("expired").is_none());
}
