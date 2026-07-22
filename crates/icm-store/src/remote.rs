//! Remote store client (F-001, feature `remote-store`).
//!
//! [`RemoteHttpStore`] implements the store traits by forwarding each
//! call to a central `icm serve --http` node's `POST /rpc` endpoint over
//! the blocking `ureq` client (matching the synchronous store surface —
//! no async runtime). It holds no database and, crucially, no embedder:
//! the server embeds text, so a thin client needs neither a model nor a
//! local DB. Selected at runtime via `ICM_DB_BACKEND=remote` +
//! `ICM_REMOTE_URL` (see `backend.rs`).
//!
//! Method names and `params` shapes mirror `icm-cli`'s `rpc_dispatch`;
//! both sides share [`icm_core::RpcRequest`] / [`icm_core::RpcResponse`].

use std::collections::HashMap;

use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use icm_core::{
    Concept, ConceptLink, Fact, FactsStats, FactsStore, Feedback, FeedbackStats, FeedbackStore,
    IcmError, IcmResult, Label, Memoir, MemoirStats, MemoirStore, Memory, MemoryStore, Message,
    Relation, Role, RpcRequest, RpcResponse, Session, StoreStats, TopicHealth, TranscriptHit,
    TranscriptStats, TranscriptStore,
};

/// A store backed by a remote `icm serve --http` node.
pub struct RemoteHttpStore {
    /// Base URL of the remote node (scheme+host+port), no trailing slash.
    base_url: String,
    /// Optional Bearer token, sent when the server requires auth.
    token: Option<String>,
    agent: ureq::Agent,
}

impl RemoteHttpStore {
    /// Build a client for `base_url` (e.g. `http://192.168.1.10:11435`).
    pub fn new(base_url: &str, token: Option<String>) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            token,
            agent: ureq::agent(),
        }
    }

    /// Issue one store-RPC call, returning the raw JSON `result`. Any
    /// transport failure, non-2xx status, or `error` envelope becomes an
    /// [`IcmError::Remote`].
    fn call(&self, method: &str, params: Value) -> IcmResult<Value> {
        let url = format!("{}/rpc", self.base_url);
        let mut req = self.agent.post(&url);
        if let Some(t) = &self.token {
            req = req.set("Authorization", &format!("Bearer {t}"));
        }
        let envelope = serde_json::to_value(RpcRequest::new(method, params))
            .map_err(|e| IcmError::Remote(format!("encode {method}: {e}")))?;
        let resp = req
            .send_json(envelope)
            .map_err(|e| IcmError::Remote(format!("{method}: {e}")))?;
        let parsed: RpcResponse = resp
            .into_json()
            .map_err(|e| IcmError::Remote(format!("{method}: bad response: {e}")))?;
        if let Some(err) = parsed.error {
            return Err(IcmError::Remote(format!("{method}: {err}")));
        }
        Ok(parsed.result.unwrap_or(Value::Null))
    }

    /// [`Self::call`] plus decoding the result into `T`.
    fn call_de<T: DeserializeOwned>(&self, method: &str, params: Value) -> IcmResult<T> {
        let v = self.call(method, params)?;
        serde_json::from_value(v).map_err(|e| IcmError::Remote(format!("decode {method}: {e}")))
    }
}

impl MemoryStore for RemoteHttpStore {
    fn store(&self, memory: Memory) -> IcmResult<String> {
        self.call_de("memory.store", json!({ "memory": memory }))
    }

    fn get(&self, id: &str) -> IcmResult<Option<Memory>> {
        self.call_de("memory.get", json!({ "id": id }))
    }

    fn update(&self, memory: &Memory) -> IcmResult<()> {
        self.call("memory.update", json!({ "memory": memory }))?;
        Ok(())
    }

    fn delete(&self, id: &str) -> IcmResult<()> {
        self.call("memory.delete", json!({ "id": id }))?;
        Ok(())
    }

    fn search_by_keywords(&self, keywords: &[&str], limit: usize) -> IcmResult<Vec<Memory>> {
        self.call_de(
            "memory.search_by_keywords",
            json!({ "keywords": keywords, "limit": limit }),
        )
    }

    fn search_fts(&self, query: &str, limit: usize) -> IcmResult<Vec<Memory>> {
        self.call_de("memory.search_fts", json!({ "query": query, "limit": limit }))
    }

