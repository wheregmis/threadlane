use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::GitError;

type Entry<V> = Arc<OnceLock<(Instant, Result<V, GitError>)>>;

/// Share both completed responses and in-flight work without holding the map
/// lock across I/O. Removing an entry also prevents an old flight from
/// repopulating the cache after a mutation.
pub(super) struct ResponseCache<K, V> {
    entries: Mutex<HashMap<K, Entry<V>>>,
}

impl<K: Eq + Hash + Clone, V: Clone> ResponseCache<K, V> {
    pub(super) fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    pub(super) fn get_or_fetch(
        &self,
        key: K,
        ttl: Duration,
        fetch: impl FnOnce() -> Result<V, GitError>,
    ) -> Result<V, GitError> {
        let entry = {
            let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            entries.retain(|_, entry| {
                entry.get().is_none_or(|(completed, result)| {
                    let lifetime = if result.is_err() {
                        Duration::from_secs(60)
                    } else {
                        ttl
                    };
                    now.saturating_duration_since(*completed) < lifetime
                })
            });
            // Bound search variants; never evict a running request.
            let capacity = if entries.contains_key(&key) { 256 } else { 255 };
            while entries.len() > capacity {
                let oldest = entries
                    .iter()
                    .filter(|(candidate, _)| *candidate != &key)
                    .filter_map(|(key, entry)| entry.get().map(|(at, _)| (key.clone(), *at)))
                    .min_by_key(|(_, at)| *at);
                if let Some((key, _)) = oldest {
                    entries.remove(&key);
                } else {
                    break;
                }
            }
            entries.entry(key).or_default().clone()
        };
        entry
            .get_or_init(|| {
                let result = fetch();
                (Instant::now(), result)
            })
            .1
            .clone()
    }

    pub(super) fn invalidate(&self, matches: impl Fn(&K) -> bool) {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|key, _| !matches(key));
    }
}

#[cfg(test)]
mod tests {
    use super::ResponseCache;
    use crate::GitError;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Barrier,
    };
    use std::time::Duration;

    #[test]
    fn concurrent_reads_share_one_fetch_and_cache_errors() {
        let cache = ResponseCache::new();
        let barrier = Barrier::new(12);
        let calls = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..12 {
                scope.spawn(|| {
                    barrier.wait();
                    assert_eq!(
                        cache
                            .get_or_fetch("repo", Duration::from_secs(300), || {
                                calls.fetch_add(1, Ordering::SeqCst);
                                Ok(42)
                            })
                            .unwrap(),
                        42
                    );
                });
            }
        });
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let error = GitError::new("repo", "offline");
        assert_eq!(
            cache.get_or_fetch("error", Duration::from_secs(300), || Err(error.clone())),
            Err(error.clone())
        );
        assert_eq!(
            cache.get_or_fetch("error", Duration::from_secs(300), || panic!(
                "retried too soon"
            )),
            Err(error)
        );
    }

    #[test]
    fn invalidation_during_fetch_does_not_restore_old_response() {
        let cache = ResponseCache::new();
        assert_eq!(
            cache
                .get_or_fetch("repo", Duration::from_secs(300), || {
                    cache.invalidate(|key| *key == "repo");
                    Ok(1)
                })
                .unwrap(),
            1
        );
        assert_eq!(
            cache
                .get_or_fetch("repo", Duration::from_secs(300), || Ok(2))
                .unwrap(),
            2
        );
        assert_eq!(
            cache
                .get_or_fetch("repo", Duration::ZERO, || Ok(3))
                .unwrap(),
            3
        );
        let bounded = ResponseCache::new();
        for key in 0..300 {
            bounded
                .get_or_fetch(key, Duration::from_secs(300), || Ok(key))
                .unwrap();
        }
        assert_eq!(bounded.entries.lock().unwrap().len(), 256);
    }
}
