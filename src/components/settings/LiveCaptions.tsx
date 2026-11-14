import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { SettingContainer } from "../ui/SettingContainer";
import { Dropdown } from "../ui/Dropdown";
import type { DropdownOption } from "../ui/Dropdown";
import { useSettings } from "../../hooks/useSettings";
import { commands } from "@/bindings";
import type { ModelInfo } from "@/bindings";

interface LiveCaptionsProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

const MOONSHINE_SMALL_DEFAULT_ID =
  "handy-computer/moonshine-streaming-small-gguf/moonshine-streaming-small-Q8_0.gguf";

export const LiveCaptions: React.FC<LiveCaptionsProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating, refreshSettings } = useSettings();
    const enabled = getSetting("live_captions_enabled") ?? false;
    const currentModelId = getSetting("live_captions_model") as string | undefined;

    const [models, setModels] = useState<ModelInfo[]>([]);
    const [loadingModels, setLoadingModels] = useState(true);
    const [modelError, setModelError] = useState<string | null>(null);

    // Fetch streaming-capable models on mount
    useEffect(() => {
      let mounted = true;
      const fetchModels = async () => {
        try {
          const result = await commands.getAvailableModels();
          if (result.status === "ok" && mounted) {
            // Filter to streaming models only
            const streamingModels = result.data.filter(
              (m) => m.supports_streaming === true,
            );
            // Sort: downloaded first, then by size ascending
            streamingModels.sort((a, b) => {
              if (a.is_downloaded && !b.is_downloaded) return -1;
              if (!a.is_downloaded && b.is_downloaded) return 1;
              return a.size_mb - b.size_mb;
            });
            setModels(streamingModels);
          } else if (mounted && result.status === "error") {
            setModelError(result.error || "Failed to load models");
          }
        } catch (err) {
          if (mounted) {
            setModelError(String(err));
          }
        } finally {
          if (mounted) {
            setLoadingModels(false);
          }
        }
      };
      fetchModels();
      return () => {
        mounted = false;
      };
    }, []);

    const handleToggleChange = (checked: boolean) => {
      console.log(
        "[Live Captions] User toggled live captions in settings:",
        checked,
      );
      updateSetting("live_captions_enabled", checked);
    };

    const handleModelChange = async (modelId: string) => {
      const { toast } = await import("sonner");
      try {
        const result = await commands.changeLiveCaptionsModelSetting(modelId);
        if (result.status === "ok") {
          // Refresh settings to update local state
          await refreshSettings();
        } else {
          toast.error(t("errors.settingsLoadFailed"), {
            description: result.error,
          });
        }
      } catch (err) {
        toast.error(t("errors.settingsLoadFailed"), {
          description: String(err),
        });
      }
    };

    // Build dropdown options: "Same as main transcription model" first, then streaming models
    const modelOptions: DropdownOption[] = [
      {
        value: "",
        label: t("settings.advanced.liveCaptions.modelUseMainModel"),
        tooltip: t("settings.advanced.liveCaptions.modelUseMainModelDescription"),
      },
      ...models.map((m) => {
        let label = `${m.name} (${m.size_mb} MB)`;
        if (!m.is_downloaded) {
          label += ` ${t("settings.advanced.liveCaptions.notDownloaded")}`;
        }
        if (m.is_downloading) {
          label += ` ${t("settings.advanced.liveCaptions.downloading")}`;
        }
        return {
          value: m.id,
          label,
          tooltip: m.description,
        };
      }),
    ];

    // Determine selected value:
    // - "" (empty) → "Same as main transcription model" option
    // - matches a model option → that model
    // - non-empty but unknown → show Moonshine default as placeholder (preserves existing intent)
    const selectedValue =
      currentModelId !== undefined && currentModelId.length > 0
        ? modelOptions.some((opt) => opt.value === currentModelId)
          ? currentModelId
          : MOONSHINE_SMALL_DEFAULT_ID
        : "";

    const isModelUpdating = isUpdating("live_captions_model");

    return (
      <div className="space-y-2">
        <ToggleSwitch
          checked={enabled}
          onChange={handleToggleChange}
          isUpdating={isUpdating("live_captions_enabled")}
          label={t("settings.advanced.liveCaptions.label")}
          description={t("settings.advanced.liveCaptions.description")}
          descriptionMode={descriptionMode}
          grouped={grouped}
        />
        {enabled && (
          <SettingContainer
            title={t("settings.advanced.liveCaptions.modelLabel")}
            description={t("settings.advanced.liveCaptions.modelDescription")}
            descriptionMode={descriptionMode}
            grouped={grouped}
            layout="horizontal"
          >
            {loadingModels ? (
              <span className="text-sm text-mid-gray">{t("common.loading")}</span>
            ) : modelError ? (
              <span className="text-sm text-red-500">{modelError}</span>
            ) : modelOptions.length === 0 ? (
              <span className="text-sm text-mid-gray">
                {t("settings.advanced.liveCaptions.noStreamingModels")}
              </span>
            ) : (
              <Dropdown
                options={modelOptions}
                selectedValue={selectedValue}
                onSelect={handleModelChange}
                disabled={isModelUpdating}
                placeholder={t("settings.advanced.liveCaptions.modelPlaceholder")}
              />
            )}
          </SettingContainer>
        )}
      </div>
    );
  },
);
