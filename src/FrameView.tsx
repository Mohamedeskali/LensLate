import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties } from "react";
import {
  currentMonitor,
  getCurrentWindow,
  LogicalSize,
  PhysicalPosition,
} from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { LazyStore } from "@tauri-apps/plugin-store";
import { useTranslation } from "react-i18next";
import {
  choosePlacement,
  computeLayout,
  CROP_OFFSET_PX,
  cropRectToCss,
  cropScale,
  cssBoxToCrop,
  fitFontSize,
  frameSizeFor,
  overlayItems,
  padRect,
  panelHeight,
  rgb,
  windowSizeFor,
  type Box,
  type Placement,
  type Size,
} from "./layout";
import { isRtlLang, languagesIncluding } from "./languages";
import {
  DISPLAY_MODES,
  ENGINE_NAMES,
  ENGINES,
  type DisplayMode,
  type EngineId,
  type ModelsEvent,
  type OcrResult,
  type OcrScript,
  type Rect,
  type Settings,
  type TranslateError,
  type TranslateResult,
} from "./types";

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
  | { type: "frame"; width: number; height: number; ms: number }
  | { type: "error"; message: string };

type SavedGeometry = {
  frameWidth?: number;
  frameHeight?: number;
  x?: number;
  y?: number;
};

const SCRIPTS: OcrScript[] = ["auto", "latin", "arabic"];
const MODE_ICONS: Record<DisplayMode, string> = {
  panel: "▭",
  overlay: "◩",
  side: "◨",
  original: "¶",
};
const FONT_STACK =
  'Inter, "Segoe UI", "Noto Sans", "Noto Sans Arabic", "Noto Naskh Arabic", system-ui, sans-serif';
const DEFAULT_FRAME: Size = { width: 480, height: 160 };
const geometryStore = new LazyStore("window-state.json");

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

/** Resolves after the next paint, so the screen shows the new DOM. */
function nextPaint() {
  return new Promise<void>((resolve) =>
    requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
  );
}

function normalize(text: string) {
  return text.toLowerCase().replace(/[^\p{L}\p{N}]/gu, "");
}

/** OCR of our own overlay (the Rust pipeline has the same guard). */
function isOwnRender(text: string, shown: string | undefined) {
  if (!shown) return false;
  const a = normalize(text);
  return a !== "" && a === normalize(shown);
}

function defaultPlacement(mode: DisplayMode): Placement {
  if (mode === "overlay") return "none";
  return mode === "side" ? "right" : "below";
}

function boxStyle(b: Box): CSSProperties {
  return { left: b.left, top: b.top, width: b.width, height: b.height };
}

let measureCtx: CanvasRenderingContext2D | null = null;
function measureText(line: string, size: number) {
  measureCtx ??= document.createElement("canvas").getContext("2d");
  if (!measureCtx) return line.length * size * 0.55;
  measureCtx.font = `${size}px ${FONT_STACK}`;
  return measureCtx.measureText(line).width;
}

function logError(message: string) {
  void invoke("log_frontend_error", { message });
}