    fn search_by_embedding(
        &self,
        embedding: &[f32],
        limit: usize,
    ) -> IcmResult<Vec<(Memory, f32)>> {
        self.call_de(
            "memory.search_by_embedding",
            json!({ "embedding": embedding, "limit": limit }),
        )
    }

    fn search_hybrid(
        &self,
        query: &str,
        _embedding: &[f32],
        limit: usize,
    ) -> IcmResult<Vec<(Memory, f32)>> {
        // Remote mode: the server embeds the query text. The caller's
        // `embedding` argument is intentionally ignored — a thin client
        // has no model (the "zero local resources" invariant).
        self.call_de("memory.search_hybrid", json!({ "query": query, "limit": limit }))
    }

    fn update_access(&self, id: &str) -> IcmResult<()> {
        self.call("memory.update_access", json!({ "id": id }))?;
        Ok(())
    }

    fn batch_update_access(&self, ids: &[&str]) -> IcmResult<usize> {
        self.call_de("memory.batch_update_access", json!({ "ids": ids }))
    }

    fn apply_decay(&self, decay_factor: f32) -> IcmResult<usize> {
        self.call_de("memory.apply_decay", json!({ "decay_factor": decay_factor }))
    }

    fn prune(&self, weight_threshold: f32) -> IcmResult<usize> {
        self.call_de("memory.prune", json!({ "weight_threshold": weight_threshold }))
    }

    fn list_all(&self) -> IcmResult<Vec<Memory>> {
        self.call_de("memory.list_all", json!({}))
    }

    fn get_by_topic(&self, topic: &str) -> IcmResult<Vec<Memory>> {
        self.call_de("memory.get_by_topic", json!({ "topic": topic }))
    }

    fn list_topics(&self) -> IcmResult<Vec<(String, usize)>> {
        self.call_de("memory.list_topics", json!({}))
    }

    fn consolidate_topic(&self, topic: &str, consolidated: Memory) -> IcmResult<()> {
        self.call(
            "memory.consolidate_topic",
            json!({ "topic": topic, "consolidated": consolidated }),
        )?;
        Ok(())
    }

    fn count(&self) -> IcmResult<usize> {
        self.call_de("memory.count", json!({}))
    }

    fn count_by_topic(&self, topic: &str) -> IcmResult<usize> {
        self.call_de("memory.count_by_topic", json!({ "topic": topic }))
    }

    fn stats(&self) -> IcmResult<StoreStats> {
        self.call_de("memory.stats", json!({}))
    }

    fn topic_health(&self, topic: &str) -> IcmResult<TopicHealth> {
        self.call_de("memory.topic_health", json!({ "topic": topic }))
    }
}

impl FactsStore for RemoteHttpStore {
    fn set_fact(&self, entity: &str, key: &str, value: &str, source: &str) -> IcmResult<String> {
        self.call_de(
            "facts.set_fact",
            json!({ "entity": entity, "key": key, "value": value, "source": source }),
        )
    }

    fn get_fact(&self, entity: &str, key: &str) -> IcmResult<Option<Fact>> {
        self.call_de("facts.get_fact", json!({ "entity": entity, "key": key }))
    }

    fn list_facts(&self, entity: &str, key_prefix: Option<&str>) -> IcmResult<Vec<Fact>> {
        self.call_de(
            "facts.list_facts",
            json!({ "entity": entity, "key_prefix": key_prefix }),
        )
    }

    fn history(&self, entity: &str, key: &str) -> IcmResult<Vec<Fact>> {
        self.call_de("facts.history", json!({ "entity": entity, "key": key }))
    }

    fn forget_fact(&self, entity: &str, key: &str) -> IcmResult<usize> {
        self.call_de("facts.forget_fact", json!({ "entity": entity, "key": key }))
    }

    fn facts_stats(&self) -> IcmResult<FactsStats> {
        self.call_de("facts.facts_stats", json!({}))
    }
}

impl FeedbackStore for RemoteHttpStore {
    fn store_feedback(&self, feedback: Feedback) -> IcmResult<String> {
        self.call_de("feedback.store_feedback", json!({ "feedback": feedback }))
    }

    fn search_feedback(
        &self,
        query: &str,
        topic: Option<&str>,
        limit: usize,
    ) -> IcmResult<Vec<Feedback>> {
        self.call_de(
            "feedback.search_feedback",
            json!({ "query": query, "topic": topic, "limit": limit }),
        )
    }

