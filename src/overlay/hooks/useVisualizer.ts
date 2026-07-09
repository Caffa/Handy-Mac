/**
 * useVisualizer — Audio level visualization and mic health warnings.
 *
 * Manages:
 * - levels: Array of audio bar heights for the visualizer
 * - Mic dead/low audio detection: Sets micDeadWarning and lowAudioWarning
 *   in the parent (useOverlaySharedState) via provided setters.
 *
 * Includes smooth bar decay, cold-start boost, and low-audio detection history.
 *
 * Scope: Audio visualization and mic health detection.
 * Dependencies: React hooks, overlay state from useOverlaySharedState.
 * Side effects: Decay timer interval, mic dead timer interval, mic-level event listener.
 */
import { listen } from "@tauri-apps/api/event";
import { useEffect, useRef, useState } from "react";
import type { OverlayState } from "./useOverlayState";

/// If no mic-level event arrives within this many milliseconds,
/// start decaying the bars to zero to avoid a frozen visualizer.
const LEVEL_TIMEOUT_MS = 300;

/// Low audio detection thresholds
const LOW_AUDIO_THRESHOLD = 0.05;
const GOOD_AUDIO_THRESHOLD = 0.08;
const LOW_AUDIO_CHECK_SAMPLES = 10;
const LOW_AUDIO_MIN_RECORDING_MS = 1500;

interface UseVisualizerOptions {
  state: OverlayState;
  isVisible: boolean;
  /** During migration: backend-derived isRecording overrides state === "recording" */
  isRecording?: boolean;
  lastLevelTimeRef: React.MutableRefObject<number>;
  recordingStartTimeRef: React.MutableRefObject<number>;
  lowAudioHistoryRef: React.MutableRefObject<number[]>;
  hadGoodAudioRef: React.MutableRefObject<boolean>;
  smoothedLevelsRef: React.MutableRefObject<number[]>;
  setMicDeadWarning: React.Dispatch<React.SetStateAction<boolean>>;
  setLowAudioWarning: React.Dispatch<React.SetStateAction<boolean>>;
}

interface UseVisualizerReturn {
  levels: number[];
}

