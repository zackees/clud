//! HTTP-based [`RemoteEmbedder`] for the four supported providers:
//! Anthropic (Voyage), OpenAI, Gemini, Ollama.
//!
//! Pure blocking `ureq` — no tokio. Each provider's request/response
//! shape is encoded in [`RemoteEmbedder::embed`]. Provider URL and the
//! API key come from env vars; see the module README for the full list.

use serde::Deserialize;
use serde_json::json;

use crate::memory::embedder::EmbedderTrait;
use crate::memory::error::MemoryError;

use super::{ENV_EMBEDDER_API_KEY, ENV_EMBEDDER_MODEL, ENV_EMBEDDER_PROVIDER, ENV_EMBEDDER_URL};

/// Supported remote embedding providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteProvider {
    Anthropic,
    OpenAi,
    Gemini,
    Ollama,
}

impl RemoteProvider {
    pub fn parse(s: &str) -> Result<Self, MemoryError> {
        match s.to_ascii_lowercase().as_str() {
            "anthropic" | "voyage" => Ok(Self::Anthropic),
            "openai" => Ok(Self::OpenAi),
            "gemini" | "google" => Ok(Self::Gemini),
            "ollama" => Ok(Self::Ollama),
            other => Err(MemoryError::EmbedderRemoteFailure {
                provider: other.to_string(),
                message: format!(
                    "unknown provider `{other}` — expected one of \
                     anthropic|openai|gemini|ollama"
                ),
            }),
        }
    }

    pub fn default_url(self) -> &'static str {
        match self {
            Self::Anthropic => "https://api.voyageai.com/v1/embeddings",
            Self::OpenAi => "https://api.openai.com/v1/embeddings",
            Self::Gemini => "https://generativelanguage.googleapis.com/v1beta/models/text-embedding-004:embedContent",
            Self::Ollama => "http://localhost:11434/api/embeddings",
        }
    }

    pub fn default_model(self) -> &'static str {
        match self {
            Self::Anthropic => "voyage-3",
            Self::OpenAi => "text-embedding-3-small",
            Self::Gemini => "text-embedding-004",
            Self::Ollama => "nomic-embed-text",
        }
    }

    pub fn default_dim(self) -> usize {
        match self {
            Self::Anthropic => 1024,
            Self::OpenAi => 1536,
            Self::Gemini => 768,
            Self::Ollama => 768,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAi => "openai",
            Self::Gemini => "gemini",
            Self::Ollama => "ollama",
        }
    }
}

/// HTTP client for a remote embedding provider. One instance per
/// process; constructed by [`RemoteEmbedder::from_env`].
pub struct RemoteEmbedder {
    provider: RemoteProvider,
    url: String,
    api_key: Option<String>,
    model: String,
    dim: usize,
    name: String,
    // Issue #257 (test seam): if set, [`embed`] returns this vector
    // verbatim instead of issuing a real HTTP request. Production code
    // never sets this — it's wired by the in-module tests that simulate
    // a provider response shape without standing up a mock server.
    #[cfg(test)]
    mock_response: Option<Vec<f32>>,
}

impl RemoteEmbedder {
    pub fn new(
        provider: RemoteProvider,
        url: String,
        api_key: Option<String>,
        model: String,
        dim: usize,
    ) -> Self {
        let name = format!("{}/{}", provider.as_str(), model);
        Self {
            provider,
            url,
            api_key,
            model,
            dim,
            name,
            #[cfg(test)]
            mock_response: None,
        }
    }

    pub fn from_env() -> Result<Self, MemoryError> {
        let provider_str = std::env::var(ENV_EMBEDDER_PROVIDER).map_err(|_| {
            MemoryError::EmbedderRemoteFailure {
                provider: "unset".to_string(),
                message: format!(
                    "{ENV_EMBEDDER_PROVIDER} not set; \
                     expected one of anthropic|openai|gemini|ollama"
                ),
            }
        })?;
        let provider = RemoteProvider::parse(&provider_str)?;
        let url =
            std::env::var(ENV_EMBEDDER_URL).unwrap_or_else(|_| provider.default_url().to_string());
        let api_key = std::env::var(ENV_EMBEDDER_API_KEY).ok();
        let model = std::env::var(ENV_EMBEDDER_MODEL)
            .unwrap_or_else(|_| provider.default_model().to_string());
        let dim = provider.default_dim();
        Ok(Self::new(provider, url, api_key, model, dim))
    }

