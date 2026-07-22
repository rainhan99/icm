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
    Concept, ConceptLink, Embedder, FactsStore, Feedback, FeedbackStore, IcmError, IcmResult,
    Label, Memoir, MemoirStore, Memory, MemoryStore, Relation, Role, RpcResponse, TranscriptStore,
};
use icm_store::Store;

#[cfg(feature = "code-graph")]
use icm_core::CodeGraphStore;

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
        "facts.history" => as_value(store.history(&want_str(p, "entity")?, &want_str(p, "key")?)?),
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

        // --- MemoirStore: memoir CRUD ---
        "memoir.create_memoir" => {
            let m: Memoir = want_val(p, "memoir")?;
            as_value(store.create_memoir(m)?)
        }
        "memoir.get_memoir" => as_value(store.get_memoir(&want_str(p, "id")?)?),
        "memoir.get_memoir_by_name" => as_value(store.get_memoir_by_name(&want_str(p, "name")?)?),
        "memoir.update_memoir" => {
            let m: Memoir = want_val(p, "memoir")?;
            store.update_memoir(&m)?;
            Ok(Value::Null)
        }
        "memoir.delete_memoir" => {
            store.delete_memoir(&want_str(p, "id")?)?;
            Ok(Value::Null)
        }
        "memoir.list_memoirs" => as_value(store.list_memoirs()?),

        // --- MemoirStore: concept CRUD ---
        "memoir.add_concept" => {
            let c: Concept = want_val(p, "concept")?;
            as_value(store.add_concept(c)?)
        }
        "memoir.get_concept" => as_value(store.get_concept(&want_str(p, "id")?)?),
        "memoir.get_concept_by_name" => {
            as_value(store.get_concept_by_name(&want_str(p, "memoir_id")?, &want_str(p, "name")?)?)
        }
        "memoir.update_concept" => {
            let c: Concept = want_val(p, "concept")?;
            store.update_concept(&c)?;
            Ok(Value::Null)
        }
        "memoir.delete_concept" => {
            store.delete_concept(&want_str(p, "id")?)?;
            Ok(Value::Null)
        }

        // --- MemoirStore: concept search ---
        "memoir.list_concepts" => as_value(store.list_concepts(&want_str(p, "memoir_id")?)?),
        "memoir.search_concepts_fts" => as_value(store.search_concepts_fts(
            &want_str(p, "memoir_id")?,
            &want_str(p, "query")?,
            opt_usize(p, "limit", 10),
        )?),
        "memoir.search_concepts_by_label" => {
            let memoir_id = want_str(p, "memoir_id")?;
            let label: Label = want_val(p, "label")?;
            as_value(store.search_concepts_by_label(
                &memoir_id,
                &label,
                opt_usize(p, "limit", 10),
            )?)
        }
        "memoir.search_all_concepts_fts" => as_value(
            store.search_all_concepts_fts(&want_str(p, "query")?, opt_usize(p, "limit", 10))?,
        ),

        // --- MemoirStore: refinement ---
        "memoir.refine_concept" => {
            let id = want_str(p, "id")?;
            let def = want_str(p, "new_definition")?;
            let sources: Vec<String> = want_val(p, "new_source_ids")?;
            store.refine_concept(&id, &def, &sources)?;
            Ok(Value::Null)
        }

        // --- MemoirStore: graph ---
        "memoir.add_link" => {
            let link: ConceptLink = want_val(p, "link")?;
            as_value(store.add_link(link)?)
        }
        "memoir.get_links_from" => as_value(store.get_links_from(&want_str(p, "concept_id")?)?),
        "memoir.get_links_to" => as_value(store.get_links_to(&want_str(p, "concept_id")?)?),
        "memoir.delete_link" => {
            store.delete_link(&want_str(p, "id")?)?;
            Ok(Value::Null)
        }
        "memoir.get_neighbors" => {
            let concept_id = want_str(p, "concept_id")?;
            let relation: Option<Relation> =
                match p.get("relation") {
                    Some(Value::Null) | None => None,
                    Some(v) => Some(serde_json::from_value(v.clone()).map_err(|e| {
                        IcmError::InvalidInput(format!("bad param 'relation': {e}"))
                    })?),
                };
            as_value(store.get_neighbors(&concept_id, relation)?)
        }
        "memoir.get_neighborhood" => {
            as_value(store.get_neighborhood(&want_str(p, "concept_id")?, opt_usize(p, "depth", 1))?)
        }
        "memoir.get_links_for_memoir" => {
            as_value(store.get_links_for_memoir(&want_str(p, "memoir_id")?)?)
        }

        // --- MemoirStore: stats ---
        "memoir.memoir_stats" => as_value(store.memoir_stats(&want_str(p, "memoir_id")?)?),
        "memoir.batch_memoir_concept_counts" => as_value(store.batch_memoir_concept_counts()?),

        // --- TranscriptStore ---
        "transcript.create_session" => as_value(store.create_session(
            &want_str(p, "agent")?,
            opt_str(p, "project").as_deref(),
            opt_str(p, "metadata").as_deref(),
        )?),
        "transcript.ensure_session" => as_value(store.ensure_session(
            &want_str(p, "id")?,
            &want_str(p, "agent")?,
            opt_str(p, "project").as_deref(),
            opt_str(p, "metadata").as_deref(),
        )?),
        "transcript.get_session" => as_value(store.get_session(&want_str(p, "id")?)?),
        "transcript.list_sessions" => as_value(
            store.list_sessions(opt_str(p, "project").as_deref(), opt_usize(p, "limit", 50))?,
        ),
        "transcript.record_message" => {
            let role: Role = want_val(p, "role")?;
            let tokens: Option<i64> = p.get("tokens").and_then(Value::as_i64);
            as_value(store.record_message(
                &want_str(p, "session_id")?,
                role,
                &want_str(p, "content")?,
                opt_str(p, "tool_name").as_deref(),
                tokens,
                opt_str(p, "metadata").as_deref(),
            )?)
        }
        "transcript.list_session_messages" => as_value(store.list_session_messages(
            &want_str(p, "session_id")?,
            opt_usize(p, "limit", 100),
            opt_usize(p, "offset", 0),
        )?),
        "transcript.search_transcripts" => as_value(store.search_transcripts(
            &want_str(p, "query")?,
            opt_str(p, "session_id").as_deref(),
            opt_str(p, "project").as_deref(),
            opt_usize(p, "limit", 10),
        )?),
        "transcript.forget_session" => {
            store.forget_session(&want_str(p, "id")?)?;
            Ok(Value::Null)
        }
        "transcript.transcript_stats" => as_value(store.transcript_stats()?),

        // --- CodeGraphStore (F-002) ---
        #[cfg(feature = "code-graph")]
        "code.index_file" => {
            let file: icm_core::CodeFile = want_val(p, "file")?;
            let symbols: Vec<icm_core::Symbol> = want_val(p, "symbols")?;
            let refs: Vec<icm_core::Ref> = want_val(p, "refs")?;
            store.index_file(&file, &symbols, &refs)?;
            Ok(Value::Null)
        }
        #[cfg(feature = "code-graph")]
        "code.delete_file" => {
            store.delete_file(&want_str(p, "path")?)?;
            Ok(Value::Null)
        }
        #[cfg(feature = "code-graph")]
        "code.file_hash" => as_value(store.file_hash(&want_str(p, "path")?)?),
        #[cfg(feature = "code-graph")]
        "code.get_symbol" => as_value(store.get_symbol(&want_str(p, "id")?)?),
        #[cfg(feature = "code-graph")]
        "code.find_symbols" => {
            as_value(store.find_symbols(&want_str(p, "name")?, opt_usize(p, "limit", 10))?)
        }
        #[cfg(feature = "code-graph")]
        "code.callers" => as_value(store.callers(&want_str(p, "symbol_id")?)?),
        #[cfg(feature = "code-graph")]
        "code.callees" => as_value(store.callees(&want_str(p, "symbol_id")?)?),
        #[cfg(feature = "code-graph")]
        "code.explore" => {
            as_value(store.explore(&want_str(p, "name")?, opt_usize(p, "max_depth", 3))?)
        }
        #[cfg(feature = "code-graph")]
        "code.list_stale" => as_value(store.list_stale()?),
        #[cfg(feature = "code-graph")]
        "code.mark_stale" => {
            let paths: Vec<String> = want_val(p, "paths")?;
            store.mark_stale(&paths)?;
            Ok(Value::Null)
        }
        #[cfg(feature = "code-graph")]
        "code.code_stats" => as_value(store.code_stats()?),

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
    fn memoir_roundtrip() {
        let store = Store::in_memory().unwrap();
        // create_memoir → get_memoir returns it.
        let m = Memoir::new("proj".to_string(), "a project".to_string());
        let created = dispatch(&store, None, "memoir.create_memoir", json!({ "memoir": m }));
        let memoir_id = created.result.unwrap().as_str().unwrap().to_string();
        assert!(!memoir_id.is_empty());

        // add_concept then list_concepts shows it.
        let c = Concept::new(
            memoir_id.clone(),
            "widget".to_string(),
            "a thing".to_string(),
        );
        let added = dispatch(&store, None, "memoir.add_concept", json!({ "concept": c }));
        assert!(added.error.is_none(), "add_concept: {:?}", added.error);
        let listed = dispatch(
            &store,
            None,
            "memoir.list_concepts",
            json!({ "memoir_id": memoir_id }),
        );
        let arr = listed.result.unwrap();
        assert_eq!(arr.as_array().unwrap().len(), 1);
    }

    #[test]
    fn transcript_roundtrip() {
        let store = Store::in_memory().unwrap();
        let sid = dispatch(
            &store,
            None,
            "transcript.ensure_session",
            json!({"id": "s1", "agent": "claude", "project": "icm"}),
        );
        assert!(sid.error.is_none(), "ensure_session: {:?}", sid.error);

        let rec = dispatch(
            &store,
            None,
            "transcript.record_message",
            json!({"session_id": "s1", "role": Role::User, "content": "hello"}),
        );
        assert!(rec.error.is_none(), "record_message: {:?}", rec.error);

        let msgs = dispatch(
            &store,
            None,
            "transcript.list_session_messages",
            json!({"session_id": "s1"}),
        );
        assert_eq!(msgs.result.unwrap().as_array().unwrap().len(), 1);
    }

    #[cfg(feature = "code-graph")]
    #[test]
    fn code_dispatch_index_and_explore() {
        use icm_core::{CodeFile, CodeLanguage, Ref, RefKind, Symbol, SymbolKind};
        let store = Store::in_memory().unwrap();
        let a = Symbol {
            id: "f#a@1".into(),
            file: "f.rs".into(),
            name: "a".into(),
            kind: SymbolKind::Function,
            language: CodeLanguage::Rust,
            start_line: 1,
            end_line: 1,
            parent: None,
        };
        let b = Symbol {
            id: "f#b@2".into(),
            name: "b".into(),
            start_line: 2,
            end_line: 2,
            ..a.clone()
        };
        let call = Ref {
            from_symbol: a.id.clone(),
            target_name: "b".into(),
            target_symbol: Some(b.id.clone()),
            kind: RefKind::Call,
            line: 1,
        };
        let file = CodeFile {
            path: "f.rs".into(),
            language: CodeLanguage::Rust,
            content_hash: "h".into(),
            stale: false,
        };
        let idx = dispatch(
            &store,
            None,
            "code.index_file",
            json!({ "file": file, "symbols": [a, b], "refs": [call] }),
        );
        assert!(idx.error.is_none(), "index_file: {:?}", idx.error);

        let exp = dispatch(&store, None, "code.explore", json!({ "name": "b" }));
        let v = exp.result.unwrap();
        assert_eq!(v["symbol"]["name"], json!("b"));
        assert_eq!(v["callers"].as_array().unwrap().len(), 1);

        let stats = dispatch(&store, None, "code.code_stats", json!({}));
        assert_eq!(stats.result.unwrap()["symbols"], json!(2));
    }

    #[test]
    fn dispatch_store_then_get_roundtrip() {
        let store = Store::in_memory().unwrap();
        let m = Memory::new(
            "t".to_string(),
            "hello world".to_string(),
            Importance::Medium,
        );
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
