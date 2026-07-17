/**
 * RouterResultDisplay — Presentational component for router result preview.
 *
 * Handles three display modes:
 * 1. Router result (success/failure icons)
 * 2. Edit mode (textarea for editing transcription)
 * 3. Countdown view (clickable text with countdown)
 *
 * Scope: Presentational with callback props — no internal state management.
 * Dependencies: i18n for translations.
 */
import React from "react";
import { useTranslation } from "react-i18next";
import { commands } from "@/bindings";
import type { RouterResultEvent } from "../hooks/useOverlayState";

/// Handler icon mapping
const HANDLER_ICONS: Record<string, string> = {
  Daily: "📖",
  "Apple Note": "📝",
  "Project Devlog": "📁",
  "Tiny Experiment": "🧪",
  Zettelkasten: "🃏",
  "Story Idea": "💡",
  "Read Later": "📚",
  Idea: "💭",
  "Swipe File": "🔖",
  Concerns: "⚠️",
  "Emotional Rant": "😤",
  Correction: "✏️",
  default: "✅",
};

/// Parse handler names from summary string like "✅ Daily | ✅ Zettelkasten (my-note)"
function parseHandlerNames(summary: string | null): string[] {
  if (!summary) return [];

  return summary
    .split("|")
    .map((part) => part.trim())
    .filter((part) => part.length > 0)
    .map((part) => {
      const cleaned = part.replace(/^[✅❌⚠️]\s*/, "").trim();
      const withoutPath = cleaned.replace(/\s*\([^)]+\)\s*$/, "").trim();
      return withoutPath;
    })
    .filter((name) => name.length > 0);
}

/// Get icon for a handler name
function getHandlerIcon(handlerName: string): string {
  return HANDLER_ICONS[handlerName] || HANDLER_ICONS.default;
}

/// Countdown timer for routing confirmation (in milliseconds)
const ROUTING_COUNTDOWN_MS = 4500;

interface RouterResultDisplayProps {
  routerResult: RouterResultEvent | null;
  isEditing: boolean;
  isFadingOut: boolean;
  transcriptionPreview: string;
  editedText: string;
  countdown: number;
  direction: "ltr" | "rtl";
  overlayScale: number;
  textareaRef: React.RefObject<HTMLTextAreaElement>;
  onTranscriptionClick: () => void;
  onSendEdited: () => void;
  onCancelEdit: () => void;
  onEditedTextChange: (text: string) => void;
}

export function RouterResultDisplay({
  routerResult,
  isEditing,
  isFadingOut,
  transcriptionPreview,
  editedText,
  countdown,
  direction,
  overlayScale,
  textareaRef,
  onTranscriptionClick,
  onSendEdited,
  onCancelEdit,
  onEditedTextChange,
}: RouterResultDisplayProps) {
  const { t } = useTranslation();

  const formatCountdown = (ms: number): string => {
    const seconds = Math.ceil(ms / 1000);
    return `${seconds}s`;
  };

  // Mouse passthrough handlers for macOS overlay click-through.
  // When the mouse enters the interactive preview area, the overlay must accept
  // mouse events so the user can click/scroll/interact. When the mouse leaves,
  // events pass through to apps below.
  const handleMouseEnter = () => commands.setOverlayMousePassthrough(true).catch(() => {});
  const handleMouseLeave = () => commands.setOverlayMousePassthrough(false).catch(() => {});

  if (!transcriptionPreview && !routerResult) return null;

  return (
    <div
      dir={direction}
      className={`transcription-preview ${isEditing ? "editing" : ""} ${routerResult ? "has-result" : ""} ${isFadingOut ? "fade-out" : ""}`}
      style={{ "--overlay-scale": overlayScale } as React.CSSProperties}
      onMouseEnter={handleMouseEnter}
      onMouseLeave={handleMouseLeave}
    >
      {routerResult ? (
        <div className="router-result">
          {routerResult.success ? (
            <div className="router-success">
              <div className="handler-cards">
                {parseHandlerNames(routerResult.summary).map(
                  (handlerName, idx) => (
                    <div key={idx} className="handler-card">
                      <span className="handler-icon">
                        {getHandlerIcon(handlerName)}
                      </span>
                      <span className="handler-label">{handlerName}</span>
                    </div>
                  ),
                )}
              </div>
            </div>
          ) : (
            <div className="router-error">
              <div className="result-icon">❌</div>
              <div className="result-message">
                {routerResult.error ||
                  t("overlay.routerError", "Routing failed")}
              </div>
            </div>
          )}
        </div>
      ) : isEditing ? (
        <div className="edit-container">
          <textarea
            ref={textareaRef}
            className="transcription-edit"
            value={editedText}
            onChange={(e) => onEditedTextChange(e.target.value)}
            onKeyDown={(e) => {
              e.stopPropagation();
              if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
                e.preventDefault();
                onSendEdited();
              }
            }}
            onBlur={() => {
              // Focus lock: refocus if still in editing mode
              if (isEditing) {
                setTimeout(() => textareaRef.current?.focus(), 10);
              }
            }}
            placeholder={t("overlay.editPlaceholder", "Edit your text...")}
            dir={direction}
          />
          <div className="edit-buttons">
            <button className="edit-cancel-button" onClick={onCancelEdit}>
              {t("overlay.cancel", "Cancel")}
            </button>
            <button className="edit-send-button" onClick={onSendEdited}>
              {t("overlay.send", "Send")}
            </button>
          </div>
        </div>
      ) : (
        <div
          className="transcription-text-preview"
          onClick={onTranscriptionClick}
          title={t("overlay.clickToEdit", "Click to edit")}
        >
          {transcriptionPreview}
        </div>
      )}

      {/* Countdown progress bar */}
      {countdown > 0 && !isEditing && !routerResult && (
        <div
          className="countdown-progress"
          style={{
            width: `${(countdown / ROUTING_COUNTDOWN_MS) * 100}%`,
          }}
        />
      )}
    </div>
  );
}
