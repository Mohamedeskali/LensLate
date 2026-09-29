//! Non-secret settings, stored with tauri-plugin-store in `settings.json`.
//! API keys are not here: they live in the OS keychain (`translate::keys`).

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tauri_plugin_store::StoreExt;

use crate::translate::registry::EngineSettings;

const STORE_FILE: &str = "settings.json";
const STORE_KEY: &str = "settings";
pub const MIN_FONT_SIZE: u32 = 10;
pub const MAX_FONT_SIZE: u32 = 32;

/// How the result is shown next to the frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayMode {
    /// A card below the frame (above, or inside, when there is no room).
    #[default]
    Panel,
    /// Each OCR line covered in place with its translation.
    Overlay,
    /// A compact bubble beside the frame.
    Side,
    /// No translation drawn; the frame bar shows the original text only.
    Original,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Target language code; defaults to the system language.
    pub target_lang: String,
    pub display_mode: DisplayMode,
    /// Result text size in CSS pixels.
    pub font_size: u32,
    /// Show the original text above the translation (else translation only).
    pub show_original: bool,
    pub engines: EngineSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            target_lang: system_language(),
            display_mode: DisplayMode::Panel,
            font_size: 15,
            show_original: false,
            engines: EngineSettings::default(),
        }
    }
}

impl Settings {
    /// Clamp values edited by hand or by an older version.
    pub fn sanitized(mut self) -> Self {
        self.font_size = self.font_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
        if self.target_lang.trim().is_empty() {
            self.target_lang = system_language();
        }
        self.target_lang = self.target_lang.trim().to_string();
        self
    }
}

/// The system UI language as a translation target (`en-US` → `en`, keeping
/// the script/region only for Chinese).
pub fn system_language() -> String {
    sys_locale::get_locale()
        .map(|l| language_from_locale(&l))
        .unwrap_or_else(|| "en".into())
}

fn language_from_locale(locale: &str) -> String {
    let locale = locale
        .split(['.', '@'])
        .next()
        .unwrap_or_default()
        .replace('_', "-");
    let mut parts = locale.split('-');
    let base = parts.next().unwrap_or_default().to_ascii_lowercase();
    match base.as_str() {
        "" | "c" | "posix" => "en".into(),
        "zh" => {
            let rest: Vec<String> = parts.map(str::to_ascii_uppercase).collect();
            if rest.iter().any(|p| p == "TW" || p == "HK" || p == "HANT") {
                "zh-TW".into()
            } else {
                "zh-CN".into()
            }
        }
        _ => base,
    }
}

pub fn load(app: &AppHandle) -> Settings {
    let stored = app
        .store(STORE_FILE)
        .ok()
        .and_then(|store| store.get(STORE_KEY))
        .and_then(|v| serde_json::from_value::<Settings>(v).ok());
    stored.unwrap_or_default().sanitized()
}

pub fn save(app: &AppHandle, settings: &Settings) -> Result<(), String> {
    let store = app.store(STORE_FILE).map_err(|e| e.to_string())?;
    store.set(
        STORE_KEY,
        serde_json::to_value(settings).map_err(|e| e.to_string())?,
    );
    store.save().map_err(|e| e.to_string())?;
    let _ = app.emit("settings://changed", settings);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::translate::EngineId;

    #[test]
    fn locale_to_language() {
        assert_eq!(language_from_locale("en_US.UTF-8"), "en");
        assert_eq!(language_from_locale("ar-EG"), "ar");
        assert_eq!(language_from_locale("fr"), "fr");
        assert_eq!(language_from_locale("zh-Hant-TW"), "zh-TW");
        assert_eq!(language_from_locale("zh_CN"), "zh-CN");
        assert_eq!(language_from_locale("C"), "en");
        assert_eq!(language_from_locale("de_DE@euro"), "de");
    }

    #[test]
    fn partial_json_gets_defaults_and_is_sanitized() {
        let s: Settings = serde_json::from_str(
            r#"{"targetLang":" ar ","displayMode":"overlay","fontSize":99,"engines":{"primary":"deepl"}}"#,
        )
        .unwrap();
        let s = s.sanitized();
        assert_eq!(s.target_lang, "ar");
        assert_eq!(s.display_mode, DisplayMode::Overlay);
        assert_eq!(s.font_size, MAX_FONT_SIZE);
        assert_eq!(s.engines.primary, EngineId::Deepl);
        assert_eq!(s.engines.ollama_url, "http://localhost:11434");

        let empty: Settings = serde_json::from_str(r#"{"targetLang":"","fontSize":1}"#).unwrap();
        let empty = empty.sanitized();
        assert!(!empty.target_lang.is_empty());
        assert_eq!(empty.font_size, MIN_FONT_SIZE);
    }

    #[test]
    fn display_modes_serialize_as_snake_case() {
        for (mode, name) in [
            (DisplayMode::Panel, "panel"),
            (DisplayMode::Overlay, "overlay"),
            (DisplayMode::Side, "side"),
            (DisplayMode::Original, "original"),
        ] {
            assert_eq!(serde_json::to_string(&mode).unwrap(), format!("\"{name}\""));
        }
    }
}
