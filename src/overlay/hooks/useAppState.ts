/**
 * useAppState — Reactive frontend state derived from the backend's
 * single source of truth. Listens to "app-state" events emitted by
 * the Rust TranscriptionCoordinator.
 *
 * This hook is the SOLE authority for overlay visibility and state.
 * Visibility is a pure function of AppState: Idle = hidden, anything
 * else = visible. The useOverlaySharedState hook manages mutable UI
 * state (transcription preview, streaming text, etc.) but does NOT
 * own visibility.
 *
 * Scope: Backend-driven app state (recording, processing, etc.).
 * Dependencies: @tauri-apps/api/event.
 * Side effects: Event listener for "app-state".
 */
import { listen } from "@tauri-apps/api/event";
import { useEffect, useRef, useState } from "react";
import type { OverlayState } from "./useOverlayState";

// ─── Mirror the Rust AppState enum ────────────────────────────────────

/** Idle — no active operation */
export type AppStateIdle = { state: "Idle" };

/** Recording — actively capturing audio */
export type AppStateRecording = {
  state: "Recording";
  data: { binding_id: string };
};

/** Processing — transcribing or routing (binding_id identifies the originating action) */
export type AppStateProcessing = {
  state: "Processing";
  data: { binding_id: string | null };
};

/** UsbCycling — USB device power cycling */
export type AppStateUsbCycling = {
  state: "UsbCycling";
  data: { stage: string };
};

/** Confirming — awaiting user confirmation (router), binding_id identifies the originating action */
export type AppStateConfirming = {
  state: "Confirming";
  data: { text: string; binding_id: string | null };
};

/** Union type mirroring the Rust AppState enum */
export type AppState =
  | AppStateIdle
  | AppStateRecording
  | AppStateProcessing
  | AppStateUsbCycling
  | AppStateConfirming;

// ─── Type guards ──────────────────────────────────────────────────────

export function isIdle(s: AppState): s is AppStateIdle {
  return s.state === "Idle";
}

export function isRecording(s: AppState): s is AppStateRecording {
  return s.state === "Recording";
}

export function isProcessing(s: AppState): s is AppStateProcessing {
  return s.state === "Processing";
}

export function isUsbCycling(s: AppState): s is AppStateUsbCycling {
  return s.state === "UsbCycling";
}

export function isConfirming(s: AppState): s is AppStateConfirming {
  return s.state === "Confirming";
}

// ─── Map AppState to OverlayState ─────────────────────────────────────

/**
 * Maps the backend-driven AppState to the OverlayState string used by
 * sub-hooks and presentational components. Now that useAppState is the
 * sole visibility authority, this mapping is the canonical way to convert
 * backend state to the frontend OverlayState type.
 *
 * Note: "Processing" maps to "processing" (not "transcribing") because
 * the backend does not distinguish the two — the frontend can infer
 * "transcribing" from context if needed.
 */
export function appStateToOverlayState(appState: AppState): OverlayState {
  switch (appState.state) {
    case "Recording":
      return "recording";
    case "Processing":
      return "processing";
    case "UsbCycling":
      return "usb-cycling";
    case "Confirming":
      return "confirming";
    case "Idle":
    default:
      // When Idle, the overlay should not be visible, so the state
      // value doesn't matter much. Return "recording" as a safe default
      // that matches the initial state in useOverlaySharedState.
      return "recording";
  }
}

// ─── Hook return type ─────────────────────────────────────────────────

export interface UseAppStateReturn {
  /** Raw AppState from the backend */
  appState: AppState;
  /** Ref-stored AppState for access in callbacks without stale closures */
  appStateRef: React.MutableRefObject<AppState>;
  /** OverlayState mapping from backend AppState (canonical state type) */
  overlayState: OverlayState;
  /** Whether the overlay should be visible (any state other than Idle) */
  isVisible: boolean;
  /** Whether the backend reports an Idle state */
  isIdle: boolean;
  /** Whether the backend reports an active Recording */
  isRecording: boolean;
  /** Whether the backend reports Processing */
  isProcessing: boolean;
  /** Whether the backend reports UsbCycling */
  isUsbCycling: boolean;
  /** Whether the backend reports Confirming (router) */
  isConfirming: boolean;
  /** The binding_id when Recording (e.g. "transcribe_with_router"), or null */
  bindingId: string | null;
  /** Whether the recording is a router binding */
  isRouter: boolean;
  /** The router text when Confirming, or null */
  routerText: string | null;
  /** The USB cycling stage when UsbCycling, or null */
  usbStage: string | null;
}

// ─── Hook ─────────────────────────────────────────────────────────────

export function useAppState(): UseAppStateReturn {
  const [appState, setAppState] = useState<AppState>({ state: "Idle" });
  const appStateRef = useRef<AppState>(appState);

  // Keep ref in sync for callback access
  useEffect(() => {
    appStateRef.current = appState;
  }, [appState]);

  // Listen for app-state events from the Rust backend
  useEffect(() => {
    let unlistenFn: (() => void) | null = null;

    const setup = async () => {
      const unlisten = await listen<AppState>("app-state", (event) => {
        console.log("[AppState] Received:", JSON.stringify(event.payload));
        setAppState(event.payload);
      });
      unlistenFn = unlisten;
    };

    setup();

    return () => {
      unlistenFn?.();
    };
  }, []);

  // ─── Derived values ──────────────────────────────────────────────

  const overlayState = appStateToOverlayState(appState);
  const isVisible = appState.state !== "Idle";
  const isIdle = appState.state === "Idle";
  const isRecordingState = appState.state === "Recording";
  const isProcessingState = appState.state === "Processing";
  const isUsbCyclingState = appState.state === "UsbCycling";
  const isConfirmingState = appState.state === "Confirming";

  // Extract data from variant states.
  // binding_id persists across Recording → Processing → Confirming states
  // so isRouter remains true throughout the entire router flow.
  const bindingId = (() => {
    switch (appState.state) {
      case "Recording":
        return appState.data.binding_id;
      case "Processing":
        return appState.data.binding_id;
      case "Confirming":
        return appState.data.binding_id;
      default:
        return null;
    }
  })();
  const isRouter = bindingId === "transcribe_with_router";
  const routerText =
    appState.state === "Confirming" ? appState.data.text : null;
  const usbStage = appState.state === "UsbCycling" ? appState.data.stage : null;

  return {
    appState,
    appStateRef,
    overlayState,
    isVisible,
    isIdle,
    isRecording: isRecordingState,
    isProcessing: isProcessingState,
    isUsbCycling: isUsbCyclingState,
    isConfirming: isConfirmingState,
    bindingId,
    isRouter,
    routerText,
    usbStage,
  };
}