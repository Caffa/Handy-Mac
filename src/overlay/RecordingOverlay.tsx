/**
 * RecordingOverlay — Main coordinator for the recording overlay window.
 *
 * This component composes extracted hooks and presentational components:
 * - useAppState: Sole source of truth for visibility and state machine.
 *   Listens to `app-state` events from the Rust TranscriptionCoordinator.
 *   Visibility is a pure function of AppState: Idle = hidden, else = visible.
 * - useOverlaySharedState: Shared mutable UI state (transcription preview,
 *   streaming text, mic warnings, etc.) and settings. Does NOT own visibility.
 * - useVisualizer: Audio level bars, mic health warnings
 * - useLiveCaptions: Streaming transcription display
 * - useRouterPreview: Router confirmation/editing flow
 * - useUSBRecovery: USB power cycling state
 *
 * Presentational components:
 * - VisualizerBars, LiveCaptionsBox, MicDeadWarning, USBCyclingProgress, RouterResultDisplay
 *
 * Scope: Coordination only — delegates all logic to hooks.
 * Dependencies: All hooks, all components, icons, i18n, commands.
 */
import React, { useCallback, useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import {
  CancelIcon,
  MicrophoneIcon,
  RoutingIcon,
  TranscriptionIcon,
} from "../components/icons";
import "./RecordingOverlay.css";
import { commands } from "@/bindings";
import { useOverlaySharedState } from "./hooks/useOverlayState";
import { useAppState } from "./hooks/useAppState";
import { useVisualizer } from "./hooks/useVisualizer";
import { useLiveCaptions } from "./hooks/useLiveCaptions";
import { useRouterPreview } from "./hooks/useRouterPreview";
import { useUSBRecovery } from "./hooks/useUSBRecovery";
import {
  VisualizerBars,
  LiveCaptionsBox,
  MicDeadWarning,
  USBCyclingProgress,
  RouterResultDisplay,
} from "./components";
import type { OverlayState } from "./hooks/useOverlayState";

const RecordingOverlay: React.FC = () => {
  const { t } = useTranslation();

  // ─── Backend-driven state — SOLE source of truth for visibility ──────
  const backendState = useAppState();

  // ─── Shared overlay UI state (NOT visibility) ───────────────────────
  const sharedState = useOverlaySharedState();
  const {
    setIsVisible,
    state: legacyState,
    setState,
    overlayScale,
    direction,
    hybridEnabled,
    hybridThresholdSecs,
    recordingElapsedSecs,
    // State owned by useOverlaySharedState, shared with sub-hooks
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
    liveCaptionsEnabled,
    // State setters needed by sub-hooks
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
    // Reset function for new recordings
    resetRecordingState,
  } = sharedState;

  // ─── Derived state from backend (sole source of truth) ──────────────
  const state: OverlayState = backendState.overlayState;
  const isRecording = backendState.isRecording;
  const isConfirming = backendState.isConfirming;
  const isUsbCycling = backendState.isUsbCycling;

  // BUGFIX (router regressions from commit 2255ae8):
  // After the router subprocess is spawned, FinishGuard drops immediately,
  // freeing the coordinator and setting backend state to Idle. At that point
  // backendState.isRouter becomes false (binding_id is null for Idle) even
  // though the router result hasn't arrived yet. Use routerResult as a
  // fallback signal — it's only non-null during router flows and persists
  // for the full 10-second display window.
  const isRouter = backendState.isRouter || routerResult !== null;
  const isVisible = backendState.isVisible || routerResult !== null;

  // ─── Sync backend visibility to legacy setIsVisible ─────────────────
  // Sub-hooks like useUSBRecovery and useRouterPreview still use
  // setIsVisible for edge cases (USB cycling flash, router result timeout).
  // Keep it in sync with the backend authority.
  useEffect(() => {
    setIsVisible(backendState.isVisible);
  }, [backendState.isVisible, setIsVisible]);

  // ─── Sync backend state to legacy setState ───────────────────────────
  // Sub-hooks like useUSBRecovery and useRouterPreview still use setState
  // for USB cycling transitions and router result timeout. Keep it in
  // sync with the backend authority.
  useEffect(() => {
    setState(backendState.overlayState);
  }, [backendState.overlayState, setState]);

  // ─── Reset on new recording ──────────────────────────────────────────
  // When the backend transitions to Recording from any other state,
  // reset all mutable UI state (streaming text, warnings, etc.).
  const prevStateRef = useRef(backendState.appState.state);
  useEffect(() => {
    const currentState = backendState.appState.state;
    const prevState = prevStateRef.current;
    prevStateRef.current = currentState;

    if (currentState === "Recording" && prevState !== "Recording") {
      resetRecordingState();
    }
  }, [backendState.appState.state, resetRecordingState]);

  // ─── Visualizer (audio levels + mic warnings) ──────────────────────────
  const { levels } = useVisualizer({
    state,
    isVisible,
    isRecording,
    lastLevelTimeRef,
    recordingStartTimeRef,
    lowAudioHistoryRef,
    hadGoodAudioRef,
    smoothedLevelsRef,
    setMicDeadWarning,
    setLowAudioWarning,
  });

  // ─── Live captions (streaming transcription) ──────────────────────────
  useLiveCaptions({
    state,
    isVisible,
    isRecording,
    liveCaptionsEnabled,
    micDeadWarning,
    lowAudioWarning,
    streamingText,
    setStreamingText,
    setStreamingSegments,
  });

  // ─── Router preview (confirmation, editing, result) ───────────────────
  const {
    handleTranscriptionClick,
    handleSendEdited,
    handleCancelEdit,
    handleEditedTextChange,
    textareaRef,
  } = useRouterPreview({
    state,
    isVisible,
    setState,
    setIsVisible,
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
  });

  // ─── USB recovery (power cycling state) ───────────────────────────────
  const { usbCyclingElapsed } = useUSBRecovery({
    state,
    setState,
    setIsVisible,
    isUsbCycling,
    setMicDeadWarning,
    setLowAudioWarning,
    setUsbCycleStage,
    usbCyclingActiveRef,
    smoothedLevelsRef,
    recordingStartTimeRef,
    lowAudioHistoryRef,
    hadGoodAudioRef,
  });

  // ─── Cancel handler ───────────────────────────────────────────────────
  // Send cancel to the backend. The backend will emit AppState::Idle,
  // which useAppState will pick up and set isVisible=false.
  // No need to set visibility locally — the state machine handles it.
  const handleCancel = useCallback(async () => {
    try {
      await commands.cancelOperation();
    } catch (err) {
      console.error("[Overlay] cancelOperation command failed:", err);
    }
    // Reset local UI state as a precaution, but do NOT set visibility.
    // The backend's cancelOperation will emit AppState::Idle which
    // useAppState will receive, setting isVisible=false.
    setStreamingText("");
    setTranscriptionPreview("");
  }, [setStreamingText, setTranscriptionPreview]);

  // ─── Icon selection ───────────────────────────────────────────────────
  const getIcon = () => {
    if (isRouter) {
      const iconColor = "#3b82f6";
      return <RoutingIcon color={iconColor} width={30} height={30} />;
    }
    if (state === "recording") {
      return <MicrophoneIcon width={30} height={30} />;
    }
    return <TranscriptionIcon width={30} height={30} />;
  };

  // ─── Overlay class names ──────────────────────────────────────────────
  const getOverlayClassNames = (): string => {
    const classes = ["recording-overlay"];
    if (isVisible) classes.push("fade-in");
    if (isRouter) classes.push("routing-mode");
    if ((micDeadWarning || lowAudioWarning) && state === "recording")
      classes.push("mic-dead-overlay");
    if (state === "usb-cycling") classes.push("usb-cycling-overlay");
    if (isEditing && state === "confirming") classes.push("editing-overlay");
    return classes.join(" ");
  };

  // ─── Render ────────────────────────────────────────────────────────────
  return (
    <>
      <div
        dir={direction}
        className={getOverlayClassNames()}
        style={{ "--overlay-scale": overlayScale } as React.CSSProperties}
      >
        <div className="overlay-left">{getIcon()}</div>

        <div className="overlay-middle">
          {/* Mic dead or low audio warning - only show during recording */}
          {backendState.appState.state === "Recording" && (
            <MicDeadWarning
              micDeadWarning={micDeadWarning}
              lowAudioWarning={lowAudioWarning}
            />
          )}
          {/* Only show warning or visualizer, not both */}
          {(micDeadWarning || lowAudioWarning) && backendState.appState.state === "Recording"
            ? null
            : backendState.appState.state === "Recording" && (
                <VisualizerBars levels={levels} isRouter={isRouter} />
              )}
          {/* Processing state: "Filing..." for router, "Processing..." for normal */}
          {state === "processing" && (
            <div
              className={`transcribing-text${isRouter ? " routing-text" : ""}`}
            >
              {isRouter ? t("overlay.filing") : t("overlay.processing")}
            </div>
          )}
          {state === "usb-cycling" && (
            <USBCyclingProgress
              usbCycleStage={usbCycleStage}
              usbCyclingElapsed={usbCyclingElapsed}
            />
          )}
          {/* Confirming state */}
          {state === "confirming" && !routerResult && (
            <div className="confirming-text">
              {isEditing
                ? t("overlay.editing", "Edit text:")
                : t("overlay.confirming", "Sending in")}
              {!isEditing && countdown > 0 && (
                <span className="countdown-timer">
                  {`${Math.ceil(countdown / 1000)}s`}
                </span>
              )}
            </div>
          )}
        </div>

        <div className="overlay-right">
          {backendState.appState.state === "Recording" && (
            <div
              className="cancel-button"
              onClick={handleCancel}
              onMouseEnter={() => commands.setOverlayMousePassthrough(true).catch(() => {})}
              onMouseLeave={() => commands.setOverlayMousePassthrough(false).catch(() => {})}
            >
              <CancelIcon
                width={33}
                height={33}
                color={isRouter ? "#3b82f6" : undefined}
              />
            </div>
          )}

        </div>
      </div>

      {/* Live captions — decoupled from mic warnings: captions and mic warnings
          are independent concerns. The MicDeadWarning component is already
          rendered separately above, so removing these gates here does NOT
          suppress the mic warning. Previously, lowAudioWarning (triggered
          during 2+ second speech pauses) would kill captions even though
          transcription was still producing text.
          Hide when router result is showing — the handler cards replace them. */}
      {isVisible &&
        backendState.appState.state === "Recording" &&
        liveCaptionsEnabled &&
        streamingText &&
        streamingText.trim() &&
        !routerResult && (
          <LiveCaptionsBox
            text={streamingText}
            direction={direction}
            overlayScale={overlayScale}
            isRouter={isRouter}
          />
        )}

      {/* Router result display / transcription preview */}
      {isRouter && (transcriptionPreview || routerResult) && (
        <RouterResultDisplay
          routerResult={routerResult}
          isEditing={isEditing}
          isFadingOut={isFadingOut}
          transcriptionPreview={transcriptionPreview}
          editedText={editedText}
          countdown={countdown}
          direction={direction}
          overlayScale={overlayScale}
          textareaRef={textareaRef}
          onTranscriptionClick={handleTranscriptionClick}
          onSendEdited={handleSendEdited}
          onCancelEdit={handleCancelEdit}
          onEditedTextChange={handleEditedTextChange}
        />
      )}
    </>
  );
};

export default RecordingOverlay;