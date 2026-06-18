import React from "react";
import { useTranslation } from "react-i18next";
import { Slider } from "../../ui/Slider";
import { useSettings } from "../../../hooks/useSettings";

interface PreRecordingBufferProps {
  descriptionMode?: "tooltip" | "inline";
  grouped?: boolean;
}

export const PreRecordingBuffer: React.FC<PreRecordingBufferProps> = ({
  descriptionMode = "tooltip",
  grouped = false,
}) => {
  const { t } = useTranslation();
  const { settings, updateSetting } = useSettings();

  const handleBufferChange = (value: number) => {
    updateSetting("pre_recording_buffer_ms", value);
  };

  return (
    <Slider
      value={settings?.pre_recording_buffer_ms ?? 0}
      onChange={handleBufferChange}
      min={0}
      max={5000}
      step={100}
      label={t("settings.debug.preRecordingBuffer.title")}
      description={t("settings.debug.preRecordingBuffer.description")}
      descriptionMode={descriptionMode}
      grouped={grouped}
      formatValue={(v) => `${v}ms`}
    />
  );
};