    fn list_feedback(&self, topic: Option<&str>, limit: usize) -> IcmResult<Vec<Feedback>> {
        self.call_de("feedback.list_feedback", json!({ "topic": topic, "limit": limit }))
    }

    fn increment_applied(&self, id: &str) -> IcmResult<()> {
        self.call("feedback.increment_applied", json!({ "id": id }))?;
        Ok(())
    }

    fn delete_feedback(&self, id: &str) -> IcmResult<()> {
        self.call("feedback.delete_feedback", json!({ "id": id }))?;
        Ok(())
    }

    fn feedback_stats(&self) -> IcmResult<FeedbackStats> {
        self.call_de("feedback.feedback_stats", json!({}))
    }
}

impl MemoirStore for RemoteHttpStore {
    fn create_memoir(&self, memoir: Memoir) -> IcmResult<String> {
        self.call_de("memoir.create_memoir", json!({ "memoir": memoir }))
    }

    fn get_memoir(&self, id: &str) -> IcmResult<Option<Memoir>> {
        self.call_de("memoir.get_memoir", json!({ "id": id }))
    }

    fn get_memoir_by_name(&self, name: &str) -> IcmResult<Option<Memoir>> {
        self.call_de("memoir.get_memoir_by_name", json!({ "name": name }))
    }

    fn update_memoir(&self, memoir: &Memoir) -> IcmResult<()> {
        self.call("memoir.update_memoir", json!({ "memoir": memoir }))?;
        Ok(())
    }

    fn delete_memoir(&self, id: &str) -> IcmResult<()> {
        self.call("memoir.delete_memoir", json!({ "id": id }))?;
        Ok(())
    }

    fn list_memoirs(&self) -> IcmResult<Vec<Memoir>> {
        self.call_de("memoir.list_memoirs", json!({}))
    }

    fn add_concept(&self, concept: Concept) -> IcmResult<String> {
        self.call_de("memoir.add_concept", json!({ "concept": concept }))
    }

    fn get_concept(&self, id: &str) -> IcmResult<Option<Concept>> {
        self.call_de("memoir.get_concept", json!({ "id": id }))
    }

    fn get_concept_by_name(&self, memoir_id: &str, name: &str) -> IcmResult<Option<Concept>> {
        self.call_de(
            "memoir.get_concept_by_name",
            json!({ "memoir_id": memoir_id, "name": name }),
        )
    }

    fn update_concept(&self, concept: &Concept) -> IcmResult<()> {
        self.call("memoir.update_concept", json!({ "concept": concept }))?;
        Ok(())
    }

    fn delete_concept(&self, id: &str) -> IcmResult<()> {
        self.call("memoir.delete_concept", json!({ "id": id }))?;
        Ok(())
    }

    fn list_concepts(&self, memoir_id: &str) -> IcmResult<Vec<Concept>> {
        self.call_de("memoir.list_concepts", json!({ "memoir_id": memoir_id }))
    }

    fn search_concepts_fts(
        &self,
        memoir_id: &str,
        query: &str,
        limit: usize,
    ) -> IcmResult<Vec<Concept>> {
        self.call_de(
            "memoir.search_concepts_fts",
            json!({ "memoir_id": memoir_id, "query": query, "limit": limit }),
        )
    }

    fn search_concepts_by_label(
        &self,
        memoir_id: &str,
        label: &Label,
        limit: usize,
    ) -> IcmResult<Vec<Concept>> {
        self.call_de(
            "memoir.search_concepts_by_label",
            json!({ "memoir_id": memoir_id, "label": label, "limit": limit }),
        )
    }

    fn search_all_concepts_fts(&self, query: &str, limit: usize) -> IcmResult<Vec<Concept>> {
        self.call_de(
            "memoir.search_all_concepts_fts",
            json!({ "query": query, "limit": limit }),
        )
    }

    fn refine_concept(
        &self,
        id: &str,
        new_definition: &str,
        new_source_ids: &[String],
    ) -> IcmResult<()> {
        self.call(
            "memoir.refine_concept",
            json!({ "id": id, "new_definition": new_definition, "new_source_ids": new_source_ids }),
        )?;
        Ok(())
    }

