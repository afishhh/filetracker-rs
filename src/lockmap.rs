use std::{borrow::Borrow, collections::HashMap, future::Future, hash::Hash, sync::Arc};

type LocksArc<K> = Arc<std::sync::Mutex<HashMap<K, Arc<tokio::sync::Mutex<()>>>>>;

pub struct LockMap<K: Hash + Eq + Send + 'static> {
    locks: LocksArc<K>,
    cleanup_worker: tokio::task::AbortHandle,
}

impl<K: Hash + Eq + Send + 'static> Drop for LockMap<K> {
    fn drop(&mut self) {
        self.cleanup_worker.abort();
    }
}

async fn cleanup_worker<K: Hash + Eq + Send>(map: LocksArc<K>) {
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
    interval.tick().await;
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        interval.tick().await;

        map.lock().unwrap().retain(|_, v| Arc::strong_count(v) > 1);
    }
}

impl<K: Hash + Eq + Send + 'static> LockMap<K> {
    pub fn new() -> Self {
        let locks = LocksArc::<K>::default();
        let cleanup_worker = tokio::spawn(cleanup_worker(locks.clone())).abort_handle();
        Self {
            locks,
            cleanup_worker,
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
