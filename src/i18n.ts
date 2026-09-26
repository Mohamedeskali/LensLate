import i18n from "i18next";
import { initReactI18next } from "react-i18next";

const systemLanguage = (): "ar" | "en" =>
  (navigator.languages[0] ?? navigator.language).toLowerCase().startsWith("ar")
    ? "ar"
    : "en";

void i18n.use(initReactI18next).init({
  lng: systemLanguage(),
  fallbackLng: "en",
  resources: {
    en: {
      translation: {
        toolbar: "Frame controls",
        language: "Change language",
        pause: "Pause translation",
        resume: "Resume translation",
        settings: "Settings",
        history: "History",
        close: "Close frame",
        translationPlaceholder: "Translation will appear here",
        capture: "Capture once",
        live: "Live capture",
        captureError: "Capture error: {message}",
      },
    },
    ar: {
      translation: {
        toolbar: "أدوات الإطار",
        language: "تغيير اللغة",
        pause: "إيقاف الترجمة مؤقتًا",
        resume: "استئناف الترجمة",
        settings: "الإعدادات",
        history: "السجل",
        close: "إغلاق الإطار",
        translationPlaceholder: "ستظهر الترجمة هنا",
        capture: "Capture once",
        live: "Live capture",
        captureError: "Capture error: {message}",
      },
    },
  },
  interpolation: { escapeValue: false },
});

export default i18n;
