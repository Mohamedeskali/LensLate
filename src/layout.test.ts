import { describe, expect, it } from "vitest";
import {
  choosePlacement,
  computeLayout,
  cropRectToCss,
  cropScale,
  cssBoxToCrop,
  fitFontSize,
  frameSizeFor,
  overlayItems,
  padRect,
  panelHeight,
  SIDE_W,
  TOOLBAR_H,
  windowSizeFor,
  type ScreenInfo,
} from "./layout";
import { isRtlLang, languagesIncluding } from "./languages";
import type { OcrResult } from "./types";

const monitor = { x: 0, y: 0, width: 1920, height: 1080 };

function screen(
  x: number,
  y: number,
  width: number,
  height: number,
  positionKnown = true,
): ScreenInfo {
  return { frame: { x, y, width, height }, monitor, positionKnown };
}

describe("panel placement", () => {
  const panelH = 140;

  it("goes below when there is room", () => {
    expect(choosePlacement("panel", panelH, screen(100, 100, 600, 200))).toBe(
      "below",
    );
  });

  it("flips above near the bottom edge and inside when nothing fits", () => {
    expect(choosePlacement("panel", panelH, screen(100, 900, 600, 150))).toBe(
      "above",
    );
    expect(choosePlacement("panel", panelH, screen(0, 60, 600, 1000))).toBe(
      "inside",
    );
  });

  it("keeps the current side while it fits (no flapping)", () => {
    // Both fit: stays above if it already is there.
    expect(
      choosePlacement("panel", panelH, screen(100, 400, 600, 200), "above"),
    ).toBe("above");
    expect(choosePlacement("panel", panelH, screen(100, 400, 600, 200))).toBe(
      "below",
    );
  });

  it("works on a second monitor with an offset", () => {
    const second = {
      frame: { x: 2100, y: -300, width: 500, height: 200 },
      monitor: { x: 1920, y: -400, width: 1280, height: 720 },
      positionKnown: true,
    };
    expect(choosePlacement("panel", panelH, second)).toBe("below");
    second.frame.y = 100;
    expect(choosePlacement("panel", panelH, second)).toBe("above");
  });

  it("uses only the frame size when the position is unknown (Wayland)", () => {
    expect(
      choosePlacement("panel", panelH, screen(0, 0, 600, 200, false)),
    ).toBe("below");
    expect(
      choosePlacement("panel", panelH, screen(0, 0, 600, 1000, false)),
    ).toBe("inside");
  });

  it("side bubble goes right, then left, then inside", () => {
    expect(choosePlacement("side", panelH, screen(100, 100, 600, 200))).toBe(
      "right",
    );
    expect(choosePlacement("side", panelH, screen(1500, 100, 400, 200))).toBe(
      "left",
    );
    expect(choosePlacement("side", panelH, screen(100, 100, 1800, 200))).toBe(
      "inside",
    );
    expect(choosePlacement("overlay", panelH, screen(0, 0, 10, 10))).toBe(
      "none",
    );
  });
});

describe("window layout", () => {
  const panelH = panelHeight(15, false);

  it("panel height grows with font size and the original text", () => {
    expect(panelHeight(15, false)).toBe(135);
    expect(panelHeight(15, true)).toBeGreaterThan(panelHeight(15, false));
    expect(panelHeight(20, false)).toBeGreaterThan(panelHeight(15, false));
  });

  it("keeps the frame size when the mode changes", () => {
    const frame = { width: 500, height: 200 };
    for (const [mode, placement] of [
      ["panel", "below"],
      ["panel", "above"],
      ["panel", "inside"],
      ["overlay", "none"],
      ["side", "right"],
      ["side", "left"],
      ["original", "below"],
    ] as const) {
      const win = windowSizeFor(mode, placement, frame, panelH);
      expect(frameSizeFor(mode, placement, win, panelH)).toEqual(frame);
      const layout = computeLayout(mode, placement, win, panelH);
      expect(layout.frame.width).toBe(500);
      expect(layout.frame.height).toBe(200);
      // The toolbar is attached to the frame's top edge.
      expect(layout.toolbar.top + TOOLBAR_H).toBe(layout.frame.top);
      expect(layout.toolbar.left).toBe(layout.frame.left);
    }
  });

  it("puts the panel outside the frame below or above", () => {
    const win = windowSizeFor(
      "panel",
      "below",
      { width: 500, height: 200 },
      panelH,
    );
    const below = computeLayout("panel", "below", win, panelH);
    expect(below.panel).toEqual({
      left: 0,
      top: TOOLBAR_H + 200,
      width: 500,
      height: panelH,
    });
    const above = computeLayout("panel", "above", win, panelH);
    expect(above.panel?.top).toBe(0);
    expect(above.frame.top).toBe(panelH + TOOLBAR_H);
  });

  it("an inside panel stays within the frame border", () => {
    const win = windowSizeFor(
      "panel",
      "inside",
      { width: 500, height: 200 },
      panelH,
    );
    const l = computeLayout("panel", "inside", win, panelH);
    const p = l.panel!;
    expect(p.top).toBeGreaterThanOrEqual(l.frame.top);
    expect(p.top + p.height).toBeLessThanOrEqual(l.frame.top + l.frame.height);
    expect(p.height).toBeLessThanOrEqual(120);
  });

  it("side bubble sits beside the frame", () => {
    const frame = { width: 400, height: 150 };
    const right = computeLayout(
      "side",
      "right",
      windowSizeFor("side", "right", frame, panelH),
      panelH,
    );
    expect(right.side).toEqual({
      left: 400,
      top: TOOLBAR_H,
      width: SIDE_W,
      height: 150,
    });
    const left = computeLayout(
      "side",
      "left",
      windowSizeFor("side", "left", frame, panelH),
      panelH,
    );
    expect(left.frame.left).toBe(SIDE_W);
    expect(left.side?.left).toBe(0);
  });
});

