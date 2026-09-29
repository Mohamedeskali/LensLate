import { useEffect, useMemo } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useTranslation } from "react-i18next";
import FrameView from "./FrameView";
import SettingsView from "./SettingsView";
import "./App.css";

function App() {
  const { t, i18n } = useTranslation();
  const view = new URLSearchParams(window.location.search).get("view");
  const windowHandle = useMemo(() => getCurrentWindow(), []);

  useEffect(() => {
    document.documentElement.lang = i18n.language;
    document.documentElement.dir = i18n.language === "ar" ? "rtl" : "ltr";
  }, [i18n.language]);

  useEffect(() => {
    if (view && view !== "frame") document.body.dataset.view = view;
  }, [view]);

  // Colours follow the system theme, live.
  useEffect(() => {
    let disposed = false;
    const applyTheme = (theme: "light" | "dark" | null) => {
      if (!disposed) document.documentElement.dataset.theme = theme ?? "light";
    };

    void windowHandle.theme().then(applyTheme);
    const unlisten = windowHandle.onThemeChanged(({ payload }) =>
      applyTheme(payload),
    );
    return () => {
      disposed = true;
      void unlisten.then((stop) => stop());
    };
  }, [windowHandle]);

  if (view === "settings") return <SettingsView />;
  if (view === "history") {
    return (
      <main className="utility">
        <h1>{t("history")}</h1>
      </main>
    );
  }
  return <FrameView />;
}

export default App;
