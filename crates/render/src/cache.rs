//! A tiny bounded memo table for per-texture D3D views.

use std::collections::HashMap;
use std::hash::Hash;

/// Maps keys to lazily created values, forgetting everything when full.
///
/// Decoders cycle through a small, fixed pool of texture slices, so the
/// working set is stable and small. Exceeding the capacity means the pool
/// was replaced (stream change, new decoder), and the old entries are dead
/// weight: dropping all of them is both the simplest and the right policy.
pub(crate) struct BoundedCache<K, V> {
    entries: HashMap<K, V>,
    capacity: usize,
}

impl<K: Hash + Eq, V: Clone> BoundedCache<K, V> {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            capacity: capacity.max(1),
        }
    }

    /// Returns the value for `key`, creating it with `create` if missing.
    pub(crate) fn get_or_try_insert<E>(
        &mut self,
        key: K,
        create: impl FnOnce() -> Result<V, E>,
    ) -> Result<V, E> {
        if let Some(value) = self.entries.get(&key) {
            return Ok(value.clone());
        }
        let value = create()?;
        if self.entries.len() >= self.capacity {
            self.entries.clear();
        }
        self.entries.insert(key, value.clone());
        Ok(value)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_once_per_key() {
        let mut cache = BoundedCache::new(4);
        let mut calls = 0;
        for _ in 0..3 {
            let v = cache.get_or_try_insert(1, || {
                calls += 1;
                Ok::<_, ()>("view")
            });
            assert_eq!(v, Ok("view"));
        }
        assert_eq!(calls, 1);
    }

    #[test]
    fn errors_are_not_cached() {
        let mut cache = BoundedCache::<u32, u32>::new(4);
        assert_eq!(cache.get_or_try_insert(1, || Err("boom")), Err("boom"));
        assert_eq!(cache.get_or_try_insert(1, || Ok::<_, &str>(7)), Ok(7));
    }

    #[test]
    fn overflow_starts_over() {
        let mut cache = BoundedCache::new(2);
        for key in 0..2 {
            cache.get_or_try_insert(key, || Ok::<_, ()>(key)).unwrap();
        }
        assert_eq!(cache.len(), 2);
        cache.get_or_try_insert(9, || Ok::<_, ()>(9)).unwrap();
        assert_eq!(cache.len(), 1);
    }
}
