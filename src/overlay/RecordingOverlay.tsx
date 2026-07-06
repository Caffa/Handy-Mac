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
import {
  PartialTranscriptionEvent,
  TranscriptionSegment,
} from "@/lib/types/events";

type OverlayState =
  | "recording"
  | "transcribing"
  | "processing"
  | "usb-cycling"
  | "confirming";
type OverlayAction = "transcribe" | "post_process" | "router";

// If no mic-level event arrives within this many milliseconds,
// start decaying the bars to zero to avoid a frozen visualizer.
// Reduced from 500ms to 300ms for more responsive visual feedback.
const LEVEL_TIMEOUT_MS = 300;

// Safety timeout for USB cycling state. If the Rust backend never
// emits a "finished" or "failed" event (e.g. event delivery failure,
// uhubctl hang, thread panic), the overlay will auto-recover after
// this duration instead of being stuck on "USB cycling…" forever.
// The backend blocks for up to ~9s (5s uhubctl + 4s settle), so
// 15s gives comfortable margin without hanging the UI for too long.
const USB_CYCLING_SAFETY_TIMEOUT_MS = 15_000;

// Countdown timer for routing confirmation (in milliseconds)
// User requested 2-3 seconds after editing stage, so increased from 2s to 4.5s
const ROUTING_COUNTDOWN_MS = 4500;

// Maximum time to wait for router result before hiding overlay (in milliseconds)
const ROUTER_RESULT_TIMEOUT_MS = 30_000;

// Time to display router result before auto-hiding (in milliseconds)
const ROUTER_RESULT_DISPLAY_MS = 10_000;

// Handler icon mapping - icons for each router destination
const HANDLER_ICONS: Record<string, string> = {
  Daily: "📖",
  "Apple Note": "📝",
  "Project Devlog": "📁",
  "Tiny Experiment": "🧪",
  Zettelkasten: "🃏",
  "Story Idea": "💡",
  "Read Later": "📚",
  Idea: "💭",
  "Swipe File": "🔖",
  Concerns: "⚠️",
  "Emotional Rant": "😤",
  Correction: "✏️",
  // Default fallback
  default: "✅",
};

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

/// Parse handler names from summary string like "✅ Daily | ✅ Zettelkasten (my-note)"
function parseHandlerNames(summary: string | null): string[] {
  if (!summary) return [];

  // Split by | and extract handler names
  // Format: "✅ Daily | ✅ Zettelkasten (my-note)" or "✅ Daily"
  return summary
    .split("|")
    .map((part) => part.trim())
    .filter((part) => part.length > 0)
    .map((part) => {
      // Remove status emoji (✅, ❌, ⚠️) from the beginning
      const cleaned = part.replace(/^[✅❌⚠️]\s*/, "").trim();
      // Remove file path in parentheses like "(my-note)"
      const withoutPath = cleaned.replace(/\s*\([^)]+\)\s*$/, "").trim();
      return withoutPath;
    })
    .filter((name) => name.length > 0);
}

/// Get icon for a handler name
function getHandlerIcon(handlerName: string): string {
  return HANDLER_ICONS[handlerName] || HANDLER_ICONS.default;
}

