use std::{collections::VecDeque, time::{Duration, Instant}};

struct Entry {
    prompt: String,
    embedding: Option<Vec<f64>>,
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

    pub fn find_exact(&mut self, prompt: &str) -> Option<String> {
        self.expire();
        let index = self.entries.iter().position(|entry| entry.prompt == prompt)?;
        Some(self.touch(index))
    }

    pub fn find_semantic(&mut self, embedding: &[f64]) -> Option<(String, f64)> {
        self.expire();
        let (index, score) = self.entries.iter().enumerate()
            .filter_map(|(index, entry)| {
                let candidate = entry.embedding.as_ref().filter(|value| value.len() == embedding.len())?;
                let score: f64 = candidate.iter().zip(embedding).map(|(a, b)| a * b).sum();
                Some((index, score.clamp(-1.0, 1.0)))
            })
            .filter(|&(_, score)| score > self.threshold)
            .max_by(|left, right| left.1.total_cmp(&right.1))?;
        Some((self.touch(index), score))
    }

    fn expire(&mut self) {
        let now = Instant::now();
        self.entries.retain(|entry| entry.expires > now);
    }

    fn touch(&mut self, index: usize) -> String {
        let entry = self.entries.remove(index).expect("matching cache entry");
        let answer = entry.answer.clone();
        self.entries.push_back(entry);
        answer
    }

    pub fn insert(&mut self, prompt: String, embedding: Option<Vec<f64>>, answer: String) -> String {
        self.expire();
        // Overlapping modes may both generate. Keep the answer that clients can already see.
        if let Some(index) = self.entries.iter().position(|entry| entry.prompt == prompt) {
            let entry = &mut self.entries[index];
            if entry.embedding.is_none() {
                entry.embedding = embedding;
            }
            return self.touch(index);
        }
        if self.entries.len() == self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back(Entry {
            prompt, embedding, answer: answer.clone(), expires: Instant::now() + self.ttl,
        });
        answer
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

#[cfg(test)]
#[path = "../tests/unit/cache.rs"]
mod tests;
