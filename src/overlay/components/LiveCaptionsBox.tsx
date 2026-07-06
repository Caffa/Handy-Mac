/**
 * LiveCaptionsBox — Presentational component for live transcription display.
 *
 * Renders streaming transcription text below the overlay pill.
 * Supports RTL direction and routing mode styling.
 *
 * Scope: Pure presentational — no state or side effects.
 */
import React from "react";

interface LiveCaptionsBoxProps {
  text: string;
  direction: "ltr" | "rtl";
  overlayScale: number;
  isRouter: boolean;
}

export function LiveCaptionsBox({
  text,
  direction,
  overlayScale,
  isRouter,
}: LiveCaptionsBoxProps) {
  return (
    <div
      dir={direction}
      className={`live-captions-box fade-in ${isRouter ? "routing-mode" : ""}`}
      style={{ "--overlay-scale": overlayScale } as React.CSSProperties}
      role="status"
      aria-live="polite"
      aria-label="Live transcription"
    >
      {text}
    </div>
  );
}
