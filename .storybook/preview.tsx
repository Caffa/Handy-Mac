import type { Preview } from "@storybook/react-vite";
import "../src/App.css";
import "../src/overlay/RecordingOverlay.css";

// Minimal i18n decorator for Storybook — avoids importing the full i18n setup
// which depends on Tauri APIs unavailable in Storybook.
import React from "react";
import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import enTranslation from "../src/i18n/locales/en/translation.json";

// Initialize a standalone i18n instance for Storybook
if (!i18n.isInitialized) {
  i18n.use(initReactI18next).init({
    resources: { en: { translation: enTranslation } },
    lng: "en",
    fallbackLng: "en",
    interpolation: { escapeValue: false },
    react: { useSuspense: false },
  });
}

const preview: Preview = {
  parameters: {
    controls: {
      matchers: {
        color: /(background|color)$/i,
        date: /Date$/i,
      },
    },
    a11y: {
      test: "todo",
    },
  },
  decorators: [
    (Story) => (
      <div style={{ padding: "1rem" }}>
        <Story />
      </div>
    ),
  ],
};

export default preview;