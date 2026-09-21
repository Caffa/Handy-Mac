/**
 * RecordingTimeoutWarning — Banner shown ~30s before a recording deadline.
 *
 * Displays the remaining seconds and an "Extend" button that adds 5 minutes
 * to both the silence window and the 1-hour hard cap. The banner replaces the
 * visualizer in the overlay middle so it remains readable and interactive.
 *
 * Scope: presentational component.
 * Dependencies: i18n.
 */
import React from "react";
import { useTranslation } from "react-i18next";

export interface RecordingTimeoutWarningProps {
  active: boolean;
  secondsRemaining: number;
  kind: "silence" | "hard_cap" | null;
  onExtend: () => void;
}

export const RecordingTimeoutWarning: React.FC<
  RecordingTimeoutWarningProps
> = ({ active, secondsRemaining, kind, onExtend }) => {
  const { t } = useTranslation();

  if (!active) return null;

  const messageKey =
    kind === "hard_cap"
      ? "overlay.timeoutWarningHardCap"
      : "overlay.timeoutWarning";

  return (
    <div className="recording-timeout-warning">
      <span className="recording-timeout-text">
        {t(messageKey, { seconds: secondsRemaining })}
      </span>
      <button
        className="recording-timeout-extend-button"
        onClick={onExtend}
        type="button"
        aria-label={t("overlay.extendButton", { minutes: 5 })}
      >
        {t("overlay.extendButton", { minutes: 5 })}
      </button>
    </div>
  );
};
