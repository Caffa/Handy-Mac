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

type OverlayState = "recording" | "transcribing" | "processing" | "usb-cycling" | "confirming";
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

// Countdown timer for routing confirmation (in milliseconds)
const ROUTING_COUNTDOWN_MS = 2000;

// Maximum time to wait for router result before hiding overlay (in milliseconds)
const ROUTER_RESULT_TIMEOUT_MS = 30_000;

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

/// Router handler result from backend
interface RouterHandlerData {
  status: string;
  handler: string;
  classification: string;
  file_path: string | null;
}

/// Router result event from backend
interface RouterResultEvent {
  success: boolean;
  summary: string | null;
  error: string | null;
  transcription_text: string;
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

  // USB cycling elapsed time for progress display
  const [usbCyclingStartTime, setUsbCyclingStartTime] = useState<number | null>(null);
  const [usbCyclingElapsed, setUsbCyclingElapsed] = useState(0);
  const usbCyclingTimerRef = useRef<ReturnType<typeof setInterval> | null>(null);

  // "Mic dead" detection: if no audio received for >1 second, show warning
  const [micDeadWarning, setMicDeadWarning] = useState(false);
  const micDeadTimerRef = useRef<ReturnType<typeof setInterval> | null>(null);

  // Transcription preview for routing mode
  const [transcriptionPreview, setTranscriptionPreview] = useState<string>("");
  
  // Routing confirmation countdown
  const [countdown, setCountdown] = useState<number>(0);
  const [isEditing, setIsEditing] = useState(false);
  const [editedText, setEditedText] = useState<string>("");
  const countdownRef = useRef<ReturnType<typeof setInterval> | null>(null);
  
  // Router result display
  const [routerResult, setRouterResult] = useState<RouterResultEvent | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  // Refs to avoid stale closures in async callbacks
  const transcriptionPreviewRef = useRef(transcriptionPreview);
  const editedTextRef = useRef(editedText);

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

  // "Mic dead" detection: warn if recording but no audio for >1s
  // This catches zombie streams after wake-from-sleep before user tries to record
  useEffect(() => {
    if (state !== "recording" || !isVisible) {
      setMicDeadWarning(false);
      if (micDeadTimerRef.current) {
        clearInterval(micDeadTimerRef.current);
        micDeadTimerRef.current = null;
      }
      return;
    }

    micDeadTimerRef.current = setInterval(() => {
      const elapsed = Date.now() - lastLevelTimeRef.current;
      // If >1 second without audio while recording, show "mic dead" warning
      if (elapsed > 1000) {
        setMicDeadWarning(true);
      } else {
        setMicDeadWarning(false);
      }
    }, 200);

    return () => {
      if (micDeadTimerRef.current) {
        clearInterval(micDeadTimerRef.current);
      }
    };
  }, [state, isVisible]);

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

