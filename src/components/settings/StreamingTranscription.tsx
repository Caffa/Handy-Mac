import React from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { useSettings } from "../../hooks/useSettings";

interface StreamingTranscriptionProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const StreamingTranscription: React.FC<StreamingTranscriptionProps> =
  React.memo(({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();
    const enabled = getSetting("streaming_transcription_enabled") ?? false;

    return (
      <ToggleSwitch
        checked={enabled}
        onChange={(checked) =>
          updateSetting("streaming_transcription_enabled", checked)
        }
        isUpdating={isUpdating("streaming_transcription_enabled")}
        label={t("settings.advanced.streamingTranscription.label")}
        description={t("settings.advanced.streamingTranscription.description")}
        descriptionMode={descriptionMode}
        grouped={grouped}
      />
    );
  });