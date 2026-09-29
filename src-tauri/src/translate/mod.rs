//! Translation engines (Phase 4).
//!
//! Every engine implements [`Translator`]. [`chain::FallbackChain`] tries a
//! user-ordered list of engines with a timeout each, and [`cache::TranslationCache`]
//! keeps recent results so unchanged text never reaches the network twice.
//! API keys live in the OS keychain ([`keys`]); they are passed to engines at
//! construction and never logged. An offline engine can be added later by
//! implementing the same trait.

use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub mod ai;
pub mod cache;
pub mod chain;
pub mod claude;
pub mod deepl;
pub mod gemini;
pub mod google_cloud;
pub mod google_free;
pub mod keys;
pub mod microsoft;
pub mod ollama;
pub mod openai;
pub mod pipeline;
pub mod registry;

/// Per-engine timeout used by the fallback chain.
pub const ENGINE_TIMEOUT: Duration = Duration::from_secs(5);

/// A language code as used by the frontend: ISO 639-1 (`en`, `ar`) with an
/// optional region or script (`zh-CN`, `pt-BR`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Lang(String);

impl Lang {
    pub fn new(code: &str) -> Lang {
        Lang(code.trim().to_string())
    }

    pub fn code(&self) -> &str {
        &self.0
    }

    /// The primary subtag, lower case (`pt-BR` → `pt`).
    pub fn base(&self) -> String {
        self.0
            .split(['-', '_'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase()
    }

    /// English name used in AI prompts; falls back to the code.
    pub fn english_name(&self) -> String {
        let name = match self.base().as_str() {
            "ar" => "Arabic",
            "de" => "German",
            "en" => "English",
            "es" => "Spanish",
            "fa" => "Persian",
            "fr" => "French",
            "he" => "Hebrew",
            "hi" => "Hindi",
            "id" => "Indonesian",
            "it" => "Italian",
            "ja" => "Japanese",
            "ko" => "Korean",
            "nl" => "Dutch",
            "pl" => "Polish",
            "pt" => "Portuguese",
            "ru" => "Russian",
            "sv" => "Swedish",
            "tr" => "Turkish",
            "uk" => "Ukrainian",
            "ur" => "Urdu",
            "zh" if self.0.eq_ignore_ascii_case("zh-TW") => "Traditional Chinese",
            "zh" => "Simplified Chinese",
            _ => return self.0.clone(),
        };
        name.to_string()
    }

    /// Whether text in this language is written right to left.
    pub fn is_rtl(&self) -> bool {
        matches!(self.base().as_str(), "ar" | "fa" | "he" | "ur")
    }
}

impl fmt::Display for Lang {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Identifies an engine in settings, the cache and events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineId {
    GoogleFree,
    Microsoft,
    GoogleCloud,
    Deepl,
    Claude,
    Openai,
    Gemini,
    Ollama,
}

impl EngineId {
    pub const ALL: [EngineId; 8] = [
        EngineId::GoogleFree,
        EngineId::Microsoft,
        EngineId::GoogleCloud,
        EngineId::Deepl,
        EngineId::Claude,
        EngineId::Openai,
        EngineId::Gemini,
        EngineId::Ollama,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            EngineId::GoogleFree => "google_free",
            EngineId::Microsoft => "microsoft",
            EngineId::GoogleCloud => "google_cloud",
            EngineId::Deepl => "deepl",
            EngineId::Claude => "claude",
            EngineId::Openai => "openai",
            EngineId::Gemini => "gemini",
            EngineId::Ollama => "ollama",
        }
    }

    /// Engines that cannot work without an API key in the keychain.
    pub fn needs_key(self) -> bool {
        !matches!(self, EngineId::GoogleFree | EngineId::Ollama)
    }
}

impl fmt::Display for EngineId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A finished translation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Translation {
    pub text: String,
    /// Source language reported by the engine (AI engines do not report one).
    pub detected_from: Option<Lang>,
    /// The engine that produced the text.
    pub engine: EngineId,
    /// Wall time of the request that produced the text.
    pub ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TranslateError {
    #[error("{0}: no API key set")]
    MissingKey(EngineId),
    #[error("{engine}: HTTP {status}: {message}")]
    Http {
        engine: EngineId,
        status: u16,
        message: String,
    },
    #[error("{engine}: authentication failed (HTTP {status})")]
    Auth { engine: EngineId, status: u16 },
    #[error("{engine}: rate limited or quota exceeded (HTTP {status})")]
    RateLimited { engine: EngineId, status: u16 },
    #[error("{0}: network error: {1}")]
    Network(EngineId, String),
    #[error("{0}: unexpected response: {1}")]
    Parse(EngineId, String),
    #[error("{0}: timed out")]
    Timeout(EngineId),
    #[error("no translation engine is configured")]
    NoEngine,
    #[error("all engines failed: {0}")]
    AllFailed(String),
}

pub type TranslateResult<T> = Result<T, TranslateError>;

#[async_trait]
pub trait Translator: Send + Sync {
    fn id(&self) -> EngineId;

    /// Translate `text` into `to`. `from = None` lets the engine detect the
    /// source language. Line breaks in `text` must survive.
    async fn translate(
        &self,
        text: &str,
        from: Option<&Lang>,
        to: &Lang,
    ) -> TranslateResult<Translation>;
}

/// Shared HTTP client; engines get their own timeouts from the chain.
pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("LensLate/", env!("CARGO_PKG_VERSION")))
        .timeout(ENGINE_TIMEOUT)
        .build()
        .unwrap_or_default()
}

/// Map a non-success HTTP status and body to an error. The body is shortened
/// so a verbose error page does not flood the log; keys are never part of it
/// because every engine sends them in headers.
pub(crate) fn status_error(
    engine: EngineId,
    status: reqwest::StatusCode,
    body: &str,
) -> TranslateError {
    let status = status.as_u16();
    match status {
        401 | 403 => TranslateError::Auth { engine, status },
        429 | 456 => TranslateError::RateLimited { engine, status },
        _ => {
            let message: String = body.trim().chars().take(200).collect();
            TranslateError::Http {
                engine,
                status,
                message,
            }
        }
    }
}

pub(crate) fn network_error(engine: EngineId, e: reqwest::Error) -> TranslateError {
    if e.is_timeout() {
        TranslateError::Timeout(engine)
    } else {
        // Without the URL: some APIs could carry secrets in it.
        TranslateError::Network(engine, e.without_url().to_string())
    }
}

/// Send a request and return the body of a successful response.
pub(crate) async fn send(
    engine: EngineId,
    request: reqwest::RequestBuilder,
) -> TranslateResult<String> {
    let response = request.send().await.map_err(|e| network_error(engine, e))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| network_error(engine, e))?;
    if !status.is_success() {
        return Err(status_error(engine, status, &body));
    }
    Ok(body)
}

