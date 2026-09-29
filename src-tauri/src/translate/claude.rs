//! Claude through the Anthropic Messages API.

use std::time::Instant;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::ai::{clean_output, system_prompt, user_message, MAX_TOKENS};
use super::{
    elapsed_ms, parse_json, send, EngineId, Lang, TranslateError, TranslateResult, Translation,
    Translator,
};

const ID: EngineId = EngineId::Claude;
pub const BASE_URL: &str = "https://api.anthropic.com";
pub const DEFAULT_MODEL: &str = "claude-haiku-4-5-20251001";
const API_VERSION: &str = "2023-06-01";

pub struct Claude {
    client: reqwest::Client,
    base_url: String,
    key: String,
    model: String,
}

impl Claude {
    pub fn new(client: reqwest::Client, key: String, model: &str) -> Self {
        Self::with_base_url(client, BASE_URL, key, model)
    }

    pub fn with_base_url(
        client: reqwest::Client,
        base_url: &str,
        key: String,
        model: &str,
    ) -> Self {
        let model = model.trim();
        Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            key,
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
impl Translator for Claude {
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
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.key)
            .header("anthropic-version", API_VERSION)
            .json(&json!({
                "model": self.model,
                "max_tokens": MAX_TOKENS,
                "temperature": 0,
                "system": system_prompt(from, to),
                "messages": [{ "role": "user", "content": user_message(text) }],
            }));
        let json = parse_json(ID, &send(ID, request).await?)?;
        let blocks = json
            .get("content")
            .and_then(Value::as_array)
            .ok_or_else(|| TranslateError::Parse(ID, "no content".into()))?;
        let raw: String = blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect();
        if raw.trim().is_empty() {
            return Err(TranslateError::Parse(ID, "empty answer".into()));
        }
        Ok(Translation {
            text: clean_output(&raw),
            detected_from: None,
            engine: ID,
            ms: elapsed_ms(start),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn request_shape_and_parse() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("x-api-key", "sk-ant"))
            .and(header("anthropic-version", "2023-06-01"))
            .and(body_partial_json(json!({
                "model": DEFAULT_MODEL,
                "messages": [{ "role": "user", "content": "<text>\nHello\nWorld\n</text>" }],
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "msg_1", "type": "message", "role": "assistant",
                "content": [{ "type": "text", "text": "مرحبا\nالعالم" }],
                "stop_reason": "end_turn"
            })))
            .expect(1)
            .mount(&server)
            .await;
        // An empty model setting falls back to the default.
        let claude = Claude::with_base_url(
            crate::translate::http_client(),
            &format!("{}/", server.uri()),
            "sk-ant".into(),
            " ",
        );
        let t = claude
            .translate("Hello\nWorld", None, &Lang::new("ar"))
            .await
            .unwrap();
        assert_eq!(t.text, "مرحبا\nالعالم");
        assert_eq!(t.engine, ID);
        assert_eq!(t.detected_from, None);

        let requests = server.received_requests().await.unwrap();
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert!(body["system"].as_str().unwrap().contains("into Arabic"));
    }

    #[tokio::test]
    async fn custom_model_and_errors() {
        let server = MockServer::start().await;
        Mock::given(body_partial_json(json!({ "model": "claude-sonnet-x" })))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "type": "error", "error": { "type": "authentication_error", "message": "invalid x-api-key" }
            })))
            .mount(&server)
            .await;
        let claude = Claude::with_base_url(
            crate::translate::http_client(),
            &server.uri(),
            "bad".into(),
            "claude-sonnet-x",
        );
        let err = claude
            .translate("Hi", None, &Lang::new("fr"))
            .await
            .unwrap_err();
        assert_eq!(
            err,
            TranslateError::Auth {
                engine: ID,
                status: 401
            }
        );
    }
}
