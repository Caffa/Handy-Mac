import React from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { useSettings } from "../../hooks/useSettings";

interface LiveCaptionsProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const LiveCaptions: React.FC<LiveCaptionsProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();
    const enabled = getSetting("live_captions_enabled") ?? true;

    const handleChange = (checked: boolean) => {
      console.log("[Live Captions] User toggled live captions in settings:", checked);
      updateSetting("live_captions_enabled", checked);
    };

    return (
      <ToggleSwitch
        checked={enabled}
        onChange={handleChange}
        isUpdating={isUpdating("live_captions_enabled")}
        label={t("settings.advanced.liveCaptions.label")}
        description={t("settings.advanced.liveCaptions.description")}
        descriptionMode={descriptionMode}
        grouped={grouped}
      />
    );
  },
);
