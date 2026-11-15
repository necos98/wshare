//! Remembers the text layout of the overlay between presents.
//!
//! Building a DirectWrite layout is by far the costliest part of drawing the
//! overlay, and it only changes with the text, the window size or the DPI.

/// What a cached layout was made for besides the text itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LayoutKey {
    pub dpi: u32,
    pub size: (u32, u32),
}

/// One cached value `T`, valid for one [`LayoutKey`] until invalidated.
pub(super) struct LayoutCache<T> {
    entry: Option<(LayoutKey, T)>,
}

impl<T: Clone> LayoutCache<T> {
    pub(super) fn new() -> Self {
        Self { entry: None }
    }

    /// The cached value for `key`, built by `make` when there is none or it
    /// was made for another key.
    pub(super) fn get_or_try_insert<E>(
        &mut self,
        key: LayoutKey,
        make: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E> {
        if let Some((cached, value)) = &self.entry
            && *cached == key
        {
            return Ok(value.clone());
        }
        let value = make()?;
        self.entry = Some((key, value.clone()));
        Ok(value)
    }

    /// The text changed: whatever was cached is stale.
    pub(super) fn invalidate(&mut self) {
        self.entry = None;
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    const KEY: LayoutKey = LayoutKey {
        dpi: 96,
        size: (800, 600),
    };

    fn build(counter: &Cell<u32>) -> Result<u32, ()> {
        counter.set(counter.get() + 1);
        Ok(counter.get())
    }

    #[test]
    fn same_key_builds_once() {
        let built = Cell::new(0);
        let mut cache = LayoutCache::new();
        assert_eq!(cache.get_or_try_insert(KEY, || build(&built)), Ok(1));
        assert_eq!(cache.get_or_try_insert(KEY, || build(&built)), Ok(1));
        assert_eq!(built.get(), 1);
    }

    #[test]
    fn a_new_size_or_dpi_rebuilds() {
        let built = Cell::new(0);
        let mut cache = LayoutCache::new();
        cache.get_or_try_insert(KEY, || build(&built)).unwrap();
        let resized = LayoutKey {
            size: (1024, 768),
            ..KEY
        };
        assert_eq!(cache.get_or_try_insert(resized, || build(&built)), Ok(2));
        let sharper = LayoutKey {
            dpi: 144,
            ..resized
        };
        assert_eq!(cache.get_or_try_insert(sharper, || build(&built)), Ok(3));
    }

    #[test]
    fn invalidation_rebuilds_even_with_the_same_key() {
        let built = Cell::new(0);
        let mut cache = LayoutCache::new();
        cache.get_or_try_insert(KEY, || build(&built)).unwrap();
        cache.invalidate();
        assert_eq!(cache.get_or_try_insert(KEY, || build(&built)), Ok(2));
    }

    #[test]
    fn a_failed_build_is_not_cached() {
        let mut cache: LayoutCache<u32> = LayoutCache::new();
        assert_eq!(cache.get_or_try_insert(KEY, || Err("no")), Err("no"));
        assert_eq!(cache.get_or_try_insert::<&str>(KEY, || Ok(7)), Ok(7));
    }
}
