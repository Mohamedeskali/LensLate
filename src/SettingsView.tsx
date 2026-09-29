import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { isRtlLang, languagesIncluding } from "./languages";
import {
  DISPLAY_MODES,
  ENGINE_NAMES,
  ENGINES,
  KEYED_ENGINES,
  type EngineId,
  type EngineSettings,
  type KeyStatus,
  type Settings,
} from "./types";

type Tab = "display" | "engines";

/** Engines in chain order first, then the unused ones. */
export function engineRows(order: EngineId[]): EngineId[] {
  return [...order, ...ENGINES.filter((id) => !order.includes(id))];
}

export function moveEngine(
  order: EngineId[],
  id: EngineId,
  delta: -1 | 1,
): EngineId[] {
  const i = order.indexOf(id);
  const j = i + delta;
  if (i < 0 || j < 0 || j >= order.length) return order;
  const next = [...order];
  [next[i], next[j]] = [next[j], next[i]];
  return next;
}

const MODEL_FIELD: Partial<Record<EngineId, keyof EngineSettings>> = {
  claude: "claudeModel",
  openai: "openaiModel",
  gemini: "geminiModel",
  ollama: "ollamaModel",
};

function TextSetting({
  label,
  value,
  onCommit,
  placeholder,
}: {
  label: string;
  value: string;
  onCommit: (value: string) => void;
  placeholder?: string;
}) {
  const [draft, setDraft] = useState(value);
  useEffect(() => setDraft(value), [value]);
  return (
    <label className="field">
      <span>{label}</span>
      <input
        type="text"
        value={draft}
        placeholder={placeholder}
        spellCheck={false}
        onChange={(e) => setDraft(e.target.value)}
        onBlur={() => draft !== value && onCommit(draft.trim())}
        onKeyDown={(e) => e.key === "Enter" && (e.target as HTMLElement).blur()}
      />
    </label>
  );
}

