import React from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { useSettings } from "../../hooks/useSettings";

interface VerificationModeProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const VerificationMode: React.FC<VerificationModeProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();

    const enabled = getSetting("verification_mode") ?? false;

    return (
      <ToggleSwitch
        checked={enabled}
        onChange={(val) => updateSetting("verification_mode", val)}
        isUpdating={isUpdating("verification_mode")}
        label={t("settings.advanced.verificationMode.label")}
        description={t("settings.advanced.verificationMode.description")}
        descriptionMode={descriptionMode}
        grouped={grouped}
      />
    );
  },
);

VerificationMode.displayName = "VerificationMode";