export default function FrameView() {
  const { t } = useTranslation();
  const win = useMemo(() => getCurrentWindow(), []);

  const [settings, setSettings] = useState<Settings | null>(null);
  const [backend, setBackend] = useState<"wayland" | "xcap">("xcap");
  const [live, setLive] = useState(false);
  const [ocr, setOcr] = useState<OcrResult | null>(null);
  const [translation, setTranslation] = useState<TranslateResult | null>(null);
  const [translateError, setTranslateError] = useState<string | null>(null);
  const [reading, setReading] = useState(false);
  const [capturing, setCapturing] = useState(false);
  const [copied, setCopied] = useState<string | null>(null);
  const [script, setScript] = useState<OcrScript>("auto");
  const [models, setModels] = useState<ModelsEvent | null>(null);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
  const [frameNotFound, setFrameNotFound] = useState(false);
  const [overlayHidden, setOverlayHidden] = useState(false);
  const [viewport, setViewport] = useState<Size>({
    width: window.innerWidth,
    height: window.innerHeight,
  });
  const [placement, setPlacement] = useState<Placement>("below");
  const [geometryReady, setGeometryReady] = useState(false);

  const mode: DisplayMode = settings?.displayMode ?? "panel";
  const fontSize = settings?.fontSize ?? 15;
  const showOriginal = (settings?.showOriginal ?? false) && mode !== "original";
  const panelH = panelHeight(fontSize, showOriginal);
  const positionKnown = backend !== "wayland";

  // Frame size in CSS pixels (what the user resized), and the geometry the
  // window was last sized for, to tell our own resizes from the user's.
  const frameSizeRef = useRef<Size>(DEFAULT_FRAME);
  const appliedRef = useRef({
    mode: "panel" as DisplayMode,
    placement: "below" as Placement,
    panelH,
    size: { width: 0, height: 0 },
  });
  const placementRef = useRef(placement);
  placementRef.current = placement;
  const translationRef = useRef(translation);
  translationRef.current = translation;
  const liveRef = useRef(live);
  liveRef.current = live;
  const reportedRef = useRef("[]");

  const layout = computeLayout(mode, placement, viewport, panelH);

  // --- settings and backend -------------------------------------------------

  useEffect(() => {
    void invoke<Settings>("get_settings").then(setSettings).catch(logError);
    void invoke<"wayland" | "xcap">("capture_backend")
      .then(setBackend)
      .catch(logError);
    const unlisten = listen<Settings>("settings://changed", ({ payload }) =>
      setSettings(payload),
    );
    return () => void unlisten.then((stop) => stop());
  }, []);

  const updateSettings = useCallback(
    (patch: (s: Settings) => Settings) => {
      if (!settings) return;
      const next = patch(settings);
      setSettings(next);
      void invoke<Settings>("update_settings", { settings: next })
        .then(setSettings)
        .catch((e: unknown) => setErrorMsg(String(e)));
    },
    [settings],
  );

  // --- geometry -------------------------------------------------------------

  const persistGeometry = useCallback(async () => {
    const position = await win.outerPosition();
    await geometryStore.set("frame", {
      frameWidth: frameSizeRef.current.width,
      frameHeight: frameSizeRef.current.height,
      x: position.x,
      y: position.y,
    } satisfies SavedGeometry);
  }, [win]);

  // Restore the frame size and position once.
  useEffect(() => {
    let disposed = false;
    void (async () => {
      const saved = await geometryStore.get<SavedGeometry>("frame");
      const monitor = await currentMonitor();
      const maxW = monitor
        ? (monitor.size.width / monitor.scaleFactor) * 0.9
        : 1600;
      const maxH = monitor
        ? (monitor.size.height / monitor.scaleFactor) * 0.8
        : 900;
      frameSizeRef.current = {
        width: Math.min(maxW, Math.max(160, saved?.frameWidth ?? 480)),
        height: Math.min(maxH, Math.max(60, saved?.frameHeight ?? 160)),
      };
      if (saved?.x !== undefined && saved.y !== undefined) {
        try {
          await win.setPosition(new PhysicalPosition(saved.x, saved.y));
        } catch {
          // Wayland compositors reject application-controlled positioning.
        }
      }
      if (!disposed) setGeometryReady(true);
    })();
    return () => {
      disposed = true;
    };
  }, [win]);

  // Size the window for the mode; keep the frame where it is on screen when
  // the panel/bubble moves to its other side.
  useEffect(() => {
    if (!geometryReady) return;
    const prev = appliedRef.current;
    const frame = frameSizeRef.current;
    const size = windowSizeFor(mode, placement, frame, panelH);
    appliedRef.current = { mode, placement, panelH, size };
    void (async () => {
      try {
        if (positionKnown && prev.size.width > 0) {
          const before = computeLayout(
            prev.mode,
            prev.placement,
            prev.size,
            prev.panelH,
          ).frame;
          const after = computeLayout(mode, placement, size, panelH).frame;
          const dx = before.left - after.left;
          const dy = before.top - after.top;
          if (dx !== 0 || dy !== 0) {
            const [pos, scale] = await Promise.all([
              win.outerPosition(),
              win.scaleFactor(),
            ]);
            await win.setPosition(
              new PhysicalPosition(
                Math.round(pos.x + dx * scale),
                Math.round(pos.y + dy * scale),
              ),
            );
          }
        }
        await win.setSize(new LogicalSize(size.width, size.height));
      } catch (e) {
        logError(`resize frame window failed: ${String(e)}`);
      }
    })();
  }, [geometryReady, mode, placement, panelH, positionKnown, win]);

  // Decide where the result goes from the frame's place on its monitor.
  const evaluatePlacement = useCallback(async () => {
    const applied = appliedRef.current;
    if (applied.mode !== mode) return;
    const monitor = await currentMonitor();
    if (!monitor) return;
    const scale = monitor.scaleFactor;
    const pos = await win.outerPosition();
    const frameBox = computeLayout(
      applied.mode,
      applied.placement,
      applied.size,
      applied.panelH,
    ).frame;
    const next = choosePlacement(
      mode,
      panelH,
      {
        frame: {
          x: pos.x / scale + frameBox.left,
          y: pos.y / scale + frameBox.top,
          width: frameSizeRef.current.width,
          height: frameSizeRef.current.height,
        },
        monitor: {
          x: monitor.position.x / scale,
          y: monitor.position.y / scale,
          width: monitor.size.width / scale,
          height: monitor.size.height / scale,
        },
        positionKnown,
      },
      placementRef.current,
    );
    setPlacement(next);
  }, [mode, panelH, positionKnown, win]);

  useEffect(() => {
    setPlacement(defaultPlacement(mode));
  }, [mode]);

  useEffect(() => {
    if (!geometryReady) return;
    void evaluatePlacement().catch((e: unknown) => logError(String(e)));
  }, [geometryReady, evaluatePlacement]);

  useEffect(() => {
    const onResize = () => {
      const size = { width: window.innerWidth, height: window.innerHeight };
      setViewport(size);
      const applied = appliedRef.current;
      const ours =
        Math.abs(size.width - applied.size.width) <= 1 &&
        Math.abs(size.height - applied.size.height) <= 1;
      if (ours || applied.size.width === 0) return;
      // The user resized: the frame takes the change.
      frameSizeRef.current = frameSizeFor(
        applied.mode,
        applied.placement,
        size,
        applied.panelH,
      );
      appliedRef.current = { ...applied, size };
      void persistGeometry();
      void evaluatePlacement();
    };
    window.addEventListener("resize", onResize);
    const unlistenMove = win.onMoved(() => {
      void persistGeometry();
      void evaluatePlacement();
    });
    return () => {
      window.removeEventListener("resize", onResize);
      void unlistenMove.then((stop) => stop());
    };
  }, [win, persistGeometry, evaluatePlacement]);

  // --- capture, OCR and translation events ----------------------------------

  const acceptOcr = useCallback((result: OcrResult) => {
    // A read of our own overlay: keep the page's result.
    if (isOwnRender(result.text, translationRef.current?.translated)) return;
    setOcr(result);
    if (translationRef.current?.original === result.text) {
      setOverlayHidden(false);
    }
  }, []);

  useEffect(() => {
    const unlisteners = [
      listen<CaptureEvent>("capture://error", ({ payload }) => {
        if (payload.type !== "error") return;
        setFrameNotFound(payload.message === "Frame not found");
        setErrorMsg(t("captureError", { message: payload.message }));
      }),
      listen("capture://stopped", () => setLive(false)),
      listen<OcrResult>("ocr://result", ({ payload }) => {
        setErrorMsg(null);
        setFrameNotFound(false);
        acceptOcr(payload);
      }),
      listen<ModelsEvent>("ocr://models", ({ payload }) => setModels(payload)),
      listen<{ message: string }>("ocr://error", ({ payload }) =>
        setErrorMsg(t("ocrError", { message: payload.message })),
      ),
      listen<TranslateResult>("translate://result", ({ payload }) => {
        setTranslateError(null);
        setTranslation(payload);
        setOverlayHidden(false);
      }),
      listen<TranslateError>("translate://error", ({ payload }) =>
        setTranslateError(payload.message),
      ),
      // The page changed under the overlay: hide it so the next capture is clean.
      listen("overlay://suspend", () => setOverlayHidden(true)),
    ];
    return () => {
      for (const unlisten of unlisteners) void unlisten.then((stop) => stop());
    };
  }, [t, acceptOcr]);

  const translationFresh =
    !!translation && !!ocr && translation.original === ocr.text;

  // --- overlay --------------------------------------------------------------

  const dpr = window.devicePixelRatio || 1;
  const cropOffset = CROP_OFFSET_PX / dpr;
  const cropArea: Box = {
    left: layout.frame.left + cropOffset,
    top: layout.frame.top + cropOffset,
    width: Math.max(0, layout.frame.width - 2 * cropOffset),
    height: Math.max(0, layout.frame.height - 2 * cropOffset),
  };
  const cropSize: Size =
    ocr && ocr.width > 0
      ? { width: ocr.width, height: ocr.height }
      : { width: cropArea.width * dpr, height: cropArea.height * dpr };
  const scale = cropScale(cropSize, cropArea);
  const items =
    mode === "overlay" && ocr && translation && translationFresh
      ? overlayItems(ocr, translation.translated)
      : [];
  const showOverlay = items.length > 0 && !overlayHidden && !capturing;
  const insideBox =
    placement === "inside" && !capturing ? (layout.panel ?? layout.side) : null;

  // Tell the backend which parts of the captured area we cover, after they
  // are on screen, so it never reads them (see capture/overlay.rs).
  const boxesKey = JSON.stringify(
    (() => {
      const boxes: Rect[] = showOverlay
        ? items.map((item) => padRect(item.rect, 3, cropSize))
        : [];
      if (insideBox) {
        const rel: Box = {
          ...insideBox,
          left: insideBox.left - cropArea.left,
          top: insideBox.top - cropArea.top,
        };
        boxes.push(padRect(cssBoxToCrop(rel, scale), 3, cropSize));
      }
      return boxes;
    })(),
  );
  useEffect(() => {
    if (boxesKey === reportedRef.current) return;
    let cancelled = false;
    void nextPaint().then(() => {
      if (cancelled) return;
      reportedRef.current = boxesKey;
      void invoke("set_overlay_boxes", { boxes: JSON.parse(boxesKey) });
    });
    return () => {
      cancelled = true;
    };
  }, [boxesKey]);

  // --- actions --------------------------------------------------------------

  const handleOneShot = useCallback(async () => {
    setErrorMsg(null);
    setFrameNotFound(false);
    setReading(true);
    // Take our own drawing off the captured area first.
    if (reportedRef.current !== "[]") {
      setCapturing(true);
      await nextPaint();
      reportedRef.current = "[]";
      await invoke("set_overlay_boxes", { boxes: [] });
    }
    try {
      acceptOcr(await invoke<OcrResult>("ocr_once"));
    } catch (e) {
      const message = String(e);
      setFrameNotFound(message.includes("Frame not found"));
      setErrorMsg(t("ocrError", { message }));
    } finally {
      setCapturing(false);
      setReading(false);
    }
  }, [t, acceptOcr]);

  const handleLiveToggle = async () => {
    setErrorMsg(null);
    try {
      if (!live) {
        await invoke("live_start");
        setLive(true);
      } else {
        await invoke("live_stop");
        setLive(false);
      }
    } catch (e) {
      setErrorMsg(t("captureError", { message: String(e) }));
    }
  };

  const handleReselect = async () => {
    setErrorMsg(null);
    setFrameNotFound(false);
    const wasLive = live;
    try {
      if (wasLive) await invoke("live_stop");
      await invoke("capture_reselect");
      if (wasLive) {
        await invoke("live_start");
      } else {
        await handleOneShot();
      }
    } catch (e) {
      setErrorMsg(t("captureError", { message: String(e) }));
      setLive(false);
    }
  };

  const handleScriptChange = (next: OcrScript) => {
    setScript(next);
    void invoke("set_ocr_script", { script: next }).catch((e: unknown) =>
      setErrorMsg(t("ocrError", { message: String(e) })),
    );
  };

  const handleCopy = async (what: "original" | "translation") => {
    const text = what === "translation" ? translation?.translated : ocr?.text;
    if (!text) return;
    await copyText(text);
    setCopied(what);
    window.setTimeout(() => setCopied(null), 1200);
  };

  const cycleMode = () => {
    const next =
      DISPLAY_MODES[(DISPLAY_MODES.indexOf(mode) + 1) % DISPLAY_MODES.length];
    updateSettings((s) => ({ ...s, displayMode: next }));
  };

  // One-shot when the frame is shown with the hotkey / tray, and on Enter.
  useEffect(() => {
    const unlisten = listen("frame://shown", () => {
      if (!liveRef.current) window.setTimeout(() => void handleOneShot(), 400);
    });
    const onKey = (event: KeyboardEvent) => {
      if (
        event.key === "Enter" &&
        !(event.target instanceof HTMLSelectElement)
      ) {
        void handleOneShot();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("keydown", onKey);
      void unlisten.then((stop) => stop());
    };
  }, [handleOneShot]);

  // --- rendering ------------------------------------------------------------

  const startResize = (direction: ResizeDirection) => {
    void win.startResizeDragging(direction).catch((error: unknown) => {
      logError(`startResizeDragging(${direction}) failed: ${String(error)}`);
    });
  };

  const frame = layout.frame;
  const zones: [ResizeDirection, string, Box][] = [
    [
      "North",
      "ns",
      {
        left: frame.left + 10,
        top: frame.top - 3,
        width: frame.width - 20,
        height: 8,
      },
    ],
    [
      "South",
      "ns",
      {
        left: frame.left + 10,
        top: frame.top + frame.height - 5,
        width: frame.width - 20,
        height: 8,
      },
    ],
    [
      "West",
      "ew",
      {
        left: frame.left - 3,
        top: frame.top + 10,
        width: 8,
        height: frame.height - 20,
      },
    ],
    [
      "East",
      "ew",
      {
        left: frame.left + frame.width - 5,
        top: frame.top + 10,
        width: 8,
        height: frame.height - 20,
      },
    ],
    [
      "NorthWest",
      "nwse",
      { left: frame.left - 3, top: frame.top - 3, width: 13, height: 13 },
    ],
    [
      "SouthEast",
      "nwse",
      {
        left: frame.left + frame.width - 10,
        top: frame.top + frame.height - 10,
        width: 13,
        height: 13,
      },
    ],
    [
      "NorthEast",
      "nesw",
      {
        left: frame.left + frame.width - 10,
        top: frame.top - 3,
        width: 13,
        height: 13,
      },
    ],
    [
      "SouthWest",
      "nesw",
      {
        left: frame.left - 3,
        top: frame.top + frame.height - 10,
        width: 13,
        height: 13,
      },
    ],
  ];

  const target = settings?.targetLang ?? "en";
  const translationDir = isRtlLang(translation?.to ?? target) ? "rtl" : "ltr";
  const status =
    models?.state === "downloading"
      ? t("modelsDownloading", {
          progress: Math.round((models.progress ?? 0) * 100),
        })
      : models?.state === "error"
        ? t("modelsError", { message: models.message ?? "" })
        : errorMsg;
  const engineHint =
    translation &&
    `${ENGINE_NAMES[translation.engine] ?? translation.engine} · ${translation.ms} ms${
      translation.cached ? ` · ${t("cached")}` : ""
    }`;
  const failuresHint =
    translation && translation.failures.length > 0
      ? t("fellBack", { failures: translation.failures.join("; ") })
      : undefined;

  const originalBlock = ocr && ocr.lines.length > 0 && (
    <div className="original-text">
      {ocr.lines
        .filter((line) => line.text.trim() !== "")
        .map((line, index) => (
          <div key={index} dir={line.rtl ? "rtl" : "ltr"}>
            {line.text}
          </div>
        ))}
    </div>
  );

  const resultBody = (
    <div
      className="result-text"
      style={{ fontSize }}
      aria-live="polite"
      onMouseDown={(e) => e.stopPropagation()}
    >
      {status && <div className="status-line">{status}</div>}
      {models?.state === "downloading" && (
        <progress max={1} value={models.progress ?? 0} />
      )}
      {frameNotFound && backend === "wayland" && (
        <div className="status-line">
          {t("frameNotFoundHint")}{" "}
          <button className="link-button" onClick={() => void handleReselect()}>
            {t("chooseScreens")}
          </button>
        </div>
      )}
      {!ocr && !status && (
        <span className="placeholder">
          {reading ? t("reading") : t("translationPlaceholder")}
        </span>
      )}
      {ocr && ocr.lines.length === 0 && (
        <span className="placeholder">{t("noText")}</span>
      )}
      {(mode === "original" || showOriginal) && originalBlock}
      {mode !== "original" && ocr && ocr.lines.length > 0 && (
        <>
          {translation ? (
            <div
              className={`translated-text${translationFresh ? "" : " stale"}`}
              dir={translationDir}
            >
              {translation.translated}
            </div>
          ) : (
            !showOriginal && (
              <>
                {originalBlock}
                <span className="placeholder">{t("translating")}</span>
              </>
            )
          )}
          {translateError && (
            <div className="status-line">
              {t("translateError", { message: translateError })}
            </div>
          )}
        </>
      )}
    </div>
  );

  const footer = (
    <div className="result-footer" onMouseDown={(e) => e.stopPropagation()}>
      <span className="engine-hint" title={failuresHint}>
        {mode !== "original" && engineHint}
        {failuresHint && " ⚠"}
      </span>
      {mode !== "original" && (
        <label className="toggle" title={t("showOriginal")}>
          <input
            type="checkbox"
            checked={settings?.showOriginal ?? false}
            onChange={(e) =>
              updateSettings((s) => ({ ...s, showOriginal: e.target.checked }))
            }
          />
          {t("showOriginal")}
        </label>
      )}
      {ocr?.text && (
        <button onClick={() => void handleCopy("original")}>
          {copied === "original" ? t("copied") : t("copyOriginal")}
        </button>
      )}
      {mode !== "original" && translation?.translated && (
        <button
          className="primary"
          onClick={() => void handleCopy("translation")}
        >
          {copied === "translation" ? t("copied") : t("copyTranslation")}
        </button>
      )}
    </div>
  );

  return (
    <main
      className={`frame-shell mode-${mode} placement-${placement}`}
      style={{ fontFamily: FONT_STACK }}
      onMouseDown={(event) => {
        const el = event.target as HTMLElement;
        if (
          event.button === 0 &&
          !el.closest(".resize-zone") &&
          !el.closest("button, select, input, label, .result-text")
        ) {
          event.preventDefault();
          void win
            .startDragging()
            .catch((e: unknown) =>
              logError(`startDragging failed: ${String(e)}`),
            );
        }
      }}
    >
      <div
        className="frame-outline"
        style={boxStyle(frame)}
        aria-hidden="true"
      />
      {zones.map(([direction, cursor, box]) => (
        <div
          key={direction}
          className={`resize-zone resize-${cursor}`}
          style={boxStyle(box)}
          onMouseDown={(event) => {
            event.preventDefault();
            event.stopPropagation();
            startResize(direction);
          }}
        />
      ))}

      {/* Attached to the top edge of the frame, outside the captured area. */}
      <nav
        className="toolbar"
        style={boxStyle(layout.toolbar)}
        aria-label={t("toolbar")}
      >
        {!layout.panel && !layout.side && status && (
          <span className="toolbar-status" title={status}>
            {status}
          </span>
        )}
        {!layout.panel &&
          !layout.side &&
          frameNotFound &&
          backend === "wayland" && (
            <button
              className="link-button"
              onClick={() => void handleReselect()}
            >
              {t("chooseScreens")}
            </button>
          )}
        <div className="toolbar-tab" onMouseDown={(e) => e.stopPropagation()}>
          <button
            title={t("capture")}
            aria-label={t("capture")}
            onClick={() => void handleOneShot()}
            disabled={reading}
          >
            文
          </button>
          <button
            title={live ? t("pause") : t("live")}
            aria-label={live ? t("pause") : t("live")}
            onClick={() => void handleLiveToggle()}
            className={live ? "live-active" : ""}
          >
            {live ? "⏸" : "▶"}
          </button>
          <button
            title={`${t("displayMode")}: ${t(`mode_${mode}`)}`}
            aria-label={t("displayMode")}
            onClick={cycleMode}
          >
            {MODE_ICONS[mode]}
          </button>
          <select
            className="optional"
            title={t("targetLanguage")}
            aria-label={t("targetLanguage")}
            value={target}
            onChange={(e) =>
              updateSettings((s) => ({ ...s, targetLang: e.target.value }))
            }
          >
            {languagesIncluding(target).map((l) => (
              <option key={l.code} value={l.code}>
                {l.name}
              </option>
            ))}
          </select>
          <select
            className="optional-2"
            title={t("engine")}
            aria-label={t("engine")}
            value={settings?.engines.primary ?? "google_free"}
            onChange={(e) =>
              updateSettings((s) => ({
                ...s,
                engines: { ...s.engines, primary: e.target.value as EngineId },
              }))
            }
          >
            {ENGINES.map((id) => (
              <option key={id} value={id}>
                {ENGINE_NAMES[id]}
              </option>
            ))}
          </select>
          <select
            className="optional-2"
            title={t("script")}
            aria-label={t("script")}
            value={script}
            onChange={(e) => handleScriptChange(e.target.value as OcrScript)}
          >
            {SCRIPTS.map((value) => (
              <option key={value} value={value}>
                {t(`script_${value}`)}
              </option>
            ))}
          </select>
          {mode === "overlay" && translation?.translated && (
            <button
              className="optional"
              title={t("copyTranslation")}
              aria-label={t("copyTranslation")}
              onClick={() => void handleCopy("translation")}
            >
              {copied === "translation" ? "✓" : "⧉"}
            </button>
          )}
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
            onClick={() => void invoke("toggle_frame_command")}
          >
            ×
          </button>
        </div>
      </nav>

      {showOverlay && (
        <div
          className="overlay-layer"
          style={boxStyle(cropArea)}
          aria-live="polite"
        >
          {items.map((item, index) => {
            const css = cropRectToCss(padRect(item.rect, 2, cropSize), scale);
            const size = fitFontSize(
              item.text,
              { width: css.width - 4, height: css.height },
              measureText,
              fontSize * 2,
            );
            return (
              <div
                key={index}
                className="overlay-line"
                dir={translationDir}
                style={{
                  ...boxStyle(css),
                  fontSize: size,
                  background: item.colors ? rgb(item.colors.bg) : undefined,
                  color: item.colors ? rgb(item.colors.fg) : undefined,
                }}
              >
                {item.text}
              </div>
            );
          })}
        </div>
      )}

      {layout.panel && (
        <section
          className={`result-panel${capturing ? " hidden" : ""}`}
          style={boxStyle(layout.panel)}
        >
          {resultBody}
          {footer}
        </section>
      )}

      {layout.side && (
        <aside
          className={`side-bubble${capturing ? " hidden" : ""}`}
          style={boxStyle(layout.side)}
        >
          {resultBody}
          {footer}
        </aside>
      )}
    </main>
  );
}
