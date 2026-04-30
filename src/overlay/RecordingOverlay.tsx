import { listen } from "@tauri-apps/api/event";
import React, { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  MicrophoneIcon,
  TranscriptionIcon,
  CancelIcon,
} from "../components/icons";
import "./RecordingOverlay.css";
import { commands } from "@/bindings";
import i18n, { syncLanguageFromSettings } from "@/i18n";
import { getLanguageDirection } from "@/lib/utils/rtl";

type OverlayState = "recording" | "transcribing" | "processing" | "usb-cycling";

// If no mic-level event arrives within this many milliseconds,
// start decaying the bars to zero to avoid a frozen visualizer.
const LEVEL_TIMEOUT_MS = 500;

const RecordingOverlay: React.FC = () => {
  const { t } = useTranslation();
  const [isVisible, setIsVisible] = useState(false);
  const [state, setState] = useState<OverlayState>("recording");
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
        const overlayState = event.payload as OverlayState;
        setState(overlayState);
        setIsVisible(true);
      });

      // Listen for hide-overlay event from Rust
      const unlistenHide = await listen("hide-overlay", () => {
        setIsVisible(false);
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
          // Return to recording state if we were cycling
          setState((prev) => (prev === "usb-cycling" ? "recording" : prev));
        },
      );

      const unlistenUsbCycleFailed = await listen<string>(
        "usb-power-cycle-failed",
        () => {
          // Return to recording state (the retry may still fail, which will
          // trigger hide-overlay from the Rust side).
          setState((prev) => (prev === "usb-cycling" ? "recording" : prev));
        },
      );

      // Cleanup function
      return () => {
        unlistenShow();
        unlistenHide();
        unlistenLevel();
        unlistenUsbCycleStart();
        unlistenUsbCycleFinished();
        unlistenUsbCycleFailed();
      };
    };

    setupEventListeners();
  }, []);

  const getIcon = () => {
    if (state === "recording") {
      return <MicrophoneIcon />;
    } else {
      return <TranscriptionIcon />;
    }
  };

  const handleCancel = useCallback(() => {
    commands.cancelOperation();
  }, []);

  return (
    <div
      dir={direction}
      className={`recording-overlay ${isVisible ? "fade-in" : ""}`}
    >
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
                  className="bar"
                  style={{
                    height: `${Math.min(20, 4 + Math.pow(v, 0.7) * 16)}px`,
                    transition: "height 80ms linear, opacity 120ms ease-out",
                    opacity: Math.max(0.2, v * 1.7),
                  }}
                />
              ))}
            </div>
          </div>
        )}
        {state === "transcribing" && (
          <div className="transcribing-text">{t("overlay.transcribing")}</div>
        )}
        {state === "processing" && (
          <div className="transcribing-text">{t("overlay.processing")}</div>
        )}
        {state === "usb-cycling" && (
          <div className="transcribing-text usb-cycling-text">
            {t("overlay.usbCycling", "USB cycling…")}
          </div>
        )}
      </div>

      <div className="overlay-right">
        {state === "recording" && (
          <div className="cancel-button" onClick={handleCancel}>
            <CancelIcon />
          </div>
        )}
      </div>
    </div>
  );
};

export default RecordingOverlay;
