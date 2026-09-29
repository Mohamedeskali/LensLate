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

type OcrScript = "auto" | "latin" | "arabic";

type OcrLine = {
  text: string;
  conf: number;
  rtl: boolean;
};

type OcrResult = {
  lines: OcrLine[];
  text: string;
  ms: number;
  script: OcrScript;
};

type ModelsEvent = {
  state: "missing" | "downloading" | "ready" | "error";
  progress: number | null;
  message: string | null;
};

const SCRIPTS: OcrScript[] = ["auto", "latin", "arabic"];

async function copyText(text: string) {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    // WebKitGTK may refuse the async clipboard API; fall back to a selection.
    const area = document.createElement("textarea");
    area.value = text;
    document.body.appendChild(area);
    area.select();
    document.execCommand("copy");
    area.remove();
  }
}

const geometryStore = new LazyStore("window-state.json");

function App() {
  const { t, i18n } = useTranslation();
  const [hovered, setHovered] = useState(false);
  const [live, setLive] = useState(false);
  const [ocr, setOcr] = useState<OcrResult | null>(null);
  const [reading, setReading] = useState(false);
  const [copied, setCopied] = useState(false);
  const [script, setScript] = useState<OcrScript>("auto");
  const [models, setModels] = useState<ModelsEvent | null>(null);
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

  const handleOcrOnce = async () => {
    setErrorMsg(null);
    setReading(true);
    try {
      setOcr(await invoke<OcrResult>("ocr_once"));
    } catch (e) {
      setErrorMsg(t("ocrError", { message: String(e) }));
    } finally {
      setReading(false);
    }
  };

  const handleScriptChange = (next: OcrScript) => {
    setScript(next);
    void invoke("set_ocr_script", { script: next }).catch((e: unknown) =>
      setErrorMsg(t("ocrError", { message: String(e) })),
    );
  };

  const handleCopy = async () => {
    if (!ocr?.text) return;
    await copyText(ocr.text);
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1200);
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

  // Listen for live capture and OCR events
  useEffect(() => {
    if (view !== "frame") return;

    const unlisteners = [
      listen<CaptureEvent>(
        "capture://error",
        (event: TauriEvent<CaptureEvent>) => {
          const payload = event.payload;
          if (payload.type === "error") {
            setErrorMsg(t("captureError", { message: payload.message }));
          }
        },
      ),
      // Backend stops live capture by itself when the frame is hidden.
      listen("capture://stopped", () => setLive(false)),
      listen<OcrResult>("ocr://result", ({ payload }) => {
        setErrorMsg(null);
        setOcr(payload);
      }),
      listen<ModelsEvent>("ocr://models", ({ payload }) => setModels(payload)),
      listen<{ message: string }>("ocr://error", ({ payload }) =>
        setErrorMsg(t("ocrError", { message: payload.message })),
      ),
    ];

    return () => {
      for (const unlisten of unlisteners) void unlisten.then((stop) => stop());
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
        // The recognized text stays selectable; everything else drags.
        if (
          event.button === 0 &&
          !target.closest(".resize-zone") &&
          !target.closest("button") &&
          !target.closest("select") &&
          !target.closest(".ocr-text")
        ) {
          event.preventDefault();
          startMove();
        }
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
          onClick={handleOcrOnce}
          disabled={reading}
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
        <select
          className="script-select"
          title={t("script")}
          aria-label={t("script")}
          value={script}
          onChange={(event) =>
            handleScriptChange(event.target.value as OcrScript)
          }
        >
          {SCRIPTS.map((value) => (
            <option key={value} value={value}>
              {t(`script_${value}`)}
            </option>
          ))}
        </select>
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
        {models?.state === "downloading" && (
          <div className="models-progress" role="status">
            <span>
              {t("modelsDownloading", {
                progress: Math.round((models.progress ?? 0) * 100),
              })}
            </span>
            <progress max={1} value={models.progress ?? 0} />
          </div>
        )}
        {models?.state === "error" && (
          <span className="capture-error">
            {t("modelsError", { message: models.message ?? "" })}
          </span>
        )}
        {errorMsg && models?.state !== "error" && (
          <span className="capture-error">{errorMsg}</span>
        )}
        {!errorMsg && models?.state !== "downloading" && (
          <>
            <div className="ocr-text" aria-live="polite">
              {reading && !ocr && <span>{t("reading")}</span>}
              {!reading && !ocr && <span>{t("translationPlaceholder")}</span>}
              {ocr && ocr.lines.length === 0 && <span>{t("noText")}</span>}
              {ocr?.lines.map((line, index) => (
                <div key={index} dir={line.rtl ? "rtl" : "ltr"}>
                  {line.text}
                </div>
              ))}
            </div>
            {ocr && ocr.text && (
              <button
                className="copy-button"
                title={t("copy")}
                onClick={() => void handleCopy()}
              >
                {copied ? t("copied") : t("copy")}
              </button>
            )}
          </>
        )}
      </div>
    </main>
  );
}

export default App;
