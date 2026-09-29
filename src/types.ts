// Payloads shared with the Rust side (src-tauri/src).

export type OcrScript = "auto" | "latin" | "arabic";

export type Rect = { x: number; y: number; w: number; h: number };

export type OcrLine = {
  text: string;
  conf: number;
  rect: Rect;
  rtl: boolean;
};

export type LineColors = {
  bg: [number, number, number];
  fg: [number, number, number];
};

/** `ocr://result` and the return value of `ocr_once`. */
export type OcrResult = {
  lines: OcrLine[];
  text: string;
  ms: number;
  script: OcrScript;
  /** Size of the recognized crop; line rects are in its pixels. */
  width: number;
  height: number;
  colors: LineColors[];
};

export type EngineId =
  | "google_free"
  | "microsoft"
  | "google_cloud"
  | "deepl"
  | "claude"
  | "openai"
  | "gemini"
  | "ollama";

export const ENGINES: EngineId[] = [
  "google_free",
  "microsoft",
  "google_cloud",
  "deepl",
  "claude",
  "openai",
  "gemini",
  "ollama",
];

/** Engines that need an API key in the keychain. */
export const KEYED_ENGINES: EngineId[] = [
  "microsoft",
  "google_cloud",
  "deepl",
  "claude",
  "openai",
  "gemini",
];

export const ENGINE_NAMES: Record<EngineId, string> = {
  google_free: "Google",
  microsoft: "Microsoft",
  google_cloud: "Google Cloud",
  deepl: "DeepL",
  claude: "Claude",
  openai: "OpenAI",
  gemini: "Gemini",
  ollama: "Ollama",
};

/** `translate://result` */
export type TranslateResult = {
  original: string;
  translated: string;
  from: string | null;
  to: string;
  engine: EngineId;
  ms: number;
  cached: boolean;
  failures: string[];
  rtl: boolean;
};

/** `translate://error` */
export type TranslateError = { original: string; message: string };

export type ModelsEvent = {
  state: "missing" | "downloading" | "ready" | "error";
  progress: number | null;
  message: string | null;
};

export type DisplayMode = "panel" | "overlay" | "side" | "original";

export const DISPLAY_MODES: DisplayMode[] = [
  "panel",
  "overlay",
  "side",
  "original",
];

export type EngineSettings = {
  primary: EngineId;
  order: EngineId[];
  claudeModel: string;
  openaiModel: string;
  geminiModel: string;
  ollamaModel: string;
  ollamaUrl: string;
  microsoftRegion: string;
};

export type Settings = {
  targetLang: string;
  displayMode: DisplayMode;
  fontSize: number;
  showOriginal: boolean;
  engines: EngineSettings;
};

export type KeyStatus = Partial<Record<EngineId, boolean>>;
