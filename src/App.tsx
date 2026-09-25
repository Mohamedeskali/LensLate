import { useEffect, useState } from "react";
import {
  currentMonitor,
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
    void windowHandle.startResizeDragging(direction).catch((error: unknown) => {
      void invoke("log_frontend_error", {
        message: `startResizeDragging(${direction}) failed: ${String(error)}`,
      });
    });
  };

  const startMove = () => {
    void windowHandle.startDragging().catch((error: unknown) => {
      void invoke("log_frontend_error", {
        message: `startDragging failed: ${String(error)}`,
      });
    });
  };

  useEffect(() => {
    document.documentElement.lang = i18n.language;
    document.documentElement.dir = i18n.language === "ar" ? "rtl" : "ltr";
  }, [i18n.language]);

  useEffect(() => {
    if (view && view !== "frame") document.body.dataset.view = view;
  }, [view]);

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

  useEffect(() => {
    if (view !== "frame") return;

    let disposed = false;
    const restoreGeometry = async () => {
      const monitor = await currentMonitor();
      const saved = await geometryStore.get<{
        width?: number;
        height?: number;
        x?: number;
        y?: number;
      }>("frame");
      const minWidth = 120;
      const minHeight = 60;
      const maxWidth = monitor
        ? Math.max(minWidth, monitor.size.width * 0.8)
        : 480;
      const maxHeight = monitor
        ? Math.max(minHeight, monitor.size.height * 0.8)
        : 160;
      const width = Math.min(maxWidth, Math.max(minWidth, saved?.width ?? 480));
      const height = Math.min(
        maxHeight,
        Math.max(minHeight, saved?.height ?? 160),
      );

      await windowHandle.setSize(new PhysicalSize(width, height));
      if (saved?.x !== undefined && saved.y !== undefined) {
        try {
          await windowHandle.setPosition(
            new PhysicalPosition(saved.x, saved.y),
          );
        } catch {
          // Wayland compositors may reject application-controlled positioning.
        }
      }
    };
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
      if (disposed) return;
      await restoreGeometry();
    })();

    const unlistenResize = windowHandle.onResized(() => void persistGeometry());
    const unlistenMove = windowHandle.onMoved(() => void persistGeometry());
    return () => {
      disposed = true;
      void persistGeometry();
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
        const target = event.target as HTMLElement;
        if (
          event.button === 0 &&
          !target.closest(".resize-zone") &&
          !target.closest("button")
        )
          event.preventDefault();
        if (
          event.button === 0 &&
          !target.closest(".resize-zone") &&
          !target.closest("button")
        )
          startMove();
      }}
    >
      <div
        className="resize-zone resize-n"
        onMouseDown={(event) => {
          event.preventDefault();
          event.stopPropagation();
          startResize("North");
        }}
      />
      <div
        className="resize-zone resize-ne"
        onMouseDown={(event) => {
          event.preventDefault();
          event.stopPropagation();
          startResize("NorthEast");
        }}
      />
      <div
        className="resize-zone resize-e"
        onMouseDown={(event) => {
          event.preventDefault();
          event.stopPropagation();
          startResize("East");
        }}
      />
      <div
        className="resize-zone resize-se"
        onMouseDown={(event) => {
          event.preventDefault();
          event.stopPropagation();
          startResize("SouthEast");
        }}
      />
      <div
        className="resize-zone resize-s"
        onMouseDown={(event) => {
          event.preventDefault();
          event.stopPropagation();
          startResize("South");
        }}
      />
      <div
        className="resize-zone resize-sw"
        onMouseDown={(event) => {
          event.preventDefault();
          event.stopPropagation();
          startResize("SouthWest");
        }}
      />
      <div
        className="resize-zone resize-w"
        onMouseDown={(event) => {
          event.preventDefault();
          event.stopPropagation();
          startResize("West");
        }}
      />
      <div
        className="resize-zone resize-nw"
        onMouseDown={(event) => {
          event.preventDefault();
          event.stopPropagation();
          startResize("NorthWest");
        }}
      />
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
