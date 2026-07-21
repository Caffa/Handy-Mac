/**
 * useOverlaySharedState — Shared overlay state (NOT visibility).
 *
 * Manages the shared mutable state needed by overlay sub-hooks and
 * presentational components:
 * - Transcription preview, streaming text, router result
 * - Mic health warnings, USB cycling stage
 * - Overlay settings (scale, hybrid mode, live captions)
 * - Recording elapsed timer
 * - Refs for audio level tracking, timing, etc.
 *
 * Visibility and state machine logic are owned by useAppState, which
 * listens to `app-state` events from the Rust TranscriptionCoordinator.
 * This hook provides `resetRecordingState()` for use when the backend
 * transitions to Recording from Idle, resetting all mutable UI state.
 *
 * Scope: Shared overlay UI state only (not visibility).
 * Dependencies: @/bindings, @/i18n.
 * Side effects: Settings fetch on mount, recording elapsed timer, language sync.
 */
import { useEffect, useRef, useState } from "react";
import { commands } from "@/bindings";
import i18n, { syncLanguageFromSettings } from "@/i18n";
import { getLanguageDirection } from "@/lib/utils/rtl";
import type { TranscriptionSegment } from "@/lib/types/events";

/// Module-level cache for liveCaptionsEnabled so that subsequent recordings
/// can start with the cached value immediately instead of waiting for an
/// async fetch. The first recording after app launch may still miss captions
/// if the fetch hasn't resolved, but every recording after that will have the
/// value instantly available.
let cachedLiveCaptionsEnabled: boolean | undefined = undefined;

/// Overlay state phases — re-exported for use by sub-hooks and useAppState.
/// Visibility is derived from AppState (Idle = hidden, anything else = visible).
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

/// Router result event from backend
export interface RouterResultEvent {
  success: boolean;
  summary: string | null;
  error: string | null;
  transcription_text: string;
}

/// USB cycle stage type (used in state setters)
export type UsbCycleStage = { stage: string; message: string };

interface UseOverlaySharedStateReturn {
  // Overlay settings
  overlayScale: number;
  direction: "ltr" | "rtl";
  hybridEnabled: boolean;
  hybridThresholdSecs: number;
  recordingElapsedSecs: number;
  liveCaptionsEnabled: boolean;
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
  setStreamingSegments: React.Dispatch<React.SetStateAction<Array<TranscriptionSegment>>>;
  setRouterResult: React.Dispatch<React.SetStateAction<RouterResultEvent | null>>;
  setIsEditing: React.Dispatch<React.SetStateAction<boolean>>;
  setEditedText: React.Dispatch<React.SetStateAction<string>>;
  setCountdown: React.Dispatch<React.SetStateAction<number>>;
  setMicDeadWarning: React.Dispatch<React.SetStateAction<boolean>>;
  setLowAudioWarning: React.Dispatch<React.SetStateAction<boolean>>;
  setUsbCycleStage: React.Dispatch<React.SetStateAction<UsbCycleStage | null>>;
  setIsFadingOut: React.Dispatch<React.SetStateAction<boolean>>;
  // Refs shared across hooks
  lastLevelTimeRef: React.MutableRefObject<number>;
  recordingStartTimeRef: React.MutableRefObject<number>;
  lowAudioHistoryRef: React.MutableRefObject<number[]>;
  hadGoodAudioRef: React.MutableRefObject<boolean>;
  smoothedLevelsRef: React.MutableRefObject<number[]>;
  usbCyclingActiveRef: React.MutableRefObject<boolean>;
  transcriptionPreviewRef: React.MutableRefObject<string>;
  // Reset function: call when a new recording starts (transition to Recording)
  resetRecordingState: () => void;
  // Expose setState for sub-hooks that still need it (USB recovery, router preview)
  setState: React.Dispatch<React.SetStateAction<OverlayState>>;
  setIsVisible: React.Dispatch<React.SetStateAction<boolean>>;
  state: OverlayState;
  isVisible: boolean;
}

