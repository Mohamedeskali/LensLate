import { useEffect, useState } from "react";
import {
  currentMonitor,
  getCurrentWindow,
  PhysicalPosition,
  PhysicalSize,
} from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import type { Event as TauriEvent } from "@tauri-apps/api/event";
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

type CaptureEvent =
  | {
      type: "frame";
      thumbnailPngBase64: string;
      width: number;
      height: number;
      ms: number;
      skipped: number;
    }
  | { type: "error"; message: string };

const geometryStore = new LazyStore("window-state.json");

function App() {
  const { t, i18n } = useTranslation();
  const [hovered, setHovered] = useState(false);
  const [live, setLive] = useState(false);
  const [thumbnail, setThumbnail] = useState<string | null>(null);
  const [captureInfo, setCaptureInfo] = useState<string>("");
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
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

  const handleCaptureOnce = async () => {
    setErrorMsg(null);
    try {
      const result = await invoke<CaptureEvent>("capture_once");
      if (result.type === "frame") {
        setThumbnail(result.thumbnailPngBase64);
        setCaptureInfo(
          `${result.width}×${result.height} px · ${result.ms} ms${result.skipped ? ` · skipped ${result.skipped}` : ""}`,
        );
      } else {
        setErrorMsg(t("captureError", { message: result.message }));
        setCaptureInfo("");
      }
    } catch (e) {
      setErrorMsg(t("captureError", { message: String(e) }));
    }
  };

  const handleLiveToggle = async () => {
    setErrorMsg(null);
    if (!live) {
      try {
        await invoke("live_start");
        setLive(true);
      } catch (e) {
        setErrorMsg(t("captureError", { message: String(e) }));
      }
    } else {
      try {
        await invoke("live_stop");
        setLive(false);
      } catch (e) {
        setErrorMsg(t("captureError", { message: String(e) }));
      }
    }
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
      const minHeight = 100;
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

  // Listen for live capture events
  useEffect(() => {
    if (view !== "frame") return;

    let unlistenFrame: (() => void) | null = null;
    let unlistenError: (() => void) | null = null;
    let unlistenStopped: (() => void) | null = null;

    const setupListeners = async () => {
      unlistenFrame = await listen<CaptureEvent>(
        "capture://frame",
        (event: TauriEvent<CaptureEvent>) => {
          const payload = event.payload;
          if (payload.type === "frame") {
            setThumbnail(payload.thumbnailPngBase64);
            setCaptureInfo(
              `${payload.width}×${payload.height} px · ${payload.ms} ms${payload.skipped ? ` · skipped ${payload.skipped}` : ""}`,
            );
          }
        },
      );

      unlistenError = await listen<CaptureEvent>(
        "capture://error",
        (event: TauriEvent<CaptureEvent>) => {
          const payload = event.payload;
          if (payload.type === "error") {
            setErrorMsg(t("captureError", { message: payload.message }));
          }
        },
      );

      // Backend stops live capture by itself when the frame is hidden.
      unlistenStopped = await listen("capture://stopped", () => setLive(false));
    };

    setupListeners();

    return () => {
      unlistenFrame?.();
      unlistenError?.();
      unlistenStopped?.();
    };
  }, [view, t]);

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

      {/* Toolbar sits outside the capture area (in the gutter above the border) */}
      <nav
        className={`toolbar${hovered ? " toolbar-visible" : ""}`}
        aria-label={t("toolbar")}
        onMouseDown={(event) => event.stopPropagation()}
      >
        <button
          title={t("capture")}
          aria-label={t("capture")}
          onClick={handleCaptureOnce}
        >
          文
        </button>
        <button
          title={live ? t("pause") : t("live")}
          aria-label={live ? t("pause") : t("live")}
          onClick={handleLiveToggle}
          className={live ? "live-active" : ""}
        >
          {live ? "⏸" : "▶"}
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

      <div className="translation-bar">
        {errorMsg && <span className="capture-error">{errorMsg}</span>}
        {thumbnail && !errorMsg && (
          <>
            <img
              src={`data:image/png;base64,${thumbnail}`}
              alt="Capture thumbnail"
              className="capture-thumbnail"
            />
            <span className="capture-info">{captureInfo}</span>
          </>
        )}
        {!thumbnail && !errorMsg && <span>{t("translationPlaceholder")}</span>}
      </div>
    </main>
  );
}

export default App;
