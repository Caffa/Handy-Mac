/**
 * useRouterPreview — Router result preview, confirmation countdown, and editing.
 *
 * Manages the router confirmation flow:
 * - transcriptionPreview: The transcribed text shown for confirmation
 * - routerResult: Result from the router (success/failure)
 * - countdown: Auto-send countdown timer
 * - isEditing: Whether user is editing the transcription
 * - editedText: The edited text value
 * - isFadingOut: Whether the preview is fading out
 *
 * The state values are owned by the parent (useOverlayState) and updated via
 * setters — this ensures that resets from show-overlay events propagate correctly.
 *
 * Scope: Router confirmation flow (preview → edit → send).
 * Dependencies: @tauri-apps/api/event, @/bindings.
 * Side effects: Event listeners for transcription-preview, router-result, routing-state;
 *               countdown interval, fade-out timer, router result timeout.
 */
import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useRef } from "react";
import { commands } from "@/bindings";
import type { OverlayState, RouterResultEvent } from "./useOverlayState";

/// Countdown timer for routing confirmation (in milliseconds)
const ROUTING_COUNTDOWN_MS = 4500;

/// Time to display router result before auto-hiding
const ROUTER_RESULT_DISPLAY_MS = 10_000;

interface UseRouterPreviewOptions {
  state: OverlayState;
  isVisible: boolean;
  setState: React.Dispatch<React.SetStateAction<OverlayState>>;
  setIsVisible: React.Dispatch<React.SetStateAction<boolean>>;
  isRouter: boolean;
  /** During migration: backend-derived isConfirming overrides state === "confirming" */
  isConfirming?: boolean;
  transcriptionPreview: string;
  transcriptionPreviewRef: React.MutableRefObject<string>;
  routerResult: RouterResultEvent | null;
  isEditing: boolean;
  editedText: string;
  countdown: number;
  isFadingOut: boolean;
  setTranscriptionPreview: React.Dispatch<React.SetStateAction<string>>;
  setRouterResult: React.Dispatch<
    React.SetStateAction<RouterResultEvent | null>
  >;
  setIsEditing: React.Dispatch<React.SetStateAction<boolean>>;
  setEditedText: React.Dispatch<React.SetStateAction<string>>;
  setCountdown: React.Dispatch<React.SetStateAction<number>>;
  setIsFadingOut: React.Dispatch<React.SetStateAction<boolean>>;
}

interface UseRouterPreviewReturn {
  sendRoutingConfirmation: (text: string) => Promise<void>;
  handleTranscriptionClick: () => void;
  handleSendEdited: () => void;
  handleCancelEdit: () => void;
  handleEditedTextChange: (text: string) => void;
  textareaRef: React.RefObject<HTMLTextAreaElement>;
}