export function useOverlaySharedState(): UseOverlaySharedStateReturn {
  // ─── Visibility state — driven by useAppState in RecordingOverlay, ───
  // ─── but kept here for sub-hooks that need setIsVisible/setState.     ───
  // ─── These are set from RecordingOverlay via props, not from events.  ───
  const [isVisible, setIsVisible] = useState(false);
  const [state, setState] = useState<OverlayState>("recording");

  // Hybrid mode indicator state
  const [hybridEnabled, setHybridEnabled] = useState(false);
  const [hybridThresholdSecs, setHybridThresholdSecs] = useState(20);

  // Live captions setting — initialize from module cache if available
  const [liveCaptionsEnabled, setLiveCaptionsEnabled] = useState(
    cachedLiveCaptionsEnabled ?? false,
  );

  const [recordingElapsedSecs, setRecordingElapsedSecs] = useState(0);
  const recordingStartRef = useRef<number>(0);
  const elapsedTimerRef = useRef<ReturnType<typeof setInterval> | null>(null);

  // Overlay scale setting
  const [overlayScale, setOverlayScale] = useState(1.0);

  const direction = getLanguageDirection(i18n.language);

  // Refs shared across hooks (needed for reset when recording starts)
  const lastLevelTimeRef = useRef<number>(Date.now());
  const recordingStartTimeRef = useRef<number>(0);
  const lowAudioHistoryRef = useRef<number[]>([]);
  const hadGoodAudioRef = useRef<boolean>(false);
  const smoothedLevelsRef = useRef<number[]>(Array(16).fill(0));
  const usbCyclingActiveRef = useRef(false);

  // State setters needed by the reset logic
  const [transcriptionPreview, setTranscriptionPreview] = useState<string>("");
  const [streamingText, setStreamingText] = useState<string>("");
  const [streamingSegments, setStreamingSegments] = useState<TranscriptionSegment[]>([]);
  const [routerResult, setRouterResult] = useState<RouterResultEvent | null>(null);
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

  // ─── Reset function ─────────────────────────────────────────────────────
  // Called when the backend transitions to Recording from Idle.
  // Replaces the old show-overlay handler's reset logic.
  const resetRecordingState = () => {
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
  };

  // Proactively fetch live captions setting on mount so the cache is warm
  // before the first recording starts. Without this, the setting defaults
  // to false and partial-transcription events are ignored until the
  // isVisible-triggered fetch resolves.
  useEffect(() => {
    const prefetch = async () => {
      try {
        const result = await commands.getAppSettings();
        if (result.status === "ok" && result.data) {
          const enabled = result.data.live_captions_enabled ?? false;
          cachedLiveCaptionsEnabled = enabled;
          setLiveCaptionsEnabled(enabled);
          console.log("[Live Captions] Prefetched setting on mount:", enabled);
        }
      } catch {
        // Silently ignore — will retry when overlay becomes visible
      }
    };
    prefetch();
  }, []);

  // Fetch hybrid mode + live captions settings when overlay becomes visible
  useEffect(() => {
    if (!isVisible) return;
    const fetchSettings = async () => {
      try {
        const result = await commands.getAppSettings();
        if (result.status === "ok" && result.data) {
          setHybridEnabled(result.data.hybrid_mode_enabled ?? false);
          setHybridThresholdSecs(result.data.hybrid_threshold_secs ?? 20);
          const captionsEnabled = result.data.live_captions_enabled ?? false;
          // Update both React state and module-level cache
          cachedLiveCaptionsEnabled = captionsEnabled;
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

  // ─── Language sync on overlay show ─────────────────────────────────────
  // Sync language when overlay becomes visible. Previously this was done in
  // the show-overlay handler; now it's triggered by visibility from useAppState.
  useEffect(() => {
    if (!isVisible) return;
    syncLanguageFromSettings();
  }, [isVisible]);

  return {
    isVisible,
    setIsVisible,
    state,
    setState,
    overlayScale,
    direction,
    hybridEnabled,
    hybridThresholdSecs,
    recordingElapsedSecs,
    liveCaptionsEnabled,
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
    // Reset function
    resetRecordingState,
  };
}