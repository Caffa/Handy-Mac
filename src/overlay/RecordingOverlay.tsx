import { listen } from "@tauri-apps/api/event";
import React, { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  CancelIcon,
  MicrophoneIcon,
  RoutingIcon,
  TranscriptionIcon,
} from "../components/icons";
import "./RecordingOverlay.css";
import { commands } from "@/bindings";
import i18n, { syncLanguageFromSettings } from "@/i18n";
import { getLanguageDirection } from "@/lib/utils/rtl";

type OverlayState = "recording" | "transcribing" | "processing" | "usb-cycling";
type OverlayAction = "transcribe" | "post_process" | "router";

// If no mic-level event arrives within this many milliseconds,
// start decaying the bars to zero to avoid a frozen visualizer.
const LEVEL_TIMEOUT_MS = 500;

// Safety timeout for USB cycling state. If the Rust backend never
// emits a "finished" or "failed" event (e.g. event delivery failure,
// uhubctl hang, thread panic), the overlay will auto-recover after
// this duration instead of being stuck on "USB cycling…" forever.
// The backend blocks for up to ~9s (5s uhubctl + 4s settle), so
// 15s gives comfortable margin without hanging the UI for too long.
const USB_CYCLING_SAFETY_TIMEOUT_MS = 15_000;

/// Parse a compound payload of the form "state:action" emitted by the Rust
/// backend. Legacy payloads (no colon) are treated as state-only with action
/// defaulting to "transcribe".
function parseOverlayPayload(payload: string): {
  state: OverlayState;
  action: OverlayAction;
} {
  const colonIndex = payload.indexOf(":");
  if (colonIndex === -1) {
    return { state: payload as OverlayState, action: "transcribe" };
  }
  const statePart = payload.slice(0, colonIndex);
  const actionPart = payload.slice(colonIndex + 1);
  return {
    state: statePart as OverlayState,
    action: actionPart as OverlayAction,
  };
}

