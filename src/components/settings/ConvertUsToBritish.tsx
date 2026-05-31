import React from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { useSettings } from "../../hooks/useSettings";

interface ConvertUsToBritishProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const ConvertUsToBritish: React.FC<ConvertUsToBritishProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();

    const convertUsToBritish = getSetting("convert_us_to_british") || false;

    return (
      <ToggleSwitch
        checked={convertUsToBritish}
        onChange={(enabled) => updateSetting("convert_us_to_british", enabled)}
        isUpdating={isUpdating("convert_us_to_british")}
        label={t("settings.advanced.convertUsToBritish.label")}
        description={t("settings.advanced.convertUsToBritish.description")}
        descriptionMode={descriptionMode}
        grouped={grouped}
      />
    );
  },
);