  // Track USB cycling elapsed time
  useEffect(() => {
    if (state === "usb-cycling" && isVisible) {
      setUsbCyclingStartTime(Date.now());
      usbCyclingTimerRef.current = setInterval(() => {
        if (usbCyclingStartTime) {
          setUsbCyclingElapsed(
            Math.floor((Date.now() - usbCyclingStartTime) / 1000),
          );
        }
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
  }, [state, isVisible, usbCyclingStartTime]);

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

  // Keep refs in sync with state
  useEffect(() => {
    transcriptionPreviewRef.current = transcriptionPreview;
  }, [transcriptionPreview]);
  
  useEffect(() => {
    editedTextRef.current = editedText;
  }, [editedText]);

  // Countdown timer for routing confirmation
  useEffect(() => {
    if (state === "confirming" && !isEditing && countdown > 0) {
      countdownRef.current = setInterval(() => {
        setCountdown((prev) => {
          if (prev <= 100) {
            // Countdown finished - send the transcription
            sendRoutingConfirmation(transcriptionPreviewRef.current);
            return 0;
          }
          return prev - 100;
        });
      }, 100);
    }

    return () => {
      if (countdownRef.current) {
        clearInterval(countdownRef.current);
        countdownRef.current = null;
      }
    };
  }, [state, isEditing, countdown]);

  // Focus textarea when entering edit mode
  useEffect(() => {
    if (isEditing && textareaRef.current) {
      textareaRef.current.focus();
      textareaRef.current.setSelectionRange(
        textareaRef.current.value.length,
        textareaRef.current.value.length
      );
    }
  }, [isEditing]);

  // Handle router result timeout
  useEffect(() => {
    if (routerResult) {
      const timeout = setTimeout(() => {
        setRouterResult(null);
        setIsVisible(false);
      }, 5000); // Show result for 5 seconds
      
      return () => clearTimeout(timeout);
    }
  }, [routerResult]);

  // Send routing confirmation to backend
  const sendRoutingConfirmation = useCallback(async (text: string) => {
    try {
      // Emit event to confirm routing with (possibly edited) text
      await commands.confirmRouting(text);
    } catch (e) {
      console.error("Failed to send routing confirmation:", e);
    }
  }, []);

  // Handle clicking on transcription to enter edit mode
  const handleTranscriptionClick = useCallback(() => {
    if (state === "confirming" && !isEditing) {
      setIsEditing(true);
      setEditedText(transcriptionPreviewRef.current);
      // Pause countdown by setting isEditing
      if (countdownRef.current) {
        clearInterval(countdownRef.current);
        countdownRef.current = null;
      }
    }
  }, [state, isEditing]);

  // Handle sending edited text
  const handleSendEdited = useCallback(() => {
    setIsEditing(false);
    sendRoutingConfirmation(editedText);
  }, [editedText, sendRoutingConfirmation]);

  // Handle cancel during editing
  const handleCancelEdit = useCallback(() => {
    setIsEditing(false);
    setEditedText("");
    // Restart countdown from where we paused
    setCountdown(ROUTING_COUNTDOWN_MS);
  }, []);

  // Handle opening result file
  const handleOpenFile = useCallback(async (filePath: string) => {
    try {
      await commands.openPath(filePath);
    } catch (e) {
      console.error("Failed to open file:", e);
    }
  }, []);

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
        // Reset editing state on new recording
        if (parsed.state === "recording") {
          setTranscriptionPreview("");
          setRouterResult(null);
          setIsEditing(false);
          setEditedText("");
          setCountdown(0);
          // Reset mic-level timestamp for dead-mic detection
          lastLevelTimeRef.current = Date.now();
          setMicDeadWarning(false);
        }
      });

      // Listen for hide-overlay event from Rust
      const unlistenHide = await listen("hide-overlay", () => {
        // Functional update to avoid dependency on 'state'
        setState((current) => {
          if (current !== "usb-cycling") {
            setIsVisible(false);
            setTranscriptionPreview(""); // Clear preview when hiding
            setRouterResult(null);
            setIsEditing(false);
            setCountdown(0);
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
          setMicDeadWarning(false); // Clear warning during recovery
        },
      );

      const unlistenUsbCycleFinished = await listen<string>(
        "usb-power-cycle-finished",
        () => {
          setUsbCycleStage(null);
          setMicDeadWarning(false);
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
          setMicDeadWarning(false);
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
        setMicDeadWarning(false); // Clear warning during recovery
      });

      // Listen for transcription preview (for routing mode)
      const unlistenTranscriptionPreview = await listen<string>(
        "transcription-preview",
        (event) => {
          setTranscriptionPreview(event.payload);
          // Start countdown when we receive the transcription
          setCountdown(ROUTING_COUNTDOWN_MS);
          setState("confirming");
          setIsEditing(false);
          setEditedText("");
        },
      );

      // Listen for routing state changes
      const unlistenRoutingState = await listen<string>(
        "routing-state",
        (event) => {
          const newState = event.payload;
          if (newState === "processing") {
            setState("processing");
          }
        },
      );

      // Listen for router result
      const unlistenRouterResult = await listen<RouterResultEvent>(
        "router-result",
        (event) => {
          setRouterResult(event.payload);
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
        unlistenUsbCycleStage();
        unlistenTranscriptionPreview();
        unlistenRoutingState();
        unlistenRouterResult();
      };
    };

    setupEventListeners();
  }, []);

  const getIcon = () => {
    if (isRouter) {
      // Paper-plane icon for routing actions (blue #3b82f6)
      const iconColor = "#3b82f6";
      if (state === "recording") {
        return <RoutingIcon color={iconColor} width={22} height={22} />;
      }
      return <RoutingIcon color={iconColor} width={22} height={22} />;
    }
    if (state === "recording") {
      return <MicrophoneIcon width={22} height={22} />;
    }
    return <TranscriptionIcon width={22} height={22} />;
  };

  const getOverlayClassNames = (): string => {
    const classes = ["recording-overlay"];
    if (isVisible) classes.push("fade-in");
    if (isRouter) classes.push("routing-mode");
    // Add enlarged overlay for "mic dead" state
    if (micDeadWarning && state === "recording") classes.push("mic-dead-overlay");
    // Add editing state for larger overlay
    if (isEditing && state === "confirming") classes.push("editing-overlay");
    return classes.join(" ");
  };

  const handleCancel = useCallback(() => {
    commands.cancelOperation();
  }, []);

  // Format countdown seconds
  const formatCountdown = (ms: number): string => {
    const seconds = Math.ceil(ms / 1000);
    return `${seconds}s`;
  };

  return (
    <>
      <div dir={direction} className={getOverlayClassNames()}>
        <div className="overlay-left">{getIcon()}</div>

        <div className="overlay-middle">
          {/* Mic dead warning - show when recording but no audio for >1s */}
          {micDeadWarning && state === "recording" && (
            <div className="mic-dead-warning">
              {t("overlay.micDead")}
            </div>
          )}
          
          {/* Normal recording state */}
          {state === "recording" && !micDeadWarning && (
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
                      height: `${Math.min(25, 5 + Math.pow(v, 0.7) * 22)}px`,
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
                <>
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
                  {/* Show elapsed time during USB cycling */}
                  {usbCyclingElapsed > 0 && (
                    <div className="usb-cycling-time">
                      {t("overlay.usbCyclingTime", { seconds: usbCyclingElapsed })}
                    </div>
                  )}
                </>
              )}
            </div>
          )}
          {/* Confirming state - waiting for user to confirm or edit */}
          {state === "confirming" && !routerResult && (
            <div className="confirming-text">
              {isEditing ? t("overlay.editing", "Edit text:") : t("overlay.confirming", "Sending in")}
              {!isEditing && countdown > 0 && (
                <span className="countdown-timer">{formatCountdown(countdown)}</span>
              )}
            </div>
          )}
        </div>

        <div className="overlay-right">
          {state === "recording" && (
            <div className="cancel-button" onClick={handleCancel}>
              <CancelIcon
                width={25}
                height={25}
                color={isRouter ? "#3b82f6" : undefined}
              />
            </div>
          )}
          {state === "confirming" && isEditing && (
            <div className="confirm-buttons">
              <div className="confirm-send-button" onClick={handleSendEdited}>
                {t("overlay.send", "Send")}
              </div>
            </div>
          )}
        </div>
      </div>

      {/* Transcription preview for routing mode - with edit capability */}
      {isRouter && (transcriptionPreview || routerResult) && (
        <div 
          dir={direction} 
          className={`transcription-preview ${isEditing ? "editing" : ""} ${routerResult ? "has-result" : ""} ${state === "processing" ? "processing" : ""}`}
        >
          {routerResult ? (
            // Router result display
            <div className="router-result">
              {routerResult.success ? (
                <div className="router-success">
                  <div className="result-icon">✅</div>
                  <div className="result-summary">{routerResult.summary}</div>
                </div>
              ) : (
                <div className="router-error">
                  <div className="result-icon">❌</div>
                  <div className="result-message">
                    {routerResult.error || t("overlay.routerError", "Routing failed")}
                  </div>
                </div>
              )}
            </div>
          ) : isEditing ? (
            // Edit mode - show textarea
            <div className="edit-container">
              <textarea
                ref={textareaRef}
                className="transcription-edit"
                value={editedText}
                onChange={(e) => setEditedText(e.target.value)}
                placeholder={t("overlay.editPlaceholder", "Edit your text...")}
                dir={direction}
              />
              <div className="edit-buttons">
                <button className="edit-cancel-button" onClick={handleCancelEdit}>
                  {t("overlay.cancel", "Cancel")}
                </button>
              </div>
            </div>
          ) : (
            // Countdown view - show text with countdown overlay
            <div 
              className="transcription-text-preview" 
              onClick={handleTranscriptionClick}
              title={t("overlay.clickToEdit", "Click to edit")}
            >
              {transcriptionPreview}
            </div>
          )}
          
          {/* Countdown progress bar */}
          {state === "confirming" && !isEditing && !routerResult && countdown > 0 && (
            <div 
              className="countdown-progress"
              style={{ width: `${(countdown / ROUTING_COUNTDOWN_MS) * 100}%` }}
            />
          )}
        </div>
      )}
    </>
  );
};

export default RecordingOverlay;