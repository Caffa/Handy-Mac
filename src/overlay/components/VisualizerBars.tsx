/**
 * VisualizerBars — Presentational component for audio level bars.
 *
 * Renders a set of vertical bars representing real-time audio levels.
 * Bars use CSS transitions for smooth animation and opacity for depth.
 *
 * Animation uses asymmetric cubic-bezier easing:
 * - Rise (bars going up): fast ease-out (100ms) — snappy response to speech
 * - Fall (bars going down): slower ease-in (180ms) — smooth decay, no abrupt collapse
 *
 * This follows the animation principle that exiting is slower than entering,
 * giving the visualizer a fluid, organic feel without losing responsiveness.
 *
 * Scope: Pure presentational — no state or side effects.
 */
import React, { useRef } from "react";

interface VisualizerBarsProps {
  levels: number[];
  isRouter: boolean;
}

// Cubic-bezier curves for organic motion
const RISE_EASE = "cubic-bezier(0.25, 0.46, 0.45, 0.94)"; // ease-out-quad: gentle deceleration
const FALL_EASE = "cubic-bezier(0.4, 0, 0.6, 1)"; // ease-in-out: smooth decay
const RISE_MS = 120; // gentle rise — not too sudden
const FALL_MS = 100; // responsive fall — feels natural

export function VisualizerBars({ levels, isRouter }: VisualizerBarsProps) {
  // Track previous heights to determine rise vs fall direction
  const prevHeightsRef = useRef<number[]>(Array(levels.length).fill(0));

  return (
    <div className="bars-container">
      {levels.map((v, i) => {
        const height = Math.min(35, 7 + Math.pow(v, 0.7) * 28);
        const prevHeight = prevHeightsRef.current[i] ?? 0;
        const isRising = height > prevHeight + 0.5; // threshold to avoid micro-jitter

        // Update tracked height (imperative, no re-render)
        prevHeightsRef.current[i] = height;

        const easing = isRising ? RISE_EASE : FALL_EASE;
        const duration = isRising ? RISE_MS : FALL_MS;

        return (
          <div
            key={i}
            className={`bar${isRouter ? " routing-bar" : ""}`}
            style={{
              height: `${height}px`,
              transition: `height ${duration}ms ${easing}, opacity 120ms ease-out`,
              opacity: Math.max(0.2, v * 1.7),
            }}
          />
        );
      })}
    </div>
  );
}