export function useRouterPreview(
  options: UseRouterPreviewOptions,
): UseRouterPreviewReturn {
  const {
    state,
    setIsVisible,
    setState,
    isRouter,
    isConfirming,
    transcriptionPreview,
    transcriptionPreviewRef,
    routerResult,
    isEditing,
    editedText,
    countdown,
    isFadingOut,
    setTranscriptionPreview,
    setRouterResult,
    setIsEditing,
    setEditedText,
    setCountdown,
    setIsFadingOut,
  } = options;

  // During migration: prefer backend-derived isConfirming when provided,
  // otherwise fall back to state === "confirming".
  // Also consider transcriptionPreview being non-empty as a confirming signal —
  // this bridges the timing gap where transcription-preview arrives before the
  // app-state Confirming event, so the user can click to edit immediately.
  const effectivelyConfirming =
    isConfirming ?? (state === "confirming" || !!transcriptionPreview);

  const countdownRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const fadeOutTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  // Keep transcriptionPreviewRef in sync
  useEffect(() => {
    transcriptionPreviewRef.current = transcriptionPreview;
  }, [transcriptionPreview]);

  // Send routing confirmation to backend
  const sendRoutingConfirmation = useCallback(async (text: string) => {
    try {
      await commands.confirmRouting(text);
    } catch (e) {
      console.error("Failed to send routing confirmation:", e);
    }
  }, []);

  // Handle clicking on transcription to enter edit mode
  const handleTranscriptionClick = useCallback(() => {
    if (effectivelyConfirming && !isEditing) {
      setIsEditing(true);
      setEditedText(transcriptionPreviewRef.current);
      if (countdownRef.current) {
        clearInterval(countdownRef.current);
        countdownRef.current = null;
      }
    }
  }, [effectivelyConfirming, isEditing]);

  // Handle sending edited text
  const handleSendEdited = useCallback(() => {
    setIsEditing(false);
    sendRoutingConfirmation(editedText);
  }, [editedText, sendRoutingConfirmation]);

  // Handle cancel during editing
  const handleCancelEdit = useCallback(() => {
    setIsEditing(false);
    setEditedText("");
    setCountdown(ROUTING_COUNTDOWN_MS);
  }, []);

  // Handle edited text change
  const handleEditedTextChange = useCallback((text: string) => {
    setEditedText(text);
  }, []);

  // Countdown timer for routing confirmation
  useEffect(() => {
    if (effectivelyConfirming && !isEditing && countdown > 0) {
      countdownRef.current = setInterval(() => {
        setCountdown((prev) => {
          if (prev <= 100) {
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
  }, [effectivelyConfirming, isEditing, countdown]);

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

  // Toggle overlay key window status for keyboard input on macOS.
  // When editing, the overlay must accept keyboard input (can_become_key_window=true)
  // so the user can type in the textarea. When editing ends, restore the default
  // behavior (can_become_key_window=false) so the overlay doesn't steal focus.
  useEffect(() => {
    const enableKeyWindow = async () => {
      try {
        await commands.setOverlayCanBecomeKey(isEditing);
      } catch (e) {
        console.error("Failed to set overlay key window status:", e);
      }
    };
    enableKeyWindow();

    // Cleanup: ensure we restore can_become_key=false when the component unmounts
    // or when editing ends.
    return () => {
      if (isEditing) {
        commands.setOverlayCanBecomeKey(false).catch((e) => {
          console.error("Failed to restore overlay key window status:", e);
        });
      }
    };
  }, [isEditing]);

  // Router result display timeout with race condition fix
  useEffect(() => {
    if (routerResult) {
      const timeout = setTimeout(() => {
        setRouterResult(null);
        setState((current) => {
          if (
            current === "recording" ||
            current === "transcribing" ||
            current === "processing" ||
            current === "confirming"
          ) {
            console.log("[Router] Keeping overlay visible: active state =", current);
            return current;
          }
          console.log("[Router] Hiding overlay after result display, state =", current);
          setIsVisible(false);
          setTranscriptionPreview("");
          setCountdown(0);
          return current;
        });
      }, ROUTER_RESULT_DISPLAY_MS);

      return () => clearTimeout(timeout);
    }
  }, [routerResult]);

  // Handle transcription preview fade-out when entering processing state
  useEffect(() => {
    if (fadeOutTimerRef.current) {
      clearTimeout(fadeOutTimerRef.current);
      fadeOutTimerRef.current = null;
    }

    if (state === "processing" && transcriptionPreview && !routerResult) {
      fadeOutTimerRef.current = setTimeout(() => {
        setIsFadingOut(true);
        setTimeout(() => {
          setTranscriptionPreview("");
          setIsFadingOut(false);
        }, 100);
      }, 2000);
    } else {
      setIsFadingOut(false);
    }

    return () => {
      if (fadeOutTimerRef.current) {
        clearTimeout(fadeOutTimerRef.current);
      }
    };
  }, [state, transcriptionPreview, routerResult]);

  // Event listeners for transcription preview, router result, routing state
  useEffect(() => {
    let unlistenTranscriptionPreview: (() => void) | null = null;
    let unlistenRouterResult: (() => void) | null = null;
    let unlistenRoutingState: (() => void) | null = null;

    const setup = async () => {
      unlistenTranscriptionPreview = await listen<string>(
        "transcription-preview",
        (event) => {
          setTranscriptionPreview(event.payload);
          setCountdown(ROUTING_COUNTDOWN_MS);
          setState("confirming");
          setIsEditing(false);
          setEditedText("");
          setIsFadingOut(false);
        },
      );

      unlistenRoutingState = await listen<string>("routing-state", (event) => {
        const newState = event.payload;
        if (newState === "processing") {
          setState("processing");
        }
      });

      unlistenRouterResult = await listen<RouterResultEvent>(
        "router-result",
        (event) => {
          setRouterResult(event.payload);
        },
      );
    };

    setup();

    return () => {
      unlistenTranscriptionPreview?.();
      unlistenRouterResult?.();
      unlistenRoutingState?.();
    };
  }, []);

  return {
    sendRoutingConfirmation,
    handleTranscriptionClick,
    handleSendEdited,
    handleCancelEdit,
    handleEditedTextChange,
    textareaRef,
  };
}
