//! Microsoft Translator (Azure AI Translator) REST API v3.

use std::time::Instant;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{
    elapsed_ms, parse_json, send, EngineId, Lang, TranslateError, TranslateResult, Translation,
    Translator,
};

const ID: EngineId = EngineId::Microsoft;
pub const URL: &str = "https://api.cognitive.microsofttranslator.com/translate";

pub struct Microsoft {
    client: reqwest::Client,
    url: String,
    key: String,
    /// Azure region of the resource; required for regional/multi-service keys.
    region: Option<String>,
}

impl Microsoft {
    pub fn new(client: reqwest::Client, key: String, region: Option<String>) -> Self {
        Self::with_url(client, URL, key, region)
    }

    pub fn with_url(
        client: reqwest::Client,
        url: &str,
        key: String,
        region: Option<String>,
    ) -> Self {
        Self {
            client,
            url: url.to_string(),
            key,
            region: region.filter(|r| !r.trim().is_empty()),
        }
    }
}

#[async_trait]
impl Translator for Microsoft {
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
        let mut query = vec![("api-version", "3.0"), ("to", to.code())];
        if let Some(from) = from {
            query.push(("from", from.code()));
        }
        let mut request = self
            .client
            .post(&self.url)
            .query(&query)
            .header("Ocp-Apim-Subscription-Key", &self.key)
            .json(&json!([{ "Text": text }]));
        if let Some(region) = &self.region {
            request = request.header("Ocp-Apim-Subscription-Region", region);
        }
        let json = parse_json(ID, &send(ID, request).await?)?;
        let item = json.get(0).ok_or_else(|| parse_err("empty list"))?;
        let translated = item
            .pointer("/translations/0/text")
            .and_then(Value::as_str)
            .ok_or_else(|| parse_err("no translations[0].text"))?;
        let detected_from = item
            .pointer("/detectedLanguage/language")
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

fn parse_err(what: &str) -> TranslateError {
    TranslateError::Parse(ID, what.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, header, method, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn request_shape_and_parse() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(query_param("api-version", "3.0"))
            .and(query_param("to", "ar"))
            .and(header("Ocp-Apim-Subscription-Key", "ms-key"))
            .and(header("Ocp-Apim-Subscription-Region", "westeurope"))
            .and(body_json(json!([{ "Text": "Hi\nthere" }])))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
                "detectedLanguage": { "language": "en", "score": 1.0 },
                "translations": [{ "text": "مرحبا\nهناك", "to": "ar" }]
            }])))
            .expect(1)
            .mount(&server)
            .await;
        let ms = Microsoft::with_url(
            crate::translate::http_client(),
            &server.uri(),
            "ms-key".into(),
            Some("westeurope".into()),
        );
        let t = ms
            .translate("Hi\nthere", None, &Lang::new("ar"))
            .await
            .unwrap();
        assert_eq!(t.text, "مرحبا\nهناك");
        assert_eq!(t.detected_from, Some(Lang::new("en")));
        assert_eq!(t.engine, EngineId::Microsoft);
    }

    #[tokio::test]
    async fn source_language_is_sent_and_errors_map() {
        let server = MockServer::start().await;
        Mock::given(query_param("from", "de"))
            .respond_with(ResponseTemplate::new(401).set_body_string("{\"error\":{}}"))
            .mount(&server)
            .await;
        let ms = Microsoft::with_url(
            crate::translate::http_client(),
            &server.uri(),
            "bad".into(),
            Some("  ".into()),
        );
        let err = ms
            .translate("Hallo", Some(&Lang::new("de")), &Lang::new("en"))
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