function KeySetting({
  engine,
  stored,
  onStatus,
}: {
  engine: EngineId;
  stored: boolean;
  onStatus: (status: KeyStatus, error?: string) => void;
}) {
  const { t } = useTranslation();
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);

  const run = async (command: string, args: Record<string, unknown>) => {
    setBusy(true);
    try {
      onStatus(await invoke<KeyStatus>(command, args));
      setDraft("");
    } catch (e) {
      onStatus({}, String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="key-row">
      <label className="field">
        <span>{t("apiKey")}</span>
        <input
          type="password"
          autoComplete="off"
          spellCheck={false}
          value={draft}
          placeholder={stored ? "••••••••" : ""}
          onChange={(e) => setDraft(e.target.value)}
        />
      </label>
      <button
        disabled={busy || draft.trim() === ""}
        onClick={() => void run("set_api_key", { engine, key: draft })}
      >
        {t("saveKey")}
      </button>
      <button
        disabled={busy || !stored}
        onClick={() => void run("delete_api_key", { engine })}
      >
        {t("removeKey")}
      </button>
      <span className={`key-status${stored ? " ok" : ""}`}>
        {stored ? `✓ ${t("keySaved")}` : t("noKey")}
      </span>
    </div>
  );
}

export default function SettingsView() {
  const { t } = useTranslation();
  const [tab, setTab] = useState<Tab>("display");
  const [settings, setSettings] = useState<Settings | null>(null);
  const [keys, setKeys] = useState<KeyStatus>({});
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    void invoke<Settings>("get_settings")
      .then(setSettings)
      .catch((e: unknown) => setError(String(e)));
    void invoke<KeyStatus>("api_key_status")
      .then(setKeys)
      .catch((e: unknown) => setError(String(e)));
    const unlisten = listen<Settings>("settings://changed", ({ payload }) =>
      setSettings(payload),
    );
    return () => void unlisten.then((stop) => stop());
  }, []);

  if (!settings) {
    return (
      <main className="utility">
        <h1>{t("settings")}</h1>
        {error && <p className="error">{error}</p>}
      </main>
    );
  }

  const save = (next: Settings) => {
    setSettings(next);
    setError(null);
    void invoke<Settings>("update_settings", { settings: next })
      .then(setSettings)
      .catch((e: unknown) => setError(String(e)));
  };
  const saveEngines = (patch: Partial<EngineSettings>) =>
    save({ ...settings, engines: { ...settings.engines, ...patch } });
  const order = settings.engines.order;

  return (
    <main className="utility settings">
      <h1>{t("settings")}</h1>
      <div className="tabs" role="tablist">
        {(["display", "engines"] as Tab[]).map((id) => (
          <button
            key={id}
            role="tab"
            aria-selected={tab === id}
            className={tab === id ? "active" : ""}
            onClick={() => setTab(id)}
          >
            {t(id === "display" ? "tabDisplay" : "tabEngines")}
          </button>
        ))}
      </div>
      {error && <p className="error">{error}</p>}

      {tab === "display" && (
        <section className="page">
          <fieldset>
            <legend>{t("displayMode")}</legend>
            {DISPLAY_MODES.map((mode) => (
              <label key={mode} className="radio">
                <input
                  type="radio"
                  name="mode"
                  checked={settings.displayMode === mode}
                  onChange={() => save({ ...settings, displayMode: mode })}
                />
                <span>
                  <strong>{t(`mode_${mode}`)}</strong>
                  <small>{t(`modeHelp_${mode}`)}</small>
                </span>
              </label>
            ))}
          </fieldset>

          <label className="field">
            <span>
              {t("fontSize")}: {settings.fontSize}px
            </span>
            <input
              type="range"
              min={10}
              max={32}
              value={settings.fontSize}
              onChange={(e) =>
                save({ ...settings, fontSize: Number(e.target.value) })
              }
            />
          </label>
          <div className="preview" style={{ fontSize: settings.fontSize }}>
            {t("previewText")}
          </div>

          <label className="field">
            <span>{t("targetLanguage")}</span>
            <select
              value={settings.targetLang}
              dir={isRtlLang(settings.targetLang) ? "rtl" : "ltr"}
              onChange={(e) =>
                save({ ...settings, targetLang: e.target.value })
              }
            >
              {languagesIncluding(settings.targetLang).map((l) => (
                <option key={l.code} value={l.code}>
                  {l.name}
                </option>
              ))}
            </select>
          </label>

          <label className="check">
            <input
              type="checkbox"
              checked={settings.showOriginal}
              onChange={(e) =>
                save({ ...settings, showOriginal: e.target.checked })
              }
            />
            {t("showOriginal")}
          </label>
        </section>
      )}

      {tab === "engines" && (
        <section className="page">
          <label className="field">
            <span>{t("primaryEngine")}</span>
            <select
              value={settings.engines.primary}
              onChange={(e) =>
                saveEngines({ primary: e.target.value as EngineId })
              }
            >
              {ENGINES.map((id) => (
                <option key={id} value={id}>
                  {ENGINE_NAMES[id]}
                </option>
              ))}
            </select>
          </label>
          <p className="help">
            {t("fallbackOrder")}: {t("fallbackHelp")}
          </p>

          <ol className="engine-list">
            {engineRows(order).map((id) => {
              const inChain = order.includes(id);
              const index = order.indexOf(id);
              const modelField = MODEL_FIELD[id];
              return (
                <li key={id} className={inChain ? "" : "unused"}>
                  <div className="engine-head">
                    <label className="check">
                      <input
                        type="checkbox"
                        checked={inChain}
                        title={t("useAsFallback")}
                        onChange={(e) =>
                          saveEngines({
                            order: e.target.checked
                              ? [...order, id]
                              : order.filter((x) => x !== id),
                          })
                        }
                      />
                      <strong>{ENGINE_NAMES[id]}</strong>
                    </label>
                    <small>{t(`engineHelp_${id}`)}</small>
                    <span className="spacer" />
                    <button
                      title={t("moveUp")}
                      aria-label={t("moveUp")}
                      disabled={!inChain || index === 0}
                      onClick={() =>
                        saveEngines({ order: moveEngine(order, id, -1) })
                      }
                    >
                      ↑
                    </button>
                    <button
                      title={t("moveDown")}
                      aria-label={t("moveDown")}
                      disabled={!inChain || index === order.length - 1}
                      onClick={() =>
                        saveEngines({ order: moveEngine(order, id, 1) })
                      }
                    >
                      ↓
                    </button>
                  </div>
                  {KEYED_ENGINES.includes(id) && (
                    <KeySetting
                      engine={id}
                      stored={!!keys[id]}
                      onStatus={(status, err) => {
                        if (err) setError(err);
                        else {
                          setError(null);
                          setKeys(status);
                        }
                      }}
                    />
                  )}
                  {modelField && (
                    <TextSetting
                      label={t("model")}
                      value={settings.engines[modelField] as string}
                      onCommit={(value) => saveEngines({ [modelField]: value })}
                    />
                  )}
                  {id === "ollama" && (
                    <TextSetting
                      label={t("ollamaUrl")}
                      value={settings.engines.ollamaUrl}
                      placeholder="http://localhost:11434"
                      onCommit={(value) => saveEngines({ ollamaUrl: value })}
                    />
                  )}
                  {id === "microsoft" && (
                    <TextSetting
                      label={t("region")}
                      value={settings.engines.microsoftRegion}
                      placeholder="westeurope"
                      onCommit={(value) =>
                        saveEngines({ microsoftRegion: value })
                      }
                    />
                  )}
                </li>
              );
            })}
          </ol>
        </section>
      )}
    </main>
  );
}
