/**
 * useOverlayState — Core overlay visibility and state machine management.
 *
 * Manages:
 * - isVisible: Whether the overlay is shown
 * - state: Current overlay phase (recording, transcribing, processing, etc.)
 * - action: Current action type (transcribe, post_process, router)
 * - overlayScale: Display scaling factor
 * - hybridEnabled/hybridThresholdSecs: Hybrid mode settings
 * - recordingElapsedSecs: Seconds elapsed during recording
 *
 * Listens for show-overlay and hide-overlay events from the Rust backend.
 *
 * Scope: Overlay lifecycle — show, hide, state transitions.
 * Dependencies: @tauri-apps/api/event, @/bindings, @/i18n
 * Side effects: Event listeners for show-overlay and hide-overlay.
 */
import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useRef, useState } from "react";
import { commands } from "@/bindings";
import i18n, { syncLanguageFromSettings } from "@/i18n";
import { getLanguageDirection } from "@/lib/utils/rtl";

/// Overlay state phases
export type OverlayState =
  | "recording"
  | "transcribing"
  | "processing"
  | "usb-cycling"
  | "confirming";

/// Overlay action types
export type OverlayAction = "transcribe" | "post_process" | "router";

/// Parse a compound payload of the form "state:action" emitted by the Rust
/// backend. Legacy payloads (no colon) are treated as state-only with action
/// defaulting to "transcribe".
export function parseOverlayPayload(payload: string): {
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

interface UseOverlayStateOptions {
  /** Called when overlay resets (new recording, cancel, etc.) */
  onReset?: () => void;
}

interface UseOverlayStateReturn {
  isVisible: boolean;
  setIsVisible: React.Dispatch<React.SetStateAction<boolean>>;
  state: OverlayState;
  setState: React.Dispatch<React.SetStateAction<OverlayState>>;
  action: OverlayAction;
  isRouter: boolean;
  overlayScale: number;
  direction: "ltr" | "rtl";
  hybridEnabled: boolean;
  hybridThresholdSecs: number;
  recordingElapsedSecs: number;
  // State values shared with sub-hooks and components
  transcriptionPreview: string;
  streamingText: string;
  routerResult: RouterResultEvent | null;
  isEditing: boolean;
  editedText: string;
  countdown: number;
  isFadingOut: boolean;
  micDeadWarning: boolean;
  lowAudioWarning: boolean;
  usbCycleStage: { stage: string; message: string } | null;
  // State setters needed by other hooks
  setTranscriptionPreview: React.Dispatch<React.SetStateAction<string>>;
  setStreamingText: React.Dispatch<React.SetStateAction<string>>;
  setStreamingSegments: React.Dispatch<
    React.SetStateAction<import("@/lib/types/events").TranscriptionSegment[]>
  >;
  setRouterResult: React.Dispatch<
    React.SetStateAction<RouterResultEvent | null>
  >;
  setIsEditing: React.Dispatch<React.SetStateAction<boolean>>;
  setEditedText: React.Dispatch<React.SetStateAction<string>>;
  setCountdown: React.Dispatch<React.SetStateAction<number>>;
  setMicDeadWarning: React.Dispatch<React.SetStateAction<boolean>>;
  setLowAudioWarning: React.Dispatch<React.SetStateAction<boolean>>;
  setUsbCycleStage: React.Dispatch<
    React.SetStateAction<{ stage: string; message: string } | null>
  >;
  setIsFadingOut: React.Dispatch<React.SetStateAction<boolean>>;
  lastLevelTimeRef: React.MutableRefObject<number>;
  recordingStartTimeRef: React.MutableRefObject<number>;
  lowAudioHistoryRef: React.MutableRefObject<number[]>;
  hadGoodAudioRef: React.MutableRefObject<boolean>;
  smoothedLevelsRef: React.MutableRefObject<number[]>;
  usbCyclingActiveRef: React.MutableRefObject<boolean>;
  transcriptionPreviewRef: React.MutableRefObject<string>;
  liveCaptionsEnabled: boolean;
}

/// Router result event from backend
export interface RouterResultEvent {
  success: boolean;
  summary: string | null;
  error: string | null;
  transcription_text: string;
}

export function useOverlayState(
  options?: UseOverlayStateOptions,
): UseOverlayStateReturn {
  const [isVisible, setIsVisible] = useState(false);
  const [state, setState] = useState<OverlayState>("recording");
  const [action, setAction] = useState<OverlayAction>("transcribe");

  // Hybrid mode indicator state
  const [hybridEnabled, setHybridEnabled] = useState(false);
  const [hybridThresholdSecs, setHybridThresholdSecs] = useState(20);

  // Live captions setting
  const [liveCaptionsEnabled, setLiveCaptionsEnabled] = useState(true);

  const [recordingElapsedSecs, setRecordingElapsedSecs] = useState(0);
  const recordingStartRef = useRef<number>(0);
  const elapsedTimerRef = useRef<ReturnType<typeof setInterval> | null>(null);

  // Overlay scale setting
  const [overlayScale, setOverlayScale] = useState(1.0);

  const direction = getLanguageDirection(i18n.language);

  // Refs shared across hooks (needed for reset in show-overlay handler)
  const lastLevelTimeRef = useRef<number>(Date.now());
  const recordingStartTimeRef = useRef<number>(0);
  const lowAudioHistoryRef = useRef<number[]>([]);
  const hadGoodAudioRef = useRef<boolean>(false);
  const smoothedLevelsRef = useRef<number[]>(Array(16).fill(0));
  const usbCyclingActiveRef = useRef(false);

  // State setters needed by the show-overlay handler for reset
  const [transcriptionPreview, setTranscriptionPreview] = useState<string>("");
  const [streamingText, setStreamingText] = useState<string>("");
  const [streamingSegments, setStreamingSegments] = useState<
    import("@/lib/types/events").TranscriptionSegment[]
  >([]);
  const [routerResult, setRouterResult] = useState<RouterResultEvent | null>(
    null,
  );
  const [isEditing, setIsEditing] = useState(false);
  const [editedText, setEditedText] = useState<string>("");
  const [countdown, setCountdown] = useState<number>(0);
  const [micDeadWarning, setMicDeadWarning] = useState(false);
  const [lowAudioWarning, setLowAudioWarning] = useState(false);
  const [usbCycleStage, setUsbCycleStage] = useState<{
    stage: string;
    message: string;
  } | null>(null);

  // Fade-out state for transcription preview dismissal
  const [isFadingOut, setIsFadingOut] = useState(false);

  // Keep transcriptionPreview ref in sync for countdown callback
  const transcriptionPreviewRef = useRef(transcriptionPreview);
  useEffect(() => {
    transcriptionPreviewRef.current = transcriptionPreview;
  }, [transcriptionPreview]);

  const editedTextRef = useRef(editedText);
  useEffect(() => {
    editedTextRef.current = editedText;
  }, [editedText]);

  const isRouter = action === "router";

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

  // Listen for show-overlay and hide-overlay events
  useEffect(() => {
    const setupEventListeners = async () => {
      const unlistenShow = await listen("show-overlay", async (event) => {
        await syncLanguageFromSettings();
        const payload = event.payload as string;
        const parsed = parseOverlayPayload(payload);
        setState(parsed.state);
        setAction(parsed.action);
        setIsVisible(true);

        if (parsed.state === "recording") {
          console.log(
            "[Live Captions] Recording started — liveCaptionsEnabled:",
            liveCaptionsEnabled,
          );
          usbCyclingActiveRef.current = false;
          setTranscriptionPreview("");
          setStreamingText("");
          setStreamingSegments([]);
          setRouterResult(null);
          setIsEditing(false);
          setEditedText("");
          setCountdown(0);
          lastLevelTimeRef.current = Date.now();
          recordingStartTimeRef.current = Date.now();
          setMicDeadWarning(false);
          setLowAudioWarning(false);
          lowAudioHistoryRef.current = [];
          hadGoodAudioRef.current = false;
        }
      });

      const unlistenHide = await listen<{ force?: boolean }>(
        "hide-overlay",
        (event) => {
          const { force } = event.payload || {};

          setState((current) => {
            if (force) {
              setIsVisible(false);
              setTranscriptionPreview("");
              setStreamingText("");
              setStreamingSegments([]);
              setRouterResult(null);
              setIsEditing(false);
              setCountdown(0);
              return current;
            }

            if (
              current === "recording" ||
              current === "transcribing" ||
              current === "processing" ||
              current === "confirming"
            ) {
              return current;
            }
            if (current !== "usb-cycling") {
              setIsVisible(false);
              setTranscriptionPreview("");
              setStreamingText("");
              setStreamingSegments([]);
              setRouterResult(null);
              setIsEditing(false);
              setCountdown(0);
            }
            return current;
          });
        },
      );

      return () => {
        unlistenShow();
        unlistenHide();
      };
    };

    setupEventListeners();
  }, []);

  return {
    isVisible,
    setIsVisible,
    state,
    setState,
    action,
    isRouter,
    overlayScale,
    direction,
    hybridEnabled,
    hybridThresholdSecs,
    recordingElapsedSecs,
    // State values shared with sub-hooks and components
    transcriptionPreview,
    streamingText,
    routerResult,
    isEditing,
    editedText,
    countdown,
    isFadingOut,
    micDeadWarning,
    lowAudioWarning,
    usbCycleStage,
    // State setters needed by other hooks
    setTranscriptionPreview,
    setStreamingText,
    setStreamingSegments,
    setRouterResult,
    setIsEditing,
    setEditedText,
    setCountdown,
    setMicDeadWarning,
    setLowAudioWarning,
    setUsbCycleStage,
    setIsFadingOut,
    // Refs shared across hooks
    lastLevelTimeRef,
    recordingStartTimeRef,
    lowAudioHistoryRef,
    hadGoodAudioRef,
    smoothedLevelsRef,
    usbCyclingActiveRef,
    transcriptionPreviewRef,
    liveCaptionsEnabled,
  };
}
