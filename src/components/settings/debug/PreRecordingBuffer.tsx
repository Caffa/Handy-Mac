import React, { useCallback, useRef, useEffect, useState } from "react";
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
 * backend call so it only fires after the user stops dragging the slider
 * for 500ms, while still updating the UI immediately on every step.
 */
const DEBOUNCE_MS = 500;

export const PreRecordingBuffer: React.FC<PreRecordingBufferProps> = ({
  descriptionMode = "tooltip",
  grouped = false,
}) => {
  const { t } = useTranslation();
  const { settings, updateSetting } = useSettings();
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Local state for immediate UI feedback during slider drag
  const [localValue, setLocalValue] = useState<number | null>(null);

  // Sync local state with settings when they change
  useEffect(() => {
    if (settings?.pre_recording_buffer_ms !== undefined) {
      setLocalValue(settings.pre_recording_buffer_ms);
    }
  }, [settings?.pre_recording_buffer_ms]);

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
      // Update local state immediately for responsive UI
      setLocalValue(value);

      // Cancel any pending debounced update
      if (debounceRef.current !== null) {
        clearTimeout(debounceRef.current);
      }

      // Debounce the backend call to avoid rapid stop/recreate/start cycles
      debounceRef.current = setTimeout(() => {
        updateSetting("pre_recording_buffer_ms", value);
        debounceRef.current = null;
      }, DEBOUNCE_MS);
    },
    [updateSetting],
  );

  return (
    <Slider
      value={localValue ?? settings?.pre_recording_buffer_ms ?? 0}
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