    fn add_link(&self, link: ConceptLink) -> IcmResult<String> {
        self.call_de("memoir.add_link", json!({ "link": link }))
    }

    fn get_links_from(&self, concept_id: &str) -> IcmResult<Vec<ConceptLink>> {
        self.call_de("memoir.get_links_from", json!({ "concept_id": concept_id }))
    }

    fn get_links_to(&self, concept_id: &str) -> IcmResult<Vec<ConceptLink>> {
        self.call_de("memoir.get_links_to", json!({ "concept_id": concept_id }))
    }

    fn delete_link(&self, id: &str) -> IcmResult<()> {
        self.call("memoir.delete_link", json!({ "id": id }))?;
        Ok(())
    }

    fn get_neighbors(
        &self,
        concept_id: &str,
        relation: Option<Relation>,
    ) -> IcmResult<Vec<Concept>> {
        self.call_de(
            "memoir.get_neighbors",
            json!({ "concept_id": concept_id, "relation": relation }),
        )
    }

    fn get_neighborhood(
        &self,
        concept_id: &str,
        depth: usize,
    ) -> IcmResult<(Vec<Concept>, Vec<ConceptLink>)> {
        self.call_de(
            "memoir.get_neighborhood",
            json!({ "concept_id": concept_id, "depth": depth }),
        )
    }

    fn get_links_for_memoir(&self, memoir_id: &str) -> IcmResult<Vec<ConceptLink>> {
        self.call_de(
            "memoir.get_links_for_memoir",
            json!({ "memoir_id": memoir_id }),
        )
    }

    fn memoir_stats(&self, memoir_id: &str) -> IcmResult<MemoirStats> {
        self.call_de("memoir.memoir_stats", json!({ "memoir_id": memoir_id }))
    }

    fn batch_memoir_concept_counts(&self) -> IcmResult<HashMap<String, usize>> {
        self.call_de("memoir.batch_memoir_concept_counts", json!({}))
    }
}

impl TranscriptStore for RemoteHttpStore {
    fn create_session(
        &self,
        agent: &str,
        project: Option<&str>,
        metadata: Option<&str>,
    ) -> IcmResult<String> {
        self.call_de(
            "transcript.create_session",
            json!({ "agent": agent, "project": project, "metadata": metadata }),
        )
    }

    fn ensure_session(
        &self,
        id: &str,
        agent: &str,
        project: Option<&str>,
        metadata: Option<&str>,
    ) -> IcmResult<String> {
        self.call_de(
            "transcript.ensure_session",
            json!({ "id": id, "agent": agent, "project": project, "metadata": metadata }),
        )
    }

    fn get_session(&self, id: &str) -> IcmResult<Option<Session>> {
        self.call_de("transcript.get_session", json!({ "id": id }))
    }

