//! Caching embedder decorator (F-001, feature `cloud-embeddings`).
//!
//! Wraps any [`Embedder`] with a cache keyed on `(role, model, text)` so
//! repeated embeddings — common when re-storing or re-recalling similar
//! content — avoid a round-trip to a paid cloud endpoint. On the central
//! node this is where "embedding cache usage" (surfaced by `/cache`)
//! comes from.
//!
//! Two layers are added incrementally: this file provides the in-memory
//! LRU hot layer + atomic metrics; the disk-persistent layer (survives
//! restarts) is added on top.
//!
//! `role` distinguishes documents from queries because instruction-tuned
//! models (e5) embed the SAME text differently for `embed` vs
//! `embed_query`; collapsing them would return the wrong vector.

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use lru::LruCache;
use sha2::{Digest, Sha256};

use crate::cache::CacheMetrics;
use crate::embedder::Embedder;
use crate::error::IcmResult;

/// An [`Embedder`] wrapper adding a `(role, model, text)`-keyed cache.
pub struct CachingEmbedder {
    inner: Box<dyn Embedder + Send + Sync>,
    mem: Mutex<LruCache<String, Vec<f32>>>,
    /// When set, embeddings are also persisted here so the cache survives
    /// process restarts (the central node runs long-lived).
    dir: Option<PathBuf>,
    model: String,
    metrics: Arc<CacheMetrics>,
}

const DOC: &str = "doc";
const QRY: &str = "qry";

fn cache_key(role: &str, model: &str, text: &str) -> String {
    let mut h = Sha256::new();
    h.update(role.as_bytes());
    h.update([0u8]);
    h.update(model.as_bytes());
    h.update([0u8]);
    h.update(text.trim().as_bytes());
    let digest = h.finalize();
    let mut s = String::with_capacity(64);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

impl CachingEmbedder {
    /// In-memory-only cache with `capacity` hot entries.
    pub fn in_memory(inner: Box<dyn Embedder + Send + Sync>, model: &str, capacity: usize) -> Self {
        Self::build(inner, model, capacity, None)
    }

    /// Cache backed by an on-disk layer under `dir` plus the in-memory
    /// hot layer. The directory is created if missing; a creation failure
    /// degrades gracefully to in-memory only (logged by the caller).
    pub fn with_dir(
        inner: Box<dyn Embedder + Send + Sync>,
        model: &str,
        capacity: usize,
        dir: &Path,
    ) -> Self {
        let dir = if std::fs::create_dir_all(dir).is_ok() {
            Some(dir.to_path_buf())
        } else {
            None
        };
        Self::build(inner, model, capacity, dir)
    }

    fn build(
        inner: Box<dyn Embedder + Send + Sync>,
        model: &str,
        capacity: usize,
        dir: Option<PathBuf>,
    ) -> Self {
        let cap = NonZeroUsize::new(capacity.max(1)).expect("capacity >= 1");
        Self {
            inner,
            mem: Mutex::new(LruCache::new(cap)),
            dir,
            model: model.to_string(),
            metrics: Arc::new(CacheMetrics::default()),
        }
    }

    /// Path for a cache key: `<dir>/<first2>/<key>.bin`, sharding by the
    /// key prefix to keep directory sizes bounded.
    fn disk_path(&self, key: &str) -> Option<PathBuf> {
        let dir = self.dir.as_ref()?;
        Some(dir.join(&key[..2]).join(format!("{key}.bin")))
    }

    /// Read a cached vector from disk (little-endian f32), recording a
    /// disk hit. Returns `None` on any absence/parse error (treated as a
    /// miss — never a hard failure).
    fn disk_get(&self, key: &str) -> Option<Vec<f32>> {
        let path = self.disk_path(key)?;
        let bytes = std::fs::read(&path).ok()?;
        if bytes.is_empty() || bytes.len() % 4 != 0 {
            return None;
        }
        let vec: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        self.metrics.record_disk_hit();
        Some(vec)
    }

    /// Persist a vector to disk (best-effort; write failures are ignored
    /// so a read-only/full disk never breaks embedding).
    fn disk_put(&self, key: &str, vec: &[f32]) {
        let Some(path) = self.disk_path(key) else {
            return;
        };
        if let Some(parent) = path.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                return;
            }
        }
        let mut bytes = Vec::with_capacity(vec.len() * 4);
        for f in vec {
            bytes.extend_from_slice(&f.to_le_bytes());
        }
        if std::fs::write(&path, &bytes).is_ok() {
            self.metrics.add_disk_bytes(bytes.len() as u64);
        }
    }

    /// Shared handle to the metrics, for the HTTP `/cache/stats` layer.
    pub fn metrics(&self) -> Arc<CacheMetrics> {
        Arc::clone(&self.metrics)
    }

    /// Look up `key` in the hot cache, recording a hit on success.
    fn mem_get(&self, key: &str) -> Option<Vec<f32>> {
        let mut c = self.mem.lock().ok()?;
        let v = c.get(key).cloned();
        if v.is_some() {
            self.metrics.record_hit();
        }
        v
    }

    /// Insert into the hot cache and refresh the entry-count gauge.
    fn mem_put(&self, key: String, vec: Vec<f32>) {
        if let Ok(mut c) = self.mem.lock() {
            c.put(key, vec);
            self.metrics.set_entries(c.len() as u64);
        }
    }

    /// Core path shared by `embed` / `embed_query`: cache by `(role,
    /// model, text)`, falling through to the inner embedder on a miss.
    fn embed_role(&self, role: &str, text: &str) -> IcmResult<Vec<f32>> {
        let key = cache_key(role, &self.model, text);
        if let Some(v) = self.mem_get(&key) {
            return Ok(v);
        }
        if let Some(v) = self.disk_get(&key) {
            self.mem_put(key, v.clone());
            return Ok(v);
        }
        self.metrics.record_miss();
        let vec = if role == QRY {
            self.inner.embed_query(text)?
        } else {
            self.inner.embed(text)?
        };
        self.mem_put(key.clone(), vec.clone());
        self.disk_put(&key, &vec);
        Ok(vec)
    }
}

