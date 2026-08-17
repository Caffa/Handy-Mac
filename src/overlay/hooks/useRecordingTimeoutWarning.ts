/**
 * useRecordingTimeoutWarning — Listens for backend recording-timeout warnings
 * and drives the overlay countdown + Extend button.
 *
 * The backend emits `recording-timeout-warning` events starting ~30s before
 * the effective recording deadline (silence window or hard cap). This hook
 * keeps a local `secondsRemaining` counter that ticks down smoothly between
 * backend re-emits, and exposes `extend()` to call the backend command.
 *
 * Scope: timeout-warning UI state only.
 * Dependencies: @tauri-apps/api/event, @/bindings, @/i18n.
 * Side effects: event listener, interval tick.
 */
import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { commands } from "@/bindings";

/// Backend payload for `recording-timeout-warning`.
export interface RecordingTimeoutWarningEvent {
  seconds_remaining: number;
  kind: "silence" | "hard_cap";
  deadline_at_epoch_ms: number;
}

export interface UseRecordingTimeoutWarningReturn {
  active: boolean;
  secondsRemaining: number;
  kind: "silence" | "hard_cap" | null;
  extend: () => void;
  clear: () => void;
}

export function useRecordingTimeoutWarning(): UseRecordingTimeoutWarningReturn {
  const [active, setActive] = useState(false);
  const [secondsRemaining, setSecondsRemaining] = useState(0);
  const [kind, setKind] = useState<"silence" | "hard_cap" | null>(null);
  const deadlineRef = useRef<number | null>(null);
  const tickRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const clearTick = () => {
    if (tickRef.current) {
      clearInterval(tickRef.current);
      tickRef.current = null;
    }
  };

  const clear = () => {
    clearTick();
    deadlineRef.current = null;
    setActive(false);
    setSecondsRemaining(0);
    setKind(null);
  };

  const extend = () => {
    // Add 5 minutes (300 seconds) to both deadlines. The backend will
    // recompute the deadline and re-emit a warning if still within 30s.
    commands.extendRecording(300).catch((err) => {
      console.error("[RecordingTimeoutWarning] extend failed:", err);
    });
    // Hide the banner immediately; backend will re-arm if still close to deadline.
    clear();
  };

  useEffect(() => {
    let unlisten: (() => void) | null = null;

    const setup = async () => {
      unlisten = await listen<RecordingTimeoutWarningEvent>(
        "recording-timeout-warning",
        (event) => {
          const payload = event.payload;
          const newKind = payload.kind;
          const remaining = Math.max(0, payload.seconds_remaining);
          const deadline = payload.deadline_at_epoch_ms;

          deadlineRef.current = deadline;
          setKind(newKind);
          setSecondsRemaining(remaining);
          setActive(remaining > 0);

          // Reset the tick interval so it counts from the latest backend value.
          clearTick();
          if (remaining > 0) {
            tickRef.current = setInterval(() => {
              const now = Date.now();
              const deadlineMs = deadlineRef.current;
              if (deadlineMs == null) return;
              const next = Math.max(0, Math.ceil((deadlineMs - now) / 1000));
              setSecondsRemaining(next);
              if (next === 0) {
                clearTick();
                setActive(false);
              }
            }, 250);
          }
        },
      );
    };

    setup();

    return () => {
      clearTick();
      unlisten?.();
    };
  }, []);

  return {
    active,
    secondsRemaining,
    kind,
    extend,
    clear,
  };
}
