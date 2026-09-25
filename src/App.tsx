import { useEffect, useState } from "react";
import {
  getCurrentWindow,
  PhysicalPosition,
  PhysicalSize,
} from "@tauri-apps/api/window";
import { invoke } from "@tauri-apps/api/core";
import { LazyStore } from "@tauri-apps/plugin-store";
import { useTranslation } from "react-i18next";
import "./App.css";

type ResizeDirection =
  | "East"
  | "North"
  | "NorthEast"
  | "NorthWest"
  | "South"
  | "SouthEast"
  | "SouthWest"
  | "West";

const geometryStore = new LazyStore("window-state.json");

function App() {
  const { t, i18n } = useTranslation();
  const [hovered, setHovered] = useState(false);
  const view = new URLSearchParams(window.location.search).get("view");
  const windowHandle = getCurrentWindow();

  const startResize = (direction: ResizeDirection) => {
    void windowHandle.startResizeDragging(direction);
  };

  const startMove = () => {
    void windowHandle.startDragging();
  };

  useEffect(() => {
    document.documentElement.lang = i18n.language;
    document.documentElement.dir = i18n.language === "ar" ? "rtl" : "ltr";
  }, [i18n.language]);

  useEffect(() => {
    if (view && view !== "frame") document.body.dataset.view = view;
  }, [view]);

  useEffect(() => {
    if (view !== "frame") return;

    let disposed = false;
    const persistGeometry = async () => {
      const [size, position] = await Promise.all([
        windowHandle.innerSize(),
        windowHandle.outerPosition(),
      ]);
      if (disposed) return;
      await geometryStore.set("frame", {
        width: size.width,
        height: size.height,
        x: position.x,
        y: position.y,
      });
    };

    void (async () => {
      const saved = await geometryStore.get<{
        width?: number;
        height?: number;
        x?: number;
        y?: number;
      }>("frame");
      if (disposed || !saved) return;
      if (saved.width && saved.height) {
        await windowHandle.setSize(new PhysicalSize(saved.width, saved.height));
      }
      if (saved.x !== undefined && saved.y !== undefined) {
        await windowHandle.setPosition(new PhysicalPosition(saved.x, saved.y));
      }
    })();

    const unlistenResize = windowHandle.onResized(() => void persistGeometry());
    const unlistenMove = windowHandle.onMoved(() => void persistGeometry());
    return () => {
      disposed = true;
      void unlistenResize.then((unlisten) => unlisten());
      void unlistenMove.then((unlisten) => unlisten());
    };
  }, [view, windowHandle]);

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
      onMouseDown={(event) => {
        if (event.button === 0 && event.target === event.currentTarget) startMove();
      }}
    >
      <div className="resize-zone resize-n" onMouseDown={() => startResize("North")} />
      <div className="resize-zone resize-ne" onMouseDown={() => startResize("NorthEast")} />
      <div className="resize-zone resize-e" onMouseDown={() => startResize("East")} />
      <div className="resize-zone resize-se" onMouseDown={() => startResize("SouthEast")} />
      <div className="resize-zone resize-s" onMouseDown={() => startResize("South")} />
      <div className="resize-zone resize-sw" onMouseDown={() => startResize("SouthWest")} />
      <div className="resize-zone resize-w" onMouseDown={() => startResize("West")} />
      <div className="resize-zone resize-nw" onMouseDown={() => startResize("NorthWest")} />
      <div className="frame-outline" aria-hidden="true" />
      <nav
        className={`toolbar${hovered ? " toolbar-visible" : ""}`}
        aria-label={t("toolbar")}
        onMouseDown={(event) => event.stopPropagation()}
      >
        <button
          title={t("language")}
          aria-label={t("language")}
          onClick={() => undefined}
        >
          文
        </button>
        <button
          title={t("pause")}
          aria-label={t("pause")}
          onClick={() => undefined}
        >
          ▶
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
