use crate::error::IcmResult;
use crate::memory::{Memory, StoreStats, TopicHealth};

/// Similarity score above which a new memory is considered a duplicate of an existing one.
pub const DEDUP_SIMILARITY_THRESHOLD: f32 = 0.85;

/// Find an existing memory that is similar enough to be considered a duplicate.
///
/// Returns the closest match and its similarity score if the score exceeds `threshold`
/// and the match belongs to the same topic. Returns `None` otherwise.
pub fn find_similar_memory(
    store: &dyn MemoryStore,
    embed_text: &str,
    embedding: &[f32],
    topic: &str,
    threshold: f32,
) -> IcmResult<Option<(Memory, f32)>> {
    let similar = store.search_hybrid(embed_text, embedding, 1)?;
    Ok(similar
        .into_iter()
        .find(|(m, score)| *score > threshold && m.topic == topic))
}

/// Heuristic near-duplicate temporal supersession (F-003).
///
/// Before storing a new memory, call this to mark an existing **active**,
/// same-topic near-duplicate (cosine similarity > `threshold`) as
/// superseded (sets `superseded_at` via `update`). Returns the superseded
/// id, or `None` if nothing qualifies.
///
/// ## Why pure cosine, not the hybrid dedup score
///
/// Dedup ([`find_similar_memory`]) ranks on the hybrid FTS+vector score
/// (`0.3·FTS + 0.7·cosine`). That score weights the vector term at 0.7, so
/// even an *identical* embedding tops out around ~0.85 once the diluting
/// FTS term is folded in — a "90% similar" threshold expressed against the
/// hybrid score would be practically unreachable and the feature a no-op.
/// Near-duplicate is fundamentally a *semantic* notion, so supersession
/// ranks on the raw cosine similarity ([`MemoryStore::search_by_embedding`],
/// `distance_metric=cosine` → score is the cosine), where `threshold`
/// reads honestly as "≥ threshold cosine-similar" and 0.90 is both
/// meaningful and reachable.
///
/// Scope/limitation (authorized): this catches near-DUPLICATES (high
/// vector similarity), NOT semantic contradictions with low similarity
/// (e.g. "lives in NYC" → "moved to SF"). True contradiction detection
/// needs an LLM and is out of phase-1 scope.
///
/// `threshold >= 1.0` (or an empty embedding) disables supersession and
/// returns `None` — the store then behaves exactly as before.
pub fn supersede_similar(
    store: &dyn MemoryStore,
    topic: &str,
    embedding: &[f32],
    threshold: f32,
) -> IcmResult<Option<String>> {
    if threshold >= 1.0 || embedding.is_empty() {
        return Ok(None);
    }
    // `search_by_embedding` already excludes superseded rows, so we only
    // ever supersede an active match. Same-topic guard prevents superseding
    // an unrelated memory that merely happens to be embedding-close.
    let top = store.search_by_embedding(embedding, 1)?;
    match top
        .into_iter()
        .find(|(m, score)| *score > threshold && m.topic == topic)
    {
        Some((mut existing, _score)) => {
            existing.superseded_at = Some(chrono::Utc::now());
            let id = existing.id.clone();
            store.update(&existing)?;
            Ok(Some(id))
        }
        None => Ok(None),
    }
}

pub trait MemoryStore {
    // CRUD
    fn store(&self, memory: Memory) -> IcmResult<String>;
    fn get(&self, id: &str) -> IcmResult<Option<Memory>>;
    fn update(&self, memory: &Memory) -> IcmResult<()>;
    fn delete(&self, id: &str) -> IcmResult<()>;

    // Search
    fn search_by_keywords(&self, keywords: &[&str], limit: usize) -> IcmResult<Vec<Memory>>;
    fn search_fts(&self, query: &str, limit: usize) -> IcmResult<Vec<Memory>>;
    fn search_by_embedding(&self, embedding: &[f32], limit: usize)
        -> IcmResult<Vec<(Memory, f32)>>;
    fn search_hybrid(
        &self,
        query: &str,
        embedding: &[f32],
        limit: usize,
    ) -> IcmResult<Vec<(Memory, f32)>>;

    // Lifecycle
    fn update_access(&self, id: &str) -> IcmResult<()>;
    fn batch_update_access(&self, ids: &[&str]) -> IcmResult<usize>;
    fn apply_decay(&self, decay_factor: f32) -> IcmResult<usize>;
    fn prune(&self, weight_threshold: f32) -> IcmResult<usize>;

    // Organization
    fn list_all(&self) -> IcmResult<Vec<Memory>>;

    /// List every memory INCLUDING superseded ones (F-003). Powers the
    /// `--include-superseded` inspection switch. The default delegates to
    /// [`list_all`](Self::list_all) — which already excludes superseded —
    /// so backends that do not persist `superseded_at` behave identically
    /// (zero-regression). Backends that persist it override this.
    fn list_all_including_superseded(&self) -> IcmResult<Vec<Memory>> {
        self.list_all()
    }

    fn get_by_topic(&self, topic: &str) -> IcmResult<Vec<Memory>>;
    fn list_topics(&self) -> IcmResult<Vec<(String, usize)>>;
    fn consolidate_topic(&self, topic: &str, consolidated: Memory) -> IcmResult<()>;

    // Stats
    fn count(&self) -> IcmResult<usize>;
    fn count_by_topic(&self, topic: &str) -> IcmResult<usize>;
    fn stats(&self) -> IcmResult<StoreStats>;
    fn topic_health(&self, topic: &str) -> IcmResult<TopicHealth>;
}
