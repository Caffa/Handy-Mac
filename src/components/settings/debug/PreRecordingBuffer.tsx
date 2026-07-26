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

  // Refs to track the latest values for the unmount cleanup without
  // re-registering the effect on every render (which would cause the
  // cleanup to flush stale values prematurely).
  const latestValueRef = useRef<number | null>(null);
  const updateSettingRef = useRef(updateSetting);
  updateSettingRef.current = updateSetting;

  // Keep latestValueRef in sync with state + settings
  latestValueRef.current = localValue ?? settings?.pre_recording_buffer_ms ?? null;

  // Sync local state with settings when they change
  useEffect(() => {
    if (settings?.pre_recording_buffer_ms !== undefined) {
      setLocalValue(settings.pre_recording_buffer_ms);
    }
  }, [settings?.pre_recording_buffer_ms]);

  // Flush pending debounced update on unmount to prevent lost changes.
  // deps=[] ensures this only runs on unmount, NOT when localValue changes.
  // We read values from refs to avoid stale closures.
  useEffect(() => {
    return () => {
      if (debounceRef.current !== null) {
        clearTimeout(debounceRef.current);
        debounceRef.current = null;
        // Flush the last known value to ensure it's saved
        const currentValue = latestValueRef.current;
        if (currentValue != null) {
          updateSettingRef.current("pre_recording_buffer_ms", currentValue);
        }
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

      // Debounce the backend call to avoid rapid stop/recreate/start cycles.
      // Save immediately on first change, then debounce subsequent rapid changes.
      if (debounceRef.current === null) {
        // First change in this interaction — save immediately
        updateSetting("pre_recording_buffer_ms", value);
        debounceRef.current = setTimeout(() => {
          debounceRef.current = null;
        }, DEBOUNCE_MS);
      } else {
        // Rapid changes while dragging — debounce
        debounceRef.current = setTimeout(() => {
          updateSetting("pre_recording_buffer_ms", value);
          debounceRef.current = null;
        }, DEBOUNCE_MS);
      }
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
