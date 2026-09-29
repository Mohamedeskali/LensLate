//! DeepL API v2. Free-plan keys end in `:fx` and use `api-free.deepl.com`;
//! all others use the Pro endpoint.

use std::time::Instant;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{
    elapsed_ms, parse_json, send, EngineId, Lang, TranslateError, TranslateResult, Translation,
    Translator,
};

const ID: EngineId = EngineId::Deepl;
pub const FREE_URL: &str = "https://api-free.deepl.com/v2/translate";
pub const PRO_URL: &str = "https://api.deepl.com/v2/translate";

pub fn endpoint_for_key(key: &str) -> &'static str {
    if key.trim_end().ends_with(":fx") {
        FREE_URL
    } else {
        PRO_URL
    }
}

/// DeepL target codes: English and Portuguese need a variant, Chinese a script.
pub fn target_code(lang: &Lang) -> String {
    let code = lang.code().replace('_', "-").to_ascii_uppercase();
    match code.as_str() {
        "EN" => "EN-US".into(),
        "PT" => "PT-BR".into(),
        "ZH" | "ZH-CN" => "ZH-HANS".into(),
        "ZH-TW" => "ZH-HANT".into(),
        _ => code,
    }
}

/// DeepL source codes have no variants.
pub fn source_code(lang: &Lang) -> String {
    lang.base().to_ascii_uppercase()
}

pub struct Deepl {
    client: reqwest::Client,
    url: String,
    key: String,
}

impl Deepl {
    pub fn new(client: reqwest::Client, key: String) -> Self {
        let url = endpoint_for_key(&key);
        Self::with_url(client, url, key)
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
impl Translator for Deepl {
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
        let mut body = json!({
            "text": [text],
            "target_lang": target_code(to),
            "preserve_formatting": true,
        });
        if let Some(from) = from {
            body["source_lang"] = json!(source_code(from));
        }
        let request = self
            .client
            .post(&self.url)
            .header(
                "Authorization",
                format!("DeepL-Auth-Key {}", self.key.trim()),
            )
            .json(&body);
        let json = parse_json(ID, &send(ID, request).await?)?;
        let item = json
            .pointer("/translations/0")
            .ok_or_else(|| TranslateError::Parse(ID, "no translations[0]".into()))?;
        let translated = item
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| TranslateError::Parse(ID, "no text".into()))?;
        let detected_from = item
            .get("detected_source_language")
            .and_then(Value::as_str)
            .map(|code| Lang::new(&code.to_ascii_lowercase()));
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
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn endpoint_and_codes() {
        assert_eq!(endpoint_for_key("abc:fx"), FREE_URL);
        assert_eq!(endpoint_for_key("abc"), PRO_URL);
        assert_eq!(target_code(&Lang::new("en")), "EN-US");
        assert_eq!(target_code(&Lang::new("pt")), "PT-BR");
        assert_eq!(target_code(&Lang::new("zh-TW")), "ZH-HANT");
        assert_eq!(target_code(&Lang::new("ar")), "AR");
        assert_eq!(source_code(&Lang::new("en-GB")), "EN");
    }

    #[tokio::test]
    async fn request_shape_and_parse() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2/translate"))
            .and(header("Authorization", "DeepL-Auth-Key k:fx"))
            .and(body_json(json!({
                "text": ["Guten Morgen\nWelt"],
                "target_lang": "EN-US",
                "preserve_formatting": true,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "translations": [{ "detected_source_language": "DE", "text": "Good morning\nWorld" }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let deepl = Deepl::with_url(
            crate::translate::http_client(),
            &format!("{}/v2/translate", server.uri()),
            "k:fx".into(),
        );
        let t = deepl
            .translate("Guten Morgen\nWelt", None, &Lang::new("en"))
            .await
            .unwrap();
        assert_eq!(t.text, "Good morning\nWorld");
        assert_eq!(t.detected_from, Some(Lang::new("de")));
    }

    #[tokio::test]
    async fn quota_exceeded_maps_to_rate_limited() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_json(json!({
                "text": ["x"], "target_lang": "AR", "preserve_formatting": true, "source_lang": "EN"
            })))
            .respond_with(ResponseTemplate::new(456))
            .mount(&server)
            .await;
        let deepl = Deepl::with_url(crate::translate::http_client(), &server.uri(), "k".into());
        let err = deepl
            .translate("x", Some(&Lang::new("en")), &Lang::new("ar"))
            .await
            .unwrap_err();
        assert_eq!(
            err,
            TranslateError::RateLimited {
                engine: ID,
                status: 456
            }
        );
    }
}
