/**
 * useUSBRecovery — USB power cycling state management.
 *
 * Manages USB power-cycle event handling and elapsed time tracking.
 * The usbCycleStage state is owned by the parent (useOverlayState) and
 * updated via setters — this ensures that resets from show-overlay events
 * propagate correctly.
 *
 * Scope: USB device power cycling recovery flow.
 * Dependencies: @tauri-apps/api/event.
 * Side effects: Event listeners for USB power-cycle events, safety timeout timer,
 *               elapsed time interval.
 */
import { listen } from "@tauri-apps/api/event";
import { useEffect, useRef, useState } from "react";
import type { OverlayState } from "./useOverlayState";

/// Safety timeout for USB cycling state (in milliseconds)
const USB_CYCLING_SAFETY_TIMEOUT_MS = 15_000;

interface UseUSBRecoveryOptions {
  state: OverlayState;
  setState: React.Dispatch<React.SetStateAction<OverlayState>>;
  setIsVisible: React.Dispatch<React.SetStateAction<boolean>>;
  /** During migration: backend-derived isUsbCycling overrides state === "usb-cycling" */
  isUsbCycling?: boolean;
  setMicDeadWarning: React.Dispatch<React.SetStateAction<boolean>>;
  setLowAudioWarning: React.Dispatch<React.SetStateAction<boolean>>;
  setUsbCycleStage: React.Dispatch<
    React.SetStateAction<{ stage: string; message: string } | null>
  >;
  usbCyclingActiveRef: React.MutableRefObject<boolean>;
  smoothedLevelsRef: React.MutableRefObject<number[]>;
  recordingStartTimeRef: React.MutableRefObject<number>;
  lowAudioHistoryRef: React.MutableRefObject<number[]>;
  hadGoodAudioRef: React.MutableRefObject<boolean>;
}

interface UseUSBRecoveryReturn {
  usbCyclingElapsed: number;
}

export function useUSBRecovery(
  options: UseUSBRecoveryOptions,
): UseUSBRecoveryReturn {
  const {
    state,
    setState,
    setIsVisible,
    isUsbCycling,
    setMicDeadWarning,
    setLowAudioWarning,
    setUsbCycleStage,
    usbCyclingActiveRef,
    smoothedLevelsRef,
    recordingStartTimeRef,
    lowAudioHistoryRef,
    hadGoodAudioRef,
  } = options;

  // During migration: prefer backend-derived isUsbCycling when provided,
  // otherwise fall back to state === "usb-cycling"
  const effectivelyUsbCycling = isUsbCycling ?? state === "usb-cycling";

  const [usbCyclingStartTime, setUsbCyclingStartTime] = useState<number | null>(
    null,
  );
  const [usbCyclingElapsed, setUsbCyclingElapsed] = useState(0);
  const usbCyclingTimerRef = useRef<ReturnType<typeof setInterval> | null>(
    null,
  );

  // Safety timeout: if we stay in "usb-cycling" for too long, fall back
  useEffect(() => {
    if (!effectivelyUsbCycling) return;

    const timer = setTimeout(() => {
      setState((prev) => {
        if (prev === "usb-cycling") {
          console.warn(
            "USB cycling safety timeout: no completion event received after %dms, recovering to recording",
            USB_CYCLING_SAFETY_TIMEOUT_MS,
          );
          usbCyclingActiveRef.current = false;
          return "recording";
        }
        return prev;
      });
    }, USB_CYCLING_SAFETY_TIMEOUT_MS);

    return () => clearTimeout(timer);
  }, [effectivelyUsbCycling]);

  // Track USB cycling elapsed time
  useEffect(() => {
    if (effectivelyUsbCycling) {
      setUsbCyclingStartTime(Date.now());
      usbCyclingTimerRef.current = setInterval(() => {
        setUsbCyclingStartTime((startTime) => {
          if (startTime) {
            setUsbCyclingElapsed(Math.floor((Date.now() - startTime) / 1000));
          }
          return startTime;
        });
      }, 100);
    } else {
      if (usbCyclingTimerRef.current) {
        clearInterval(usbCyclingTimerRef.current);
        usbCyclingTimerRef.current = null;
      }
      setUsbCyclingStartTime(null);
      setUsbCyclingElapsed(0);
    }
    return () => {
      if (usbCyclingTimerRef.current) {
        clearInterval(usbCyclingTimerRef.current);
      }
    };
  }, [effectivelyUsbCycling]);

  // USB power-cycle event listeners
  useEffect(() => {
    let unlistenUsbCycleStart: (() => void) | null = null;
    let unlistenUsbCycleFinished: (() => void) | null = null;
    let unlistenUsbCycleFailed: (() => void) | null = null;
    let unlistenUsbCycleStage: (() => void) | null = null;

    const setup = async () => {
      unlistenUsbCycleStart = await listen<string>(
        "usb-power-cycle-started",
        () => {
          usbCyclingActiveRef.current = true;
          setState("usb-cycling");
          setMicDeadWarning(false);
          setLowAudioWarning(false);
        },
      );

      unlistenUsbCycleFinished = await listen<string>(
        "usb-power-cycle-finished",
        () => {
          usbCyclingActiveRef.current = false;
          setUsbCycleStage(null);
          setMicDeadWarning(false);
          setLowAudioWarning(false);
          // Reset all audio level tracking state
          smoothedLevelsRef.current = Array(16).fill(0);
          lowAudioHistoryRef.current = [];
          recordingStartTimeRef.current = Date.now();
          hadGoodAudioRef.current = false;
          setIsVisible(false);
          setState((prev) => (prev === "usb-cycling" ? "recording" : prev));
          setTimeout(() => {
            setIsVisible(true);
          }, 50);
        },
      );

      unlistenUsbCycleFailed = await listen<string>(
        "usb-power-cycle-failed",
        () => {
          usbCyclingActiveRef.current = false;
          setUsbCycleStage(null);
          setMicDeadWarning(false);
          setLowAudioWarning(false);
          smoothedLevelsRef.current = Array(16).fill(0);
          lowAudioHistoryRef.current = [];
          recordingStartTimeRef.current = Date.now();
          hadGoodAudioRef.current = false;
          setState((prev) => (prev === "usb-cycling" ? "recording" : prev));
          setIsVisible(false);
        },
      );

      unlistenUsbCycleStage = await listen<{
        stage: string;
        message: string;
      }>("usb-power-cycle-stage", (event) => {
        if (!usbCyclingActiveRef.current) {
          console.log(
            "[usb-cycling] Ignoring stale stage event:",
            event.payload.stage,
          );
          return;
        }
        setUsbCycleStage(event.payload);
        setState((prev) => {
          if (prev === "recording") {
            return "usb-cycling";
          }
          return prev;
        });
        setIsVisible(true);
        setMicDeadWarning(false);
      });
    };

    setup();

    return () => {
      unlistenUsbCycleStart?.();
      unlistenUsbCycleFinished?.();
      unlistenUsbCycleFailed?.();
      unlistenUsbCycleStage?.();
    };
  }, []);

  return { usbCyclingElapsed };
}
