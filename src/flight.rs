use std::{collections::HashMap, sync::{Arc, Mutex}};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};

type ResultChannel<T> = watch::Receiver<Option<Arc<T>>>;

struct Entry<T> {
    result: ResultChannel<T>,
    waiters: Arc<Semaphore>,
}

pub struct Flights<T> {
    entries: Mutex<HashMap<Arc<str>, Entry<T>>>,
    capacity: usize,
    waiters_per_key: usize,
}

pub enum Admission<T> {
    Lead(Leader<T>, Waiter<T>),
    Join(Waiter<T>),
    Saturated,
}

pub struct Leader<T> {
    flights: Arc<Flights<T>>,
    key: Arc<str>,
    result: watch::Sender<Option<Arc<T>>>,
}

pub struct Waiter<T> {
    result: ResultChannel<T>,
    _permit: Option<OwnedSemaphorePermit>,
}

impl<T> Flights<T> {
    pub fn new(capacity: usize, waiters_per_key: usize) -> Self {
        Self { entries: Mutex::new(HashMap::new()), capacity, waiters_per_key }
    }

    pub fn len(&self) -> usize {
        self.entries.lock().expect("in-flight lock").len()
    }

    pub fn admit(self: &Arc<Self>, prompt: &str) -> Admission<T> {
        let mut entries = self.entries.lock().expect("in-flight lock");
        if let Some(entry) = entries.get(prompt) {
            let Ok(permit) = entry.waiters.clone().try_acquire_owned() else {
                return Admission::Saturated;
            };
            return Admission::Join(Waiter { result: entry.result.clone(), _permit: Some(permit) });
        }
        if entries.len() >= self.capacity {
            return Admission::Saturated;
        }

        let (send, receive) = watch::channel(None);
        let key: Arc<str> = Arc::from(prompt);
        entries.insert(key.clone(), Entry {
            result: receive.clone(),
            waiters: Arc::new(Semaphore::new(self.waiters_per_key)),
        });
        Admission::Lead(
            Leader { flights: self.clone(), key, result: send },
            Waiter { result: receive, _permit: None },
        )
    }
}

impl<T> Leader<T> {
    pub fn complete(self, value: T) {
        self.result.send_replace(Some(Arc::new(value)));
    }
}

impl<T> Drop for Leader<T> {
    fn drop(&mut self) {
        self.flights.entries.lock().expect("in-flight lock").remove(&self.key);
    }
}

impl<T> Waiter<T> {
    pub async fn receive(mut self) -> Option<Arc<T>> {
        self.result.wait_for(Option::is_some).await.ok()?.clone()
    }
}

#[cfg(test)]
#[path = "../tests/unit/flight.rs"]
mod tests;
