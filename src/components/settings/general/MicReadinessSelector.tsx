import React from "react";
import { useTranslation } from "react-i18next";
import { SettingContainer } from "../../ui/SettingContainer";
import { useSettings } from "../../../hooks/useSettings";

type MicReadinessMode = "off" | "brief" | "always";

interface MicReadinessSelectorProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

const MODES: { value: MicReadinessMode; labelKey: string }[] = [
  { value: "off", labelKey: "settings.general.micReadiness.off" },
  { value: "brief", labelKey: "settings.general.micReadiness.brief" },
  { value: "always", labelKey: "settings.general.micReadiness.always" },
];

export const MicReadinessSelector: React.FC<MicReadinessSelectorProps> = ({
  descriptionMode = "tooltip",
  grouped = false,
}) => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();

  const alwaysOn = getSetting("always_on_microphone") ?? false;
  const lazyClose = getSetting("lazy_stream_close") ?? false;

  const currentMode: MicReadinessMode = alwaysOn
    ? "always"
    : lazyClose
      ? "brief"
      : "off";

  const updating =
    isUpdating("always_on_microphone") || isUpdating("lazy_stream_close");

  const handleModeChange = async (mode: MicReadinessMode) => {
    if (mode === currentMode || updating) return;

    if (mode === "off") {
      await updateSetting("always_on_microphone", false);
      await updateSetting("lazy_stream_close", false);
    } else if (mode === "brief") {
      await updateSetting("always_on_microphone", false);
      await updateSetting("lazy_stream_close", true);
    } else {
      await updateSetting("always_on_microphone", true);
      await updateSetting("lazy_stream_close", true);
    }
  };

  return (
    <SettingContainer
      title={t("settings.general.micReadiness.title")}
      description={t("settings.general.micReadiness.description")}
      descriptionMode={descriptionMode}
      grouped={grouped}
    >
      <div
        className={`flex rounded-lg border border-mid-gray/80 p-1 space-x-1 ${updating ? "opacity-50" : ""}`}
        role="radiogroup"
        aria-label={t("settings.general.micReadiness.title")}
      >
        {MODES.map((mode) => {
          const selected = mode.value === currentMode;
          return (
            <button
              key={mode.value}
              type="button"
              role="radio"
              aria-checked={selected}
              disabled={updating}
              onClick={() => handleModeChange(mode.value)}
              className={`flex-1 px-3 py-1.5 text-sm font-medium rounded-md transition-colors duration-150 disabled:cursor-not-allowed ${
                selected
                  ? "bg-logo-primary text-white"
                  : "text-text/80 hover:bg-mid-gray/10"
              }`}
            >
              {t(mode.labelKey)}
            </button>
          );
        })}
      </div>
    </SettingContainer>
  );
};
