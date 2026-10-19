/**
 * LiveCaptionsBox — Presentational component for live transcription display.
 *
 * Renders streaming transcription text below the overlay pill.
 * Supports RTL direction and routing mode styling, and shows a "Live"
 * indicator while the backend is emitting stream-text events.
 *
 * Scope: Pure presentational — no state or side effects beyond i18n.
 */
import React from "react";
import { useTranslation } from "react-i18next";

interface LiveCaptionsBoxProps {
  /** Transcribed text; omitted when the box shows a loading notice. */
  text?: string;
  direction: "ltr" | "rtl";
  overlayScale: number;
  isRouter: boolean;
  streaming?: boolean;
  /** Optional notice (e.g. "Loading model...") rendered in place of text
   *  while the model is still loading and no transcription has arrived. */
  notice?: string;
}

export function LiveCaptionsBox({
  text,
  direction,
  overlayScale,
  isRouter,
  streaming = false,
  notice,
}: LiveCaptionsBoxProps) {
  const { t } = useTranslation();

  return (
    <div
      dir={direction}
      className={`live-captions-box fade-in ${isRouter ? "routing-mode" : ""}`}
      style={{ "--overlay-scale": overlayScale } as React.CSSProperties}
      role="status"
      aria-live="polite"
      aria-label="Live transcription"
    >
      {streaming && (
        <span
          className="live-captions-indicator"
          aria-label={t("overlay.liveIndicator")}
        >
          <span className="live-captions-dot" aria-hidden="true" />
          <span className="live-captions-label">{t("overlay.live")}</span>
        </span>
      )}
      {notice ? <span className="live-captions-loading">{notice}</span> : text}
    </div>
  );
}
