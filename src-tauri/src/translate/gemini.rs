//! Google Gemini through the Generative Language API (`generateContent`).

use std::time::Instant;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::ai::{clean_output, system_prompt, user_message, MAX_TOKENS};
use super::{
    elapsed_ms, parse_json, send, EngineId, Lang, TranslateError, TranslateResult, Translation,
    Translator,
};

const ID: EngineId = EngineId::Gemini;
pub const BASE_URL: &str = "https://generativelanguage.googleapis.com";
pub const DEFAULT_MODEL: &str = "gemini-2.5-flash";

pub struct Gemini {
    client: reqwest::Client,
    base_url: String,
    key: String,
    model: String,
}

impl Gemini {
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
impl Translator for Gemini {
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
        let url = format!(
            "{}/v1beta/models/{}:generateContent",
            self.base_url,
            urlencoding::encode(&self.model)
        );
        let request = self
            .client
            .post(url)
            .header("x-goog-api-key", &self.key)
            .json(&json!({
                "systemInstruction": { "parts": [{ "text": system_prompt(from, to) }] },
                "contents": [{ "role": "user", "parts": [{ "text": user_message(text) }] }],
                "generationConfig": { "temperature": 0, "maxOutputTokens": MAX_TOKENS },
            }));
        let json = parse_json(ID, &send(ID, request).await?)?;
        let parts = json
            .pointer("/candidates/0/content/parts")
            .and_then(Value::as_array)
            .ok_or_else(|| TranslateError::Parse(ID, "no candidates[0].content.parts".into()))?;
        let raw: String = parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
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
            .and(path("/v1beta/models/gemini-2.5-flash:generateContent"))
            .and(header("x-goog-api-key", "g-key"))
            .and(body_partial_json(json!({
                "contents": [{ "role": "user", "parts": [{ "text": "<text>\nHola\n</text>" }] }],
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "candidates": [{ "content": { "role": "model", "parts": [{ "text": "Hello" }, { "text": "\n" }] } }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let gemini = Gemini::with_base_url(
            crate::translate::http_client(),
            &server.uri(),
            "g-key".into(),
            "",
        );
        let t = gemini
            .translate("Hola", None, &Lang::new("en"))
            .await
            .unwrap();
        assert_eq!(t.text, "Hello");
        assert_eq!(t.engine, ID);
    }

    #[tokio::test]
    async fn blocked_answer_is_a_parse_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "promptFeedback": { "blockReason": "SAFETY" }
            })))
            .mount(&server)
            .await;
        let gemini = Gemini::with_base_url(
            crate::translate::http_client(),
            &server.uri(),
            "k".into(),
            "m",
        );
        assert!(matches!(
            gemini.translate("x", None, &Lang::new("en")).await,
            Err(TranslateError::Parse(ID, _))
        ));
    }
}
