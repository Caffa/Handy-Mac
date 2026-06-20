import React from "react";
import { useTranslation } from "react-i18next";
import { useSettings } from "../../hooks/useSettings";
import { SettingContainer } from "../ui/SettingContainer";

interface WordCorrectionModeSelectorProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const WordCorrectionModeSelector: React.FC<
  WordCorrectionModeSelectorProps
> = ({ descriptionMode = "tooltip", grouped = false }) => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();
  const currentMode = getSetting("word_correction_mode") || "WordBias";

  const modes = [
    { value: "WordBias", label: t("settings.debug.wordCorrectionMode.wordBias") },
    {
      value: "Pronunciation",
      label: t("settings.debug.wordCorrectionMode.pronunciation"),
    },
    {
      value: "Replacement",
      label: t("settings.debug.wordCorrectionMode.replacement"),
    },
  ] as const;

  const handleModeChange = (mode: (typeof modes)[number]["value"]) => {
    updateSetting("word_correction_mode", mode);
  };

  return (
    <SettingContainer
      title={t("settings.debug.wordCorrectionMode.label")}
      description={t("settings.debug.wordCorrectionMode.description")}
      descriptionMode={descriptionMode}
      grouped={grouped}
    >
      <div className="flex flex-col gap-2">
        {modes.map((mode) => (
          <label
            key={mode.value}
            className="flex items-center gap-2 cursor-pointer"
          >
            <input
              type="radio"
              name="word_correction_mode"
              value={mode.value}
              checked={currentMode === mode.value}
              onChange={() => handleModeChange(mode.value)}
              disabled={isUpdating("word_correction_mode")}
              className="w-4 h-4 text-primary focus:ring-primary border-mid-gray/30"
            />
            <span className="text-sm text-text-primary">{mode.label}</span>
          </label>
        ))}
      </div>
    </SettingContainer>
  );
};

WordCorrectionModeSelector.displayName = "WordCorrectionModeSelector";