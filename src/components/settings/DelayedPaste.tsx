import React from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { useSettings } from "../../hooks/useSettings";

interface DelayedPasteProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const DelayedPaste: React.FC<DelayedPasteProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();

    const enabled = getSetting("delayed_paste_enabled") ?? true;

    return (
      <ToggleSwitch
        checked={enabled}
        onChange={(enabled) => updateSetting("delayed_paste_enabled", enabled)}
        isUpdating={isUpdating("delayed_paste_enabled")}
        label={t("settings.advanced.delayedPaste.label")}
        description={t("settings.advanced.delayedPaste.description")}
        descriptionMode={descriptionMode}
        grouped={grouped}
      />
    );
  },
);
