import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { AlertCircle, RefreshCw, Trash2, Clock, FileAudio } from "lucide-react";
import { commands, type RetryableTranscription, type TranscriptionFailure } from "@/bindings";
import { Button } from "@/components/ui/Button";

/**
 * Format a timestamp for display
 */
function formatTimestamp(timestamp: number, locale: string): string {
  const date = new Date(timestamp * 1000);
  return date.toLocaleString(locale, {
    year: "numeric",
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

/**
 * Get human-readable description of failure type
 */
function getFailureDescription(failure: TranscriptionFailure | null): string {
  if (!failure) return "Unknown error";

  if (typeof failure === "string") {
    // Handle "SilentAudio" case
    return "Audio was silent or empty";
  }

  if ("ModelLoadFailure" in failure) {
    return `Model "${failure.ModelLoadFailure.model_id}" failed to load: ${failure.ModelLoadFailure.error}`;
  }
  if ("InferenceFailure" in failure) {
    return `Transcription failed with "${failure.InferenceFailure.model_id}": ${failure.InferenceFailure.error}`;
  }
  if ("EnginePanic" in failure) {
    return `Model "${failure.EnginePanic.model_id}" crashed during transcription`;
  }
  if ("Timeout" in failure) {
    return `Model "${failure.Timeout.model_id}" timed out after ${failure.Timeout.duration_secs}s`;
  }
  if ("ResourceUnavailable" in failure) {
    return `${failure.ResourceUnavailable.resource} unavailable: ${failure.ResourceUnavailable.error}`;
  }
  if ("Unknown" in failure) {
    return failure.Unknown.error;
  }

  return "Unknown error";
}

/**
 * Check if failure type can be retried
 */
function canRetry(failure: TranscriptionFailure | null): boolean {
  if (!failure) return true;

  if (typeof failure === "string") {
    // SilentAudio
    return false;
  }

  if ("EnginePanic" in failure) return false;

  return true;
}

interface RetryQueueProps {
  className?: string;
}

export const RetryQueue: React.FC<RetryQueueProps> = ({ className }) => {
  const { t, i18n } = useTranslation();
  const [entries, setEntries] = useState<RetryableTranscription[]>([]);
  const [loading, setLoading] = useState(true);
  const [retrying, setRetrying] = useState<string | null>(null);

  // Load retry queue
  const loadQueue = async () => {
    setLoading(true);
    try {
      const result = await commands.getRetryQueue();
      if (result.status === "ok") {
        setEntries(result.data);
      } else {
        console.error("Failed to load retry queue:", result.error);
      }
    } catch (error) {
      console.error("Failed to load retry queue:", error);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    loadQueue();
    // Refresh every 30 seconds
    const interval = setInterval(loadQueue, 30000);
    return () => clearInterval(interval);
  }, []);

  // Retry a specific entry
  const handleRetry = async (entryId: string) => {
    setRetrying(entryId);
    try {
      const result = await commands.retryTranscription(entryId);
      if (result.status === "ok") {
        toast.success(t("settings.retryQueue.retrySuccess", "Retry successful"));
        await loadQueue();
      } else {
        toast.error(
          t("settings.retryQueue.retryError", "Retry failed: {{error}}", {
            error: result.error,
          })
        );
      }
    } catch (error) {
      toast.error(
        t("settings.retryQueue.retryError", "Retry failed: {{error}}", {
          error: String(error),
        })
      );
    } finally {
      setRetrying(null);
    }
  };

  // Remove from queue
  const handleRemove = async (entryId: string) => {
    try {
      const result = await commands.removeFromRetryQueue(entryId);
      if (result.status === "ok") {
        toast.success(t("settings.retryQueue.removed", "Entry removed"));
        await loadQueue();
      } else {
        toast.error(result.error);
      }
    } catch (error) {
      toast.error(
        t("settings.retryQueue.removeError", "Failed to remove: {{error}}", {
          error: String(error),
        })
      );
    }
  };

  // Clear all entries
  const handleClearAll = async () => {
    try {
      const result = await commands.clearRetryQueue();
      if (result.status === "ok") {
        toast.success(t("settings.retryQueue.cleared", "Queue cleared"));
        setEntries([]);
      } else {
        toast.error(result.error);
      }
    } catch (error) {
      toast.error(
        t("settings.retryQueue.clearError", "Failed to clear queue: {{error}}", {
          error: String(error),
        })
      );
    }
  };

  // Retry all entries
  const handleRetryAll = async () => {
    const readyEntries = entries.filter((e) => !e.is_processing);
    let successCount = 0;
    let failCount = 0;

    for (const entry of readyEntries) {
      try {
        const result = await commands.retryTranscription(entry.id);
        if (result.status === "ok") {
          successCount++;
        } else {
          failCount++;
        }
      } catch {
        failCount++;
      }
    }

    if (successCount > 0) {
      toast.success(
        t("settings.retryQueue.batchSuccess", "{{count}} entries retried successfully", {
          count: successCount,
        })
      );
    }
    if (failCount > 0) {
      toast.error(
        t("settings.retryQueue.batchFail", "{{count}} entries failed to retry", {
          count: failCount,
        })
      );
    }

    await loadQueue();
  };

  if (loading) {
    return null;
  }

  if (entries.length === 0) {
    return null;
  }

  return (
    <div className={className}>
      <div className="px-4 flex items-center justify-between mb-2">
        <div className="flex items-center gap-2">
          <AlertCircle className="w-4 h-4 text-amber-500" />
          <h3 className="text-sm font-medium text-text">
            {t("settings.retryQueue.title", "Failed Transcriptions")}
          </h3>
          <span className="text-xs text-text/60">
            ({entries.length})
          </span>
        </div>
        <div className="flex items-center gap-2">
          <Button
            variant="ghost"
            size="sm"
            onClick={handleRetryAll}
            disabled={entries.every((e) => e.is_processing)}
            className="text-xs"
          >
            <RefreshCw className="w-3 h-3 mr-1" />
            {t("settings.retryQueue.retryAll", "Retry All")}
          </Button>
          <Button
            variant="ghost"
            size="sm"
            onClick={handleClearAll}
            className="text-xs text-red-500 hover:text-red-600"
          >
            <Trash2 className="w-3 h-3 mr-1" />
            {t("settings.retryQueue.clearAll", "Clear")}
          </Button>
        </div>
      </div>

      <div className="bg-background border border-amber-500/20 rounded-lg overflow-hidden">
        <div className="divide-y divide-mid-gray/20">
          {entries.map((entry) => (
            <RetryEntry
              key={entry.id}
              entry={entry}
              onRetry={() => handleRetry(entry.id)}
              onRemove={() => handleRemove(entry.id)}
              isRetrying={retrying === entry.id}
            />
          ))}
        </div>
      </div>
    </div>
  );
};

interface RetryEntryProps {
  entry: RetryableTranscription;
  onRetry: () => void;
  onRemove: () => void;
  isRetrying: boolean;
}

const RetryEntry: React.FC<RetryEntryProps> = ({
  entry,
  onRetry,
  onRemove,
  isRetrying,
}) => {
  const { t, i18n } = useTranslation();

  const formatRetryTime = (timestamp: number | null): string => {
    if (timestamp === null) {
      return t("settings.retryQueue.readyNow", "Ready now");
    }
    const retryAt = new Date(timestamp * 1000);
    const now = new Date();
    const diffMs = retryAt.getTime() - now.getTime();

    if (diffMs <= 0) {
      return t("settings.retryQueue.readyNow", "Ready now");
    }

    const diffMins = Math.floor(diffMs / 60000);
    if (diffMins < 60) {
      return t("settings.retryQueue.retryInMinutes", "Retry in {{mins}}m", { mins: diffMins });
    }

    const diffHours = Math.floor(diffMins / 60);
    return t("settings.retryQueue.retryInHours", "Retry in {{hours}}h", { hours: diffHours });
  };

  const fileName = entry.audio_path.split("/").pop() || entry.audio_path;
  const timestamp = new Date(entry.timestamp * 1000);

  return (
    <div className="px-4 py-3 flex flex-col gap-2">
      {/* Header row */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <FileAudio className="w-4 h-4 text-text/60" />
          <span className="text-sm font-mono text-text/80">{fileName}</span>
        </div>
        <div className="flex items-center gap-1">
          <Button
            variant="ghost"
            size="sm"
            onClick={onRetry}
            disabled={isRetrying || entry.is_processing || !canRetry(entry.last_failure)}
            className="text-xs"
          >
            <RefreshCw className={`w-3 h-3 mr-1 ${isRetrying ? "animate-spin" : ""}`} />
            {isRetrying
              ? t("settings.retryQueue.retrying", "Retrying...")
              : t("settings.retryQueue.retry", "Retry")}
          </Button>
          <Button
            variant="ghost"
            size="sm"
            onClick={onRemove}
            disabled={isRetrying}
            className="text-xs text-text/60 hover:text-red-500"
          >
            <Trash2 className="w-3 h-3" />
          </Button>
        </div>
      </div>

      {/* Error message */}
      <div className="text-xs text-red-500/80 flex items-start gap-2">
        <AlertCircle className="w-3 h-3 mt-0.5 flex-shrink-0" />
        <span className="break-all">{getFailureDescription(entry.last_failure)}</span>
      </div>

      {/* Metadata row */}
      <div className="flex items-center gap-4 text-xs text-text/60">
        <div className="flex items-center gap-1">
          <Clock className="w-3 h-3" />
          <span>{formatTimestamp(entry.timestamp, i18n.language)}</span>
        </div>
        <div>
          {t("settings.retryQueue.attempts", "Attempt {{current}}/{{max}}", {
            current: entry.retry_count,
            max: entry.max_retries,
          })}
        </div>
        <div className="font-medium">
          {formatRetryTime(entry.next_retry_at)}
        </div>
        {entry.fallback_models.length > 0 && (
          <div>
            {t("settings.retryQueue.fallback", "Fallback: {{count}} models", {
              count: entry.fallback_models.length,
            })}
          </div>
        )}
      </div>

      {/* Model info */}
      <div className="text-xs text-text/50">
        {t("settings.retryQueue.model", "Model: {{modelId}}", {
          modelId: entry.model_id,
        })}
        {entry.current_model_index > 0 && (
          <span className="ml-2 text-amber-500">
            ({t("settings.retryQueue.usingFallback", "using fallback")})
          </span>
        )}
      </div>
    </div>
  );
};