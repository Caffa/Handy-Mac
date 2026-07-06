/**
 * VisualizerBars — Presentational component for audio level bars.
 *
 * Renders a set of vertical bars representing real-time audio levels.
 * Bars use CSS transitions for smooth animation and opacity for depth.
 *
 * Scope: Pure presentational — no state or side effects.
 */
import React from "react";

interface VisualizerBarsProps {
  levels: number[];
  isRouter: boolean;
}

export function VisualizerBars({ levels, isRouter }: VisualizerBarsProps) {
  return (
    <div className="bars-container">
      {levels.map((v, i) => (
        <div
          key={i}
          className={`bar${isRouter ? " routing-bar" : ""}`}
          style={{
            height: `${Math.min(35, 7 + Math.pow(v, 0.7) * 28)}px`,
            transition: "height 80ms linear, opacity 120ms ease-out",
            opacity: Math.max(0.2, v * 1.7),
          }}
        />
      ))}
    </div>
  );
}
