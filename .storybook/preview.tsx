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

/**
 * Background colors matching the app's actual CSS:
 * - Light:   --color-background: #fbfbfb (App.css :root)
 * - Dark:    --color-background: #2c2b29 (App.css @media prefers-color-scheme: dark)
 * - Overlay: #000000cc            (RecordingOverlay.css .recording-overlay)
 */
const BACKGROUNDS = {
  values: [
    { name: "light", value: "#fbfbfb" },
    { name: "dark", value: "#2c2b29" },
    { name: "overlay", value: "#000000cc" },
  ],
  default: "light",
};

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
      context: "storybook",
      config: {
        rules: [
          // Disable the region rule — Storybook stories don't always
          // need landmark regions for isolated component testing
          { id: "region", enabled: false },
        ],
      },
    },
    backgrounds: BACKGROUNDS,
  },
  decorators: [
    // Decorator that wraps stories in a container and sets color-scheme
    // to match the selected background. Since this project uses
    // @media (prefers-color-scheme: dark) (not class-based), we apply
    // CSS color-scheme on the wrapper so Tailwind/CSS custom properties
    // resolve correctly for the chosen background.
    (Story, context) => {
      const bgName = context.globals?.backgrounds?.value;
      const isDark =
        bgName === "#2c2b29" || bgName === "#000000cc";
      const colorScheme = isDark ? "dark" : "light";

      return (
        <div
          style={{
            padding: "1rem",
            colorScheme,
            minHeight: "100%",
          }}
        >
          <Story />
        </div>
      );
    },
  ],
};

export default preview;