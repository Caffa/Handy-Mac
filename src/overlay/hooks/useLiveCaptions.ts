/**
 * useLiveCaptions — Streaming transcription display logic.
 *
 * Manages the partial-transcription event listener and segment merging.
 * The streamingText and streamingSegments state are owned by the parent
 * (useOverlaySharedState) and updated via setters — this ensures that resets
 * propagate correctly.
 *
 * Scope: Live caption display during recording.
 * Dependencies: React hooks, PartialTranscriptionEvent type.
 * Side effects: partial-transcription event listener, debug logging.
 */
import { listen } from "@tauri-apps/api/event";
import { useEffect, useRef } from "react";
import {
  PartialTranscriptionEvent,
  TranscriptionSegment,
} from "@/lib/types/events";
import type { OverlayState } from "./useOverlayState";

/// Filter filler words from the start of streaming transcription text.
function filterStreamingText(text: string): string {
  const fillerWords = [
    "okay",
    "yeah",
    "um",
    "uh",
    "so",
    "like",
    "you know",
    "right",
    "well",
  ];
  const trimmed = text.trim();
  if (!trimmed) return "";

  const words = trimmed.split(/\s+/);
  if (words.length < 2) {
    const lowerText = trimmed.toLowerCase();
    const isFiller = fillerWords.some(
      (fw) =>
        lowerText === fw || lowerText === `${fw}.` || lowerText === `${fw},`,
    );
    if (isFiller) {
      console.log("[Live Captions] Filler filter removed ALL text:", {
        original: text.substring(0, 100),
        filtered: "",
        reason: "single-filler-word",
      });
      return "";
    }
    return trimmed;
  }

  const firstWord = words[0].toLowerCase().replace(/[.,!?]/, "");
  const isFillerStart = fillerWords.includes(firstWord);

  if (isFillerStart && words.length >= 2) {
    const secondWord = words[1].toLowerCase().replace(/[.,!?]/, "");
    const isContinuation =
      fillerWords.includes(secondWord) ||
      secondWord === "and" ||
      secondWord === "but";

    const filteredText = (() => {
      if (isContinuation && words.length === 2) return "";
      if (isContinuation) return words.slice(1).join(" ");
      return words.slice(1).join(" ");
    })();

    if (!filteredText) {
      console.log("[Live Captions] Filler filter removed ALL text:", {
        original: text.substring(0, 100),
        filtered: "",
        reason: "filler-continuation-pair",
      });
    } else {
      console.log("[Live Captions] Text after filter:", {
        originalText: text.substring(0, 100),
        filteredText: filteredText.substring(0, 100),
        wasFiltered: text !== filteredText,
      });
    }
    return filteredText;
  }

  return trimmed;
}

interface UseLiveCaptionsOptions {
  state: OverlayState;
  isVisible: boolean;
  /** During migration: backend-derived isRecording overrides state === "recording" */
  isRecording?: boolean;
  liveCaptionsEnabled: boolean;
  micDeadWarning: boolean;
  lowAudioWarning: boolean;
  streamingText: string;
  setStreamingText: React.Dispatch<React.SetStateAction<string>>;
  setStreamingSegments: React.Dispatch<React.SetStateAction<TranscriptionSegment[]>>;
}

interface UseLiveCaptionsReturn {
  streamingText: string;
  liveCaptionsEnabled: boolean;
}

