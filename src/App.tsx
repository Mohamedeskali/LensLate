import { useEffect, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import "./App.css";

function App() {
  const { t, i18n } = useTranslation();
  const [hovered, setHovered] = useState(false);
  const [paused, setPaused] = useState(false);
  const view = new URLSearchParams(window.location.search).get("view");
  const windowHandle = getCurrentWindow();

  useEffect(() => {
    document.documentElement.lang = i18n.language;
    document.documentElement.dir = i18n.language === "ar" ? "rtl" : "ltr";
  }, [i18n.language]);

  useEffect(() => {
    if (view && view !== "frame") document.body.dataset.view = view;
  }, [view]);

  if (view === "settings" || view === "history") {
    return (
      <main className="utility">
        <h1>{view === "settings" ? t("settings") : t("history")}</h1>
      </main>
    );
  }

  return (
    <main
      className="frame-shell"
      onMouseEnter={() => setHovered(true)}
      onMouseLeave={() => setHovered(false)}
    >
      <div className="frame-outline" aria-hidden="true" />
      <nav
        className={`toolbar${hovered ? " toolbar-visible" : ""}`}
        aria-label={t("toolbar")}
      >
        <button
          title={t("language")}
          aria-label={t("language")}
          onClick={() =>
            void i18n.changeLanguage(i18n.language === "en" ? "ar" : "en")
          }
        >
          文
        </button>
        <button
          title={paused ? t("resume") : t("pause")}
          aria-label={paused ? t("resume") : t("pause")}
          onClick={() => setPaused(!paused)}
        >
          {paused ? "▶" : "Ⅱ"}
        </button>
        <button
          title={t("settings")}
          aria-label={t("settings")}
          onClick={() => void invoke("open_settings")}
        >
          ⚙
        </button>
        <button
          title={t("close")}
          aria-label={t("close")}
          onClick={() => void windowHandle.hide()}
        >
          ×
        </button>
      </nav>
      <div className="translation-bar">{t("translationPlaceholder")}</div>
    </main>
  );
}

export default App;
