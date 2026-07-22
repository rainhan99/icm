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

use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use icm_core::{
    IcmError, IcmResult, Memory, MemoryStore, RpcRequest, RpcResponse, StoreStats, TopicHealth,
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
