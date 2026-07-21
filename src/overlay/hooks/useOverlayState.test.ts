/**
 * Tests for useOverlaySharedState — shared overlay state (NOT visibility).
 *
 * Covers:
 * - Initial state values
 * - State updates via setters (streaming text, warnings, mic levels)
 * - Reset behavior (resetRecordingState)
 * - parseOverlayPayload utility
 * - Settings fetch on mount / visibility change
 */
import { renderHook, act } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import {
  useOverlaySharedState,
  parseOverlayPayload,
  type OverlayState,
  type OverlayAction,
  type RouterResultEvent,
} from "./useOverlayState";
import { commands } from "@/bindings";

describe("parseOverlayPayload", () => {
  it("parses state-only payload (no colon)", () => {
    const result = parseOverlayPayload("recording");
    expect(result).toEqual({ state: "recording", action: "transcribe" });
  });

  it("parses compound state:action payload", () => {
    const result = parseOverlayPayload("confirming:router");
    expect(result).toEqual({ state: "confirming", action: "router" });
  });

  it("parses processing:transcribe payload", () => {
    const result = parseOverlayPayload("processing:transcribe");
    expect(result).toEqual({ state: "processing", action: "transcribe" });
  });

  it("parses streaming:post_process payload", () => {
    const result = parseOverlayPayload("streaming:post_process");
    expect(result).toEqual({ state: "streaming", action: "post_process" });
  });

  it("handles payload with multiple colons (only splits on first)", () => {
    const result = parseOverlayPayload("confirming:router:something");
    expect(result.state).toBe("confirming");
    expect(result.action).toBe("router:something");
  });
});