export function useVisualizer(
  options: UseVisualizerOptions,
): UseVisualizerReturn {
  const {
    state,
    isVisible,
    isRecording,
    lastLevelTimeRef,
    recordingStartTimeRef,
    lowAudioHistoryRef,
    hadGoodAudioRef,
    smoothedLevelsRef,
    setMicDeadWarning,
    setLowAudioWarning,
  } = options;

  // During migration: prefer backend-derived isRecording when provided,
  // otherwise fall back to state === "recording"
  const effectivelyRecording = isRecording ?? state === "recording";

  const [levels, setLevels] = useState<number[]>(Array(16).fill(0));

  // Clear visualizer bars immediately when state transitions away from recording.
  // This prevents the frozen bars bug: after cancel, the overlay may still
  // receive mic-level events briefly, but the visualizer should already be
  // fading to zero. Without this effect, bars can freeze at non-zero values
  // because the decay timer alone may not reach zero fast enough.
  useEffect(() => {
    if (!effectivelyRecording || !isVisible) {
      setLevels(Array(9).fill(0));
      // Also reset the smoothed levels ref so the next recording starts clean
      smoothedLevelsRef.current = Array(16).fill(0);
    }
  }, [effectivelyRecording, isVisible]);

  const decayTimerRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const micDeadTimerRef = useRef<ReturnType<typeof setInterval> | null>(null);

  // Decay timer: if we haven't received mic-level data for LEVEL_TIMEOUT_MS,
  // smoothly fade the bars toward zero so the overlay doesn't freeze.
  useEffect(() => {
    decayTimerRef.current = setInterval(() => {
      const elapsed = Date.now() - lastLevelTimeRef.current;
      if (elapsed > LEVEL_TIMEOUT_MS) {
        const decayFactor = Math.max(0.3, 1 - elapsed / 1000);
        setLevels((prev) => {
          const newLevels = prev.map((v) => v * decayFactor);
          return newLevels.map((v) => (v < 0.01 ? 0 : v));
        });
      }
    }, 60);

    return () => {
      if (decayTimerRef.current) {
        clearInterval(decayTimerRef.current);
      }
    };
  }, []);

  // "Mic dead" detection: warn if recording but no audio for >1s
  useEffect(() => {
    if (!effectivelyRecording || !isVisible) {
      setMicDeadWarning(false);
      if (micDeadTimerRef.current) {
        clearInterval(micDeadTimerRef.current);
        micDeadTimerRef.current = null;
      }
      return;
    }

    micDeadTimerRef.current = setInterval(() => {
      const elapsed = Date.now() - lastLevelTimeRef.current;
      setMicDeadWarning(elapsed > 1000);
    }, 200);

    return () => {
      if (micDeadTimerRef.current) {
        clearInterval(micDeadTimerRef.current);
      }
    };
  }, [effectivelyRecording, isVisible]);

  // Low audio level detection
  useEffect(() => {
    if (!effectivelyRecording || !isVisible) {
      setLowAudioWarning(false);
      lowAudioHistoryRef.current = [];
      return;
    }

    const elapsed = Date.now() - recordingStartTimeRef.current;
    if (elapsed < LOW_AUDIO_MIN_RECORDING_MS) {
      setLowAudioWarning(false);
      return;
    }

    if (hadGoodAudioRef.current) {
      setLowAudioWarning(false);
      return;
    }

    const history = lowAudioHistoryRef.current;
    if (history.length >= LOW_AUDIO_CHECK_SAMPLES) {
      const allBelowThreshold = history.every(
        (level) => level < LOW_AUDIO_THRESHOLD,
      );

      console.log(
        "[audio-level] history:",
        history.map((l) => l.toFixed(3)).join(", "),
        "| threshold:",
        LOW_AUDIO_THRESHOLD,
        "| allBelow:",
        allBelowThreshold,
        "| elapsed:",
        (elapsed / 1000).toFixed(1) + "s",
      );

      if (allBelowThreshold) {
        console.warn(
          "[audio-level] ⚠️ Low audio warning triggered - all samples below threshold",
        );
      }

      setLowAudioWarning(allBelowThreshold);
    } else {
      setLowAudioWarning(false);
    }
  }, [effectivelyRecording, isVisible, levels]);

  // Listen for mic-level updates
  useEffect(() => {
    let unlistenLevel: (() => void) | null = null;

    const setup = async () => {
      unlistenLevel = await listen<number[]>("mic-level", (event) => {
        lastLevelTimeRef.current = Date.now();
        const newLevels = event.payload as number[];

        // Apply minimal smoothing for responsiveness
        const smoothed = smoothedLevelsRef.current.map((prev, i) => {
          const target = newLevels[i] || 0;
          const delta = Math.abs(target - prev);
          const avgLevel = (prev + target) / 2;
          const isBarelyMoving = delta < 0.02 && avgLevel > 0.05;

          let alpha: number;
          if (prev < 0.05) {
            alpha = 0.8;
          } else if (isBarelyMoving) {
            alpha = 0.7;
          } else {
            alpha = 0.6;
          }

          return prev * (1 - alpha) + target * alpha;
        });

        smoothedLevelsRef.current = smoothed;
        setLevels(smoothed.slice(0, 9));

        // Track low audio levels during recording
        const maxLevel = Math.max(...newLevels);

        console.log(
          "[audio-level] maxLevel:",
          maxLevel.toFixed(3),
          "| goodThreshold:",
          GOOD_AUDIO_THRESHOLD,
          "| hadGoodAudio:",
          hadGoodAudioRef.current,
        );

        if (maxLevel >= GOOD_AUDIO_THRESHOLD) {
          hadGoodAudioRef.current = true;
          console.log(
            "[audio-level] ✓ Good audio detected, suppressing low-audio warning",
          );
        }

        lowAudioHistoryRef.current.push(maxLevel);
        if (lowAudioHistoryRef.current.length > LOW_AUDIO_CHECK_SAMPLES) {
          lowAudioHistoryRef.current.shift();
        }
      });
    };

    setup();

    return () => {
      unlistenLevel?.();
    };
  }, []);

  return { levels };
}
