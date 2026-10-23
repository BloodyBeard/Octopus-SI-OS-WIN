use std::borrow::Borrow;
use std::hash::Hash;
use std::num::NonZeroUsize;
use std::sync::Mutex;
use std::sync::MutexGuard;

use lru::LruCache;
use sha1::Digest;
use sha1::Sha1;

pub struct BlockingLruCache<K, V> {
    inner: Mutex<LruCache<K, V>>,
}

impl<K, V> BlockingLruCache<K, V>
where
    K: Eq + Hash,
{
    #[must_use]
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self {
            inner: Mutex::new(LruCache::new(capacity)),
        }
    }

    pub fn get_or_insert_with(&self, key: K, value: impl FnOnce() -> V) -> V
    where
        V: Clone,
    {
        let mut guard = self.blocking_lock();
        if let Some(v) = guard.get(&key) {
            return v.clone();
        }
        let v = value();
        guard.put(key, v.clone());
        v
    }

    pub fn get_or_try_insert_with<E>(
        &self,
        key: K,
        value: impl FnOnce() -> Result<V, E>,
    ) -> Result<V, E>
    where
        V: Clone,
    {
        let mut guard = self.blocking_lock();
        if let Some(v) = guard.get(&key) {
            return Ok(v.clone());
        }
        let v = value()?;
        guard.put(key, v.clone());
        Ok(v)
    }

    #[must_use]
    pub fn try_with_capacity(capacity: usize) -> Option<Self> {
        NonZeroUsize::new(capacity).map(Self::new)
    }

    pub fn get<Q>(&self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
        V: Clone,
    {
        let mut guard = self.blocking_lock();
        guard.get(key).cloned()
    }

    pub fn insert(&self, key: K, value: V) -> Option<V> {
        let mut guard = self.blocking_lock();
        guard.put(key, value)
    }

    pub fn remove<Q>(&self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let mut guard = self.blocking_lock();
        guard.pop(key)
    }

    pub fn clear(&self) {
        self.blocking_lock().clear();
    }

    pub fn with_mut<R>(&self, callback: impl FnOnce(&mut LruCache<K, V>) -> R) -> R {
        callback(&mut self.blocking_lock())
    }

    pub fn blocking_lock(&self) -> MutexGuard<'_, LruCache<K, V>> {
        // This cache has a synchronous API; no guard crosses an await point.
        // An async mutex + block_in_place panics in the provider's current-thread runtime.
        self.inner.lock().expect("image cache mutex poisoned")
    }
}

#[must_use]
pub fn sha1_digest(bytes: &[u8]) -> [u8; 20] {
    let mut hasher = Sha1::new();
    hasher.update(bytes);
    let result = hasher.finalize();
    let mut out = [0; 20];
    out.copy_from_slice(&result);
    out
}

#[cfg(test)]
mod tests {
    use super::BlockingLruCache;
    use std::num::NonZeroUsize;

    #[tokio::test(flavor = "multi_thread")]
    async fn stores_and_retrieves_values() {
        let cache = BlockingLruCache::new(NonZeroUsize::new(2).expect("capacity"));

        assert!(cache.get(&"first").is_none());
        cache.insert("first", 1);
        assert_eq!(cache.get(&"first"), Some(1));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn evicts_least_recently_used() {
        let cache = BlockingLruCache::new(NonZeroUsize::new(2).expect("capacity"));
        cache.insert("a", 1);
        cache.insert("b", 2);
        assert_eq!(cache.get(&"a"), Some(1));

        cache.insert("c", 3);

        assert!(cache.get(&"b").is_none());
        assert_eq!(cache.get(&"a"), Some(1));
        assert_eq!(cache.get(&"c"), Some(3));
    }

    #[test]
    fn stores_without_runtime() {
        let cache = BlockingLruCache::new(NonZeroUsize::new(2).expect("capacity"));
        cache.insert("first", 1);
        assert_eq!(cache.get(&"first"), Some(1));

        assert_eq!(cache.get_or_insert_with("first", || 2), 1);

        assert_eq!(cache.remove(&"first"), Some(1));
        cache.clear();

        let result = cache.with_mut(|inner| {
            inner.put("tmp", 3);
            inner.get(&"tmp").cloned()
        });
        assert_eq!(result, Some(3));
        assert_eq!(cache.get(&"tmp"), Some(3));

        assert_eq!(cache.blocking_lock().len(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn caches_images_on_the_provider_runtime() {
        let cache = BlockingLruCache::new(NonZeroUsize::new(2).expect("capacity"));
        assert_eq!(cache.get_or_try_insert_with("image", || Ok::<_, ()>(7)), Ok(7));
        assert_eq!(cache.get_or_insert_with("image", || panic!("cached image recomputed")), 7);
        assert_eq!(cache.get(&"image"), Some(7));
    }
}