describe("useOverlaySharedState", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  describe("initial state values", () => {
    it("returns default state values", () => {
      const { result } = renderHook(() => useOverlaySharedState());

      expect(result.current.isVisible).toBe(false);
      expect(result.current.state).toBe("recording");
      expect(result.current.transcriptionPreview).toBe("");
      expect(result.current.streamingText).toBe("");
      expect(result.current.routerResult).toBeNull();
      expect(result.current.isEditing).toBe(false);
      expect(result.current.editedText).toBe("");
      expect(result.current.countdown).toBe(0);
      expect(result.current.isFadingOut).toBe(false);
      expect(result.current.micDeadWarning).toBe(false);
      expect(result.current.lowAudioWarning).toBe(false);
      expect(result.current.usbCycleStage).toBeNull();
      expect(result.current.overlayScale).toBe(1.0);
      expect(result.current.hybridEnabled).toBe(false);
      expect(result.current.hybridThresholdSecs).toBe(20);
      expect(result.current.liveCaptionsEnabled).toBe(false);
      expect(result.current.recordingElapsedSecs).toBe(0);
      expect(result.current.direction).toBe("ltr");
    });

    it("returns all setter functions", () => {
      const { result } = renderHook(() => useOverlaySharedState());

      expect(typeof result.current.setTranscriptionPreview).toBe("function");
      expect(typeof result.current.setStreamingText).toBe("function");
      expect(typeof result.current.setStreamingSegments).toBe("function");
      expect(typeof result.current.setRouterResult).toBe("function");
      expect(typeof result.current.setIsEditing).toBe("function");
      expect(typeof result.current.setEditedText).toBe("function");
      expect(typeof result.current.setCountdown).toBe("function");
      expect(typeof result.current.setMicDeadWarning).toBe("function");
      expect(typeof result.current.setLowAudioWarning).toBe("function");
      expect(typeof result.current.setUsbCycleStage).toBe("function");
      expect(typeof result.current.setIsFadingOut).toBe("function");
      expect(typeof result.current.setState).toBe("function");
      expect(typeof result.current.setIsVisible).toBe("function");
    });

    it("returns ref objects", () => {
      const { result } = renderHook(() => useOverlaySharedState());

      expect(result.current.lastLevelTimeRef).toHaveProperty("current");
      expect(result.current.recordingStartTimeRef).toHaveProperty("current");
      expect(result.current.lowAudioHistoryRef).toHaveProperty("current");
      expect(result.current.hadGoodAudioRef).toHaveProperty("current");
      expect(result.current.smoothedLevelsRef).toHaveProperty("current");
      expect(result.current.usbCyclingActiveRef).toHaveProperty("current");
      expect(result.current.transcriptionPreviewRef).toHaveProperty("current");
    });

    it("initializes smoothedLevelsRef with 16 zeros", () => {
      const { result } = renderHook(() => useOverlaySharedState());
      expect(result.current.smoothedLevelsRef.current).toEqual(
        Array(16).fill(0),
      );
    });

    it("initializes lowAudioHistoryRef as empty array", () => {
      const { result } = renderHook(() => useOverlaySharedState());
      expect(result.current.lowAudioHistoryRef.current).toEqual([]);
    });
  });

  describe("state updates", () => {
    it("updates transcriptionPreview via setter", () => {
      const { result } = renderHook(() => useOverlaySharedState());

      act(() => {
        result.current.setTranscriptionPreview("Hello world");
      });

      expect(result.current.transcriptionPreview).toBe("Hello world");
    });

    it("updates streamingText via setter", () => {
      const { result } = renderHook(() => useOverlaySharedState());

      act(() => {
        result.current.setStreamingText("Streaming text...");
      });

      expect(result.current.streamingText).toBe("Streaming text...");
    });

    it("updates routerResult via setter", () => {
      const { result } = renderHook(() => useOverlaySharedState());
      const routerResult: RouterResultEvent = {
        success: true,
        summary: "Test summary",
        error: null,
        transcription_text: "Test text",
      };

      act(() => {
        result.current.setRouterResult(routerResult);
      });

      expect(result.current.routerResult).toEqual(routerResult);
    });

    it("updates micDeadWarning via setter", () => {
      const { result } = renderHook(() => useOverlaySharedState());

      act(() => {
        result.current.setMicDeadWarning(true);
      });

      expect(result.current.micDeadWarning).toBe(true);
    });

    it("updates lowAudioWarning via setter", () => {
      const { result } = renderHook(() => useOverlaySharedState());

      act(() => {
        result.current.setLowAudioWarning(true);
      });

      expect(result.current.lowAudioWarning).toBe(true);
    });

    it("updates usbCycleStage via setter", () => {
      const { result } = renderHook(() => useOverlaySharedState());
      const stage = { stage: "power_cycle", message: "Cycling USB power" };

      act(() => {
        result.current.setUsbCycleStage(stage);
      });

      expect(result.current.usbCycleStage).toEqual(stage);
    });

    it("updates isEditing and editedText", () => {
      const { result } = renderHook(() => useOverlaySharedState());

      act(() => {
        result.current.setIsEditing(true);
        result.current.setEditedText("Edited text");
      });

      expect(result.current.isEditing).toBe(true);
      expect(result.current.editedText).toBe("Edited text");
    });

    it("updates countdown", () => {
      const { result } = renderHook(() => useOverlaySharedState());

      act(() => {
        result.current.setCountdown(4500);
      });

      expect(result.current.countdown).toBe(4500);
    });

    it("updates isFadingOut", () => {
      const { result } = renderHook(() => useOverlaySharedState());

      act(() => {
        result.current.setIsFadingOut(true);
      });

      expect(result.current.isFadingOut).toBe(true);
    });

    it("updates overlayState via setState", () => {
      const { result } = renderHook(() => useOverlaySharedState());

      act(() => {
        result.current.setState("confirming");
      });

      expect(result.current.state).toBe("confirming");
    });

    it("updates isVisible via setIsVisible", () => {
      const { result } = renderHook(() => useOverlaySharedState());

      act(() => {
        result.current.setIsVisible(true);
      });

      expect(result.current.isVisible).toBe(true);
    });
  });

  describe("resetRecordingState", () => {
    it("resets all recording-related state to defaults", () => {
      const { result } = renderHook(() => useOverlaySharedState());

      // Set various state values
      act(() => {
        result.current.setTranscriptionPreview("Some preview");
        result.current.setStreamingText("Some streaming");
        result.current.setRouterResult({
          success: true,
          summary: "test",
          error: null,
          transcription_text: "text",
        });
        result.current.setIsEditing(true);
        result.current.setEditedText("Some edit");
        result.current.setCountdown(3000);
        result.current.setMicDeadWarning(true);
        result.current.setLowAudioWarning(true);
      });

      // Verify values were set
      expect(result.current.transcriptionPreview).toBe("Some preview");
      expect(result.current.streamingText).toBe("Some streaming");
      expect(result.current.routerResult).not.toBeNull();
      expect(result.current.isEditing).toBe(true);
      expect(result.current.micDeadWarning).toBe(true);

      // Reset
      act(() => {
        result.current.resetRecordingState();
      });

      // Verify everything is reset
      expect(result.current.transcriptionPreview).toBe("");
      expect(result.current.streamingText).toBe("");
      expect(result.current.routerResult).toBeNull();
      expect(result.current.isEditing).toBe(false);
      expect(result.current.editedText).toBe("");
      expect(result.current.countdown).toBe(0);
      expect(result.current.micDeadWarning).toBe(false);
      expect(result.current.lowAudioWarning).toBe(false);
    });

    it("resets ref values", () => {
      const { result } = renderHook(() => useOverlaySharedState());

      // Modify refs
      result.current.lowAudioHistoryRef.current = [0.1, 0.2];
      result.current.hadGoodAudioRef.current = true;
      result.current.usbCyclingActiveRef.current = true;

      act(() => {
        result.current.resetRecordingState();
      });

      expect(result.current.lowAudioHistoryRef.current).toEqual([]);
      expect(result.current.hadGoodAudioRef.current).toBe(false);
      expect(result.current.usbCyclingActiveRef.current).toBe(false);
    });

    it("updates lastLevelTimeRef and recordingStartTimeRef to current time", () => {
      const { result } = renderHook(() => useOverlaySharedState());
      const beforeReset = Date.now();

      act(() => {
        result.current.resetRecordingState();
      });

      const afterReset = Date.now();
      expect(result.current.lastLevelTimeRef.current).toBeGreaterThanOrEqual(beforeReset);
      expect(result.current.lastLevelTimeRef.current).toBeLessThanOrEqual(afterReset);
      expect(result.current.recordingStartTimeRef.current).toBeGreaterThanOrEqual(beforeReset);
    });
  });

  describe("settings fetch on mount", () => {
    it("calls getAppSettings on mount to prefetch live captions setting", () => {
      renderHook(() => useOverlaySharedState());
      expect(commands.getAppSettings).toHaveBeenCalled();
    });
  });

  describe("ref synchronization", () => {
    it("keeps transcriptionPreviewRef in sync with transcriptionPreview state", () => {
      const { result } = renderHook(() => useOverlaySharedState());

      act(() => {
        result.current.setTranscriptionPreview("Synced text");
      });

      // The ref should update via useEffect
      expect(result.current.transcriptionPreviewRef.current).toBe("Synced text");
    });
  });
});