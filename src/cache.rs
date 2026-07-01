//! A two-tier cache for Lore command results that are fully determined by
//! immutable, content-addressed inputs (revision hashes). Mutable results
//! (working-tree diffs, `status`, `history`, `branches`, `locks`, ...) must
//! never be routed through this cache — see `CommandRequest::cached_read` in
//! `crate::lore` for the call sites that are safe to cache.
//!
//! The in-memory tier gives instant repeats within a session; the filesystem
//! tier (under the platform cache directory) survives restarts. Both tiers
//! expire entries after a configurable TTL, and the filesystem tier is
//! additionally capped by total size, oldest-first.

use std::{
    collections::{HashMap, VecDeque},
    hash::{DefaultHasher, Hash, Hasher},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::model::LoreEvent;

/// Bumped whenever the on-disk entry format changes, so stale entries from a
/// previous LazyLore version are invalidated automatically rather than
/// causing a deserialization error.
const CACHE_SCHEMA_VERSION: u32 = 1;

/// Cheaply-cloneable handle to the cache. Cloning shares the same underlying
/// memory table and disk directory. `RevisionCache::disabled()` produces a
/// no-op handle so callers never need to branch on whether caching is on.
#[derive(Clone)]
pub struct RevisionCache {
    inner: Option<Arc<CacheInner>>,
}

struct CacheInner {
    mem: Mutex<MemCache>,
    /// Directory this cache instance's entries live in. Already scoped to a
    /// single repository by the caller (see `App::new`). `None` means the
    /// filesystem tier is disabled (memory-only).
    dir: Option<PathBuf>,
    ttl: Duration,
    max_disk_bytes: u64,
    max_mem_entries: usize,
}

#[derive(Default)]
struct MemCache {
    map: HashMap<u64, MemEntry>,
    /// FIFO insertion order, used to evict the oldest entry once `map` grows
    /// past `max_mem_entries`. Mirrors the bound used for `command_history`
    /// in `App::push_record`.
    order: VecDeque<u64>,
}

#[derive(Clone)]
struct MemEntry {
    inserted: Instant,
    events: Arc<Vec<LoreEvent>>,
}

#[derive(Serialize, Deserialize)]
struct DiskEntry {
    v: u32,
    created_unix_ms: u128,
    events: Vec<LoreEvent>,
}

impl RevisionCache {
    /// A cache that never stores or returns anything. Used when caching is
    /// disabled in config.
    pub fn disabled() -> Self {
        Self { inner: None }
    }

    pub fn new(
        dir: Option<PathBuf>,
        ttl: Duration,
        max_disk_bytes: u64,
        max_mem_entries: usize,
    ) -> Self {
        Self {
            inner: Some(Arc::new(CacheInner {
                mem: Mutex::new(MemCache::default()),
                dir,
                ttl,
                max_disk_bytes,
                max_mem_entries,
            })),
        }
    }

    /// Derive a stable cache key from the repository root and the exact argv
    /// passed to `lore`. Only meaningful for requests whose output is fully
    /// determined by immutable revision hashes/paths in `args`.
    pub fn key(repository: &Path, args: &[String]) -> u64 {
        let mut hasher = DefaultHasher::new();
        CACHE_SCHEMA_VERSION.hash(&mut hasher);
        repository.hash(&mut hasher);
        args.hash(&mut hasher);
        hasher.finish()
    }

    pub async fn get(&self, key: u64) -> Option<Arc<Vec<LoreEvent>>> {
        let inner = self.inner.as_ref()?;

        if let Some(events) = Self::mem_get(inner, key) {
            return Some(events);
        }

        let events = Self::disk_get(inner, key).await?;
        Self::mem_put(inner, key, events.clone());
        Some(events)
    }

    pub async fn put(&self, key: u64, events: &[LoreEvent]) {
        let Some(inner) = self.inner.as_ref() else {
            return;
        };
        let events = Arc::new(events.to_vec());
        Self::mem_put(inner, key, events.clone());
        Self::disk_put(inner, key, &events).await;
    }

    fn mem_get(inner: &CacheInner, key: u64) -> Option<Arc<Vec<LoreEvent>>> {
        let mut mem = inner.mem.lock().unwrap();
        let entry = mem.map.get(&key)?;
        if entry.inserted.elapsed() > inner.ttl {
            mem.map.remove(&key);
            mem.order.retain(|k| *k != key);
            return None;
        }
        Some(entry.events.clone())
    }

    fn mem_put(inner: &CacheInner, key: u64, events: Arc<Vec<LoreEvent>>) {
        let mut mem = inner.mem.lock().unwrap();
        if !mem.map.contains_key(&key) {
            mem.order.push_back(key);
        }
        mem.map.insert(
            key,
            MemEntry {
                inserted: Instant::now(),
                events,
            },
        );
        while mem.order.len() > inner.max_mem_entries {
            let Some(oldest) = mem.order.pop_front() else {
                break;
            };
            mem.map.remove(&oldest);
        }
    }

    async fn disk_get(inner: &CacheInner, key: u64) -> Option<Arc<Vec<LoreEvent>>> {
        let dir = inner.dir.as_ref()?;
        let path = entry_path(dir, key);
        let bytes = tokio::fs::read(&path).await.ok()?;
        let entry: DiskEntry = serde_json::from_slice(&bytes).ok()?;
        if entry.v != CACHE_SCHEMA_VERSION
            || unix_now_ms().saturating_sub(entry.created_unix_ms) > inner.ttl.as_millis()
        {
            let _ = tokio::fs::remove_file(&path).await;
            return None;
        }
        Some(Arc::new(entry.events))
    }

    async fn disk_put(inner: &CacheInner, key: u64, events: &[LoreEvent]) {
        let Some(dir) = inner.dir.as_ref() else {
            return;
        };
        if tokio::fs::create_dir_all(dir).await.is_err() {
            return;
        }
        let entry = DiskEntry {
            v: CACHE_SCHEMA_VERSION,
            created_unix_ms: unix_now_ms(),
            events: events.to_vec(),
        };
        let Ok(bytes) = serde_json::to_vec(&entry) else {
            return;
        };
        // Write-then-rename so a crash or concurrent read never observes a
        // half-written entry.
        let path = entry_path(dir, key);
        let tmp = path.with_extension("json.tmp");
        if tokio::fs::write(&tmp, &bytes).await.is_ok() {
            let _ = tokio::fs::rename(&tmp, &path).await;
        }
    }

    /// Delete expired entries and, if the directory is still over the disk
    /// budget, remove the oldest files until it is back under the cap.
    /// Intended to run once at startup in a spawned task; never blocks the
    /// caller and never panics on I/O errors.
    pub async fn prune(&self) {
        let Some(inner) = self.inner.as_ref() else {
            return;
        };
        let Some(dir) = inner.dir.as_ref() else {
            return;
        };
        let Ok(mut read_dir) = tokio::fs::read_dir(dir).await else {
            return;
        };

        let mut files: Vec<(PathBuf, u128, u64)> = Vec::new();
        while let Ok(Some(entry)) = read_dir.next_entry().await {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Ok(metadata) = entry.metadata().await else {
                continue;
            };
            let modified_ms = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_millis())
                .unwrap_or(0);
            files.push((path, modified_ms, metadata.len()));
        }

        let now = unix_now_ms();
        let mut kept = Vec::with_capacity(files.len());
        for (path, modified_ms, size) in files {
            if now.saturating_sub(modified_ms) > inner.ttl.as_millis() {
                let _ = tokio::fs::remove_file(&path).await;
            } else {
                kept.push((path, modified_ms, size));
            }
        }

        let mut total: u64 = kept.iter().map(|(_, _, size)| size).sum();
        if total <= inner.max_disk_bytes {
            return;
        }
        kept.sort_by_key(|(_, modified_ms, _)| *modified_ms);
        for (path, _, size) in kept {
            if total <= inner.max_disk_bytes {
                break;
            }
            if tokio::fs::remove_file(&path).await.is_ok() {
                total = total.saturating_sub(size);
            }
        }
    }
}