pub(crate) fn parse_json(engine: EngineId, body: &str) -> TranslateResult<serde_json::Value> {
    serde_json::from_str(body).map_err(|e| TranslateError::Parse(engine, e.to_string()))
}

pub(crate) fn elapsed_ms(start: std::time::Instant) -> u64 {
    start.elapsed().as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lang_helpers() {
        assert_eq!(Lang::new("pt-BR").base(), "pt");
        assert_eq!(Lang::new("zh_TW").base(), "zh");
        assert_eq!(Lang::new("ar").english_name(), "Arabic");
        assert_eq!(Lang::new("zh-TW").english_name(), "Traditional Chinese");
        assert_eq!(Lang::new("xx").english_name(), "xx");
        assert!(Lang::new("ar").is_rtl());
        assert!(Lang::new("he").is_rtl());
        assert!(!Lang::new("en").is_rtl());
    }

    #[test]
    fn engine_ids_round_trip() {
        for id in EngineId::ALL {
            let json = serde_json::to_string(&id).unwrap();
            assert_eq!(json, format!("\"{}\"", id.as_str()));
            assert_eq!(serde_json::from_str::<EngineId>(&json).unwrap(), id);
        }
        assert!(!EngineId::GoogleFree.needs_key());
        assert!(!EngineId::Ollama.needs_key());
        assert!(EngineId::Deepl.needs_key());
    }

    #[test]
    fn status_mapping() {
        use reqwest::StatusCode;
        let e = EngineId::Deepl;
        assert_eq!(
            status_error(e, StatusCode::FORBIDDEN, ""),
            TranslateError::Auth {
                engine: e,
                status: 403
            }
        );
        assert_eq!(
            status_error(e, StatusCode::from_u16(456).unwrap(), ""),
            TranslateError::RateLimited {
                engine: e,
                status: 456
            }
        );
        let long = "x".repeat(1000);
        match status_error(e, StatusCode::INTERNAL_SERVER_ERROR, &long) {
            TranslateError::Http {
                status, message, ..
            } => {
                assert_eq!(status, 500);
                assert_eq!(message.len(), 200);
            }
            other => panic!("{other:?}"),
        }
    }
}
