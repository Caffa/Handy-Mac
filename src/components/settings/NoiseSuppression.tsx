import React from "react";
import { useTranslation } from "react-i18next";
import { useSettings } from "../../hooks/useSettings";
import { Dropdown } from "../ui/Dropdown";
import { SettingContainer } from "../ui/SettingContainer";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import type { NoiseSuppressionLevel as NoiseSuppressionLevelType } from "../../bindings";

// Valid noise suppression level values for runtime validation
const VALID_LEVELS: NoiseSuppressionLevelType[] = ["Low", "Medium", "High"];

// Validate and normalize noise suppression level value
const validateNoiseSuppressionLevel = (
  value: unknown,
): NoiseSuppressionLevelType => {
  if (
    typeof value === "string" &&
    VALID_LEVELS.includes(value as NoiseSuppressionLevelType)
  ) {
    return value as NoiseSuppressionLevelType;
  }
  return "Medium";
};

export const NoiseSuppression: React.FC = React.memo(() => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();

  const enabled = getSetting("noise_suppression_enabled") ?? false;
  const level = validateNoiseSuppressionLevel(
    getSetting("noise_suppression_level"),
  );

  const levelOptions = [
    {
      value: "Low",
      label: t("settings.advanced.noiseSuppression.low"),
      tooltip: t("settings.advanced.noiseSuppression.lowTooltip"),
    },
    {
      value: "Medium",
      label: t("settings.advanced.noiseSuppression.medium"),
      tooltip: t("settings.advanced.noiseSuppression.mediumTooltip"),
    },
    {
      value: "High",
      label: t("settings.advanced.noiseSuppression.high"),
      tooltip: t("settings.advanced.noiseSuppression.highTooltip"),
    },
  ];

  return (
    <div className="space-y-3">
      <ToggleSwitch
        checked={enabled}
        onChange={(checked) =>
          updateSetting("noise_suppression_enabled", checked)
        }
        disabled={isUpdating("noise_suppression_enabled")}
        isUpdating={isUpdating("noise_suppression_enabled")}
        label={t("settings.advanced.noiseSuppression.label")}
        description={t("settings.advanced.noiseSuppression.description")}
        descriptionMode="tooltip"
        grouped={true}
      />
      {enabled && (
        <SettingContainer
          title={t("settings.advanced.noiseSuppression.levelTitle")}
          description={t("settings.advanced.noiseSuppression.levelDescription")}
          descriptionMode="tooltip"
          grouped={true}
        >
          <Dropdown
            selectedValue={level}
            options={levelOptions}
            onSelect={(value) =>
              updateSetting(
                "noise_suppression_level",
                validateNoiseSuppressionLevel(value),
              )
            }
            disabled={isUpdating("noise_suppression_level")}
            placeholder={t(
              "settings.advanced.noiseSuppression.levelPlaceholder",
            )}
          />
        </SettingContainer>
      )}
    </div>
  );
});
