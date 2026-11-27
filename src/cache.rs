use std::{collections::VecDeque, time::{Duration, Instant}};

struct Entry {
    embedding: Vec<f64>,
    answer: String,
    expires: Instant,
}

pub struct Cache {
    entries: VecDeque<Entry>,
    capacity: usize,
    ttl: Duration,
    threshold: f64,
}

impl Cache {
    pub fn new(capacity: usize, ttl: Duration, threshold: f64) -> Self {
        Self { entries: VecDeque::new(), capacity, ttl, threshold }
    }

    pub fn find(&mut self, embedding: &[f64]) -> Option<(String, f64)> {
        let now = Instant::now();
        self.entries.retain(|entry| entry.expires > now);
        let (index, score) = self.entries.iter().enumerate()
            .filter(|(_, entry)| entry.embedding.len() == embedding.len())
            .map(|(index, entry)| {
                let score: f64 = entry.embedding.iter().zip(embedding).map(|(a, b)| a * b).sum();
                (index, score.clamp(-1.0, 1.0))
            })
            .filter(|&(_, score)| score > self.threshold)
            .max_by(|left, right| left.1.total_cmp(&right.1))?;
        let entry = self.entries.remove(index).expect("matching cache entry");
        let answer = entry.answer.clone();
        self.entries.push_back(entry);
        Some((answer, score))
    }

    pub fn insert(&mut self, embedding: Vec<f64>, answer: String) {
        let now = Instant::now();
        self.entries.retain(|entry| entry.expires > now && entry.embedding != embedding);
        if self.entries.len() == self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back(Entry { embedding, answer, expires: now + self.ttl });
    }
}

pub fn normalize(mut vector: Vec<f64>) -> Option<Vec<f64>> {
    if vector.is_empty() || vector.len() > 16_384 || vector.iter().any(|value| !value.is_finite()) {
        return None;
    }
    let max = vector.iter().fold(0.0_f64, |peak, value| peak.max(value.abs()));
    if max == 0.0 {
        return None;
    }
    for value in &mut vector {
        *value /= max;
    }
    let norm = vector.iter().map(|value| value * value).sum::<f64>().sqrt();
    for value in &mut vector {
        *value /= norm;
    }
    Some(vector)
}
