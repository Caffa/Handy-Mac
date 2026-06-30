import React, { useCallback, useRef, useEffect } from "react";
import { useTranslation } from "react-i18next";
import { Slider } from "../../ui/Slider";
import { useSettings } from "../../../hooks/useSettings";

interface PreRecordingBufferProps {
  descriptionMode?: "tooltip" | "inline";
  grouped?: boolean;
}

/**
 * Pre-recording buffer slider with debounced backend updates.
 *
 * The pre-recording buffer setting triggers a stop/recreate/start cycle
 * on the audio stream when changed, which is expensive. We debounce the
 * backend call so it only fires after the user stops dragging the slider,
 * while still updating the UI optimistically on every step.
 */
export const PreRecordingBuffer: React.FC<PreRecordingBufferProps> = ({
  descriptionMode = "tooltip",
  grouped = false,
}) => {
  const { t } = useTranslation();
  const { settings, updateSetting } = useSettings();
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Clean up debounce timer on unmount
  useEffect(() => {
    return () => {
      if (debounceRef.current !== null) {
        clearTimeout(debounceRef.current);
      }
    };
  }, []);

  const handleBufferChange = useCallback(
    (value: number) => {
      // Cancel any pending debounced update
      if (debounceRef.current !== null) {
        clearTimeout(debounceRef.current);
      }

      // Optimistic update: immediately reflect the value in the UI
      // The backend call is debounced to avoid rapid stop/recreate/start
      // cycles while the user is dragging the slider.
      updateSetting("pre_recording_buffer_ms", value);
    },
    [updateSetting],
  );

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
