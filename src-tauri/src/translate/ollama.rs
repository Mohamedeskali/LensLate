//! A local Ollama server (`/api/chat`). No key; the base URL is a setting.

use std::time::Instant;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::ai::{clean_output, system_prompt, user_message};
use super::{
    elapsed_ms, parse_json, send, EngineId, Lang, TranslateError, TranslateResult, Translation,
    Translator,
};

const ID: EngineId = EngineId::Ollama;
pub const DEFAULT_URL: &str = "http://localhost:11434";
pub const DEFAULT_MODEL: &str = "qwen2.5";

pub struct Ollama {
    client: reqwest::Client,
    base_url: String,
    model: String,
}

impl Ollama {
    pub fn new(client: reqwest::Client, base_url: &str, model: &str) -> Self {
        let base_url = base_url.trim().trim_end_matches('/');
        let model = model.trim();
        Self {
            client,
            base_url: if base_url.is_empty() {
                DEFAULT_URL
            } else {
                base_url
            }
            .to_string(),
            model: if model.is_empty() {
                DEFAULT_MODEL
            } else {
                model
            }
            .to_string(),
        }
    }
}

#[async_trait]
impl Translator for Ollama {
    fn id(&self) -> EngineId {
        ID
    }

    async fn translate(
        &self,
        text: &str,
        from: Option<&Lang>,
        to: &Lang,
    ) -> TranslateResult<Translation> {
        let start = Instant::now();
        let request = self
            .client
            .post(format!("{}/api/chat", self.base_url))
            .json(&json!({
                "model": self.model,
                "stream": false,
                "options": { "temperature": 0 },
                "messages": [
                    { "role": "system", "content": system_prompt(from, to) },
                    { "role": "user", "content": user_message(text) },
                ],
            }));
        let json = parse_json(ID, &send(ID, request).await?)?;
        let raw = json
            .pointer("/message/content")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| TranslateError::Parse(ID, "no message.content".into()))?;
        Ok(Translation {
            text: clean_output(raw),
            detected_from: None,
            engine: ID,
            ms: elapsed_ms(start),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn request_shape_and_parse() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .and(body_partial_json(json!({ "model": "llama3.2", "stream": false })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "model": "llama3.2", "message": { "role": "assistant", "content": "Hallo\nWelt" }, "done": true
            })))
            .expect(1)
            .mount(&server)
            .await;
        let ollama = Ollama::new(
            crate::translate::http_client(),
            &format!(" {}/ ", server.uri()),
            "llama3.2",
        );
        let t = ollama
            .translate("Hello\nworld", None, &Lang::new("de"))
            .await
            .unwrap();
        assert_eq!(t.text, "Hallo\nWelt");
        assert_eq!(t.engine, ID);
        // Authorization is never sent to a local server.
        let requests = server.received_requests().await.unwrap();
        assert!(!requests[0].headers.contains_key("authorization"));
    }

    #[tokio::test]
    async fn missing_model_is_an_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(404).set_body_string(r#"{"error":"model 'x' not found"}"#),
            )
            .mount(&server)
            .await;
        let ollama = Ollama::new(crate::translate::http_client(), &server.uri(), "x");
        match ollama.translate("a", None, &Lang::new("de")).await {
            Err(TranslateError::Http {
                status, message, ..
            }) => {
                assert_eq!(status, 404);
                assert!(message.contains("not found"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn defaults() {
        let o = Ollama::new(crate::translate::http_client(), "", "");
        assert_eq!(o.base_url, DEFAULT_URL);
        assert_eq!(o.model, DEFAULT_MODEL);
    }
}
