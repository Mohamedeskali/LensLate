//! Google Translate through its public web endpoints (no key). Two endpoints
//! are known to work; the one that answered last is tried first, the other
//! one when it is rate limited or returns something unexpected.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use async_trait::async_trait;
use serde_json::Value;

use super::{
    elapsed_ms, parse_json, send, EngineId, Lang, TranslateError, TranslateResult, Translation,
    Translator,
};

const ID: EngineId = EngineId::GoogleFree;
pub const GTX_URL: &str = "https://translate.googleapis.com/translate_a/single";
pub const DICT_URL: &str = "https://clients5.google.com/translate_a/t";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Endpoint {
    /// `client=gtx`: `[[["seg", "orig", …], …], null, "en", …]`
    Gtx,
    /// `client=dict-chrome-ex`: `[["text", "en"]]`, or `["text"]` with a source language.
    Dict,
}

pub struct GoogleFree {
    client: reqwest::Client,
    gtx_url: String,
    dict_url: String,
    /// Index into `[Gtx, Dict]` of the endpoint to try first.
    preferred: AtomicUsize,
}

impl GoogleFree {
    pub fn new(client: reqwest::Client) -> Self {
        Self::with_urls(client, GTX_URL, DICT_URL)
    }

    pub fn with_urls(client: reqwest::Client, gtx_url: &str, dict_url: &str) -> Self {
        Self {
            client,
            gtx_url: gtx_url.to_string(),
            dict_url: dict_url.to_string(),
            preferred: AtomicUsize::new(0),
        }
    }

    async fn call(
        &self,
        endpoint: Endpoint,
        text: &str,
        from: Option<&Lang>,
        to: &Lang,
    ) -> TranslateResult<(String, Option<Lang>)> {
        let sl = from.map_or("auto", Lang::code);
        let request = match endpoint {
            Endpoint::Gtx => self.client.post(&self.gtx_url).query(&[
                ("client", "gtx"),
                ("sl", sl),
                ("tl", to.code()),
                ("dt", "t"),
            ]),
            Endpoint::Dict => self.client.post(&self.dict_url).query(&[
                ("client", "dict-chrome-ex"),
                ("sl", sl),
                ("tl", to.code()),
            ]),
        };
        let body = send(ID, request.form(&[("q", text)])).await?;
        let json = parse_json(ID, &body)?;
        match endpoint {
            Endpoint::Gtx => parse_gtx(&json),
            Endpoint::Dict => parse_dict(&json),
        }
    }
}

fn unexpected(what: &str) -> TranslateError {
    TranslateError::Parse(ID, what.to_string())
}

fn parse_gtx(json: &Value) -> TranslateResult<(String, Option<Lang>)> {
    let segments = json
        .get(0)
        .and_then(Value::as_array)
        .ok_or_else(|| unexpected("no sentence list"))?;
    let text: String = segments
        .iter()
        .filter_map(|s| s.get(0).and_then(Value::as_str))
        .collect();
    let detected = json.get(2).and_then(Value::as_str).map(Lang::new);
    Ok((text, detected))
}

fn parse_dict(json: &Value) -> TranslateResult<(String, Option<Lang>)> {
    let first = json
        .as_array()
        .and_then(|a| a.first())
        .ok_or_else(|| unexpected("empty response"))?;
    match first {
        Value::String(text) => Ok((text.clone(), None)),
        Value::Array(pair) => {
            let text = pair
                .first()
                .and_then(Value::as_str)
                .ok_or_else(|| unexpected("no text"))?;
            let detected = pair.get(1).and_then(Value::as_str).map(Lang::new);
            Ok((text.to_string(), detected))
        }
        _ => Err(unexpected("unknown shape")),
    }
}

