import React from "react";
import { useTranslation } from "react-i18next";
import { useSettings } from "../../hooks/useSettings";
import { Dropdown } from "../ui/Dropdown";
import { SettingContainer } from "../ui/SettingContainer";
import type { VadSensitivity as VadSensitivityType } from "../../bindings";

interface VadSensitivityProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const VadSensitivity: React.FC<VadSensitivityProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();

    const vadSensitivity: VadSensitivityType =
      (getSetting("vad_sensitivity") as VadSensitivityType) || "balanced";

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
            updateSetting("vad_sensitivity", value as VadSensitivityType)
          }
          disabled={isUpdating("vad_sensitivity")}
          placeholder={t("settings.advanced.vadSensitivity.placeholder")}
        />
      </SettingContainer>
    );
  },
);
