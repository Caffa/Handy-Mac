/**
 * useModelLoadIndicator — Tracks in-flight model loads for the overlay.
 *
 * Recording kicks the model load asynchronously (the start path spawns the
 * load off the coordinator thread in actions/transcribe.rs), and
 * transcribe_internal waits on it at stop. On a cold start (first use, or
 * after the idle unload) the user can finish speaking before the load does;
 * without this state the overlay shows a stuck "Transcribing..." after stop
 * or an empty live preview during recording for the whole load (upstream
 * 20a6685e + 417dc6ac).
 *
 * The backend emits `model-state-changed` from
 * managers/transcription.rs::load_model_with_device. Only loading_completed
 * /loading_failed end a load: `unloaded` can arrive from the idle watcher or
 * the slot watchdog mid-load and is not ordered against an in-flight load.
 *
 * Scope: model-load indicator state only (no visibility ownership).
 * Dependencies: @tauri-apps/api/event.
 * Side effects: model-state-changed listener, slow-load timer.
 */
import { listen } from "@tauri-apps/api/event";
import { useEffect, useRef, useState } from "react";
import type { ModelStateEvent } from "@/lib/types/events";

/// Only call out a model load once it has run this long. Warm loads finish
/// well under this (~0.2s on Apple Silicon), so the common case never flashes
/// a loading notice while the user is speaking; cold loads can take far
/// longer (upstream 417dc6ac).
const SLOW_MODEL_LOAD_MS = 2000;

export interface UseModelLoadIndicatorReturn {
  /** A model load is in flight (drives the post-stop working label). */
  modelLoading: boolean;
  /** The in-flight load has been running longer than SLOW_MODEL_LOAD_MS
   *  (drives the live-preview loading notice while recording). */
  modelLoadSlow: boolean;
}

export function useModelLoadIndicator(): UseModelLoadIndicatorReturn {
  const [modelLoading, setModelLoading] = useState(false);
  const [modelLoadSlow, setModelLoadSlow] = useState(false);
  const slowTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    let unlisten: (() => void) | null = null;

    const clearSlowTimer = () => {
      if (slowTimerRef.current) {
        clearTimeout(slowTimerRef.current);
        slowTimerRef.current = null;
      }
    };

    const setup = async () => {
      unlisten = await listen<ModelStateEvent>(
        "model-state-changed",
        (event) => {
          const type = event.payload.event_type;
          if (type === "loading_started") {
            clearSlowTimer();
            setModelLoading(true);
            setModelLoadSlow(false);
            slowTimerRef.current = setTimeout(
              () => setModelLoadSlow(true),
              SLOW_MODEL_LOAD_MS,
            );
          } else if (
            type === "loading_completed" ||
            type === "loading_failed"
          ) {
            clearSlowTimer();
            setModelLoading(false);
            setModelLoadSlow(false);
          }
          // Other events (e.g. `unloaded` from the idle watcher) are not
          // ordered against an in-flight load and must not clear it.
        },
      );
    };

    setup();

    return () => {
      clearSlowTimer();
      unlisten?.();
    };
  }, []);

  return { modelLoading, modelLoadSlow };
}
