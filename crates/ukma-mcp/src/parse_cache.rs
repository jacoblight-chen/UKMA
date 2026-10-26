//! Parse-result cache: a [`ParserPort`] decorator that reuses the parse of an
//! unchanged file instead of re-parsing it.
//!
//! The code domain parses lazily — a query parses the files it touches, every
//! time. Across processes that is repeated work with an identical result, so
//! this decorator stores each parse in the project cache, keyed by
//! `(parser version, language, path, content sha256)`:
//!
//! - a **changed** file has a new content hash, so it parses normally;
//! - an **unchanged** file is answered from the cache, with no parse at all;
//! - an **upgraded parser** (new [`PARSE_CACHE_VERSION`]) cannot read old
//!   entries, so stale structure can never be served as current.
//!
//! Honest boundaries: a failed parse is never cached (a transient or
//! capability failure must stay visible), and an unreadable payload falls back
//! to a real parse rather than failing the query — the cache is disposable by
//! design.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use code_intelligence::{CodeResult, ParsedSource, ParserPort, SupportLevel};
use storage_ports::snapshot::{sha256_hex, ParseCache, ParseKey};

/// The cache format version of the parse results this build produces.
///
/// Bump it whenever the extractor's output changes shape or meaning: entries
/// written by an older parser then simply stop matching.
pub const PARSE_CACHE_VERSION: &str = "1";

/// A [`ParserPort`] that answers unchanged files from the project cache.
pub struct CachingParser {
    inner: Arc<dyn ParserPort>,
    cache: Arc<dyn ParseCache>,
    hits: AtomicU64,
    misses: AtomicU64,
    poisoned: AtomicU64,
    write_errors: AtomicU64,
    last_write_error: std::sync::Mutex<Option<String>>,
}

impl CachingParser {
    /// Wrap `inner` with a persistent cache.
    pub fn new(inner: Arc<dyn ParserPort>, cache: Arc<dyn ParseCache>) -> Self {
        Self {
            inner,
            cache,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            poisoned: AtomicU64::new(0),
            write_errors: AtomicU64::new(0),
            last_write_error: std::sync::Mutex::new(None),
        }
    }

    /// How many parses were answered from the cache.
    pub fn hits(&self) -> u64 {
        self.hits.load(Ordering::Relaxed)
    }

    /// How many parses had to run.
    pub fn misses(&self) -> u64 {
        self.misses.load(Ordering::Relaxed)
    }

    /// How many cached payloads were unreadable (and re-parsed).
    pub fn poisoned(&self) -> u64 {
        self.poisoned.load(Ordering::Relaxed)
    }

    /// How many cache writes failed (the parse still succeeded).
    pub fn write_errors(&self) -> u64 {
        self.write_errors.load(Ordering::Relaxed)
    }

    /// The last write failure, when one happened (diagnostics only).
    pub fn last_write_error(&self) -> Option<String> {
        self.last_write_error
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
    }

    fn key(&self, path: &str, language: &str, source: &str) -> ParseKey {
        ParseKey {
            parser_version: PARSE_CACHE_VERSION.to_string(),
            language: language.to_string(),
            path: path.to_string(),
            content_sha256: sha256_hex(source),
        }
    }
}

impl std::fmt::Debug for CachingParser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachingParser")
            .field("hits", &self.hits())
            .field("misses", &self.misses())
            .field("poisoned", &self.poisoned())
            .field("write_errors", &self.write_errors())
            .finish()
    }
}

impl ParserPort for CachingParser {
    fn support(&self, language: &str) -> SupportLevel {
        // Capability is a property of the parser, never of the cache.
        self.inner.support(language)
    }

    fn parse(&self, path: &str, language: &str, source: &str) -> CodeResult<ParsedSource> {
        let key = self.key(path, language, source);
        if let Ok(Some(payload)) = self.cache.get(&key) {
            match serde_json::from_str::<ParsedSource>(&payload) {
                Ok(parsed) => {
                    self.hits.fetch_add(1, Ordering::Relaxed);
                    return Ok(parsed);
                }
                // A payload this build cannot read is a stale cache entry, not
                // a broken request: fall through and parse again.
                Err(_) => {
                    self.poisoned.fetch_add(1, Ordering::Relaxed);
                }
            }
        }

        self.misses.fetch_add(1, Ordering::Relaxed);
        let parsed = self.inner.parse(path, language, source)?;
        if let Ok(payload) = serde_json::to_string(&parsed) {
            let created_at = now_rfc3339();
            // A cache that cannot be written must not fail an otherwise valid
            // parse; the failure is counted and reported instead.
            if let Err(error) = self.cache.put(&key, &payload, &created_at) {
                self.write_errors.fetch_add(1, Ordering::Relaxed);
                if let Ok(mut guard) = self.last_write_error.lock() {
                    *guard = Some(error.to_string());
                }
            }
        }
        Ok(parsed)
    }
}

