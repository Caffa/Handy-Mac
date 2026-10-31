/**
 * useWriteForMe — "Write for me" fast-path progress (generating / pasting).
 *
 * The router action's write-for-me fast path emits `write-progress` events:
 * - `{ phase: "generating" }` before the LLM call
 * - `{ phase: "pasting", ms: 500 }` right before pasting the result
 *
 * This hook listens for those events and drives a countdown for the paste
 * phase so the overlay can show what's happening instead of a stuck
 * "Processing..." label while the model generates.
 *
 * Scope: Write-for-me fast path display only.
 * Dependencies: @tauri-apps/api/event.
 * Side effects: Event listener for write-progress; countdown interval.
 */
import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useRef, useState } from "react";

export type WriteForMePhase = "generating" | "pasting";

export interface WriteProgressEvent {
  phase: WriteForMePhase;
  /** Countdown duration in milliseconds (pasting phase only). */
  ms?: number;
}

interface UseWriteForMeReturn {
  /** Current phase, or null when no write-progress is active. */
  phase: WriteForMePhase | null;
  /** Milliseconds remaining in the paste countdown (pasting phase only). */
  msRemaining: number;
  /** Clear the phase/countdown (called when a new recording starts). */
  reset: () => void;
}

export function useWriteForMe(): UseWriteForMeReturn {
  const [phase, setPhase] = useState<WriteForMePhase | null>(null);
  const [msRemaining, setMsRemaining] = useState(0);
  const intervalRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const clearCountdown = useCallback(() => {
    if (intervalRef.current) {
      clearInterval(intervalRef.current);
      intervalRef.current = null;
    }
  }, []);

  const reset = useCallback(() => {
    clearCountdown();
    setPhase(null);
    setMsRemaining(0);
  }, [clearCountdown]);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let disposed = false;

    const setup = async () => {
      unlisten = await listen<WriteProgressEvent>("write-progress", (event) => {
        const { phase: nextPhase, ms } = event.payload;
        clearCountdown();
        setPhase(nextPhase);

        if (nextPhase === "pasting" && typeof ms === "number" && ms > 0) {
          const deadline = Date.now() + ms;
          setMsRemaining(ms);
          intervalRef.current = setInterval(() => {
            const remaining = Math.max(0, deadline - Date.now());
            setMsRemaining(remaining);
            if (remaining <= 0) {
              clearCountdown();
            }
          }, 100);
        } else {
          setMsRemaining(0);
        }
      });

      if (disposed) {
        unlisten();
        unlisten = null;
      }
    };

    setup();

    return () => {
      disposed = true;
      unlisten?.();
      clearCountdown();
    };
  }, [clearCountdown]);

  return { phase, msRemaining, reset };
}
