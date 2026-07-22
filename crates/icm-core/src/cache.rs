//! Embedding-cache metrics (F-001).
//!
//! Pure atomic counters — no I/O, no extra dependencies — so this module
//! is always compiled regardless of the `cloud-embeddings` feature. That
//! lets the HTTP layer expose `/cache/stats` and the caching embedder
//! populate it, without the two being coupled through a feature gate.

use std::sync::atomic::{AtomicU64, Ordering};

/// Atomic counters describing embedding-cache behaviour. Cheap to read
/// (no lock), safe to share across threads via `Arc`.
#[derive(Debug, Default)]
pub struct CacheMetrics {
    hits: AtomicU64,
    misses: AtomicU64,
    disk_hits: AtomicU64,
    entries: AtomicU64,
    disk_bytes: AtomicU64,
}

impl CacheMetrics {
    /// In-memory (hot) hits.
    pub fn hits(&self) -> u64 {
        self.hits.load(Ordering::Relaxed)
    }
    /// Misses that fell through to the underlying embedder.
    pub fn misses(&self) -> u64 {
        self.misses.load(Ordering::Relaxed)
    }
    /// Hits served from the disk layer.
    pub fn disk_hits(&self) -> u64 {
        self.disk_hits.load(Ordering::Relaxed)
    }
    /// Distinct entries currently held in the in-memory cache.
    pub fn entries(&self) -> u64 {
        self.entries.load(Ordering::Relaxed)
    }
    /// Bytes written to the disk layer.
    pub fn disk_bytes(&self) -> u64 {
        self.disk_bytes.load(Ordering::Relaxed)
    }
    /// Underlying-embedder calls avoided = hot hits + disk hits. The
    /// headline "API calls saved" figure for cloud embedders.
    pub fn calls_saved(&self) -> u64 {
        self.hits() + self.disk_hits()
    }

    // --- mutators (used by the caching embedder) ---
    pub fn record_hit(&self) {
        self.hits.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_miss(&self) {
        self.misses.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_disk_hit(&self) {
        self.disk_hits.fetch_add(1, Ordering::Relaxed);
    }
    pub fn set_entries(&self, n: u64) {
        self.entries.store(n, Ordering::Relaxed);
    }
    pub fn add_disk_bytes(&self, n: u64) {
        self.disk_bytes.fetch_add(n, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_accumulate() {
        let m = CacheMetrics::default();
        m.record_hit();
        m.record_hit();
        m.record_miss();
        m.record_disk_hit();
        m.set_entries(5);
        m.add_disk_bytes(128);
        assert_eq!(m.hits(), 2);
        assert_eq!(m.misses(), 1);
        assert_eq!(m.disk_hits(), 1);
        assert_eq!(m.entries(), 5);
        assert_eq!(m.disk_bytes(), 128);
        assert_eq!(m.calls_saved(), 3);
    }
}
