//! OpenAI-compatible cloud embedder (F-001, feature `cloud-embeddings`).
//!
//! Implements [`Embedder`] by calling any endpoint that speaks the OpenAI
//! `POST /v1/embeddings` contract (OpenAI, Azure OpenAI, vLLM, LocalAI,
//! Ollama's OpenAI shim, …). Uses the blocking `ureq` client so it fits
//! the synchronous [`Embedder`] trait with no async runtime — the same
//! choice the OpenSearch backend makes.
//!
//! In the three-tier deployment this runs on the CENTRAL node, so a
//! single API key is configured once and thin clients never embed
//! locally (that is the "save local resources" invariant).

use serde::Deserialize;
use serde_json::json;

use crate::embedder::Embedder;
use crate::error::{IcmError, IcmResult};

/// An embedder backed by an OpenAI-compatible `/embeddings` endpoint.
pub struct OpenAiEmbedder {
    /// Base URL up to and including the API version, e.g.
    /// `https://api.openai.com/v1`. The request path `/embeddings` is
    /// appended, so trailing slashes are trimmed.
    base_url: String,
    api_key: String,
    model: String,
    dims: usize,
}

#[derive(Deserialize)]
struct EmbeddingsResponse {
    data: Vec<EmbeddingDatum>,
}

#[derive(Deserialize)]
struct EmbeddingDatum {
    embedding: Vec<f32>,
}

impl OpenAiEmbedder {
    /// Build an embedder. `dims` is the model's output dimensionality
    /// (e.g. 1536 for `text-embedding-3-small`); it is authoritative for
    /// [`Embedder::dimensions`] and is used by the dimension guard so a
    /// mismatch with the store is caught before any network call.
    pub fn new(base_url: &str, api_key: &str, model: &str, dims: usize) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            model: model.to_string(),
            dims,
        }
    }

    fn endpoint(&self) -> String {
        format!("{}/embeddings", self.base_url)
    }

    /// POST a batch of inputs and return one vector per input, in order.
    fn request(&self, inputs: &[&str]) -> IcmResult<Vec<Vec<f32>>> {
        let body = json!({ "model": self.model, "input": inputs });
        // NB: the API key is placed only in the Authorization header and
        // is never logged, even on error (errors carry status + endpoint,
        // not credentials).
        let resp = ureq::post(&self.endpoint())
            .set("Authorization", &format!("Bearer {}", self.api_key))
            .set("Content-Type", "application/json")
            .send_json(body)
            .map_err(|e| IcmError::Embedding(format!("embeddings request failed: {e}")))?;

        let parsed: EmbeddingsResponse = resp
            .into_json()
            .map_err(|e| IcmError::Embedding(format!("invalid embeddings response: {e}")))?;

        if parsed.data.len() != inputs.len() {
            return Err(IcmError::Embedding(format!(
                "expected {} embeddings, got {}",
                inputs.len(),
                parsed.data.len()
            )));
        }
        let vecs: Vec<Vec<f32>> = parsed.data.into_iter().map(|d| d.embedding).collect();
        // Validate dimensionality up front so a misconfigured `dims`
        // surfaces here rather than as a silent search-quality regression.
        for v in &vecs {
            if v.len() != self.dims {
                return Err(IcmError::Embedding(format!(
                    "model returned {}-dim vector, config declares {}",
                    v.len(),
                    self.dims
                )));
            }
        }
        Ok(vecs)
    }
}

impl Embedder for OpenAiEmbedder {
    fn embed(&self, text: &str) -> IcmResult<Vec<f32>> {
        let mut out = self.request(&[text])?;
        out.pop()
            .ok_or_else(|| IcmError::Embedding("empty embedding result".into()))
    }

    fn embed_batch(&self, texts: &[&str]) -> IcmResult<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        self.request(texts)
    }

    fn dimensions(&self) -> usize {
        self.dims
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    /// Minimal one-shot HTTP mock: accepts one connection, returns a
    /// fixed OpenAI-shaped JSON body. Zero test dependencies.
    struct MockServer {
        base: String,
    }

    fn mock_embeddings_server(vectors: Vec<Vec<f32>>) -> MockServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                // Read the FULL request (headers + Content-Length body)
                // before responding. Responding early and closing races
                // with the client still writing its body, which can RST
                // the socket and make the client miss the response.
                let mut acc: Vec<u8> = Vec::new();
                let mut tmp = [0u8; 1024];
                loop {
                    let header_end = acc.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4);
                    if let Some(hend) = header_end {
                        let headers = String::from_utf8_lossy(&acc[..hend]).to_lowercase();
                        let want = headers
                            .split("content-length:")
                            .nth(1)
                            .and_then(|s| s.split("\r\n").next())
                            .and_then(|s| s.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if acc.len() >= hend + want {
                            break;
                        }
                    }
                    match stream.read(&mut tmp) {
                        Ok(0) => break,
                        Ok(n) => acc.extend_from_slice(&tmp[..n]),
                        Err(_) => break,
                    }
                }
                let data: Vec<_> = vectors
                    .iter()
                    .map(|v| serde_json::json!({ "embedding": v }))
                    .collect();
                let body = serde_json::json!({ "data": data }).to_string();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
            }
        });
        MockServer {
            base: format!("http://{addr}"),
        }
    }

    impl MockServer {
        fn base_url(&self) -> &str {
            &self.base
        }
    }

    #[test]
    fn embeds_via_openai_shape() {
        let server = mock_embeddings_server(vec![vec![0.1_f32; 4]]);
        let e = OpenAiEmbedder::new(server.base_url(), "test-key", "text-embedding-3-small", 4);
        let v = e.embed("hello").expect("embed");
        assert_eq!(v.len(), 4);
        assert_eq!(e.dimensions(), 4);
    }

    #[test]
    fn empty_batch_is_no_network_call() {
        // No server needed: an empty batch must short-circuit.
        let e = OpenAiEmbedder::new("http://127.0.0.1:1", "k", "m", 4);
        assert!(e.embed_batch(&[]).expect("empty").is_empty());
    }
}