describe("overlay", () => {
  const ocr: OcrResult = {
    text: "Hello world\nGood morning",
    ms: 10,
    script: "latin",
    width: 800,
    height: 200,
    lines: [
      {
        text: "Hello world",
        conf: 0.9,
        rtl: false,
        rect: { x: 10, y: 10, w: 300, h: 40 },
      },
      { text: " ", conf: 0.1, rtl: false, rect: { x: 0, y: 0, w: 1, h: 1 } },
      {
        text: "Good morning",
        conf: 0.9,
        rtl: false,
        rect: { x: 10, y: 80, w: 320, h: 40 },
      },
    ],
    colors: [
      { bg: [255, 255, 255], fg: [0, 0, 0] },
      { bg: [1, 1, 1], fg: [2, 2, 2] },
      { bg: [250, 250, 250], fg: [10, 10, 10] },
    ],
  };

  it("pairs each translated line with its box", () => {
    const items = overlayItems(ocr, "مرحبا بالعالم\nصباح الخير\n");
    expect(items).toHaveLength(2);
    expect(items[0]).toEqual({
      rect: { x: 10, y: 10, w: 300, h: 40 },
      text: "مرحبا بالعالم",
      colors: { bg: [255, 255, 255], fg: [0, 0, 0] },
    });
    expect(items[1].colors?.bg).toEqual([250, 250, 250]);
  });

  it("falls back to one block over all lines when counts differ", () => {
    const items = overlayItems(ocr, "Bonjour le monde, bonjour");
    expect(items).toEqual([
      {
        rect: { x: 10, y: 10, w: 320, h: 110 },
        text: "Bonjour le monde, bonjour",
        colors: { bg: [255, 255, 255], fg: [0, 0, 0] },
      },
    ]);
    expect(overlayItems({ ...ocr, lines: [] }, "x")).toEqual([]);
    expect(overlayItems(ocr, "  ")).toEqual([]);
  });

  it("maps crop pixels to CSS and back (HiDPI crop)", () => {
    // 800 crop pixels shown in 400 CSS pixels (scale 2).
    const s = cropScale(
      { width: 800, height: 200 },
      { width: 400, height: 100 },
    );
    expect(s).toEqual({ sx: 0.5, sy: 0.5 });
    const css = cropRectToCss({ x: 10, y: 20, w: 300, h: 40 }, s);
    expect(css).toEqual({ left: 5, top: 10, width: 150, height: 20 });
    expect(cssBoxToCrop(css, s)).toEqual({ x: 10, y: 20, w: 300, h: 40 });
    // A fractional CSS box is rounded outwards.
    expect(cssBoxToCrop({ left: 5.3, top: 0, width: 1, height: 1 }, s)).toEqual(
      {
        x: 10,
        y: 0,
        w: 3,
        h: 2,
      },
    );
  });

  it("pads boxes within the crop", () => {
    expect(
      padRect({ x: 1, y: 5, w: 10, h: 10 }, 3, { width: 12, height: 100 }),
    ).toEqual({
      x: 0,
      y: 2,
      w: 12,
      h: 16,
    });
  });

  it("fits the font to the box", () => {
    // A font whose glyphs are 0.5em wide.
    const measure = (line: string, size: number) => line.length * size * 0.5;
    expect(fitFontSize("short", { width: 500, height: 20 }, measure, 30)).toBe(
      16,
    );
    expect(
      fitFontSize("a".repeat(100), { width: 200, height: 40 }, measure, 30),
    ).toBe(8);
    expect(
      fitFontSize("a".repeat(40), { width: 200, height: 40 }, measure, 30),
    ).toBe(10);
    expect(
      fitFontSize("two\nlines", { width: 500, height: 40 }, measure, 30),
    ).toBe(16);
    expect(fitFontSize("big", { width: 500, height: 200 }, measure, 24)).toBe(
      24,
    );
  });
});

describe("languages", () => {
  it("knows right-to-left languages", () => {
    expect(isRtlLang("ar")).toBe(true);
    expect(isRtlLang("fa-IR")).toBe(true);
    expect(isRtlLang("en")).toBe(false);
    expect(isRtlLang(null)).toBe(false);
  });

  it("keeps an unknown stored target selectable", () => {
    expect(languagesIncluding("en").some((l) => l.code === "xx")).toBe(false);
    const list = languagesIncluding("xx");
    expect(list[list.length - 1]).toEqual({ code: "xx", name: "xx" });
  });
});
