//! LRU cache of finished translations, keyed by text, languages and the
//! engine the chain starts with.

use std::num::NonZeroUsize;
use std::sync::Mutex;

use lru::LruCache;

use super::{EngineId, Lang, Translation};

pub const DEFAULT_CAPACITY: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheKey {
    pub text: String,
    pub from: Option<Lang>,
    pub to: Lang,
    pub engine: EngineId,
}

impl CacheKey {
    pub fn new(text: &str, from: Option<&Lang>, to: &Lang, engine: EngineId) -> Self {
        Self {
            text: text.to_string(),
            from: from.cloned(),
            to: to.clone(),
            engine,
        }
    }
}

pub struct TranslationCache {
    inner: Mutex<LruCache<CacheKey, Translation>>,
}

impl TranslationCache {
    pub fn new(capacity: usize) -> Self {
        let capacity = NonZeroUsize::new(capacity).unwrap_or(NonZeroUsize::MIN);
        Self {
            inner: Mutex::new(LruCache::new(capacity)),
        }
    }

    pub fn get(&self, key: &CacheKey) -> Option<Translation> {
        self.lock().get(key).cloned()
    }

    pub fn put(&self, key: CacheKey, value: Translation) {
        self.lock().put(key, value);
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&self) {
        self.lock().clear();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, LruCache<CacheKey, Translation>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Default for TranslationCache {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(text: &str) -> Translation {
        Translation {
            text: text.into(),
            detected_from: None,
            engine: EngineId::GoogleFree,
            ms: 1,
        }
    }

    #[test]
    fn keys_distinguish_all_fields() {
        let cache = TranslationCache::new(10);
        let ar = Lang::new("ar");
        let en = Lang::new("en");
        cache.put(CacheKey::new("hi", None, &ar, EngineId::GoogleFree), t("a"));
        assert_eq!(
            cache
                .get(&CacheKey::new("hi", None, &ar, EngineId::GoogleFree))
                .unwrap()
                .text,
            "a"
        );
        assert!(cache
            .get(&CacheKey::new("hi", Some(&en), &ar, EngineId::GoogleFree))
            .is_none());
        assert!(cache
            .get(&CacheKey::new("hi", None, &en, EngineId::GoogleFree))
            .is_none());
        assert!(cache
            .get(&CacheKey::new("hi", None, &ar, EngineId::Deepl))
            .is_none());
        assert!(cache
            .get(&CacheKey::new("Hi", None, &ar, EngineId::GoogleFree))
            .is_none());
    }

    #[test]
    fn evicts_least_recently_used() {
        let cache = TranslationCache::new(2);
        let to = Lang::new("ar");
        let key = |s: &str| CacheKey::new(s, None, &to, EngineId::GoogleFree);
        cache.put(key("a"), t("A"));
        cache.put(key("b"), t("B"));
        assert!(cache.get(&key("a")).is_some()); // a is now most recent
        cache.put(key("c"), t("C"));
        assert!(cache.get(&key("b")).is_none());
        assert!(cache.get(&key("a")).is_some());
        assert_eq!(cache.len(), 2);
        assert_eq!(
            TranslationCache::default()
                .inner
                .lock()
                .unwrap()
                .cap()
                .get(),
            500
        );
    }
}