    /// Issue an HTTP request to the configured provider URL and parse
    /// the response into a single vector. Split out so the test code
    /// can short-circuit before hitting the network.
    fn embed_http(&self, text: &str) -> Result<Vec<f32>, MemoryError> {
        let (body, auth_header): (serde_json::Value, Option<(&str, String)>) = match self.provider {
            RemoteProvider::Anthropic => {
                let body = json!({ "input": [text], "model": self.model });
                let h = self
                    .api_key
                    .as_ref()
                    .map(|k| ("Authorization", format!("Bearer {k}")));
                (body, h)
            }
            RemoteProvider::OpenAi => {
                let body = json!({ "input": text, "model": self.model });
                let h = self
                    .api_key
                    .as_ref()
                    .map(|k| ("Authorization", format!("Bearer {k}")));
                (body, h)
            }
            RemoteProvider::Gemini => {
                let body = json!({
                    "model": format!("models/{}", self.model),
                    "content": { "parts": [{ "text": text }] },
                });
                let h = self.api_key.as_ref().map(|k| ("x-goog-api-key", k.clone()));
                (body, h)
            }
            RemoteProvider::Ollama => {
                let body = json!({ "model": self.model, "prompt": text });
                (body, None)
            }
        };

        let mut req = ureq::post(&self.url).set("Content-Type", "application/json");
        if let Some((name, value)) = auth_header {
            req = req.set(name, &value);
        }

        let body_str =
            serde_json::to_string(&body).map_err(|e| MemoryError::EmbedderRemoteFailure {
                provider: self.provider.as_str().to_string(),
                message: format!("serialize body: {e}"),
            })?;
        let resp = req
            .send_string(&body_str)
            .map_err(|e| MemoryError::EmbedderRemoteFailure {
                provider: self.provider.as_str().to_string(),
                message: format!("http error to {}: {e}", self.url),
            })?;

        let status = resp.status();
        let text_body = resp
            .into_string()
            .map_err(|e| MemoryError::EmbedderRemoteFailure {
                provider: self.provider.as_str().to_string(),
                message: format!("read body: {e}"),
            })?;
        if !(200..300).contains(&status) {
            let excerpt: String = text_body.chars().take(256).collect();
            return Err(MemoryError::EmbedderRemoteFailure {
                provider: self.provider.as_str().to_string(),
                message: format!("HTTP {status}: {excerpt}"),
            });
        }

        parse_response(self.provider, &text_body)
    }
}

#[cfg(test)]
impl RemoteEmbedder {
    pub(crate) fn with_mock_response(mut self, response: Vec<f32>) -> Self {
        self.dim = response.len();
        self.mock_response = Some(response);
        self
    }
}

impl EmbedderTrait for RemoteEmbedder {
    fn embed(&self, text: &str) -> Result<Vec<f32>, MemoryError> {
        #[cfg(test)]
        if let Some(mock) = &self.mock_response {
            // Branch on text emptiness only as a sanity check; mocks just
            // echo their preset vector.
            if text.is_empty() {
                return Err(MemoryError::EmbedderRemoteFailure {
                    provider: self.provider.as_str().to_string(),
                    message: "empty text".to_string(),
                });
            }
            return Ok(mock.clone());
        }
        self.embed_http(text)
    }

    fn dim(&self) -> usize {
        self.dim
    }

    fn name(&self) -> &str {
        &self.name
    }
}

#[derive(Deserialize)]
struct OpenAiEmbedding {
    embedding: Vec<f32>,
}

#[derive(Deserialize)]
struct OpenAiResponse {
    data: Vec<OpenAiEmbedding>,
}

#[derive(Deserialize)]
struct AnthropicEmbedding {
    embedding: Vec<f32>,
}

#[derive(Deserialize)]
struct AnthropicResponse {
    data: Vec<AnthropicEmbedding>,
}

#[derive(Deserialize)]
struct GeminiValues {
    values: Vec<f32>,
}

#[derive(Deserialize)]
struct GeminiResponse {
    embedding: GeminiValues,
}