impl Embedder for CachingEmbedder {
    fn embed(&self, text: &str) -> IcmResult<Vec<f32>> {
        self.embed_role(DOC, text)
    }

    fn embed_query(&self, text: &str) -> IcmResult<Vec<f32>> {
        self.embed_role(QRY, text)
    }

    fn embed_batch(&self, texts: &[&str]) -> IcmResult<Vec<Vec<f32>>> {
        // Serve cached items from the hot layer; batch only the misses to
        // the inner embedder so cloud cost scales with cache-miss count.
        let mut out: Vec<Option<Vec<f32>>> = Vec::with_capacity(texts.len());
        let mut miss_idx: Vec<usize> = Vec::new();
        let mut miss_txt: Vec<&str> = Vec::new();
        for (i, t) in texts.iter().enumerate() {
            let key = cache_key(DOC, &self.model, t);
            if let Some(v) = self.mem_get(&key) {
                out.push(Some(v));
            } else if let Some(v) = self.disk_get(&key) {
                self.mem_put(key, v.clone());
                out.push(Some(v));
            } else {
                out.push(None);
                miss_idx.push(i);
                miss_txt.push(t);
            }
        }
        if !miss_txt.is_empty() {
            for _ in 0..miss_txt.len() {
                self.metrics.record_miss();
            }
            let fresh = self.inner.embed_batch(&miss_txt)?;
            for (slot, vec) in miss_idx.into_iter().zip(fresh) {
                let key = cache_key(DOC, &self.model, texts[slot]);
                self.mem_put(key.clone(), vec.clone());
                self.disk_put(&key, &vec);
                out[slot] = Some(vec);
            }
        }
        Ok(out.into_iter().map(|v| v.unwrap_or_default()).collect())
    }

    fn dimensions(&self) -> usize {
        self.inner.dimensions()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A stub embedder counting how many times the inner layer is hit.
    #[derive(Clone)]
    struct CountingEmbedder {
        vec: Vec<f32>,
        calls: Arc<AtomicUsize>,
    }
    impl CountingEmbedder {
        fn new(vec: Vec<f32>) -> Self {
            Self {
                vec,
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }
        fn calls(&self) -> usize {
            self.calls.load(Ordering::Relaxed)
        }
    }
    impl Embedder for CountingEmbedder {
        fn embed(&self, _t: &str) -> IcmResult<Vec<f32>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(self.vec.clone())
        }
        fn embed_batch(&self, texts: &[&str]) -> IcmResult<Vec<Vec<f32>>> {
            self.calls.fetch_add(texts.len(), Ordering::Relaxed);
            Ok(texts.iter().map(|_| self.vec.clone()).collect())
        }
        fn dimensions(&self) -> usize {
            self.vec.len()
        }
    }

    #[test]
    fn second_embed_hits_cache() {
        let inner = CountingEmbedder::new(vec![1.0, 2.0]);
        let c = CachingEmbedder::in_memory(Box::new(inner.clone()), "modelX", 8);
        let _ = c.embed("foo").unwrap();
        let _ = c.embed("foo").unwrap();
        assert_eq!(inner.calls(), 1, "inner embedder called once");
        assert_eq!(c.metrics().hits(), 1);
        assert_eq!(c.metrics().misses(), 1);
        assert_eq!(c.metrics().entries(), 1);
    }

    #[test]
    fn doc_and_query_roles_do_not_collide() {
        // e5-style asymmetry: same text, different role → different key.
        let k_doc = cache_key(DOC, "m", "hello");
        let k_qry = cache_key(QRY, "m", "hello");
        assert_ne!(k_doc, k_qry);
    }

    #[test]
    fn disk_cache_survives_new_instance() {
        let dir = tempfile::tempdir().unwrap();
        let inner = CountingEmbedder::new(vec![3.0]);
        {
            let c = CachingEmbedder::with_dir(Box::new(inner.clone()), "mX", 8, dir.path());
            c.embed("bar").unwrap();
        }
        // Fresh instance, empty hot cache: must read the prior vector off
        // disk without touching the (new) inner embedder.
        let inner2 = CountingEmbedder::new(vec![3.0]);
        let c2 = CachingEmbedder::with_dir(Box::new(inner2.clone()), "mX", 8, dir.path());
        let v = c2.embed("bar").unwrap();
        assert_eq!(v, vec![3.0]);
        assert_eq!(inner2.calls(), 0, "served from disk, inner untouched");
        assert_eq!(c2.metrics().disk_hits(), 1);
    }

    #[test]
    fn batch_only_embeds_misses() {
        let inner = CountingEmbedder::new(vec![0.5]);
        let c = CachingEmbedder::in_memory(Box::new(inner.clone()), "m", 8);
        let _ = c.embed("a").unwrap(); // warm "a" (1 call)
        let out = c.embed_batch(&["a", "b", "c"]).unwrap();
        assert_eq!(out.len(), 3);
        // "a" served from cache; only "b","c" hit the inner (2 more).
        assert_eq!(inner.calls(), 3);
    }
}