/// Filter filler words from the start of streaming transcription text.
function filterStreamingText(text: string): string {
  const fillerWords = [
    "okay",
    "yeah",
    "um",
    "uh",
    "so",
    "like",
    "you know",
    "right",
    "well",
  ];
  const trimmed = text.trim();
  if (!trimmed) return "";

  const words = trimmed.split(/\s+/);
  if (words.length < 2) {
    const lowerText = trimmed.toLowerCase();
    const isFiller = fillerWords.some(
      (fw) =>
        lowerText === fw || lowerText === `${fw}.` || lowerText === `${fw},`,
    );
    if (isFiller) {
      console.log("[Live Captions] Text before filter (single word filler):", {
        text: text.substring(0, 100),
        length: text.length,
        filteredOut: true,
      });
      return "";
    }
    return trimmed;
  }

  const firstWord = words[0].toLowerCase().replace(/[.,!?]/, "");
  const isFillerStart = fillerWords.includes(firstWord);

  if (isFillerStart && words.length >= 2) {
    const secondWord = words[1].toLowerCase().replace(/[.,!?]/, "");
    const isContinuation =
      fillerWords.includes(secondWord) ||
      secondWord === "and" ||
      secondWord === "but";

    // Determine the filtered result (same logic as before)
    const filteredText = (() => {
      if (isContinuation && words.length === 2) return "";
      if (isContinuation) return words.slice(1).join(" ");
      return words.slice(1).join(" ");
    })();
    console.log("[Live Captions] Text after filter:", {
      originalText: text.substring(0, 100),
      filteredText: filteredText.substring(0, 100),
      wasFiltered: text !== filteredText,
    });
    return filteredText;
  }

  return trimmed;
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

  // Live captions setting (fetched from backend on mount/visibility change)
  const [liveCaptionsEnabled, setLiveCaptionsEnabled] = useState(true); // default to enabled
  const [recordingElapsedSecs, setRecordingElapsedSecs] = useState(0);
  const recordingStartRef = useRef<number>(0);
  const elapsedTimerRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const [usbCycleStage, setUsbCycleStage] = useState<{
    stage: string;
    message: string;
  } | null>(null);

  // USB cycling elapsed time for progress display
  const [usbCyclingStartTime, setUsbCyclingStartTime] = useState<number | null>(
    null,
  );
  const [usbCyclingElapsed, setUsbCyclingElapsed] = useState(0);
  const usbCyclingTimerRef = useRef<ReturnType<typeof setInterval> | null>(
    null,
  );

  // "Mic dead" detection: if no audio received for >1 second, show warning
  const [micDeadWarning, setMicDeadWarning] = useState(false);
  const micDeadTimerRef = useRef<ReturnType<typeof setInterval> | null>(null);

  // Low audio level detection: if max level is consistently below threshold, show warning
  // This catches cases where the mic "works" but captures almost nothing (e.g., dead/muted mic)
  const [lowAudioWarning, setLowAudioWarning] = useState(false);
  const lowAudioHistoryRef = useRef<number[]>([]);
  const recordingStartTimeRef = useRef<number>(0);
  const hadGoodAudioRef = useRef<boolean>(false); // Track if we've seen good audio during this recording
  // The visualizer outputs normalized 0-1 values based on dB range (-55dB to -8dB).
  // A value of 0.05 corresponds to roughly -54dB, indicating very low audio levels.
  // Normal speech typically produces values in the 0.2-0.5 range.
  const LOW_AUDIO_THRESHOLD = 0.05;
  const GOOD_AUDIO_THRESHOLD = 0.08; // If we see this level, mic is working fine
  const LOW_AUDIO_CHECK_SAMPLES = 10; // Check last 10 level samples (~800ms at 80ms intervals)
  const LOW_AUDIO_MIN_RECORDING_MS = 1500; // Don't warn until at least 1.5s of recording

  // Transcription preview for routing mode
  const [transcriptionPreview, setTranscriptionPreview] = useState<string>("");

  // Streaming transcription text (shown during recording)
  const [streamingText, setStreamingText] = useState<string>("");
  const [streamingSegments, setStreamingSegments] = useState<
    TranscriptionSegment[]
  >([]);

  // Routing confirmation countdown
  const [countdown, setCountdown] = useState<number>(0);
  const [isEditing, setIsEditing] = useState(false);
  const [editedText, setEditedText] = useState<string>("");
  const countdownRef = useRef<ReturnType<typeof setInterval> | null>(null);

  // Router result display
  const [routerResult, setRouterResult] = useState<RouterResultEvent | null>(
    null,
  );
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  // Fade-out state for transcription preview dismissal
  const [isFadingOut, setIsFadingOut] = useState(false);
  const fadeOutTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Refs to avoid stale closures in async callbacks
  const transcriptionPreviewRef = useRef(transcriptionPreview);
  const editedTextRef = useRef(editedText);

  // Track whether USB cycling is currently active (to ignore stale stage events)
  const usbCyclingActiveRef = useRef(false);

  const isRouter = action === "router";

  // Decay timer: if we haven't received mic-level data for LEVEL_TIMEOUT_MS,
  // smoothly fade the bars toward zero so the overlay doesn't freeze.
  useEffect(() => {
    decayTimerRef.current = setInterval(() => {
      const elapsed = Date.now() - lastLevelTimeRef.current;
      if (elapsed > LEVEL_TIMEOUT_MS) {
        // Faster exponential decay toward zero for responsive visual feedback
        // The longer we wait, the faster we decay
        const decayFactor = Math.max(0.3, 1 - elapsed / 1000);
        setLevels((prev) => {
          const newLevels = prev.map((v) => v * decayFactor);
          // Snap to zero when very small
          return newLevels.map((v) => (v < 0.01 ? 0 : v));
        });
      }
    }, 60); // Faster timer for more responsive visual feedback

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

  // Low audio level detection: warn if all recent levels are below threshold
  // This catches cases where mic "works" but captures almost nothing
  useEffect(() => {
    if (state !== "recording" || !isVisible) {
      setLowAudioWarning(false);
      lowAudioHistoryRef.current = [];
      return;
    }

    // Don't warn until we've been recording for a bit (avoid false positives on startup)
    const elapsed = Date.now() - recordingStartTimeRef.current;
    if (elapsed < LOW_AUDIO_MIN_RECORDING_MS) {
      setLowAudioWarning(false);
      return;
    }

    // If we've already seen good audio during this recording, don't warn
    // (user spoke successfully earlier and is just pausing to think)
    if (hadGoodAudioRef.current) {
      setLowAudioWarning(false);
      return;
    }

    // Check if we have enough samples and all are below threshold
    const history = lowAudioHistoryRef.current;
    if (history.length >= LOW_AUDIO_CHECK_SAMPLES) {
      const allBelowThreshold = history.every(
        (level) => level < LOW_AUDIO_THRESHOLD,
      );

      // Debug logging for low-audio warning evaluation
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
  }, [state, isVisible, levels]);

  // Fetch hybrid mode + live captions settings when overlay becomes visible
  useEffect(() => {
    if (!isVisible) return;
    const fetchSettings = async () => {
      try {
        const result = await commands.getAppSettings();
        if (result.status === "ok" && result.data) {
          setHybridEnabled(result.data.hybrid_mode_enabled ?? false);
          setHybridThresholdSecs(result.data.hybrid_threshold_secs ?? 20);
          const captionsEnabled = result.data.live_captions_enabled ?? true;
          setLiveCaptionsEnabled(captionsEnabled);
          console.log("[Live Captions] Settings loaded:", {
            enabled: captionsEnabled,
            selectedModel: result.data.selected_model,
          });
        }
      } catch {
        // Silently ignore — indicator simply won't show
      }
    };
    fetchSettings();
  }, [isVisible]);

  // Overlay scale setting (1.0 = normal, 2.0 = double size)
  const [overlayScale, setOverlayScale] = useState(1.0);

  // Fetch overlay scale setting
  useEffect(() => {
    const fetchOverlayScale = async () => {
      try {
        const result = await commands.getAppSettings();
        if (result.status === "ok" && result.data) {
          setOverlayScale(result.data.overlay_scale ?? 1.0);
        }
      } catch {
        // Silently ignore — default to 1.0
      }
    };
    fetchOverlayScale();
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
          usbCyclingActiveRef.current = false; // Reset ref when safety timeout triggers
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

  // Debug effect for live captions visibility
  // Logs specific reasons why captions might not show, making it easy to
  // diagnose which condition is failing in production.
  useEffect(() => {
    if (state === "recording" && isVisible) {
      const reasons: string[] = [];
      if (!liveCaptionsEnabled) reasons.push("liveCaptionsEnabled=false");
      if (micDeadWarning) reasons.push("micDeadWarning=true");
      if (lowAudioWarning) reasons.push("lowAudioWarning=true");
      if (!streamingText.trim()) reasons.push(`streamingText="${streamingText.substring(0, 50)}..."`);

      if (reasons.length > 0) {
        console.log("[Live Captions] Not showing:", reasons.join(", "));
      } else {
        console.log("[Live Captions] ✓ Showing:", streamingText.substring(0, 50));
      }
    }
  }, [state, isVisible, liveCaptionsEnabled, micDeadWarning, lowAudioWarning, streamingText]);

  // Detect if backend didn't set up streaming — warn after 3 seconds with no transcription
  useEffect(() => {
    if (state === "recording" && liveCaptionsEnabled) {
      const timeout = setTimeout(() => {
        if (!streamingText) {
          console.warn(
            "[Live Captions] No transcription received after 3 seconds — check backend logs for initialization issues",
          );
        }
      }, 3000);
      return () => clearTimeout(timeout);
    }
  }, [state, liveCaptionsEnabled]);

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
      console.log("[Edit Mode] Focusing textarea");
      textareaRef.current.focus();
      textareaRef.current.setSelectionRange(
        textareaRef.current.value.length,
        textareaRef.current.value.length,
      );
    }
  }, [isEditing]);

  // ============================================================================
  // BUGFIX (2026-06-15): Router Filing Race Condition
  // ============================================================================
  // PROBLEM: When Handy is routing and filing a note, if the user starts a new
  // transcription during the 5-second result display, the overlay would disappear
  // mid-recording.
  //
  // ROOT CAUSE: Router thread is fire-and-forget. It emits `router-result` event,
  // frontend shows result for 5 seconds with a timeout that calls setIsVisible(false).
  // If user starts new recording during those 5 seconds, the timeout fires and hides
  // the overlay even though recording is active.
  //
  // FIX: Check current state before hiding. The setState updater function lets us
  // inspect current state at timeout-fire time. If state is recording/transcribing/
  // processing/confirming, keep overlay visible and just clear the router result.
  //
  // See learning-log.md "Router Filing Race Condition — Overlay Dismissal Bug (2026-06-15)"
  // for full documentation.
  // ============================================================================
  useEffect(() => {
    if (routerResult) {
      const timeout = setTimeout(() => {
        setRouterResult(null);
        // Check if a new recording/transcription is active before hiding overlay.
        // The state may have changed since this timeout was set (user started new recording).
        setState((current) => {
          if (
            current === "recording" ||
            current === "transcribing" ||
            current === "processing" ||
            current === "confirming"
          ) {
            // New transcription is active — keep overlay visible, just clear the router result.
            // The overlay will stay visible for the new recording/transcription.
            return current;
          }
          // No active transcription — safe to hide overlay.
          setIsVisible(false);
          setTranscriptionPreview("");
          setCountdown(0);
          return current;
        });
      }, ROUTER_RESULT_DISPLAY_MS); // Show result for configurable duration

      return () => clearTimeout(timeout);
    }
  }, [routerResult]);

  // Handle transcription preview fade-out when entering processing state
  useEffect(() => {
    // Clear any existing timer
    if (fadeOutTimerRef.current) {
      clearTimeout(fadeOutTimerRef.current);
      fadeOutTimerRef.current = null;
    }

    if (state === "processing" && transcriptionPreview && !routerResult) {
      // Start fade-out after a short delay (like a toast)
      fadeOutTimerRef.current = setTimeout(() => {
        setIsFadingOut(true);
        // Clear the preview after fade-out animation completes (100ms)
        setTimeout(() => {
          setTranscriptionPreview("");
          setIsFadingOut(false);
        }, 100);
      }, 2000); // Show for 2 seconds before fading out
    } else {
      // Reset fade-out state when not in processing
      setIsFadingOut(false);
    }

    return () => {
      if (fadeOutTimerRef.current) {
        clearTimeout(fadeOutTimerRef.current);
      }
    };
  }, [state, transcriptionPreview, routerResult]);

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
          console.log(
            "[Live Captions] Recording started — liveCaptionsEnabled:",
            liveCaptionsEnabled,
          );
          // CRITICAL: Reset USB cycling state when starting a new recording.
          // This ensures stale stage events from a previous cycle don't affect
          // the new recording. Without this, the ref could remain true if the
          // finished/failed event wasn't properly received.
          usbCyclingActiveRef.current = false;
          setTranscriptionPreview("");
          setStreamingText(""); // Clear streaming text
          setStreamingSegments([]);
          setRouterResult(null);
          setIsEditing(false);
          setEditedText("");
          setCountdown(0);
          // Reset mic-level timestamp for dead-mic detection
          lastLevelTimeRef.current = Date.now();
          recordingStartTimeRef.current = Date.now();
          setMicDeadWarning(false);
          setLowAudioWarning(false);
          lowAudioHistoryRef.current = [];
          hadGoodAudioRef.current = false; // Reset good audio flag for new recording
        }
      });

      // Listen for hide-overlay event from Rust
      const unlistenHide = await listen<{ force?: boolean }>(
        "hide-overlay",
        (event) => {
          const { force } = event.payload || {};

          // ============================================================================
          // BUGFIX (2026-06-29): Cancel Operation Support
          // ============================================================================
          // When force: true (from cancel operation), always hide regardless of state.
          // When force: false/undefined (from normal completion), check state to avoid
          // hiding during a new recording that started after the hide was triggered.
          //
          // Original BUGFIX (2026-06-17): Router Filing Race Condition — Hide Overlay Event
          // PROBLEM: When router finishes filing and user has already started a new
          // transcription, the hide-overlay event would hide the overlay mid-recording.
          //
          // ROOT CAUSE: Router thread emits hide-overlay immediately when it finishes.
          // If user started new recording between router finish and hide event emission,
          // the event still arrives at frontend with stale state assumption.
          //
          // FIX: Check current state before hiding. If recording/transcribing/processing/
          // confirming, keep overlay visible — unless force: true (cancel).
          //
          // See learning-log.md "Router Filing Race Condition" for full documentation.
          // ============================================================================
          setState((current) => {
            // If forced (cancel), always hide
            if (force) {
              setIsVisible(false);
              setTranscriptionPreview("");
              setStreamingText("");
              setStreamingSegments([]);
              setRouterResult(null);
              setIsEditing(false);
              setCountdown(0);
              return current; // Return unchanged state, setIsVisible handles visibility
            }

            // Don't hide if a new recording/transcription is active
            if (
              current === "recording" ||
              current === "transcribing" ||
              current === "processing" ||
              current === "confirming"
            ) {
              // New transcription is active — ignore the hide event
              return current;
            }
            if (current !== "usb-cycling") {
              setIsVisible(false);
              setTranscriptionPreview(""); // Clear preview when hiding
              setStreamingText(""); // Clear streaming text when hiding
              setStreamingSegments([]);
              setRouterResult(null);
              setIsEditing(false);
              setCountdown(0);
            }
            return current;
          });
        },
      );

      // Listen for mic-level updates
      const unlistenLevel = await listen<number[]>("mic-level", (event) => {
        lastLevelTimeRef.current = Date.now();
        const newLevels = event.payload as number[];

        // Apply minimal smoothing for responsiveness
        // Faster convergence when previous value is near zero (cold start)
        // Auto-boost when target is significantly higher than prev (bars barely moving)
        const smoothed = smoothedLevelsRef.current.map((prev, i) => {
          const target = newLevels[i] || 0;

          // Calculate how much the bar is moving
          const delta = Math.abs(target - prev);
          const avgLevel = (prev + target) / 2;

          // Auto-adjustment: if bars are barely moving (low delta relative to avg level),
          // use even faster convergence to snap them into action
          const isBarelyMoving = delta < 0.02 && avgLevel > 0.05;

          // Cold start: if prev is near zero, use fastest convergence
          // Barely moving: use fast convergence to snap into action
          // Normal smoothing: 40% prev + 60% target (more responsive than old 70/30)
          let alpha: number;
          if (prev < 0.05) {
            alpha = 0.8; // Cold start: fastest convergence
          } else if (isBarelyMoving) {
            alpha = 0.7; // Barely moving: fast convergence to auto-adjust
          } else {
            alpha = 0.6; // Normal: responsive smoothing
          }

          return prev * (1 - alpha) + target * alpha;
        });

        smoothedLevelsRef.current = smoothed;
        setLevels(smoothed.slice(0, 9));

        // Track low audio levels during recording
        // Calculate max level from the incoming audio data
        const maxLevel = Math.max(...newLevels);

        // Debug logging for audio level threshold tuning
        console.log(
          "[audio-level] maxLevel:",
          maxLevel.toFixed(3),
          "| goodThreshold:",
          GOOD_AUDIO_THRESHOLD,
          "| hadGoodAudio:",
          hadGoodAudioRef.current,
        );

        // If we see good audio levels, mark that the mic is working
        if (maxLevel >= GOOD_AUDIO_THRESHOLD) {
          hadGoodAudioRef.current = true;
          console.log(
            "[audio-level] ✓ Good audio detected, suppressing low-audio warning",
          );
        }

        lowAudioHistoryRef.current.push(maxLevel);
        // Keep only last N samples
        if (lowAudioHistoryRef.current.length > LOW_AUDIO_CHECK_SAMPLES) {
          lowAudioHistoryRef.current.shift();
        }
      });

      // Listen for USB power-cycle events from Rust
      const unlistenUsbCycleStart = await listen<string>(
        "usb-power-cycle-started",
        () => {
          // Only transition if we are currently recording (overlay is visible)
          // This shows the user that the USB device is being power-cycled.
          usbCyclingActiveRef.current = true; // Mark USB cycling as active
          setState("usb-cycling");
          setMicDeadWarning(false); // Clear warning during recovery
          setLowAudioWarning(false); // Clear low audio warning during recovery
        },
      );

      const unlistenUsbCycleFinished = await listen<string>(
        "usb-power-cycle-finished",
        () => {
          usbCyclingActiveRef.current = false; // Mark USB cycling as inactive
          setUsbCycleStage(null);
          setMicDeadWarning(false);
          setLowAudioWarning(false);
          // Reset all audio level tracking state to start fresh after USB cycling.
          // This ensures the frontend matches app restart behavior where all
          // state is initialized fresh. Without this reset:
          // 1. smoothedLevelsRef retains elevated values from before cycling
          // 2. lowAudioHistoryRef may contain zeros from the reset period
          // 3. recordingStartTimeRef is stale (recording started before cycling)
          // All of these can cause "low audio" warnings or insensitive volume bars.
          setLevels(Array(16).fill(0));
          smoothedLevelsRef.current = Array(16).fill(0);
          lowAudioHistoryRef.current = [];
          recordingStartTimeRef.current = Date.now(); // Reset recording timer
          hadGoodAudioRef.current = false; // Reset good audio flag after USB cycling
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
          usbCyclingActiveRef.current = false; // Mark USB cycling as inactive
          setUsbCycleStage(null);
          setMicDeadWarning(false);
          setLowAudioWarning(false);
          // Reset all audio level tracking state on failure too
          setLevels(Array(16).fill(0));
          smoothedLevelsRef.current = Array(16).fill(0);
          lowAudioHistoryRef.current = [];
          recordingStartTimeRef.current = Date.now();
          hadGoodAudioRef.current = false; // Reset good audio flag after USB cycling
          setState((prev) => (prev === "usb-cycling" ? "recording" : prev));
          setIsVisible(false);
        },
      );

      const unlistenUsbCycleStage = await listen<{
        stage: string;
        message: string;
      }>("usb-power-cycle-stage", (event) => {
        // Ignore stage events if USB cycling is no longer active.
        // This prevents stale stage events from transitioning the state back
        // to "usb-cycling" after the cycle has already finished.
        if (!usbCyclingActiveRef.current) {
          console.log(
            "[usb-cycling] Ignoring stale stage event:",
            event.payload.stage,
          );
          return;
        }
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
          // Reset fade-out state for new transcription
          setIsFadingOut(false);
        },
      );

      // Listen for partial transcription during streaming.
      // Receives structured payload with segments and timestamps.
      // Accumulates segments by timestamp to prevent early content from
      // disappearing when Whisper re-interprets longer audio buffers.
      console.log(
        "[Live Captions] Registering partial-transcription event listener",
      );
      const unlistenPartialTranscription =
        await listen<PartialTranscriptionEvent>(
          "partial-transcription",
          (event) => {
            const { text, segments } = event.payload;
            console.log("[Live Captions] Event received:", {
              hasText: !!text,
              textLength: text?.length || 0,
              hasSegments: !!segments,
              segmentCount: segments?.length || 0,
              liveCaptionsEnabled,
              currentState: state,
              isVisible,
            });

            if (segments && segments.length > 0) {
              // Merge new segments with accumulated segments by timestamp.
              // Any existing segment that overlaps with a new segment is
              // replaced. Non-overlapping existing segments are preserved.
              // Also compute display text from the merged result in one pass.
              setStreamingSegments((prev) => {
                const merged = [...prev];
                for (const ns of segments) {
                  const kept = merged.filter(
                    (es) => !(es.start < ns.end && es.end > ns.start),
                  );
                  kept.push(ns);
                  merged.splice(0, merged.length, ...kept);
                }
                merged.sort((a, b) => a.start - b.start);

                // Derive display text from merged segments
                const text = merged.map((s) => s.text).join(" ");
                setStreamingText((prevText) => {
                  const filtered = filterStreamingText(text);
                  if (!filtered) {
                    console.log(
                      "[Live Captions] Filler filter removed ALL text — raw:",
                      JSON.stringify(text),
                      "| keeping previous:",
                      JSON.stringify(prevText),
                    );
                  } else {
                    console.log(
                      "[Live Captions] streamingText set to:",
                      JSON.stringify(filtered),
                    );
                  }
                  return filtered || prevText;
                });

                return merged;
              });
            } else {
              // Fallback for models without timestamps (e.g. Moonshine)
              setStreamingText((prev) => {
                if (prev === text) {
                  console.log(
                    "[Live Captions] Text unchanged from previous — skipping update",
                  );
                  return prev;
                }
                const filtered = filterStreamingText(text);
                if (!filtered) {
                  console.log(
                    "[Live Captions] Filler filter removed ALL text (fallback) — raw:",
                    JSON.stringify(text),
                    "| keeping previous:",
                    JSON.stringify(prev),
                  );
                } else {
                  console.log(
                    "[Live Captions] streamingText set to (fallback):",
                    JSON.stringify(filtered),
                  );
                }
                return filtered || prev;
              });
            }
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
        unlistenPartialTranscription();
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
    // Add enlarged overlay for "mic dead" or "low audio" states
    if ((micDeadWarning || lowAudioWarning) && state === "recording")
      classes.push("mic-dead-overlay");
    // Add enlarged overlay for USB cycling progress
    if (state === "usb-cycling") classes.push("usb-cycling-overlay");
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
      <div
        dir={direction}
        className={getOverlayClassNames()}
        style={{ "--overlay-scale": overlayScale } as React.CSSProperties}
      >
        <div className="overlay-left">{getIcon()}</div>

        <div className="overlay-middle">
          {/* Mic dead or low audio warning - show when recording but no audio or very low levels */}
          {(micDeadWarning || lowAudioWarning) && state === "recording" && (
            <div className="mic-dead-warning">
              {micDeadWarning
                ? t("overlay.micDead")
                : t("overlay.lowAudio", "Low audio - check microphone")}
            </div>
          )}
          {/* Volume bars during recording - inside the pill */}
          {state === "recording" && !micDeadWarning && !lowAudioWarning && (
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
                    {["resolving", "cycling", "waiting", "recovered"].map(
                      (s) => (
                        <div
                          key={s}
                          className={`usb-cycling-dot ${
                            [
                              "resolving",
                              "cycling",
                              "waiting",
                              "recovered",
                            ].indexOf(usbCycleStage.stage) >=
                            [
                              "resolving",
                              "cycling",
                              "waiting",
                              "recovered",
                            ].indexOf(s)
                              ? "dot-active"
                              : ""
                          } ${usbCycleStage.stage === s ? "dot-current" : ""}`}
                        />
                      ),
                    )}
                  </div>
                  {/* Show elapsed time during USB cycling */}
                  {usbCyclingElapsed > 0 && (
                    <div className="usb-cycling-time">
                      {t("overlay.usbCyclingTime", {
                        seconds: usbCyclingElapsed,
                      })}
                    </div>
                  )}
                </>
              )}
            </div>
          )}
          {/* Confirming state - waiting for user to confirm or edit */}
          {state === "confirming" && !routerResult && (
            <div className="confirming-text">
              {isEditing
                ? t("overlay.editing", "Edit text:")
                : t("overlay.confirming", "Sending in")}
              {!isEditing && countdown > 0 && (
                <span className="countdown-timer">
                  {formatCountdown(countdown)}
                </span>
              )}
            </div>
          )}
        </div>

        <div className="overlay-right">
          {state === "recording" && (
            <div className="cancel-button" onClick={handleCancel}>
              <CancelIcon
                width={33}
                height={33}
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

      {/* Live captions - positioned below the pill */}
      {isVisible &&
        state === "recording" &&
        liveCaptionsEnabled &&
        !micDeadWarning &&
        !lowAudioWarning &&
        streamingText &&
        streamingText.trim() && (
          <div
            dir={direction}
            className={`live-captions-box ${isVisible ? "fade-in" : ""} ${isRouter ? "routing-mode" : ""}`}
            style={{ "--overlay-scale": overlayScale } as React.CSSProperties}
            role="status"
            aria-live="polite"
            aria-label="Live transcription"
          >
            {streamingText}
          </div>
        )}

      {/* Transcription preview for routing mode - with edit capability */}
      {isRouter && (transcriptionPreview || routerResult) && (
        <div
          dir={direction}
          className={`transcription-preview ${isEditing ? "editing" : ""} ${routerResult ? "has-result" : ""} ${isFadingOut ? "fade-out" : ""}`}
          style={{ "--overlay-scale": overlayScale } as React.CSSProperties}
        >
          {routerResult ? (
            // Router result display - icons only, no white box
            <div className="router-result">
              {routerResult.success ? (
                <div className="router-success">
                  <div className="result-icons">
                    {parseHandlerNames(routerResult.summary).map(
                      (handlerName, idx) => (
                        <span key={idx} className="handler-icon">
                          {getHandlerIcon(handlerName)}
                        </span>
                      ),
                    )}
                  </div>
                  {routerResult.summary && (
                    <div className="result-summary">{routerResult.summary}</div>
                  )}
                </div>
              ) : (
                <div className="router-error">
                  <div className="result-icon">❌</div>
                  <div className="result-message">
                    {routerResult.error ||
                      t("overlay.routerError", "Routing failed")}
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
                onKeyDown={(e) => e.stopPropagation()}
                onBlur={() => {
                  // Focus lock: refocus if still in editing mode
                  if (isEditing) {
                    setTimeout(() => textareaRef.current?.focus(), 10);
                  }
                }}
                placeholder={t("overlay.editPlaceholder", "Edit your text...")}
                dir={direction}
              />
              <div className="edit-buttons">
                <button
                  className="edit-cancel-button"
                  onClick={handleCancelEdit}
                >
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
          {state === "confirming" &&
            !isEditing &&
            !routerResult &&
            countdown > 0 && (
              <div
                className="countdown-progress"
                style={{
                  width: `${(countdown / ROUTING_COUNTDOWN_MS) * 100}%`,
                }}
              />
            )}
        </div>
      )}
    </>
  );
};

export default RecordingOverlay;
