// Translation targets offered in the frame bar and settings, by native name.

export const LANGUAGES: { code: string; name: string }[] = [
  { code: "ar", name: "العربية" },
  { code: "bn", name: "বাংলা" },
  { code: "cs", name: "Čeština" },
  { code: "da", name: "Dansk" },
  { code: "de", name: "Deutsch" },
  { code: "el", name: "Ελληνικά" },
  { code: "en", name: "English" },
  { code: "es", name: "Español" },
  { code: "fa", name: "فارسی" },
  { code: "fi", name: "Suomi" },
  { code: "fr", name: "Français" },
  { code: "he", name: "עברית" },
  { code: "hi", name: "हिन्दी" },
  { code: "hu", name: "Magyar" },
  { code: "id", name: "Bahasa Indonesia" },
  { code: "it", name: "Italiano" },
  { code: "ja", name: "日本語" },
  { code: "ko", name: "한국어" },
  { code: "ms", name: "Bahasa Melayu" },
  { code: "nl", name: "Nederlands" },
  { code: "no", name: "Norsk" },
  { code: "pl", name: "Polski" },
  { code: "pt", name: "Português" },
  { code: "ro", name: "Română" },
  { code: "ru", name: "Русский" },
  { code: "sv", name: "Svenska" },
  { code: "th", name: "ไทย" },
  { code: "tr", name: "Türkçe" },
  { code: "uk", name: "Українська" },
  { code: "ur", name: "اردو" },
  { code: "vi", name: "Tiếng Việt" },
  { code: "zh-CN", name: "中文（简体）" },
  { code: "zh-TW", name: "中文（繁體）" },
];

const RTL = new Set(["ar", "fa", "he", "ur"]);

export function isRtlLang(code: string | null | undefined): boolean {
  return !!code && RTL.has(code.split(/[-_]/)[0].toLowerCase());
}

/** The list, with `code` added when it is not a known entry. */
export function languagesIncluding(code: string) {
  return LANGUAGES.some((l) => l.code === code) || !code
    ? LANGUAGES
    : [...LANGUAGES, { code, name: code }];
}
