import assert from "node:assert/strict";
import { deriveOverlayView } from "./overlayView";
import type { RouterResultEvent } from "../hooks/useOverlayState";

const aResult: RouterResultEvent = {
  success: true,
  summary: "Route: file.txt",
  error: null,
  transcription_text: "take a note",
};

// Regression: a router result from a previous session must not suppress the
// recording pill, visualizer, cancel button, or live captions while a new
// recording is active.
{
  const view = deriveOverlayView({
    backendStateKind: "Recording" as const,
    backendIsVisible: true,
    backendIsRouter: false,
    routerResult: aResult,
  });
  assert.equal(view.isRecording, true, "recording kind detected");
  assert.equal(
    view.routerResultActive,
    false,
    "router result is inactive while recording",
  );
  assert.equal(view.isVisible, true, "overlay stays visible");
  assert.equal(view.isRouter, false, "no router styling while recording");
  assert.equal(view.showPill, true, "recording pill stays visible");
}

// Solo router result (no recording): A's result display is preserved.
{
  const view = deriveOverlayView({
    backendStateKind: "Idle" as const,
    backendIsVisible: false,
    backendIsRouter: false,
    routerResult: aResult,
  });
  assert.equal(view.isRecording, false);
  assert.equal(view.routerResultActive, true);
  assert.equal(view.isVisible, true, "result keeps overlay visible");
  assert.equal(view.isRouter, true, "result triggers router styling");
  assert.equal(view.showPill, false, "pill hidden to show result cards");
}

// Processing state while a router result is present: unchanged router behavior.
{
  const view = deriveOverlayView({
    backendStateKind: "Processing" as const,
    backendIsVisible: true,
    backendIsRouter: true,
    routerResult: aResult,
  });
  assert.equal(view.isRecording, false);
  assert.equal(view.routerResultActive, true);
  assert.equal(view.isVisible, true);
  assert.equal(view.isRouter, true);
  assert.equal(view.showPill, false);
}

// Normal recording without a router result: untouched.
{
  const view = deriveOverlayView({
    backendStateKind: "Recording" as const,
    backendIsVisible: true,
    backendIsRouter: false,
    routerResult: null,
  });
  assert.equal(view.isRecording, true);
  assert.equal(view.routerResultActive, false);
  assert.equal(view.isVisible, true);
  assert.equal(view.isRouter, false);
  assert.equal(view.showPill, true);
}

console.log("overlayView: all assertions passed");