const RecordingOverlay: React.FC = () => {
  const { t } = useTranslation();
  const [isVisible, setIsVisible] = useState(false);
  const [state, setState] = useState<OverlayState>("recording");
  const [action, setAction] = useState<OverlayAction>("transcribe");
  const [levels, setLevels] = useState<number[]>(Array(16).fill(0));
  const smoothedLevelsRef = useRef<number[]>(Array(16).fill(0));
  const lastLevelTimeRef = useRef<number>(Date.now());
  const decayTimerRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const direction = getLanguageDirection(i18n.language);

  // Hybrid mode indicator state
  const [hybridEnabled, setHybridEnabled] = useState(false);
  const [hybridThresholdSecs, setHybridThresholdSecs] = useState(20);
  const [recordingElapsedSecs, setRecordingElapsedSecs] = useState(0);
  const recordingStartRef = useRef<number>(0);
  const elapsedTimerRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const [usbCycleStage, setUsbCycleStage] = useState<{
    stage: string;
    message: string;
  } | null>(null);

  const isRouter = action === "router";

  // Decay timer: if we haven't received mic-level data for LEVEL_TIMEOUT_MS,
  // smoothly fade the bars toward zero so the overlay doesn't freeze.
  useEffect(() => {
    decayTimerRef.current = setInterval(() => {
      const elapsed = Date.now() - lastLevelTimeRef.current;
      if (elapsed > LEVEL_TIMEOUT_MS) {
        // Exponential decay toward zero — faster the longer we've waited
        const decayFactor = Math.max(0.5, 1 - elapsed / 2000);
        setLevels((prev) => {
          const newLevels = prev.map((v) => v * decayFactor);
          // Snap to zero when very small
          return newLevels.map((v) => (v < 0.01 ? 0 : v));
        });
      }
    }, 80); // roughly matches the bar transition speed

    return () => {
      if (decayTimerRef.current) {
        clearInterval(decayTimerRef.current);
      }
    };
  }, []);

  // Fetch hybrid mode settings when overlay becomes visible
  useEffect(() => {
    if (!isVisible) return;
    const fetchHybridSettings = async () => {
      try {
        const result = await commands.getAppSettings();
        if (result.status === "ok" && result.data) {
          setHybridEnabled(result.data.hybrid_mode_enabled ?? false);
          setHybridThresholdSecs(result.data.hybrid_threshold_secs ?? 20);
        }
      } catch {
        // Silently ignore — indicator simply won't show
      }
    };
    fetchHybridSettings();
  }, [isVisible]);

  // Safety timeout: if we stay in "usb-cycling" state for too long,
  // fall back to "recording" so the overlay doesn't get stuck forever.
  // This handles cases where the Rust backend fails to emit the
  // usb-power-cycle-finished or usb-power-cycle-failed event.
  useEffect(() => {
    if (state !== "usb-cycling") return;

    const timer = setTimeout(() => {
      setState((prev) => {
        if (prev === "usb-cycling") {
          console.warn(
            "USB cycling safety timeout: no completion event received after %dms, recovering to recording",
            USB_CYCLING_SAFETY_TIMEOUT_MS,
          );
          return "recording";
        }
        return prev;
      });
    }, USB_CYCLING_SAFETY_TIMEOUT_MS);

    return () => clearTimeout(timer);
  }, [state]);

  // Track recording elapsed time for hybrid mode indicator
  useEffect(() => {
    if (state === "recording" && isVisible) {
      recordingStartRef.current = Date.now();
      setRecordingElapsedSecs(0);
      elapsedTimerRef.current = setInterval(() => {
        const elapsed = (Date.now() - recordingStartRef.current) / 1000;
        setRecordingElapsedSecs(elapsed);
      }, 200);
    } else {
      if (elapsedTimerRef.current) {
        clearInterval(elapsedTimerRef.current);
        elapsedTimerRef.current = null;
      }
      setRecordingElapsedSecs(0);
    }
    return () => {
      if (elapsedTimerRef.current) {
        clearInterval(elapsedTimerRef.current);
        elapsedTimerRef.current = null;
      }
    };
  }, [state, isVisible]);

  useEffect(() => {
    const setupEventListeners = async () => {
      // Listen for show-overlay event from Rust
      const unlistenShow = await listen("show-overlay", async (event) => {
        // Sync language from settings each time overlay is shown
        await syncLanguageFromSettings();
        const payload = event.payload as string;
        const parsed = parseOverlayPayload(payload);
        setState(parsed.state);
        setAction(parsed.action);
        setIsVisible(true);
      });

      // Listen for hide-overlay event from Rust
      const unlistenHide = await listen("hide-overlay", () => {
        // Functional update to avoid dependency on 'state'
        setState((current) => {
          if (current !== "usb-cycling") {
            setIsVisible(false);
          }
          return current;
        });
      });

      // Listen for mic-level updates
      const unlistenLevel = await listen<number[]>("mic-level", (event) => {
        lastLevelTimeRef.current = Date.now();
        const newLevels = event.payload as number[];

        // Apply smoothing to reduce jitter
        const smoothed = smoothedLevelsRef.current.map((prev, i) => {
          const target = newLevels[i] || 0;
          return prev * 0.7 + target * 0.3; // Smooth transition
        });

        smoothedLevelsRef.current = smoothed;
        setLevels(smoothed.slice(0, 9));
      });

      // Listen for USB power-cycle events from Rust
      const unlistenUsbCycleStart = await listen<string>(
        "usb-power-cycle-started",
        () => {
          // Only transition if we are currently recording (overlay is visible)
          // This shows the user that the USB device is being power-cycled.
          setState("usb-cycling");
        },
      );

      const unlistenUsbCycleFinished = await listen<string>(
        "usb-power-cycle-finished",
        () => {
          setUsbCycleStage(null);
          // Close and reopen the overlay to reinitialize the transcription
          // visualizer. This fixes the "mic not listening, volume bars
          // not moving" issue after USB cycling.
          setIsVisible(false);
          setState((prev) => (prev === "usb-cycling" ? "recording" : prev));
          // Reopen after a short delay to allow the backend microphone
          // stream to stabilize and the React state to reset.
          setTimeout(() => {
            setIsVisible(true);
          }, 50);
        },
      );

      const unlistenUsbCycleFailed = await listen<string>(
        "usb-power-cycle-failed",
        () => {
          setUsbCycleStage(null);
          setState((prev) => (prev === "usb-cycling" ? "recording" : prev));
          setIsVisible(false);
        },
      );

      const unlistenUsbCycleStage = await listen<{
        stage: string;
        message: string;
      }>("usb-power-cycle-stage", (event) => {
        setUsbCycleStage(event.payload);
        // Also ensure we are in usb-cycling state and visible — the stage
        // event may arrive before the usb-power-cycle-started event, or
        // the overlay might not have transitioned/shown yet.
        setState((prev) => {
          if (prev === "recording") {
            return "usb-cycling";
          }
          return prev;
        });
        setIsVisible(true);
      });

      // Cleanup function
      return () => {
        unlistenShow();
        unlistenHide();
        unlistenLevel();
        unlistenUsbCycleStart();
        unlistenUsbCycleFinished();
        unlistenUsbCycleFailed();
        unlistenUsbCycleStage();
      };
    };

    setupEventListeners();
  }, []);

  const getIcon = () => {
    if (isRouter) {
      // Paper-plane icon for routing actions (amber #f59e0b)
      const iconColor = "#f59e0b";
      if (state === "recording") {
        return <RoutingIcon color={iconColor} width={30} height={30} />;
      }
      return <RoutingIcon color={iconColor} width={30} height={30} />;
    }
    if (state === "recording") {
      return <MicrophoneIcon width={30} height={30} />;
    }
    return <TranscriptionIcon width={30} height={30} />;
  };

  const getOverlayClassNames = (): string => {
    const classes = ["recording-overlay"];
    if (isVisible) classes.push("fade-in");
    if (isRouter) classes.push("routing-mode");
    return classes.join(" ");
  };

  const handleCancel = useCallback(() => {
    commands.cancelOperation();
  }, []);

  return (
    <div dir={direction} className={getOverlayClassNames()}>
      <div className="overlay-left">{getIcon()}</div>

      <div className="overlay-middle">
        {state === "recording" && (
          <div className="bars-wrapper">
            {hybridEnabled && (
              <div
                className={`hybrid-indicator ${recordingElapsedSecs >= hybridThresholdSecs ? "hybrid-long" : "hybrid-short"}`}
              >
                {recordingElapsedSecs >= hybridThresholdSecs
                  ? t("overlay.hybridLong")
                  : t("overlay.hybridShort")}
              </div>
            )}
            <div className="bars-container">
              {levels.map((v, i) => (
                <div
                  key={i}
                  className={`bar${isRouter ? " routing-bar" : ""}`}
                  style={{
                    height: `${Math.min(30, 6 + Math.pow(v, 0.7) * 24)}px`,
                    transition: "height 80ms linear, opacity 120ms ease-out",
                    opacity: Math.max(0.2, v * 1.7),
                  }}
                />
              ))}
            </div>
          </div>
        )}
        {state === "transcribing" && (
          <div
            className={`transcribing-text${isRouter ? " routing-text" : ""}`}
          >
            {isRouter ? t("overlay.routing") : t("overlay.transcribing")}
          </div>
        )}
        {state === "processing" && (
          <div
            className={`transcribing-text${isRouter ? " routing-text" : ""}`}
          >
            {isRouter ? t("overlay.filing") : t("overlay.processing")}
          </div>
        )}
        {state === "usb-cycling" && (
          <div className="usb-cycling-container">
            <div className="usb-cycling-stage">
              {usbCycleStage
                ? usbCycleStage.message
                : t("overlay.usbCycling", "USB cycling…")}
            </div>
            {usbCycleStage && (
              <div className="usb-cycling-progress">
                {["resolving", "cycling", "waiting", "recovered"].map((s) => (
                  <div
                    key={s}
                    className={`usb-cycling-dot ${
                      ["resolving", "cycling", "waiting", "recovered"].indexOf(
                        usbCycleStage.stage,
                      ) >=
                      ["resolving", "cycling", "waiting", "recovered"].indexOf(
                        s,
                      )
                        ? "dot-active"
                        : ""
                    } ${usbCycleStage.stage === s ? "dot-current" : ""}`}
                  />
                ))}
              </div>
            )}
          </div>
        )}
      </div>

      <div className="overlay-right">
        {state === "recording" && (
          <div className="cancel-button" onClick={handleCancel}>
            <CancelIcon width={30} height={30} />
          </div>
        )}
      </div>
    </div>
  );
};

export default RecordingOverlay;
