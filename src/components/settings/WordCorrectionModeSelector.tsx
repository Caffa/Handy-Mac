import React from "react";
import { useTranslation } from "react-i18next";
import { useSettings } from "../../hooks/useSettings";
import { SettingContainer } from "../ui/SettingContainer";
import { Dropdown, type DropdownOption } from "../ui/Dropdown";
import type { WordCorrectionMode } from "../../bindings";

interface WordCorrectionModeSelectorProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const WordCorrectionModeSelector: React.FC<
  WordCorrectionModeSelectorProps
> = ({ descriptionMode = "tooltip", grouped = false }) => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();
  const currentMode = getSetting("word_correction_mode") || "word_bias";

  const modes: DropdownOption[] = [
    {
      value: "word_bias",
      label: t("settings.debug.wordCorrectionMode.wordBias"),
    },
    {
      value: "pronunciation",
      label: t("settings.debug.wordCorrectionMode.pronunciation"),
    },
    {
      value: "replacement",
      label: t("settings.debug.wordCorrectionMode.replacement"),
    },
  ];

  const handleModeChange = (value: string) => {
    updateSetting("word_correction_mode", value as WordCorrectionMode);
  };

  return (
    <SettingContainer
      title={t("settings.debug.wordCorrectionMode.label")}
      description={t("settings.debug.wordCorrectionMode.description")}
      descriptionMode={descriptionMode}
      grouped={grouped}
    >
      <Dropdown
        options={modes}
        selectedValue={currentMode}
        onSelect={handleModeChange}
        disabled={isUpdating("word_correction_mode")}
      />
    </SettingContainer>
  );
};

WordCorrectionModeSelector.displayName = "WordCorrectionModeSelector";
