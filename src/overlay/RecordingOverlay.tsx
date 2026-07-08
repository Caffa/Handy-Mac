/**
 * RecordingOverlay — Main coordinator for the recording overlay window.
 *
 * This component composes extracted hooks and presentational components:
 * - useOverlayState: Visibility, state machine, settings, and all shared state
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
import React, { useCallback, useMemo } from "react";
import { useTranslation } from "react-i18next";
import {
  CancelIcon,
  MicrophoneIcon,
  RoutingIcon,
  TranscriptionIcon,
} from "../components/icons";
import "./RecordingOverlay.css";
import { commands } from "@/bindings";
import { useOverlayState } from "./hooks/useOverlayState";
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

  // ─── Core overlay state (legacy, kept for migration) ─────────────────
  const overlayState = useOverlayState();
  const {
    isVisible,
    setIsVisible,
    state: legacyState,
    setState,
    isRouter: legacyIsRouter,
    overlayScale,
    direction,
    hybridEnabled,
    hybridThresholdSecs,
    recordingElapsedSecs,
    // State owned by useOverlayState, shared with sub-hooks
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
  } = overlayState;

  // ─── Backend-driven state (new, gradually becomes primary) ──────────
  const backendState = useAppState();

  // ─── Dual-listening: prefer backend state when available ────────────
  // During migration, both sources are active. We use the backend state
  // as the primary source when it reports a non-Idle state, and fall
  // back to the legacy overlay state otherwise. This ensures no
  // regressions during the transition period.
  const state: OverlayState = useMemo(() => {
    if (backendState.isVisible) {
      return backendState.overlayState;
    }
    return legacyState;
  }, [backendState.isVisible, backendState.overlayState, legacyState]);

  const isRouter = useMemo(() => {
    // During migration: prefer backend's isRouter when backend reports a Recording
    // state (which carries binding_id). For Processing/Confirming/UsbCycling states,
    // the backend doesn't carry mode info, so fall back to legacy isRouter from
    // the show-overlay event payload.
    if (backendState.isVisible && backendState.isRecording) {
      return backendState.isRouter;
    }
    return legacyIsRouter;
  }, [backendState.isVisible, backendState.isRecording, backendState.isRouter, legacyIsRouter]);

  // ─── Visualizer (audio levels + mic warnings) ──────────────────────────
  // During migration: use isRecording from backend state when available
  const backendIsRecording = backendState.isVisible
    ? backendState.isRecording
    : state === "recording";

  const { levels } = useVisualizer({
    state,
    isVisible,
    isRecording: backendIsRecording,
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
    isRecording: backendIsRecording,
    liveCaptionsEnabled,
    micDeadWarning,
    lowAudioWarning,
    streamingText,
    setStreamingText,
    setStreamingSegments,
  });

  // ─── Router preview (confirmation, editing, result) ───────────────────
  // During migration: use isConfirming from backend state when available
  const backendIsConfirming = backendState.isVisible
    ? backendState.isConfirming
    : state === "confirming";

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
    isConfirming: backendIsConfirming,
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
  // During migration: use isUsbCycling from backend state when available
  const backendIsUsbCycling = backendState.isVisible
    ? backendState.isUsbCycling
    : state === "usb-cycling";

  const { usbCyclingElapsed } = useUSBRecovery({
    state,
    setState,
    setIsVisible,
    isUsbCycling: backendIsUsbCycling,
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
  const handleCancel = useCallback(async () => {
    try {
      await commands.cancelOperation();
    } catch (err) {
      console.error("[Overlay] cancelOperation command failed:", err);
    }
    // ALWAYS reset local UI state as a fallback, even if the backend
    // command failed or crashed. This ensures the overlay hides.
    setIsVisible(false);
    setStreamingText("");
    setTranscriptionPreview("");
  }, [setIsVisible, setStreamingText, setTranscriptionPreview]);

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
          {state === "recording" && (
            <MicDeadWarning
              micDeadWarning={micDeadWarning}
              lowAudioWarning={lowAudioWarning}
            />
          )}
          {/* Only show warning or visualizer, not both */}
          {(micDeadWarning || lowAudioWarning) && state === "recording"
            ? null
            : state === "recording" && (
                <VisualizerBars levels={levels} isRouter={isRouter} />
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
          {state === "recording" && (
            <div className="cancel-button" onClick={handleCancel}>
              <CancelIcon
                width={33}
                height={33}
                color={isRouter ? "#3b82f6" : undefined}
              />
            </div>
          )}

        </div>
      </div>

      {/* Live captions */}
      {isVisible &&
        state === "recording" &&
        liveCaptionsEnabled &&
        !micDeadWarning &&
        !lowAudioWarning &&
        streamingText &&
        streamingText.trim() && (
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