#[derive(Deserialize)]
struct OllamaResponse {
    embedding: Vec<f32>,
}

fn parse_response(provider: RemoteProvider, body: &str) -> Result<Vec<f32>, MemoryError> {
    let fail = |what: &str, e: serde_json::Error| MemoryError::EmbedderRemoteFailure {
        provider: provider.as_str().to_string(),
        message: format!("parse {what}: {e}"),
    };
    let v = match provider {
        RemoteProvider::OpenAi => {
            let r: OpenAiResponse = serde_json::from_str(body).map_err(|e| fail("openai", e))?;
            r.data.into_iter().next().map(|e| e.embedding)
        }
        RemoteProvider::Anthropic => {
            let r: AnthropicResponse =
                serde_json::from_str(body).map_err(|e| fail("anthropic", e))?;
            r.data.into_iter().next().map(|e| e.embedding)
        }
        RemoteProvider::Gemini => {
            let r: GeminiResponse = serde_json::from_str(body).map_err(|e| fail("gemini", e))?;
            Some(r.embedding.values)
        }
        RemoteProvider::Ollama => {
            let r: OllamaResponse = serde_json::from_str(body).map_err(|e| fail("ollama", e))?;
            Some(r.embedding)
        }
    };
    v.ok_or_else(|| MemoryError::EmbedderRemoteFailure {
        provider: provider.as_str().to_string(),
        message: "no embedding in response".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_provider_accepts_aliases() {
        assert_eq!(
            RemoteProvider::parse("Anthropic").unwrap(),
            RemoteProvider::Anthropic
        );
        assert_eq!(
            RemoteProvider::parse("voyage").unwrap(),
            RemoteProvider::Anthropic
        );
        assert_eq!(
            RemoteProvider::parse("openai").unwrap(),
            RemoteProvider::OpenAi
        );
        assert_eq!(
            RemoteProvider::parse("Google").unwrap(),
            RemoteProvider::Gemini
        );
        assert_eq!(
            RemoteProvider::parse("ollama").unwrap(),
            RemoteProvider::Ollama
        );
    }

    #[test]
    fn parse_provider_rejects_unknown() {
        let err = RemoteProvider::parse("cohere").unwrap_err();
        assert!(matches!(err, MemoryError::EmbedderRemoteFailure { .. }));
    }

    #[test]
    fn default_dims_match_provider_advertised_values() {
        assert_eq!(RemoteProvider::Anthropic.default_dim(), 1024);
        assert_eq!(RemoteProvider::OpenAi.default_dim(), 1536);
        assert_eq!(RemoteProvider::Gemini.default_dim(), 768);
        assert_eq!(RemoteProvider::Ollama.default_dim(), 768);
    }

    #[test]
    fn remote_embedder_anthropic_mock_client_returns_dim() {
        let preset = vec![0.1_f32; 1024];
        let e = RemoteEmbedder::new(
            RemoteProvider::Anthropic,
            "http://unused".to_string(),
            Some("key".to_string()),
            "voyage-3".to_string(),
            1024,
        )
        .with_mock_response(preset.clone());
        let out = e.embed("hello").unwrap();
        assert_eq!(out.len(), 1024);
        assert_eq!(out, preset);
        assert_eq!(e.dim(), 1024);
        assert!(e.name().contains("anthropic"));
    }

    #[test]
    fn parse_response_openai_returns_first_embedding() {
        let body = r#"{"data": [{"embedding": [0.1, 0.2, 0.3]}]}"#;
        let v = parse_response(RemoteProvider::OpenAi, body).unwrap();
        assert_eq!(v, vec![0.1_f32, 0.2, 0.3]);
    }

    #[test]
    fn parse_response_gemini_unwraps_values() {
        let body = r#"{"embedding": {"values": [1.0, 2.0]}}"#;
        let v = parse_response(RemoteProvider::Gemini, body).unwrap();
        assert_eq!(v, vec![1.0_f32, 2.0]);
    }

    #[test]
    fn parse_response_ollama_unwraps_embedding() {
        let body = r#"{"embedding": [0.5, 0.6]}"#;
        let v = parse_response(RemoteProvider::Ollama, body).unwrap();
        assert_eq!(v, vec![0.5_f32, 0.6]);
    }
}
