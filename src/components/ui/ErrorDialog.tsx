/**
 * ErrorDialog — A modal dialog for recoverable errors with retry/dismiss actions.
 *
 * Purpose:
 *   Shows user-friendly error messages with:
 *   - "Retry" button for transient/retriable errors (network, model load, etc.)
 *   - "Dismiss" button to close the dialog
 *   - "Show Details" toggle for technical error info
 *   - Tracks retry count to prevent infinite loops (max 3 retries)
 *
 * Scope:
 *   - Listens for "recoverable-error" Tauri events
 *   - Maps error_type to i18n keys for user-friendly messages
 *   - Invokes Tauri commands on retry when retry_command is provided
 *
 * Dependencies:
 *   - useTranslation for i18n
 *   - Tauri event listener API
 *   - Tauri invoke for retry commands
 */

import React, { useState, useCallback, useEffect } from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import {
  AlertCircle,
  RefreshCw,
  X,
  ChevronDown,
  ChevronUp,
  Volume2,
  Download,
  Cpu,
  Wifi,
} from "lucide-react";
import {
  type RecoverableErrorEvent,
  type RecoverableErrorType,
  type RecoveryAction,
} from "@/lib/types/events";
import { Button } from "@/components/ui/Button";

/** Maximum number of automatic retries before disabling the retry button. */
const MAX_RETRIES = 3;

/** Maps error types to their icon components. */
const ERROR_TYPE_ICONS: Record<RecoverableErrorType, React.ElementType> = {
  model_download: Download,
  model_load: Cpu,
  transcription: Volume2,
  audio_device: Wifi,
};

/** Maps error types to their i18n title keys. */
const ERROR_TYPE_TITLE_KEYS: Record<RecoverableErrorType, string> = {
  model_download: "errorDialog.titles.modelDownload",
  model_load: "errorDialog.titles.modelLoad",
  transcription: "errorDialog.titles.transcription",
  audio_device: "errorDialog.titles.audioDevice",
};

interface ErrorDialogEntry {
  event: RecoverableErrorEvent;
  retryCount: number;
  showDetails: boolean;
  isRetrying: boolean;
}