fn now_rfc3339() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0);
    crate::instant::format_rfc3339(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use storage_ports::snapshot::CacheError;

    /// A parser that records how often it ran.
    #[derive(Default)]
    struct SpyParser {
        calls: Mutex<Vec<String>>,
        fail: Mutex<bool>,
    }

    impl SpyParser {
        fn calls(&self) -> Vec<String> {
            self.calls.lock().expect("lock").clone()
        }
        fn set_fail(&self, fail: bool) {
            *self.fail.lock().expect("lock") = fail;
        }
    }

    impl ParserPort for SpyParser {
        fn support(&self, _language: &str) -> SupportLevel {
            SupportLevel::Ported
        }

        fn parse(&self, path: &str, language: &str, source: &str) -> CodeResult<ParsedSource> {
            self.calls.lock().expect("lock").push(path.to_string());
            if *self.fail.lock().expect("lock") {
                return Err(code_intelligence::CodeError::SourceParseFailed {
                    message: format!("spy failure on {path}"),
                });
            }
            Ok(ParsedSource {
                language: language.to_string(),
                content_hash: format!("sha256:{}", sha256_hex(source)),
                parser_version: PARSE_CACHE_VERSION.to_string(),
                symbols: Vec::new(),
                edges: Vec::new(),
                diagnostics: Vec::new(),
            })
        }
    }

    /// The port, in memory.
    #[derive(Default)]
    struct MemoryCache {
        entries: Mutex<BTreeMap<String, String>>,
    }

    impl MemoryCache {
        fn key(key: &ParseKey) -> String {
            format!(
                "{}\u{1f}{}\u{1f}{}\u{1f}{}",
                key.parser_version, key.language, key.path, key.content_sha256
            )
        }
    }

    impl ParseCache for MemoryCache {
        fn get(&self, key: &ParseKey) -> Result<Option<String>, CacheError> {
            Ok(self
                .entries
                .lock()
                .map_err(|_| CacheError::Invalid("poisoned".into()))?
                .get(&Self::key(key))
                .cloned())
        }
        fn put(&self, key: &ParseKey, payload: &str, _created_at: &str) -> Result<(), CacheError> {
            self.entries
                .lock()
                .map_err(|_| CacheError::Invalid("poisoned".into()))?
                .insert(Self::key(key), payload.to_string());
            Ok(())
        }
        fn len(&self) -> Result<usize, CacheError> {
            Ok(self
                .entries
                .lock()
                .map_err(|_| CacheError::Invalid("poisoned".into()))?
                .len())
        }
        fn clear(&self) -> Result<(), CacheError> {
            self.entries
                .lock()
                .map_err(|_| CacheError::Invalid("poisoned".into()))?
                .clear();
            Ok(())
        }
    }

    fn parser() -> (Arc<SpyParser>, Arc<MemoryCache>) {
        let inner = Arc::new(SpyParser::default());
        let cache = Arc::new(MemoryCache::default());
        (inner, cache)
    }

    #[test]
    fn the_second_parse_of_unchanged_content_never_reaches_the_parser() {
        let (inner, cache) = parser();
        let caching = CachingParser::new(inner.clone(), cache.clone());

        caching.parse("a.rs", "rust", "fn a() {}").expect("parse");
        assert_eq!(inner.calls(), vec!["a.rs"], "the first parse runs");
        assert_eq!(caching.misses(), 1);
        assert_eq!(caching.hits(), 0);

        let parsed = caching.parse("a.rs", "rust", "fn a() {}").expect("parse");
        assert_eq!(
            inner.calls(),
            vec!["a.rs"],
            "the second parse is answered from the cache"
        );
        assert_eq!(caching.hits(), 1);
        assert_eq!(caching.misses(), 1);
        assert_eq!(
            parsed.content_hash,
            format!("sha256:{}", sha256_hex("fn a() {}"))
        );
        assert_eq!(cache.len().expect("len"), 1);
    }

    #[test]
    fn changed_content_is_parsed_again() {
        let (inner, cache) = parser();
        let caching = CachingParser::new(inner.clone(), cache);
        caching.parse("a.rs", "rust", "fn a() {}").expect("parse");
        caching
            .parse("a.rs", "rust", "fn a() { b(); }")
            .expect("parse");
        assert_eq!(inner.calls().len(), 2, "new content is a new key");
        assert_eq!(caching.misses(), 2);
        assert_eq!(caching.hits(), 0);
    }

    #[test]
    fn the_same_content_in_another_path_is_parsed_separately() {
        let (inner, cache) = parser();
        let caching = CachingParser::new(inner.clone(), cache);
        caching.parse("a.rs", "rust", "fn x() {}").expect("parse");
        caching.parse("b.rs", "rust", "fn x() {}").expect("parse");
        assert_eq!(
            inner.calls(),
            vec!["a.rs", "b.rs"],
            "a symbol carries its declaration site, so the path is part of the key"
        );
    }

    #[test]
    fn a_failed_parse_is_never_cached() {
        let (inner, cache) = parser();
        inner.set_fail(true);
        let caching = CachingParser::new(inner.clone(), cache.clone());
        assert!(caching.parse("a.rs", "rust", "fn a() {}").is_err());
        assert_eq!(cache.len().expect("len"), 0, "failures are not results");

        inner.set_fail(false);
        caching.parse("a.rs", "rust", "fn a() {}").expect("parse");
        assert_eq!(cache.len().expect("len"), 1);
    }

    #[test]
    fn an_unreadable_payload_falls_back_to_a_real_parse() {
        let (inner, cache) = parser();
        let caching = CachingParser::new(inner.clone(), cache.clone());
        let key = caching.key("a.rs", "rust", "fn a() {}");
        cache.put(&key, "not json", "now").expect("seed");

        caching.parse("a.rs", "rust", "fn a() {}").expect("parse");
        assert_eq!(inner.calls(), vec!["a.rs"], "the parser ran anyway");
        assert_eq!(caching.poisoned(), 1);
        assert_eq!(caching.hits(), 0);
        // The entry was replaced with a readable payload.
        let stored = cache.get(&key).expect("get").expect("an entry");
        assert!(serde_json::from_str::<ParsedSource>(&stored).is_ok());
    }

    #[test]
    fn an_older_parser_version_does_not_answer() {
        let (inner, cache) = parser();
        let caching = CachingParser::new(inner.clone(), cache.clone());
        let mut old = caching.key("a.rs", "rust", "fn a() {}");
        old.parser_version = "0".into();
        cache
            .put(&old, "{\"language\":\"rust\"}", "now")
            .expect("seed");

        caching.parse("a.rs", "rust", "fn a() {}").expect("parse");
        assert_eq!(inner.calls(), vec!["a.rs"], "a version bump invalidates");
    }

    #[test]
    fn support_is_never_cached() {
        let (inner, cache) = parser();
        let caching = CachingParser::new(inner, cache);
        assert!(matches!(caching.support("rust"), SupportLevel::Ported));
    }

    #[test]
    fn a_real_parse_round_trips_through_the_cache() {
        let cache = Arc::new(MemoryCache::default());
        let caching = CachingParser::new(
            Arc::new(code_intelligence::parsers::engine::UnifiedParser::new()),
            cache.clone(),
        );
        let source = "fn outer() {\n    inner();\n}\n\nfn inner() {}\n";
        let first = caching.parse("a.rs", "rust", source).expect("parse");
        assert!(!first.symbols.is_empty(), "the fixture has symbols");
        let second = caching.parse("a.rs", "rust", source).expect("parse");
        assert_eq!(caching.poisoned(), 0, "the payload must be readable");
        assert_eq!(caching.hits(), 1);
        assert_eq!(caching.misses(), 1);
        assert_eq!(first, second, "a cached parse equals a fresh one");
        assert_eq!(cache.len().expect("len"), 1);
    }
}