    fn list_sessions(&self, project: Option<&str>, limit: usize) -> IcmResult<Vec<Session>> {
        self.call_de(
            "transcript.list_sessions",
            json!({ "project": project, "limit": limit }),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn record_message(
        &self,
        session_id: &str,
        role: Role,
        content: &str,
        tool_name: Option<&str>,
        tokens: Option<i64>,
        metadata: Option<&str>,
    ) -> IcmResult<String> {
        self.call_de(
            "transcript.record_message",
            json!({
                "session_id": session_id,
                "role": role,
                "content": content,
                "tool_name": tool_name,
                "tokens": tokens,
                "metadata": metadata,
            }),
        )
    }

    fn list_session_messages(
        &self,
        session_id: &str,
        limit: usize,
        offset: usize,
    ) -> IcmResult<Vec<Message>> {
        self.call_de(
            "transcript.list_session_messages",
            json!({ "session_id": session_id, "limit": limit, "offset": offset }),
        )
    }

    fn search_transcripts(
        &self,
        query: &str,
        session_id: Option<&str>,
        project: Option<&str>,
        limit: usize,
    ) -> IcmResult<Vec<TranscriptHit>> {
        self.call_de(
            "transcript.search_transcripts",
            json!({ "query": query, "session_id": session_id, "project": project, "limit": limit }),
        )
    }

    fn forget_session(&self, id: &str) -> IcmResult<()> {
        self.call("transcript.forget_session", json!({ "id": id }))?;
        Ok(())
    }

    fn transcript_stats(&self) -> IcmResult<TranscriptStats> {
        self.call_de("transcript.transcript_stats", json!({}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use icm_core::Importance;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::thread;

    /// Minimal stateful `/rpc` mock: tracks a memory count so
    /// `memory.store` → `memory.count` behaves like a real round-trip,
    /// and covers the null-result and error-envelope parsing paths.
    /// Zero test dependencies (raw TCP), `Connection: close` per request.
    fn spawn_mock_server() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                // Read the full request (headers + Content-Length body).
                let mut acc: Vec<u8> = Vec::new();
                let mut tmp = [0u8; 1024];
                loop {
                    let header_end = acc.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4);
                    if let Some(h) = header_end {
                        let headers = String::from_utf8_lossy(&acc[..h]).to_lowercase();
                        let want = headers
                            .split("content-length:")
                            .nth(1)
                            .and_then(|s| s.split("\r\n").next())
                            .and_then(|s| s.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if acc.len() >= h + want {
                            break;
                        }
                    }
                    match stream.read(&mut tmp) {
                        Ok(0) => break,
                        Ok(n) => acc.extend_from_slice(&tmp[..n]),
                        Err(_) => break,
                    }
                }
                let body_start = acc
                    .windows(4)
                    .position(|w| w == b"\r\n\r\n")
                    .map(|p| p + 4)
                    .unwrap_or(acc.len());
                let body: Value =
                    serde_json::from_slice(&acc[body_start..]).unwrap_or(Value::Null);
                let method = body.get("method").and_then(Value::as_str).unwrap_or("");
                let result = match method {
                    "memory.store" => {
                        count.fetch_add(1, Ordering::SeqCst);
                        json!({ "result": "id-123" })
                    }
                    "memory.count" => json!({ "result": count.load(Ordering::SeqCst) }),
                    "memory.get" => json!({ "result": null }),
                    "memory.boom" => json!({ "error": "boom" }),
                    // Other-trait canned answers (T14): representative
                    // simple-typed / empty-collection / null paths.
                    "facts.get_fact" => json!({ "result": null }),
                    "facts.forget_fact" => json!({ "result": 2 }),
                    "feedback.list_feedback" => json!({ "result": [] }),
                    "memoir.list_memoirs" => json!({ "result": [] }),
                    "transcript.list_sessions" => json!({ "result": [] }),
                    _ => json!({ "result": null }),
                };
                let payload = result.to_string();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    payload.len(),
                    payload
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
            }
        });
        format!("http://{addr}")
    }

    #[test]
    fn remote_store_memory_roundtrip() {
        let url = spawn_mock_server();
        let r = RemoteHttpStore::new(&url, None);

        // store → count reflects the write.
        let id = r
            .store(Memory::new(
                "t".to_string(),
                "hello".to_string(),
                Importance::Medium,
            ))
            .unwrap();
        assert_eq!(id, "id-123");
        assert_eq!(r.count().unwrap(), 1);

        // get miss → Ok(None), NOT an error.
        assert!(r.get("missing").unwrap().is_none());
    }

    #[test]
    fn remote_store_other_traits() {
        let url = spawn_mock_server();
        let r = RemoteHttpStore::new(&url, None);

        // FactsStore: null → None; number → usize.
        assert!(r.get_fact("e", "k").unwrap().is_none());
        assert_eq!(r.forget_fact("e", "k").unwrap(), 2);
        // FeedbackStore / MemoirStore / TranscriptStore: empty collections.
        assert!(r.list_feedback(None, 10).unwrap().is_empty());
        assert!(r.list_memoirs().unwrap().is_empty());
        assert!(r.list_sessions(None, 10).unwrap().is_empty());
    }

    #[test]
    fn error_envelope_maps_to_remote_error() {
        let url = spawn_mock_server();
        let r = RemoteHttpStore::new(&url, None);
        // Drive the error path via a method the mock answers with `error`.
        let err = r.call("memory.boom", json!({})).unwrap_err();
        assert!(matches!(err, IcmError::Remote(_)), "got {err:?}");
    }

    #[test]
    fn unreachable_server_is_remote_error() {
        // Nothing listening on this port → transport error, not a panic.
        let r = RemoteHttpStore::new("http://127.0.0.1:1", None);
        assert!(matches!(r.count().unwrap_err(), IcmError::Remote(_)));
    }
}