#[async_trait]
impl Translator for GoogleFree {
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
        let endpoints = [Endpoint::Gtx, Endpoint::Dict];
        let first = self.preferred.load(Ordering::Relaxed) % 2;
        let mut last_error = None;
        for index in [first, 1 - first] {
            match self.call(endpoints[index], text, from, to).await {
                Ok((translated, detected_from)) => {
                    self.preferred.store(index, Ordering::Relaxed);
                    return Ok(Translation {
                        text: translated,
                        detected_from,
                        engine: ID,
                        ms: elapsed_ms(start),
                    });
                }
                // A timeout already used the chain's budget; do not try again.
                Err(e @ TranslateError::Timeout(_)) => return Err(e),
                Err(e) => last_error = Some(e),
            }
        }
        Err(last_error.expect("two endpoints tried"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn engine(server: &MockServer) -> GoogleFree {
        GoogleFree::with_urls(
            crate::translate::http_client(),
            &format!("{}/translate_a/single", server.uri()),
            &format!("{}/translate_a/t", server.uri()),
        )
    }

    #[tokio::test]
    async fn gtx_request_and_parse() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/translate_a/single"))
            .and(query_param("client", "gtx"))
            .and(query_param("sl", "auto"))
            .and(query_param("tl", "ar"))
            .and(query_param("dt", "t"))
            .and(body_string_contains("q=Hello+world%0AGood+morning"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"[[["مرحبا بالعالم\n","Hello world\n",null,null,10],["صباح الخير","Good morning",null,null,10]],null,"en",null,null,null,1]"#,
            ))
            .expect(1)
            .mount(&server)
            .await;

        let t = engine(&server)
            .translate("Hello world\nGood morning", None, &Lang::new("ar"))
            .await
            .unwrap();
        assert_eq!(t.text, "مرحبا بالعالم\nصباح الخير");
        assert_eq!(t.detected_from, Some(Lang::new("en")));
        assert_eq!(t.engine, EngineId::GoogleFree);
    }

    #[tokio::test]
    async fn falls_back_to_dict_endpoint_and_sticks() {
        let server = MockServer::start().await;
        Mock::given(path("/translate_a/single"))
            .respond_with(ResponseTemplate::new(429).set_body_string("<html>Sorry</html>"))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/translate_a/t"))
            .and(query_param("client", "dict-chrome-ex"))
            .and(query_param("sl", "en"))
            .and(query_param("tl", "fr"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"["Bonjour"]"#))
            .expect(2)
            .mount(&server)
            .await;

        let google = engine(&server);
        let from = Lang::new("en");
        for _ in 0..2 {
            let t = google
                .translate("Hello", Some(&from), &Lang::new("fr"))
                .await
                .unwrap();
            assert_eq!(t.text, "Bonjour");
            assert_eq!(t.detected_from, None);
        }
    }

    #[tokio::test]
    async fn both_endpoints_failing_reports_error() {
        let server = MockServer::start().await;
        Mock::given(path("/translate_a/single"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;
        Mock::given(path("/translate_a/t"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;
        let err = engine(&server)
            .translate("Hello", None, &Lang::new("fr"))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            TranslateError::Parse(EngineId::GoogleFree, _)
        ));
    }

    #[test]
    fn parse_dict_shapes() {
        let auto: Value = serde_json::from_str(r#"[["مرحبًا","en"]]"#).unwrap();
        assert_eq!(
            parse_dict(&auto).unwrap(),
            ("مرحبًا".to_string(), Some(Lang::new("en")))
        );
        let fixed: Value = serde_json::from_str(r#"["Bonjour"]"#).unwrap();
        assert_eq!(parse_dict(&fixed).unwrap(), ("Bonjour".to_string(), None));
        assert!(parse_dict(&serde_json::json!([])).is_err());
        assert!(parse_gtx(&serde_json::json!({"a": 1})).is_err());
    }

    /// Real request to Google (needs the network): `cargo test google_free_live -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore]
    async fn google_free_live() {
        let google = GoogleFree::new(crate::translate::http_client());
        let t = google
            .translate("Good morning\nHow are you?", None, &Lang::new("ar"))
            .await
            .expect("Google free translation");
        println!("{} ms, from {:?}: {}", t.ms, t.detected_from, t.text);
        assert!(!t.text.is_empty());
        assert_eq!(t.text.lines().count(), 2);
    }
}
