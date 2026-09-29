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
        captureError: "Capture error: {{message}}",
        ocrError: "OCR error: {{message}}",
        script: "Text script",
        script_auto: "Auto",
        script_latin: "Latin",
        script_arabic: "Arabic",
        copy: "Copy",
        copied: "Copied",
        reading: "Reading…",
        noText: "No text found",
        modelsDownloading: "Downloading OCR models… {{progress}}%",
        modelsError: "OCR models: {{message}}",
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
        capture: "التقاط مرة واحدة",
        live: "التقاط مباشر",
        captureError: "خطأ في الالتقاط: {{message}}",
        ocrError: "خطأ في التعرف على النص: {{message}}",
        script: "خط النص",
        script_auto: "تلقائي",
        script_latin: "لاتيني",
        script_arabic: "عربي",
        copy: "نسخ",
        copied: "تم النسخ",
        reading: "جارٍ القراءة…",
        noText: "لم يُعثر على نص",
        modelsDownloading: "جارٍ تنزيل نماذج التعرف على النص… {{progress}}%",
        modelsError: "نماذج التعرف على النص: {{message}}",
      },
    },
  },
  interpolation: { escapeValue: false },
});

export default i18n;
