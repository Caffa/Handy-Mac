import React from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { useSettings } from "../../hooks/useSettings";

interface AdaptiveThresholdsProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const AdaptiveThresholds: React.FC<AdaptiveThresholdsProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();

    const enabled = getSetting("adaptive_parakeet_thresholds") ?? false;

    return (
      <ToggleSwitch
        checked={enabled}
        onChange={(val) => updateSetting("adaptive_parakeet_thresholds", val)}
        isUpdating={isUpdating("adaptive_parakeet_thresholds")}
        label={t("settings.advanced.adaptiveThresholds.label")}
        description={t("settings.advanced.adaptiveThresholds.description")}
        descriptionMode={descriptionMode}
        grouped={grouped}
      />
    );
  },
);

AdaptiveThresholds.displayName = "AdaptiveThresholds";