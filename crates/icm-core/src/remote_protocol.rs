//! Shared store-RPC contract for the remote backend (F-001).
//!
//! This module is the single source of truth that BOTH ends of the
//! three-tier deployment agree on:
//!
//! - the **client** ([`crate`]-external `RemoteHttpStore` in `icm-store`,
//!   feature `remote-store`) serializes an [`RpcRequest`] and POSTs it to
//!   the central node's `/rpc` endpoint;
//! - the **server** (`icm serve --http` in `icm-cli`) deserializes it,
//!   dispatches to its local `Store`, and returns an [`RpcResponse`].
//!
//! It lives in `icm-core` — not `icm-mcp` — on purpose: `icm-store`
//! (client) must depend on the contract, and `icm-mcp` already depends on
//! `icm-store`, so putting it in `icm-mcp` would create a dependency
//! cycle. `icm-core` is the one crate both ends already depend on.
//!
//! The envelope reuses the JSON-RPC 2.0 *shape* (a named `method` + a
//! `params` payload, answered by either a `result` or an `error`) but is
//! a deliberately small internal contract, not a full JSON-RPC server.
//! Every method name maps 1:1 to a store-trait method so the client's
//! trait impls stay thin one-line forwards.
//!
//! Pure types only — no I/O, no extra dependencies beyond `serde` /
//! `serde_json` which `icm-core` already carries — so the module is
//! always compiled regardless of feature flags.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A store operation request: a `method` name from [`ALL_METHODS`] plus a
/// JSON `params` object whose shape is defined by the corresponding
/// dispatch/forward pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcRequest {
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

impl RpcRequest {
    pub fn new(method: impl Into<String>, params: Value) -> Self {
        Self {
            method: method.into(),
            params,
        }
    }
}

/// A store operation response. Exactly one of `result` / `error` is set.
///
/// Note a `null` `result` is a *success* carrying "no value" (e.g. a
/// `get()` that found nothing → `Ok(None)`), and MUST NOT be conflated
/// with the `error` arm. This asymmetry is why we do not overload HTTP
/// status codes for "not found".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl RpcResponse {
    pub fn ok(result: Value) -> Self {
        Self {
            result: Some(result),
            error: None,
        }
    }

    pub fn fail(message: impl Into<String>) -> Self {
        Self {
            result: None,
            error: Some(message.into()),
        }
    }
}

/// Every store-RPC method name, grouped by trait. Kept as one flat slice
/// so a test can assert uniqueness and the server can validate incoming
/// method names against a closed allow-list (unknown method → error,
/// never a panic or arbitrary dispatch).
pub const ALL_METHODS: &[&str] = &[
    // --- MemoryStore (20) ---
    "memory.store",
    "memory.get",
    "memory.update",
    "memory.delete",
    "memory.search_by_keywords",
    "memory.search_fts",
    "memory.search_by_embedding",
    "memory.search_hybrid",
    "memory.update_access",
    "memory.batch_update_access",
    "memory.apply_decay",
    "memory.prune",
    "memory.list_all",
    "memory.get_by_topic",
    "memory.list_topics",
    "memory.consolidate_topic",
    "memory.count",
    "memory.count_by_topic",
    "memory.stats",
    "memory.topic_health",
    // --- FactsStore (6) ---
    "facts.set_fact",
    "facts.get_fact",
    "facts.list_facts",
    "facts.history",
    "facts.forget_fact",
    "facts.facts_stats",
    // --- FeedbackStore (6) ---
    "feedback.store_feedback",
    "feedback.search_feedback",
    "feedback.list_feedback",
    "feedback.increment_applied",
    "feedback.delete_feedback",
    "feedback.feedback_stats",
    // --- MemoirStore (25) ---
    "memoir.create_memoir",
    "memoir.get_memoir",
    "memoir.get_memoir_by_name",
    "memoir.update_memoir",
    "memoir.delete_memoir",
    "memoir.list_memoirs",
    "memoir.add_concept",
    "memoir.get_concept",
    "memoir.get_concept_by_name",
    "memoir.update_concept",
    "memoir.delete_concept",
    "memoir.list_concepts",
    "memoir.search_concepts_fts",
    "memoir.search_concepts_by_label",
    "memoir.search_all_concepts_fts",
    "memoir.refine_concept",
    "memoir.add_link",
    "memoir.get_links_from",
    "memoir.get_links_to",
    "memoir.delete_link",
    "memoir.get_neighbors",
    "memoir.get_neighborhood",
    "memoir.get_links_for_memoir",
    "memoir.memoir_stats",
    "memoir.batch_memoir_concept_counts",
    // --- TranscriptStore (9) ---
    "transcript.create_session",
    "transcript.ensure_session",
    "transcript.get_session",
    "transcript.list_sessions",
    "transcript.record_message",
    "transcript.list_session_messages",
    "transcript.search_transcripts",
    "transcript.forget_session",
    "transcript.transcript_stats",
    // --- CodeGraphStore (10, F-002) ---
    "code.index_file",
    "code.delete_file",
    "code.file_hash",
    "code.get_symbol",
    "code.find_symbols",
    "code.callers",
    "code.callees",
    "code.explore",
    "code.list_stale",
    "code.mark_stale",
    "code.code_stats",
];

/// True if `method` is a known store-RPC method (closed allow-list).
pub fn is_known_method(method: &str) -> bool {
    ALL_METHODS.contains(&method)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_roundtrips() {
        let req = RpcRequest::new("memory.count", serde_json::json!({}));
        let bytes = serde_json::to_vec(&req).expect("serialize");
        let back: RpcRequest = serde_json::from_slice(&bytes).expect("deserialize");
        assert_eq!(back.method, "memory.count");
    }

    #[test]
    fn response_null_result_is_success_not_error() {
        let r = RpcResponse::ok(Value::Null);
        assert!(r.error.is_none());
        assert_eq!(r.result, Some(Value::Null));
    }

    #[test]
    fn method_names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for m in ALL_METHODS {
            assert!(seen.insert(*m), "duplicate rpc method: {m}");
        }
    }

    #[test]
    fn allow_list_rejects_unknown() {
        assert!(is_known_method("memory.store"));
        assert!(!is_known_method("bogus.method"));
    }
}
