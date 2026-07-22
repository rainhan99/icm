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
/// same-topic near-duplicate (hybrid similarity > `threshold`) as
/// superseded (sets `superseded_at` via `update`). Returns the superseded
/// id, or `None` if nothing qualifies.
///
/// Scope/limitation (authorized): this catches near-DUPLICATES (high
/// surface + vector similarity), NOT semantic contradictions with low
/// surface similarity (e.g. "lives in NYC" → "moved to SF"). True
/// contradiction detection needs an LLM and is out of phase-1 scope.
///
/// `threshold >= 1.0` (or an empty embedding) disables supersession and
/// returns `None` — the store then behaves exactly as before.
pub fn supersede_similar(
    store: &dyn MemoryStore,
    topic: &str,
    embed_text: &str,
    embedding: &[f32],
    threshold: f32,
) -> IcmResult<Option<String>> {
    if threshold >= 1.0 || embedding.is_empty() {
        return Ok(None);
    }
    // `find_similar_memory` searches hybrid (which already excludes
    // superseded rows), so we only ever supersede an active match.
    match find_similar_memory(store, embed_text, embedding, topic, threshold)? {
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
    fn get_by_topic(&self, topic: &str) -> IcmResult<Vec<Memory>>;
    fn list_topics(&self) -> IcmResult<Vec<(String, usize)>>;
    fn consolidate_topic(&self, topic: &str, consolidated: Memory) -> IcmResult<()>;

    // Stats
    fn count(&self) -> IcmResult<usize>;
    fn count_by_topic(&self, topic: &str) -> IcmResult<usize>;
    fn stats(&self) -> IcmResult<StoreStats>;
    fn topic_health(&self, topic: &str) -> IcmResult<TopicHealth>;
}
