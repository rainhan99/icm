//! Server-side store-RPC dispatcher (F-001, feature `remote-store`).
//!
//! Maps a [`icm_core::RpcRequest`] `method` + `params` to a call on the
//! central node's local [`Store`], serializing the result back into an
//! [`icm_core::RpcResponse`]. The client counterpart is
//! `icm_store::RemoteHttpStore`; both agree on the method names in
//! [`icm_core::ALL_METHODS`] and the per-method `params` shapes defined
//! here.
//!
//! Error semantics (see the plan's HTTP-API risk analysis):
//! - a store `Ok(None)` (e.g. `get` miss) serializes to a JSON `null`
//!   **result**, never an `error` — the client maps it back to `Ok(None)`;
//! - an unknown method is a closed-allow-list rejection carried in the
//!   `error` field, never a panic;
//! - embeddings for `store` / `search_hybrid` are computed HERE, on the
//!   server, so thin clients never load a model.

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use icm_core::{
    Embedder, FactsStore, Feedback, FeedbackStore, IcmError, IcmResult, Memory, MemoryStore,
    RpcResponse,
};
use icm_store::Store;

// --- param helpers -------------------------------------------------------

fn want_str(p: &Value, k: &str) -> IcmResult<String> {
    p.get(k)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| IcmError::InvalidInput(format!("missing string param '{k}'")))
}

fn want_f32(p: &Value, k: &str) -> IcmResult<f32> {
    p.get(k)
        .and_then(Value::as_f64)
        .map(|n| n as f32)
        .ok_or_else(|| IcmError::InvalidInput(format!("missing number param '{k}'")))
}

fn opt_str(p: &Value, k: &str) -> Option<String> {
    p.get(k).and_then(Value::as_str).map(str::to_string)
}

fn opt_usize(p: &Value, k: &str, default: usize) -> usize {
    p.get(k)
        .and_then(Value::as_u64)
        .map(|n| n as usize)
        .unwrap_or(default)
}

fn want_val<T: DeserializeOwned>(p: &Value, k: &str) -> IcmResult<T> {
    let v = p
        .get(k)
        .ok_or_else(|| IcmError::InvalidInput(format!("missing param '{k}'")))?;
    serde_json::from_value(v.clone())
        .map_err(|e| IcmError::InvalidInput(format!("bad param '{k}': {e}")))
}

fn as_value<T: Serialize>(v: T) -> IcmResult<Value> {
    serde_json::to_value(v).map_err(IcmError::from)
}

// --- dispatch ------------------------------------------------------------

/// Dispatch one store-RPC call. Never panics: every failure path becomes
/// an `error` response.
pub fn dispatch(
    store: &Store,
    embedder: Option<&dyn Embedder>,
    method: &str,
    params: Value,
) -> RpcResponse {
    match dispatch_inner(store, embedder, method, &params) {
        Ok(v) => RpcResponse::ok(v),
        Err(e) => RpcResponse::fail(e.to_string()),
    }
}

