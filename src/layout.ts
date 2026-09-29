// Geometry of the frame window. Everything lives in one transparent window:
// a toolbar strip attached to the top edge of the frame, the frame (marker
// border; its inside is what gets captured) and, depending on the display
// mode, a result panel below/above the frame or a bubble beside it. The
// window is resized so the frame keeps its size when the mode changes.
//
// All sizes here are CSS (logical) pixels unless named otherwise.

import type { DisplayMode, LineColors, OcrResult, Rect } from "./types";

export const TOOLBAR_H = 30;
export const SIDE_W = 260;
export const BORDER = 2;
export const FRAME_MIN = { width: 160, height: 60 };
/** Physical pixels between the frame's outer edge and the crop (border + inset). */
export const CROP_OFFSET_PX = 5;

export type Placement =
  "below" | "above" | "inside" | "right" | "left" | "none";
export type Size = { width: number; height: number };
export type Box = { left: number; top: number; width: number; height: number };

export type Layout = {
  toolbar: Box;
  frame: Box;
  panel: Box | null;
  side: Box | null;
};

/** Height of the result panel: a few lines of text plus its footer row. */
export function panelHeight(fontSize: number, showOriginal: boolean): number {
  const lines = showOriginal ? 6 : 4;
  return Math.round(fontSize * 1.45 * lines + 48);
}

function usesPanel(mode: DisplayMode) {
  return mode === "panel" || mode === "original";
}

/** Space the window needs besides the frame. */
export function chromeFor(
  mode: DisplayMode,
  placement: Placement,
  panelH: number,
): Size {
  let width = 0;
  let height = TOOLBAR_H;
  if (usesPanel(mode) && (placement === "below" || placement === "above")) {
    height += panelH;
  }
  if (mode === "side" && (placement === "right" || placement === "left")) {
    width += SIDE_W;
  }
  return { width, height };
}

export function windowSizeFor(
  mode: DisplayMode,
  placement: Placement,
  frame: Size,
  panelH: number,
): Size {
  const chrome = chromeFor(mode, placement, panelH);
  return {
    width: Math.max(frame.width, FRAME_MIN.width) + chrome.width,
    height: Math.max(frame.height, FRAME_MIN.height) + chrome.height,
  };
}

export function frameSizeFor(
  mode: DisplayMode,
  placement: Placement,
  window: Size,
  panelH: number,
): Size {
  const chrome = chromeFor(mode, placement, panelH);
  return {
    width: Math.max(0, window.width - chrome.width),
    height: Math.max(0, window.height - chrome.height),
  };
}

export function computeLayout(
  mode: DisplayMode,
  placement: Placement,
  window: Size,
  panelH: number,
): Layout {
  const frameSize = frameSizeFor(mode, placement, window, panelH);
  const side = mode === "side";
  const left = side && placement === "left" ? SIDE_W : 0;
  const width = frameSize.width;
  let top = 0;
  let panel: Box | null = null;
  if (usesPanel(mode) && placement === "above") {
    panel = { left, top, width, height: panelH };
    top += panelH;
  }
  const toolbar = { left, top, width, height: TOOLBAR_H };
  top += TOOLBAR_H;
  const frame = { left, top, width, height: frameSize.height };
  if (usesPanel(mode) && placement === "below") {
    panel = { left, top: frame.top + frame.height, width, height: panelH };
  }
  if (usesPanel(mode) && placement === "inside") {
    const height = Math.min(panelH, Math.round(frame.height * 0.6));
    panel = {
      left: left + BORDER,
      top: frame.top + frame.height - BORDER - height,
      width: width - 2 * BORDER,
      height,
    };
  }
  let sideBox: Box | null = null;
  if (side) {
    if (placement === "right") {
      sideBox = {
        left: width,
        top: frame.top,
        width: SIDE_W,
        height: frame.height,
      };
    } else if (placement === "left") {
      sideBox = {
        left: 0,
        top: frame.top,
        width: SIDE_W,
        height: frame.height,
      };
    } else {
      // No room beside the frame: a bubble in its top-right corner.
      const bubbleW = Math.min(SIDE_W, width - 2 * BORDER);
      sideBox = {
        left: left + width - BORDER - bubbleW,
        top: frame.top + BORDER,
        width: bubbleW,
        height: Math.min(frame.height - 2 * BORDER, 160),
      };
    }
  }
  return { toolbar, frame, panel, side: sideBox };
}

export type ScreenInfo = {
  /** Frame outer box on the desktop, logical pixels. */
  frame: { x: number; y: number; width: number; height: number };
  monitor: { x: number; y: number; width: number; height: number };
  /** False on Wayland, where a window cannot know its position. */
  positionKnown: boolean;
};

