//! Engine settings (non-secret) and building the fallback chain from them.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::chain::FallbackChain;
use super::keys::SecretStore;
use super::{
    claude, deepl, gemini, google_cloud, google_free, microsoft, ollama, openai, EngineId, Lang,
    TranslateError, TranslateResult, Translation, Translator,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EngineSettings {
    /// Engine chosen in the frame bar; always tried first.
    pub primary: EngineId,
    /// User-ordered fallback chain tried after the primary engine.
    pub order: Vec<EngineId>,
    pub claude_model: String,
    pub openai_model: String,
    pub gemini_model: String,
    pub ollama_model: String,
    pub ollama_url: String,
    /// Azure region for Microsoft Translator keys that need one.
    pub microsoft_region: String,
}

impl Default for EngineSettings {
    fn default() -> Self {
        Self {
            primary: EngineId::GoogleFree,
            order: vec![EngineId::GoogleFree],
            claude_model: claude::DEFAULT_MODEL.into(),
            openai_model: openai::DEFAULT_MODEL.into(),
            gemini_model: gemini::DEFAULT_MODEL.into(),
            ollama_model: ollama::DEFAULT_MODEL.into(),
            ollama_url: ollama::DEFAULT_URL.into(),
            microsoft_region: String::new(),
        }
    }
}

impl EngineSettings {
    /// The primary engine followed by the fallback order, without duplicates.
    pub fn resolved_order(&self) -> Vec<EngineId> {
        let mut out = vec![self.primary];
        for &id in &self.order {
            if !out.contains(&id) {
                out.push(id);
            }
        }
        out
    }
}

/// Stands in for an engine whose API key is missing, so the chain reports
/// it and moves on.
struct Unavailable(EngineId);

#[async_trait]
impl Translator for Unavailable {
    fn id(&self) -> EngineId {
        self.0
    }

    async fn translate(&self, _: &str, _: Option<&Lang>, _: &Lang) -> TranslateResult<Translation> {
        Err(TranslateError::MissingKey(self.0))
    }
}

pub fn build_engine(
    id: EngineId,
    settings: &EngineSettings,
    secrets: &dyn SecretStore,
    client: &reqwest::Client,
) -> Arc<dyn Translator> {
    let key = if id.needs_key() {
        match secrets.get(id) {
            Ok(Some(key)) => key,
            Ok(None) => return Arc::new(Unavailable(id)),
            Err(e) => {
                eprintln!("[lenslate] keychain error for {id}: {e}");
                return Arc::new(Unavailable(id));
            }
        }
    } else {
        String::new()
    };
    let client = client.clone();
    match id {
        EngineId::GoogleFree => Arc::new(google_free::GoogleFree::new(client)),
        EngineId::Microsoft => Arc::new(microsoft::Microsoft::new(
            client,
            key,
            Some(settings.microsoft_region.clone()),
        )),
        EngineId::GoogleCloud => Arc::new(google_cloud::GoogleCloud::new(client, key)),
        EngineId::Deepl => Arc::new(deepl::Deepl::new(client, key)),
        EngineId::Claude => Arc::new(claude::Claude::new(client, key, &settings.claude_model)),
        EngineId::Openai => Arc::new(openai::OpenAi::new(client, key, &settings.openai_model)),
        EngineId::Gemini => Arc::new(gemini::Gemini::new(client, key, &settings.gemini_model)),
        EngineId::Ollama => Arc::new(ollama::Ollama::new(
            client,
            &settings.ollama_url,
            &settings.ollama_model,
        )),
    }
}

pub fn build_chain(
    settings: &EngineSettings,
    secrets: &dyn SecretStore,
    client: &reqwest::Client,
) -> FallbackChain {
    FallbackChain::new(
        settings
            .resolved_order()
            .into_iter()
            .map(|id| build_engine(id, settings, secrets, client))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::translate::keys::MemoryStore;

    #[test]
    fn resolved_order_puts_primary_first_without_duplicates() {
        let settings = EngineSettings {
            primary: EngineId::Deepl,
            order: vec![EngineId::Claude, EngineId::Deepl, EngineId::GoogleFree],
            ..Default::default()
        };
        assert_eq!(
            settings.resolved_order(),
            vec![EngineId::Deepl, EngineId::Claude, EngineId::GoogleFree]
        );
        assert_eq!(
            EngineSettings::default().resolved_order(),
            vec![EngineId::GoogleFree]
        );
    }

    #[test]
    fn settings_fill_missing_fields_with_defaults() {
        let s: EngineSettings = serde_json::from_str(r#"{"primary":"claude"}"#).unwrap();
        assert_eq!(s.primary, EngineId::Claude);
        assert_eq!(s.claude_model, "claude-haiku-4-5-20251001");
        assert_eq!(s.order, vec![EngineId::GoogleFree]);
    }

    #[tokio::test]
    async fn missing_key_is_reported_and_skipped() {
        let settings = EngineSettings {
            primary: EngineId::Claude,
            order: vec![EngineId::Deepl],
            ..Default::default()
        };
        let secrets = MemoryStore::default();
        let chain = build_chain(&settings, &secrets, &crate::translate::http_client());
        assert_eq!(chain.ids(), vec![EngineId::Claude, EngineId::Deepl]);
        match chain.translate("x", None, &Lang::new("ar")).await {
            Err(TranslateError::AllFailed(msg)) => {
                assert!(msg.contains("claude: no API key set"), "{msg}");
                assert!(msg.contains("deepl: no API key set"), "{msg}");
            }
            other => panic!("{other:?}"),
        }
    }
}