fn entry_path(dir: &Path, key: u64) -> PathBuf {
    dir.join(format!("{key:016x}.json"))
}

/// Derive a per-repository subdirectory of `base` (the platform cache dir) so
/// entries from different repositories never collide and can be pruned
/// independently: `<base>/revisions/<repository-hash>`.
pub fn scope_dir(base: &Path, repository: &Path) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    repository.hash(&mut hasher);
    base.join("revisions")
        .join(format!("{:016x}", hasher.finish()))
}

fn unix_now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(tag: &str) -> LoreEvent {
        LoreEvent {
            tag: tag.into(),
            data: serde_json::json!({"ok": true}),
        }
    }

    #[tokio::test]
    async fn put_then_get_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let cache = RevisionCache::new(
            Some(dir.path().to_path_buf()),
            Duration::from_secs(60),
            1024 * 1024,
            8,
        );
        let key = RevisionCache::key(Path::new("/repo"), &["revision".into(), "info".into()]);
        cache.put(key, &[event("revisionInfo")]).await;

        // A fresh handle with an empty memory tier still finds the disk entry.
        let cold = RevisionCache::new(
            Some(dir.path().to_path_buf()),
            Duration::from_secs(60),
            1024 * 1024,
            8,
        );
        let got = cold.get(key).await.expect("disk hit");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].tag, "revisionInfo");
    }

    #[tokio::test]
    async fn expired_entries_are_treated_as_misses() {
        let dir = tempfile::tempdir().unwrap();
        let cache = RevisionCache::new(
            Some(dir.path().to_path_buf()),
            Duration::from_millis(1),
            1024 * 1024,
            8,
        );
        let key = RevisionCache::key(Path::new("/repo"), &["diff".into()]);
        cache.put(key, &[event("fileDiff")]).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(cache.get(key).await.is_none());
    }

    #[tokio::test]
    async fn memory_tier_evicts_oldest_past_capacity() {
        let cache = RevisionCache::new(None, Duration::from_secs(60), 0, 2);
        for key in 0..3u64 {
            cache.put(key, &[event("x")]).await;
        }
        let mem = cache.inner.as_ref().unwrap().mem.lock().unwrap();
        assert_eq!(mem.map.len(), 2);
        assert!(!mem.map.contains_key(&0), "oldest entry should be evicted");
    }

    #[tokio::test]
    async fn prune_deletes_expired_and_enforces_disk_cap() {
        let dir = tempfile::tempdir().unwrap();
        let cache = RevisionCache::new(
            Some(dir.path().to_path_buf()),
            Duration::from_secs(3600),
            1,
            8,
        );
        cache.put(1, &[event("a")]).await;
        cache.put(2, &[event("b")]).await;
        cache.prune().await;
        let remaining = std::fs::read_dir(dir.path()).unwrap().count();
        assert!(
            remaining <= 1,
            "expected disk cap to evict down to <=1 file, found {remaining}"
        );
    }

    #[test]
    fn same_args_different_repository_produce_different_keys() {
        let args = vec![
            "revision".to_string(),
            "info".to_string(),
            "abc".to_string(),
        ];
        let key_a = RevisionCache::key(Path::new("/repo/a"), &args);
        let key_b = RevisionCache::key(Path::new("/repo/b"), &args);
        assert_ne!(key_a, key_b);
    }

    #[tokio::test]
    async fn disabled_cache_never_stores_or_returns() {
        let cache = RevisionCache::disabled();
        let key = RevisionCache::key(Path::new("/repo"), &["revision".into()]);
        cache.put(key, &[event("revisionInfo")]).await;
        assert!(cache.get(key).await.is_none());
    }
}