/**
 * Where the result goes. The panel sits below the frame, flips above when
 * the monitor's bottom edge is too close, and moves inside the frame when
 * neither fits. The current placement is kept while it still fits.
 */
export function choosePlacement(
  mode: DisplayMode,
  panelH: number,
  screen: ScreenInfo,
  current?: Placement,
): Placement {
  const { frame, monitor } = screen;
  if (mode === "overlay") return "none";
  if (mode === "side") {
    if (!screen.positionKnown) {
      return frame.width + SIDE_W <= monitor.width ? "right" : "inside";
    }
    const right = frame.x + frame.width + SIDE_W <= monitor.x + monitor.width;
    const left = frame.x - SIDE_W >= monitor.x;
    if (current === "left" && left) return "left";
    if (right) return "right";
    if (left) return "left";
    return "inside";
  }
  if (!screen.positionKnown) {
    return frame.height + TOOLBAR_H + panelH <= monitor.height
      ? "below"
      : "inside";
  }
  const below = frame.y + frame.height + panelH <= monitor.y + monitor.height;
  const above = frame.y - TOOLBAR_H - panelH >= monitor.y;
  if (current === "above" && above) return "above";
  if (below) return "below";
  if (above) return "above";
  return "inside";
}

/** Scale from crop pixels to CSS pixels of the area the crop shows. */
export function cropScale(crop: Size, area: Size) {
  return {
    sx: crop.width > 0 ? area.width / crop.width : 1,
    sy: crop.height > 0 ? area.height / crop.height : 1,
  };
}

export function cropRectToCss(r: Rect, s: { sx: number; sy: number }): Box {
  return {
    left: r.x * s.sx,
    top: r.y * s.sy,
    width: r.w * s.sx,
    height: r.h * s.sy,
  };
}

export function cssBoxToCrop(b: Box, s: { sx: number; sy: number }): Rect {
  const x = Math.floor(b.left / s.sx);
  const y = Math.floor(b.top / s.sy);
  return {
    x,
    y,
    w: Math.ceil((b.left + b.width) / s.sx) - x,
    h: Math.ceil((b.top + b.height) / s.sy) - y,
  };
}

export function padRect(r: Rect, pad: number, bounds: Size): Rect {
  const x = Math.max(0, r.x - pad);
  const y = Math.max(0, r.y - pad);
  return {
    x,
    y,
    w: Math.min(bounds.width, r.x + r.w + pad) - x,
    h: Math.min(bounds.height, r.y + r.h + pad) - y,
  };
}

export type OverlayItem = {
  rect: Rect;
  text: string;
  colors: LineColors | null;
};

/**
 * Pair translated lines with OCR line boxes. Engines keep line breaks, so
 * normally each OCR line gets its own translation; when the counts differ
 * the whole translation covers the union of the boxes.
 */
export function overlayItems(
  ocr: OcrResult,
  translated: string,
): OverlayItem[] {
  const lines = ocr.lines
    .map((line, index) => ({ line, colors: ocr.colors[index] ?? null }))
    .filter(({ line }) => line.text.trim() !== "");
  if (lines.length === 0 || translated.trim() === "") return [];
  const parts = translated
    .split("\n")
    .map((p) => p.trim())
    .filter((p) => p !== "");
  if (parts.length === lines.length) {
    return lines.map(({ line, colors }, i) => ({
      rect: line.rect,
      text: parts[i],
      colors,
    }));
  }
  const x0 = Math.min(...lines.map(({ line }) => line.rect.x));
  const y0 = Math.min(...lines.map(({ line }) => line.rect.y));
  const x1 = Math.max(...lines.map(({ line }) => line.rect.x + line.rect.w));
  const y1 = Math.max(...lines.map(({ line }) => line.rect.y + line.rect.h));
  return [
    {
      rect: { x: x0, y: y0, w: x1 - x0, h: y1 - y0 },
      text: parts.join("\n"),
      colors: lines[0].colors,
    },
  ];
}

/**
 * Largest font size (in `min..max`) at which `text` fits `box`; `measure`
 * returns the rendered width of one line at a size.
 */
export function fitFontSize(
  text: string,
  box: Size,
  measure: (line: string, size: number) => number,
  max: number,
  min = 8,
): number {
  const lines = text.split("\n");
  let size = Math.min(max, (box.height * 0.8) / lines.length);
  const widest = Math.max(...lines.map((l) => measure(l, size)));
  if (widest > box.width && widest > 0) size *= box.width / widest;
  return Math.max(min, Math.floor(size * 2) / 2);
}

export function rgb(c: [number, number, number]): string {
  return `rgb(${c[0]}, ${c[1]}, ${c[2]})`;
}