export function useLiveCaptions(
  options: UseLiveCaptionsOptions,
): UseLiveCaptionsReturn {
  const {
    state,
    isVisible,
    isRecording,
    liveCaptionsEnabled,
    micDeadWarning,
    lowAudioWarning,
    streamingText,
    setStreamingText,
    setStreamingSegments,
  } = options;

  // During migration: prefer backend-derived isRecording when provided,
  // otherwise fall back to state === "recording"
  const effectivelyRecording = isRecording ?? state === "recording";

  // Use ref for segments since we need the accumulated value in the event handler
  const segmentsRef = useRef<TranscriptionSegment[]>([]);

  // ─── Stale-closure refs ──────────────────────────────────────────────
  // The partial-transcription listener effect has [] deps (registers once),
  // so any closure variables it reads are frozen at mount time. Use refs to
  // hold the latest values so the listener always reads current state.
  const liveCaptionsEnabledRef = useRef(liveCaptionsEnabled);
  const stateRef = useRef(state);
  const isVisibleRef = useRef(isVisible);

  useEffect(() => { liveCaptionsEnabledRef.current = liveCaptionsEnabled; }, [liveCaptionsEnabled]);
  useEffect(() => { stateRef.current = state; }, [state]);
  useEffect(() => { isVisibleRef.current = isVisible; }, [isVisible]);

  // Clear accumulated segments when recording stops or overlay hides
  // to prevent segments from a previous recording bleeding into the next.
  useEffect(() => {
    if (!effectivelyRecording || !isVisible) {
      segmentsRef.current = [];
    }
  }, [effectivelyRecording, isVisible]);

  // Debug effect for live captions visibility
  useEffect(() => {
    if (effectivelyRecording && isVisible) {
      const reasons: string[] = [];
      if (!liveCaptionsEnabled) reasons.push("liveCaptionsEnabled=false");
      if (micDeadWarning) reasons.push("micDeadWarning=true");
      if (lowAudioWarning) reasons.push("lowAudioWarning=true");
      if (!streamingText.trim())
        reasons.push(`streamingText="${streamingText.substring(0, 50)}..."`);

      if (reasons.length > 0) {
        console.log("[Live Captions] Not showing:", reasons.join(", "));
      } else {
        console.log(
          "[Live Captions] ✓ Showing:",
          streamingText.substring(0, 50),
        );
      }
    }
  }, [
    effectivelyRecording,
    isVisible,
    liveCaptionsEnabled,
    micDeadWarning,
    lowAudioWarning,
    streamingText,
  ]);

  // Detect if backend didn't set up streaming
  useEffect(() => {
    if (effectivelyRecording && liveCaptionsEnabled) {
      const timeout = setTimeout(() => {
        if (!streamingText) {
          console.warn(
            "[Live Captions] No transcription received after 3 seconds — check backend logs for initialization issues",
          );
        }
      }, 3000);
      return () => clearTimeout(timeout);
    }
  }, [effectivelyRecording, liveCaptionsEnabled]);

  // Listen for partial transcription events
  useEffect(() => {
    let unlisten: (() => void) | null = null;

    console.log(
      "[Live Captions] Registering partial-transcription event listener",
    );

    const setup = async () => {
      unlisten = await listen<PartialTranscriptionEvent>(
        "partial-transcription",
        (event) => {
          const { text, segments } = event.payload;
          // Read current values from refs to avoid stale closure
          const currentLiveCaptionsEnabled = liveCaptionsEnabledRef.current;
          const currentState = stateRef.current;
          const currentIsVisible = isVisibleRef.current;

          console.log("[Live Captions] Event received:", {
            hasText: !!text,
            textLength: text?.length || 0,
            hasSegments: !!segments,
            segmentCount: segments?.length || 0,
            liveCaptionsEnabled: currentLiveCaptionsEnabled,
            currentState,
            isVisible: currentIsVisible,
          });

          if (segments && segments.length > 0) {
            // Merge segments
            const merged = [...segmentsRef.current];
            for (const ns of segments) {
              const kept = merged.filter(
                (es) => !(es.start < ns.end && es.end > ns.start),
              );
              kept.push(ns);
              merged.splice(0, merged.length, ...kept);
            }
            merged.sort((a, b) => a.start - b.start);

            const displayText = merged.map((s) => s.text).join(" ");
            const filtered = filterStreamingText(displayText);

            if (!filtered) {
              console.log(
                "[Live Captions] Filler filter removed ALL text — raw:",
                JSON.stringify(displayText),
                "| keeping previous",
              );
              // Don't update streamingText if filter removed all text
            } else {
              console.log(
                "[Live Captions] streamingText set to:",
                JSON.stringify(filtered),
              );
              setStreamingText(filtered);
            }

            segmentsRef.current = merged;
            setStreamingSegments([...merged]);
          } else {
            // Fallback for models without timestamps
            if (text) {
              setStreamingText((prev) => {
                if (prev === text) {
                  console.log(
                    "[Live Captions] Text unchanged from previous — skipping update",
                  );
                  return prev;
                }
                const filtered = filterStreamingText(text);
                if (!filtered) {
                  console.log(
                    "[Live Captions] Filler filter removed ALL text (fallback) — raw:",
                    JSON.stringify(text),
                    "| keeping previous:",
                    JSON.stringify(prev),
                  );
                  return prev;
                } else {
                  console.log(
                    "[Live Captions] streamingText set to (fallback):",
                    JSON.stringify(filtered),
                  );
                  return filtered;
                }
              });
            }
          }
        },
      );
    };

    setup();

    return () => {
      unlisten?.();
    };
  }, []);

  return {
    streamingText,
    liveCaptionsEnabled,
  };
}