import React from "react";
import { useTranslation } from "react-i18next";
import { useSettings } from "../../hooks/useSettings";
import { Dropdown } from "../ui/Dropdown";
import { SettingContainer } from "../ui/SettingContainer";
import type { VadSensitivity as VadSensitivityType } from "../../bindings";

// Valid VAD sensitivity values for runtime validation
const VALID_SENSITIVITIES: VadSensitivityType[] = [
  "very_quick",
  "quick",
  "balanced",
  "relaxed",
  "very_relaxed",
];

// Validate and normalize VAD sensitivity value
const validateVadSensitivity = (value: unknown): VadSensitivityType => {
  if (
    typeof value === "string" &&
    VALID_SENSITIVITIES.includes(value as VadSensitivityType)
  ) {
    return value as VadSensitivityType;
  }
  return "balanced";
};

interface VadSensitivityProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const VadSensitivity: React.FC<VadSensitivityProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();

    const vadSensitivity = validateVadSensitivity(getSetting("vad_sensitivity"));

    const options = [
      {
        value: "very_quick",
        label: t("settings.advanced.vadSensitivity.veryQuick"),
      },
      {
        value: "quick",
        label: t("settings.advanced.vadSensitivity.quick"),
      },
      {
        value: "balanced",
        label: t("settings.advanced.vadSensitivity.balanced"),
      },
      {
        value: "relaxed",
        label: t("settings.advanced.vadSensitivity.relaxed"),
      },
      {
        value: "very_relaxed",
        label: t("settings.advanced.vadSensitivity.veryRelaxed"),
      },
    ];

    return (
      <SettingContainer
        title={t("settings.advanced.vadSensitivity.title")}
        description={t("settings.advanced.vadSensitivity.description")}
        descriptionMode={descriptionMode}
        grouped={grouped}
      >
        <Dropdown
          selectedValue={vadSensitivity}
          options={options}
          onSelect={(value) =>
            updateSetting("vad_sensitivity", validateVadSensitivity(value))
          }
          disabled={isUpdating("vad_sensitivity")}
          placeholder={t("settings.advanced.vadSensitivity.placeholder")}
        />
      </SettingContainer>
    );
  },
);
