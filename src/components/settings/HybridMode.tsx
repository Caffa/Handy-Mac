import React from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { Slider } from "../ui/Slider";
import { useSettings } from "../../hooks/useSettings";

interface HybridModeProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const HybridMode: React.FC<HybridModeProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();

    const enabled = getSetting("hybrid_mode_enabled") ?? false;
    const thresholdSecs = getSetting("hybrid_threshold_secs") ?? 20;

    return (
      <>
        <ToggleSwitch
          checked={enabled}
          onChange={(val) => updateSetting("hybrid_mode_enabled", val)}
          isUpdating={isUpdating("hybrid_mode_enabled")}
          label={t("settings.advanced.hybridMode.label")}
          description={t("settings.advanced.hybridMode.description")}
          descriptionMode={descriptionMode}
          grouped={grouped}
        />

        {enabled && (
          <div className="mt-4 ml-2 border-l-2 border-logo-primary/20 pl-4">
            <div className="mb-4 px-3 py-2 rounded-lg bg-mid-gray/10 text-xs text-text/60">
              {t("settings.advanced.hybridMode.assignHint")}
            </div>
            <Slider
              value={thresholdSecs}
              onChange={(val) => updateSetting("hybrid_threshold_secs", val)}
              min={5}
              max={60}
              step={1}
              label={t("settings.advanced.hybridMode.thresholdLabel")}
              description={t(
                "settings.advanced.hybridMode.thresholdDescription",
              )}
              descriptionMode={descriptionMode}
              grouped={grouped}
              formatValue={(v) =>
                t("settings.advanced.hybridMode.thresholdValue", { seconds: v })
              }
            />
          </div>
        )}
      </>
    );
  },
);

HybridMode.displayName = "HybridMode";
