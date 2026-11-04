import type { RouterResultEvent } from "../hooks/useOverlayState";

/** AppState.state values as emitted by the Rust backend. */
type BackendStateKind =
  | "Idle"
  | "Recording"
  | "Processing"
  | "UsbCycling"
  | "Confirming";

export interface OverlayViewInputs {
  /** Backend AppState kind from useAppState. */
  backendStateKind: BackendStateKind;
  /** backendState.isVisible */
  backendIsVisible: boolean;
  /** backendState.isRouter */
  backendIsRouter: boolean;
  /** The router result payload, if one has arrived. */
  routerResult: RouterResultEvent | null;
}

export interface OverlayView {
  /** Whether the backend is actively recording. */
  isRecording: boolean;
  /** Whether a router result should be displayed (false while recording). */
  routerResultActive: boolean;
  /** Whether the overlay window should be visible. */
  isVisible: boolean;
  /** Whether router styling / icons should be used. */
  isRouter: boolean;
  /** Whether the recording pill (visualizer + cancel) should render. */
  showPill: boolean;
}

/**
 * Pure view-state arbitration for the recording overlay.
 *
 * Rule: an active recording's pill/visualizer/cancel button always wins over a
 * stale router result. When no recording is active, behavior is identical to
 * the pre-fix derivation (`routerResult !== null` drives visibility/router mode).
 */
export function deriveOverlayView(i: OverlayViewInputs): OverlayView {
  const isRecording = i.backendStateKind === "Recording";
  const routerResultActive = i.routerResult !== null && !isRecording;
  const isVisible = i.backendIsVisible || routerResultActive;
  const isRouter = i.backendIsRouter || routerResultActive;
  const showPill = isVisible && !routerResultActive;

  return { isRecording, routerResultActive, isVisible, isRouter, showPill };
}
