/**
 * Barrel exports for overlay hooks.
 */
export { useOverlaySharedState } from "./useOverlayState";
export type {
  OverlayState,
  OverlayAction,
  RouterResultEvent,
} from "./useOverlayState";
export { parseOverlayPayload } from "./useOverlayState";

export { useAppState } from "./useAppState";
export type {
  AppState,
  AppStateIdle,
  AppStateRecording,
  AppStateProcessing,
  AppStateUsbCycling,
  AppStateConfirming,
  UseAppStateReturn,
} from "./useAppState";
export {
  isIdle,
  isRecording as isAppStateRecording,
  isProcessing,
  isUsbCycling as isAppStateUsbCycling,
  isConfirming as isAppStateConfirming,
  appStateToOverlayState,
} from "./useAppState";

export { useVisualizer } from "./useVisualizer";
export { useLiveCaptions } from "./useLiveCaptions";
export { useRouterPreview } from "./useRouterPreview";
export { useUSBRecovery } from "./useUSBRecovery";