fn dispatch_inner(
    store: &Store,
    embedder: Option<&dyn Embedder>,
    method: &str,
    p: &Value,
) -> IcmResult<Value> {
    match method {
        // --- MemoryStore ---
        "memory.store" => {
            let mut m: Memory = want_val(p, "memory")?;
            // Server-side embedding: thin clients send text only.
            if m.embedding.is_none() {
                if let Some(e) = embedder {
                    if let Ok(v) = e.embed(&format!("{} {}", m.topic, m.summary)) {
                        m.embedding = Some(v);
                    }
                }
            }
            as_value(store.store(m)?)
        }
        "memory.get" => as_value(store.get(&want_str(p, "id")?)?),
        "memory.update" => {
            let m: Memory = want_val(p, "memory")?;
            store.update(&m)?;
            Ok(Value::Null)
        }
        "memory.delete" => {
            store.delete(&want_str(p, "id")?)?;
            Ok(Value::Null)
        }
        "memory.search_by_keywords" => {
            let kw: Vec<String> = want_val(p, "keywords")?;
            let refs: Vec<&str> = kw.iter().map(String::as_str).collect();
            as_value(store.search_by_keywords(&refs, opt_usize(p, "limit", 10))?)
        }
        "memory.search_fts" => {
            as_value(store.search_fts(&want_str(p, "query")?, opt_usize(p, "limit", 10))?)
        }
        "memory.search_by_embedding" => {
            let emb: Vec<f32> = want_val(p, "embedding")?;
            as_value(store.search_by_embedding(&emb, opt_usize(p, "limit", 10))?)
        }
        "memory.search_hybrid" => {
            let query = want_str(p, "query")?;
            let limit = opt_usize(p, "limit", 5);
            // Embed the query server-side; fall back to FTS when no
            // embedder is configured so recall still works.
            let emb = match embedder {
                Some(e) => e.embed_query(&query).unwrap_or_default(),
                None => Vec::new(),
            };
            if emb.is_empty() {
                let rows: Vec<(Memory, f32)> = store
                    .search_fts(&query, limit)?
                    .into_iter()
                    .map(|m| (m, 0.0))
                    .collect();
                as_value(rows)
            } else {
                as_value(store.search_hybrid(&query, &emb, limit)?)
            }
        }
        "memory.update_access" => {
            store.update_access(&want_str(p, "id")?)?;
            Ok(Value::Null)
        }
        "memory.batch_update_access" => {
            let ids: Vec<String> = want_val(p, "ids")?;
            let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
            as_value(store.batch_update_access(&refs)?)
        }
        "memory.apply_decay" => as_value(store.apply_decay(want_f32(p, "decay_factor")?)?),
        "memory.prune" => as_value(store.prune(want_f32(p, "weight_threshold")?)?),
        "memory.list_all" => as_value(store.list_all()?),
        "memory.get_by_topic" => as_value(store.get_by_topic(&want_str(p, "topic")?)?),
        "memory.list_topics" => as_value(store.list_topics()?),
        "memory.consolidate_topic" => {
            let topic = want_str(p, "topic")?;
            let consolidated: Memory = want_val(p, "consolidated")?;
            store.consolidate_topic(&topic, consolidated)?;
            Ok(Value::Null)
        }
        "memory.count" => as_value(store.count()?),
        "memory.count_by_topic" => as_value(store.count_by_topic(&want_str(p, "topic")?)?),
        "memory.stats" => as_value(store.stats()?),
        "memory.topic_health" => as_value(store.topic_health(&want_str(p, "topic")?)?),

        // --- FactsStore ---
        "facts.set_fact" => as_value(store.set_fact(
            &want_str(p, "entity")?,
            &want_str(p, "key")?,
            &want_str(p, "value")?,
            &want_str(p, "source")?,
        )?),
        "facts.get_fact" => {
            as_value(store.get_fact(&want_str(p, "entity")?, &want_str(p, "key")?)?)
        }
        "facts.list_facts" => {
            let entity = want_str(p, "entity")?;
            let prefix = opt_str(p, "key_prefix");
            as_value(store.list_facts(&entity, prefix.as_deref())?)
        }
        "facts.history" => {
            as_value(store.history(&want_str(p, "entity")?, &want_str(p, "key")?)?)
        }
        "facts.forget_fact" => {
            as_value(store.forget_fact(&want_str(p, "entity")?, &want_str(p, "key")?)?)
        }
        "facts.facts_stats" => as_value(store.facts_stats()?),

        // --- FeedbackStore ---
        "feedback.store_feedback" => {
            let f: Feedback = want_val(p, "feedback")?;
            as_value(store.store_feedback(f)?)
        }
        "feedback.search_feedback" => {
            let query = want_str(p, "query")?;
            let topic = opt_str(p, "topic");
            as_value(store.search_feedback(&query, topic.as_deref(), opt_usize(p, "limit", 10))?)
        }
        "feedback.list_feedback" => {
            let topic = opt_str(p, "topic");
            as_value(store.list_feedback(topic.as_deref(), opt_usize(p, "limit", 10))?)
        }
        "feedback.increment_applied" => {
            store.increment_applied(&want_str(p, "id")?)?;
            Ok(Value::Null)
        }
        "feedback.delete_feedback" => {
            store.delete_feedback(&want_str(p, "id")?)?;
            Ok(Value::Null)
        }
        "feedback.feedback_stats" => as_value(store.feedback_stats()?),

        other => Err(IcmError::InvalidInput(format!(
            "unknown store-RPC method: {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use icm_core::Importance;
    use serde_json::json;

    #[test]
    fn dispatch_memory_count() {
        let store = Store::in_memory().unwrap();
        let resp = dispatch(&store, None, "memory.count", json!({}));
        assert_eq!(resp.result.unwrap(), json!(0));
        assert!(resp.error.is_none());
    }

    #[test]
    fn dispatch_unknown_method_errors() {
        let store = Store::in_memory().unwrap();
        let resp = dispatch(&store, None, "bogus.method", json!({}));
        assert!(resp.error.is_some());
        assert!(resp.result.is_none());
    }

    #[test]
    fn facts_feedback_roundtrip() {
        let store = Store::in_memory().unwrap();
        // facts.set_fact then facts.get_fact returns the same value.
        let set = dispatch(
            &store,
            None,
            "facts.set_fact",
            json!({"entity": "user", "key": "editor", "value": "helix", "source": "test"}),
        );
        assert!(set.error.is_none(), "set_fact: {:?}", set.error);
        let got = dispatch(
            &store,
            None,
            "facts.get_fact",
            json!({"entity": "user", "key": "editor"}),
        );
        let fact = got.result.unwrap();
        assert_eq!(fact["value"], json!("helix"));

        // feedback.feedback_stats works on an empty store.
        let stats = dispatch(&store, None, "feedback.feedback_stats", json!({}));
        assert!(stats.error.is_none());
        assert!(stats.result.unwrap().is_object());
    }

    #[test]
    fn dispatch_store_then_get_roundtrip() {
        let store = Store::in_memory().unwrap();
        let m = Memory::new("t".to_string(), "hello world".to_string(), Importance::Medium);
        let stored = dispatch(&store, None, "memory.store", json!({ "memory": m }));
        let id = stored.result.unwrap().as_str().unwrap().to_string();
        assert!(!id.is_empty());

        let got = dispatch(&store, None, "memory.get", json!({ "id": id }));
        assert!(got.result.unwrap().is_object(), "get returns the memory");

        // A get miss is a null result, not an error.
        let miss = dispatch(&store, None, "memory.get", json!({ "id": "nope" }));
        assert_eq!(miss.result, Some(Value::Null));
        assert!(miss.error.is_none());
    }
}
