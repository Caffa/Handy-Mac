/**
 * MicDeadWarning — Presentational component for mic health warnings.
 *
 * Shows a warning message when the microphone is dead or audio levels are too low.
 *
 * Scope: Pure presentational — no state or side effects.
 */
import React from "react";
import { useTranslation } from "react-i18next";

interface MicDeadWarningProps {
  micDeadWarning: boolean;
  lowAudioWarning: boolean;
}

export function MicDeadWarning({
  micDeadWarning,
  lowAudioWarning,
}: MicDeadWarningProps) {
  const { t } = useTranslation();

  if (!micDeadWarning && !lowAudioWarning) return null;

  return (
    <div className="mic-dead-warning">
      {micDeadWarning
        ? t("overlay.micDead")
        : t("overlay.lowAudio", "Low audio - check microphone")}
    </div>
  );
}