export const ErrorDialog: React.FC = () => {
  const { t } = useTranslation();
  const [errors, setErrors] = useState<ErrorDialogEntry[]>([]);

  // Listen for recoverable error events from the backend
  useEffect(() => {
    const unlisten = listen<RecoverableErrorEvent>(
      "recoverable-error",
      (event) => {
        const newEntry: ErrorDialogEntry = {
          event: event.payload,
          retryCount: 0,
          showDetails: false,
          isRetrying: false,
        };

        setErrors((prev) => {
          // Dedup: if same error_type + context is already showing, update it instead of stacking
          const contextStr = event.payload.context || "";
          const existingIdx = prev.findIndex(
            (e) =>
              e.event.error_type === event.payload.error_type &&
              (e.event.context || "") === contextStr,
          );

          if (existingIdx >= 0) {
            // Update existing entry with new error details
            const updated = [...prev];
            updated[existingIdx] = {
              ...updated[existingIdx],
              event: event.payload,
              retryCount: 0, // Reset retry count on new error
              isRetrying: false,
            };
            return updated;
          }

          return [...prev, newEntry];
        });
      },
    );

    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  const dismissError = useCallback((errorId: string) => {
    setErrors((prev) => prev.filter((e) => e.event.error_id !== errorId));
  }, []);

  const toggleDetails = useCallback((errorId: string) => {
    setErrors((prev) =>
      prev.map((e) =>
        e.event.error_id === errorId
          ? { ...e, showDetails: !e.showDetails }
          : e,
      ),
    );
  }, []);

  const handleRetry = useCallback(
    async (errorId: string) => {
      const entry = errors.find((e) => e.event.error_id === errorId);
      if (!entry) return;

      const { retry_command, retry_args } = entry.event;
      if (!retry_command) {
        // No retry command available — just dismiss
        dismissError(errorId);
        return;
      }

      // Mark as retrying
      setErrors((prev) =>
        prev.map((e) =>
          e.event.error_id === errorId
            ? { ...e, retryCount: e.retryCount + 1, isRetrying: true }
            : e,
        ),
      );

      try {
        // Parse retry args and invoke the command
        const args = retry_args ? JSON.parse(retry_args) : {};
        const { invoke } = await import("@tauri-apps/api/core");
        await invoke(retry_command, args);

        // Success — dismiss the error dialog
        dismissError(errorId);
      } catch (retryError) {
        // Retry failed — update the dialog with incremented retry count
        // Don't dismiss; the user can try again or dismiss manually
        console.warn(`Retry failed for ${retry_command}:`, retryError);
        setErrors((prev) =>
          prev.map((e) =>
            e.event.error_id === errorId ? { ...e, isRetrying: false } : e,
          ),
        );
      }
    },
    [errors, dismissError],
  );

  if (errors.length === 0) return null;

  // Show only the most recent error (modal-like behavior)
  const currentError = errors[errors.length - 1];
  const { event, retryCount, showDetails, isRetrying } = currentError;
  const {
    error_id,
    error_type,
    recovery_action,
    message,
    message_key,
    message_params,
    technical_detail,
  } = event;

  const canRetry =
    recovery_action !== "permanent" &&
    retryCount < MAX_RETRIES &&
    !!event.retry_command;

  const needsUserAction = recovery_action === "user_action";

  // Resolve the display message using i18n
  const displayMessage = (() => {
    if (message_key) {
      try {
        if (message_params) {
          const params: Record<string, string> = JSON.parse(message_params);
          return t(message_key, { ...params, defaultValue: message });
        }
        return t(message_key, { defaultValue: message });
      } catch {
        return message;
      }
    }
    return message;
  })();

  const titleKey =
    ERROR_TYPE_TITLE_KEYS[error_type] || "errorDialog.titles.generic";
  const title = t(titleKey);
  const IconComponent = ERROR_TYPE_ICONS[error_type] || AlertCircle;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 backdrop-blur-sm">
      <div className="bg-background border border-mid-gray/20 rounded-xl shadow-2xl max-w-md w-full mx-4 overflow-hidden">
        {/* Header */}
        <div className="flex items-start gap-3 p-5 pb-3">
          <div className="shrink-0 mt-0.5">
            <IconComponent className="w-6 h-6 text-red-400" />
          </div>
          <div className="flex-1 min-w-0">
            <h3 className="text-text font-semibold text-base">{title}</h3>
            <p className="text-mid-gray text-sm mt-1">{displayMessage}</p>
          </div>
          <button
            onClick={() => dismissError(error_id)}
            className="shrink-0 text-mid-gray hover:text-text transition-colors cursor-pointer"
            aria-label={t("errorDialog.dismiss")}
          >
            <X className="w-4 h-4" />
          </button>
        </div>

        {/* User action notice */}
        {needsUserAction && (
          <div className="px-5 pb-2">
            <div className="bg-yellow-500/10 border border-yellow-500/20 rounded-lg px-3 py-2 text-sm text-yellow-400">
              {t("errorDialog.needsUserAction")}
            </div>
          </div>
        )}

        {/* Retry count indicator */}
        {retryCount > 0 && canRetry && (
          <div className="px-5 pb-2">
            <p className="text-xs text-mid-gray">
              {t("errorDialog.retryCount", {
                count: retryCount,
                max: MAX_RETRIES,
              })}
            </p>
          </div>
        )}

        {/* Technical details toggle */}
        {technical_detail && (
          <div className="px-5 pb-2">
            <button
              onClick={() => toggleDetails(error_id)}
              className="text-xs text-mid-gray hover:text-text transition-colors flex items-center gap-1 cursor-pointer"
            >
              {showDetails ? (
                <>
                  <ChevronUp className="w-3 h-3" />
                  {t("errorDialog.hideDetails")}
                </>
              ) : (
                <>
                  <ChevronDown className="w-3 h-3" />
                  {t("errorDialog.showDetails")}
                </>
              )}
            </button>
            {showDetails && (
              <div className="mt-2 bg-mid-gray/10 border border-mid-gray/20 rounded-lg p-3 overflow-x-auto">
                <pre className="text-xs text-mid-gray whitespace-pre-wrap break-words font-mono">
                  {technical_detail}
                </pre>
              </div>
            )}
          </div>
        )}

        {/* Action buttons */}
        <div className="flex items-center justify-end gap-2 p-4 border-t border-mid-gray/10">
          <Button
            variant="ghost"
            size="sm"
            onClick={() => dismissError(error_id)}
          >
            {t("errorDialog.dismiss")}
          </Button>
          {canRetry && (
            <Button
              variant="primary-soft"
              size="sm"
              onClick={() => handleRetry(error_id)}
              disabled={isRetrying}
            >
              {isRetrying ? (
                <span className="flex items-center gap-1.5">
                  <RefreshCw className="w-3.5 h-3.5 animate-spin" />
                  {t("errorDialog.retrying")}
                </span>
              ) : (
                <span className="flex items-center gap-1.5">
                  <RefreshCw className="w-3.5 h-3.5" />
                  {t("errorDialog.retry")}
                </span>
              )}
            </Button>
          )}
        </div>
      </div>
    </div>
  );
};

export default ErrorDialog;
