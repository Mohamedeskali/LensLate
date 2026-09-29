//! OpenAI Chat Completions API.

use std::time::Instant;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::ai::{clean_output, system_prompt, user_message};
use super::{
    elapsed_ms, parse_json, send, EngineId, Lang, TranslateError, TranslateResult, Translation,
    Translator,
};

const ID: EngineId = EngineId::Openai;
pub const BASE_URL: &str = "https://api.openai.com";
pub const DEFAULT_MODEL: &str = "gpt-4o-mini";

pub struct OpenAi {
    client: reqwest::Client,
    base_url: String,
    key: String,
    model: String,
}

impl OpenAi {
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
impl Translator for OpenAi {
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
        // No temperature: reasoning models only accept the default.
        let request = self
            .client
            .post(format!("{}/v1/chat/completions", self.base_url))
            .bearer_auth(&self.key)
            .json(&json!({
                "model": self.model,
                "messages": [
                    { "role": "system", "content": system_prompt(from, to) },
                    { "role": "user", "content": user_message(text) },
                ],
            }));
        let json = parse_json(ID, &send(ID, request).await?)?;
        let raw = json
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| TranslateError::Parse(ID, "no choices[0].message.content".into()))?;
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
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn request_shape_and_parse() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("Authorization", "Bearer sk-oa"))
            .and(body_partial_json(json!({ "model": "gpt-test" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{ "index": 0, "message": { "role": "assistant", "content": "```\nBonjour\nle monde\n```" } }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let openai = OpenAi::with_base_url(
            crate::translate::http_client(),
            &server.uri(),
            "sk-oa".into(),
            "gpt-test",
        );
        let t = openai
            .translate("Hello\nworld", Some(&Lang::new("en")), &Lang::new("fr"))
            .await
            .unwrap();
        assert_eq!(t.text, "Bonjour\nle monde");
        let requests = server.received_requests().await.unwrap();
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["messages"][0]["role"], "system");
        assert!(body["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("from English into French"));
        assert_eq!(
            body["messages"][1]["content"],
            "<text>\nHello\nworld\n</text>"
        );
    }

    #[tokio::test]
    async fn empty_answer_and_server_error() {
        let server = MockServer::start().await;
        Mock::given(header("Authorization", "Bearer empty"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "choices": [] })))
            .mount(&server)
            .await;
        Mock::given(header("Authorization", "Bearer down"))
            .respond_with(ResponseTemplate::new(503).set_body_string("overloaded"))
            .mount(&server)
            .await;
        let client = crate::translate::http_client();
        let empty = OpenAi::with_base_url(client.clone(), &server.uri(), "empty".into(), "");
        assert!(matches!(
            empty.translate("x", None, &Lang::new("fr")).await,
            Err(TranslateError::Parse(ID, _))
        ));
        let down = OpenAi::with_base_url(client, &server.uri(), "down".into(), "");
        assert_eq!(
            down.translate("x", None, &Lang::new("fr"))
                .await
                .unwrap_err(),
            TranslateError::Http {
                engine: ID,
                status: 503,
                message: "overloaded".into()
            }
        );
    }
}
