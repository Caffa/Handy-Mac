import React, { useMemo } from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { Slider } from "../ui/Slider";
import { Select, type SelectOption } from "../ui/Select";
import { SettingsGroup } from "../ui/SettingsGroup";
import { useSettings } from "../../hooks/useSettings";
import { useModelStore } from "../../stores/modelStore";

interface HybridModeProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const HybridMode: React.FC<HybridModeProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();
    const { models } = useModelStore();

    const enabled = getSetting("hybrid_mode_enabled") ?? false;
    const thresholdSecs = getSetting("hybrid_threshold_secs") ?? 20;
    const shortAudioModel = getSetting("hybrid_short_audio_model") ?? null;
    const longAudioModel = getSetting("hybrid_long_audio_model") ?? null;

    // Build model options from downloaded models
    const modelOptions: SelectOption[] = useMemo(() => {
      const downloaded = models.filter((m) => m.is_downloaded);
      return downloaded.map((m) => ({
        value: m.id,
        label: m.name,
      }));
    }, [models]);

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
          <div className="space-y-4 mt-4 ml-2 border-l-2 border-logo-primary/20 pl-4">
            <Slider
              value={thresholdSecs}
              onChange={(val) => updateSetting("hybrid_threshold_secs", val)}
              min={5}
              max={60}
              step={1}
              label={t("settings.advanced.hybridMode.thresholdLabel")}
              description={t("settings.advanced.hybridMode.thresholdDescription")}
              descriptionMode={descriptionMode}
              grouped={grouped}
              formatValue={(v) =>
                t("settings.advanced.hybridMode.thresholdValue", { seconds: v })
              }
            />

            <div className="space-y-2">
              <label className="text-sm font-medium text-text">
                {t("settings.advanced.hybridMode.shortAudioModelLabel")}
              </label>
              <p className="text-xs text-mid-gray">
                {t("settings.advanced.hybridMode.shortAudioModelDescription")}
              </p>
              <Select
                value={shortAudioModel}
                options={modelOptions}
                placeholder={t(
                  "settings.advanced.hybridMode.modelPlaceholder",
                )}
                onChange={(value) =>
                  updateSetting("hybrid_short_audio_model", value)
                }
              />
            </div>

            <div className="space-y-2">
              <label className="text-sm font-medium text-text">
                {t("settings.advanced.hybridMode.longAudioModelLabel")}
              </label>
              <p className="text-xs text-mid-gray">
                {t("settings.advanced.hybridMode.longAudioModelDescription")}
              </p>
              <Select
                value={longAudioModel}
                options={modelOptions}
                placeholder={t(
                  "settings.advanced.hybridMode.modelPlaceholder",
                )}
                onChange={(value) =>
                  updateSetting("hybrid_long_audio_model", value)
                }
              />
            </div>
          </div>
        )}
      </>
    );
  },
);

HybridMode.displayName = "HybridMode";