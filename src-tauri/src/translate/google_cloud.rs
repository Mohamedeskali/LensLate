//! Google Cloud Translation API (Basic, v2). The key goes in the
//! `X-goog-api-key` header rather than the URL so it cannot leak into logs.

use std::time::Instant;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{
    elapsed_ms, parse_json, send, EngineId, Lang, TranslateError, TranslateResult, Translation,
    Translator,
};

const ID: EngineId = EngineId::GoogleCloud;
pub const URL: &str = "https://translation.googleapis.com/language/translate/v2";

pub struct GoogleCloud {
    client: reqwest::Client,
    url: String,
    key: String,
}

impl GoogleCloud {
    pub fn new(client: reqwest::Client, key: String) -> Self {
        Self::with_url(client, URL, key)
    }

    pub fn with_url(client: reqwest::Client, url: &str, key: String) -> Self {
        Self {
            client,
            url: url.to_string(),
            key,
        }
    }
}

#[async_trait]
impl Translator for GoogleCloud {
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
        let mut body = json!({ "q": text, "target": to.code(), "format": "text" });
        if let Some(from) = from {
            body["source"] = json!(from.code());
        }
        let request = self
            .client
            .post(&self.url)
            .header("X-goog-api-key", &self.key)
            .json(&body);
        let json = parse_json(ID, &send(ID, request).await?)?;
        let item = json
            .pointer("/data/translations/0")
            .ok_or_else(|| TranslateError::Parse(ID, "no data.translations[0]".into()))?;
        let translated = item
            .get("translatedText")
            .and_then(Value::as_str)
            .ok_or_else(|| TranslateError::Parse(ID, "no translatedText".into()))?;
        let detected_from = item
            .get("detectedSourceLanguage")
            .and_then(Value::as_str)
            .map(Lang::new);
        Ok(Translation {
            text: translated.to_string(),
            detected_from,
            engine: ID,
            ms: elapsed_ms(start),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, header, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn request_shape_and_parse() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(header("X-goog-api-key", "gc-key"))
            .and(body_json(json!({
                "q": "Bonjour\nle monde", "target": "en", "format": "text", "source": "fr"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": { "translations": [{ "translatedText": "Hello\nthe world" }] }
            })))
            .expect(1)
            .mount(&server)
            .await;
        let gc = GoogleCloud::with_url(
            crate::translate::http_client(),
            &server.uri(),
            "gc-key".into(),
        );
        let t = gc
            .translate(
                "Bonjour\nle monde",
                Some(&Lang::new("fr")),
                &Lang::new("en"),
            )
            .await
            .unwrap();
        assert_eq!(t.text, "Hello\nthe world");
        assert_eq!(t.detected_from, None);
        assert_eq!(t.engine, ID);
    }

    #[tokio::test]
    async fn detected_language_and_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(header("X-goog-api-key", "ok"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": { "translations": [{ "translatedText": "Hola", "detectedSourceLanguage": "en" }] }
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(header("X-goog-api-key", "quota"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;
        let client = crate::translate::http_client();
        let ok = GoogleCloud::with_url(client.clone(), &server.uri(), "ok".into());
        let t = ok.translate("Hi", None, &Lang::new("es")).await.unwrap();
        assert_eq!(t.detected_from, Some(Lang::new("en")));
        let quota = GoogleCloud::with_url(client, &server.uri(), "quota".into());
        assert_eq!(
            quota
                .translate("Hi", None, &Lang::new("es"))
                .await
                .unwrap_err(),
            TranslateError::RateLimited {
                engine: ID,
                status: 429
            }
        );
    }
}
