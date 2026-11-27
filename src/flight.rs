use std::{collections::HashMap, sync::{Arc, Mutex}};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, SemaphorePermit, watch};

type ResultChannel<T> = watch::Receiver<Option<Arc<T>>>;

struct Entry<T> {
    result: ResultChannel<T>,
    waiters: Arc<Semaphore>,
}

type Pending<T> = HashMap<Arc<str>, Entry<T>>;

pub struct Flights<T> {
    entries: Mutex<[Pending<T>; 2]>,
    slots: Arc<Semaphore>,
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
    mode: usize,
    result: watch::Sender<Option<Arc<T>>>,
    slot: Option<OwnedSemaphorePermit>,
}

pub struct Waiter<T> {
    result: ResultChannel<T>,
    _permit: Option<OwnedSemaphorePermit>,
}

impl<T> Flights<T> {
    pub fn new(capacity: usize, waiters_per_key: usize) -> Self {
        Self {
            entries: Mutex::new([HashMap::new(), HashMap::new()]),
            slots: Arc::new(Semaphore::new(capacity)),
            capacity,
            waiters_per_key,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.lock().expect("in-flight lock").iter().map(HashMap::len).sum()
    }

    pub fn active_loads(&self) -> usize {
        self.capacity - self.slots.available_permits()
    }

    pub fn admit_uncached(&self) -> Option<SemaphorePermit<'_>> {
        self.slots.try_acquire().ok()
    }

    pub fn admit(self: &Arc<Self>, prompt: &str, exact_only: bool) -> Admission<T> {
        let mode = usize::from(exact_only);
        let mut entries = self.entries.lock().expect("in-flight lock");
        if let Some(entry) = entries[mode].get(prompt) {
            let Ok(permit) = entry.waiters.clone().try_acquire_owned() else {
                return Admission::Saturated;
            };
            return Admission::Join(Waiter { result: entry.result.clone(), _permit: Some(permit) });
        }
        let Ok(slot) = self.slots.clone().try_acquire_owned() else {
            return Admission::Saturated;
        };

        let (send, receive) = watch::channel(None);
        let key: Arc<str> = Arc::from(prompt);
        entries[mode].insert(key.clone(), Entry {
            result: receive.clone(),
            waiters: Arc::new(Semaphore::new(self.waiters_per_key)),
        });
        Admission::Lead(
            Leader { flights: self.clone(), key, mode, result: send, slot: Some(slot) },
            Waiter { result: receive, _permit: None },
        )
    }
}

impl<T> Leader<T> {
    pub fn complete(mut self, value: T) {
        self.release_capacity();
        self.result.send_replace(Some(Arc::new(value)));
    }

    fn release_capacity(&mut self) {
        let Some(slot) = self.slot.take() else { return; };
        let mut entries = self.flights.entries.lock().expect("in-flight lock");
        entries[self.mode].remove(&self.key);
        drop(slot);
    }
}

impl<T> Drop for Leader<T> {
    fn drop(&mut self) {
        self.release_capacity();
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
