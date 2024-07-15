use std::{borrow::Borrow, collections::HashMap, future::Future, hash::Hash, sync::Arc};

pub struct LockMap<K: Hash + Eq> {
    locks: std::sync::Mutex<HashMap<K, Arc<tokio::sync::Mutex<()>>>>,
}

impl<K: Hash + Eq> LockMap<K> {
    pub fn new() -> Self {
        Self {
            locks: Default::default(),
        }
    }

    pub fn lock_ref<Q>(&self, key: &Q) -> impl Future<Output = tokio::sync::OwnedMutexGuard<()>>
    where
        Q: Hash + Eq + ?Sized + ToOwned<Owned = K>,
        K: Borrow<Q>,
    {
        let mut locks = self.locks.lock().unwrap();
        locks
            .get(key)
            .map(|lock| lock.clone().lock_owned())
            .unwrap_or_else(|| {
                let new_lock: Arc<tokio::sync::Mutex<()>> = Arc::default();
                locks.insert(key.to_owned(), new_lock.clone());
                new_lock.lock_owned()
            })
    }

    pub fn lock_owned(&self, key: K) -> impl Future<Output = tokio::sync::OwnedMutexGuard<()>> {
        self.locks
            .lock()
            .unwrap()
            .entry(key)
            .or_default()
            .clone()
            .lock_owned()
    }
}
