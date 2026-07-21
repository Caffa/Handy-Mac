export interface ModelStateEvent {
  event_type: string;
  model_id?: string;
  model_name?: string;
  error?: string;
}

export interface RecordingErrorEvent {
  error_type: string;
  detail?: string;
}

export interface LowVolumeWarningEvent {
  peak_dbfs: number;
  estimated_snr_db: number;
  duration_secs: number;
  too_quiet: boolean;
  model_id: string;
  transcription: string;
}

export interface TranscriptionSegment {
  start: number;
  end: number;
  text: string;
}

export interface PartialTranscriptionEvent {
  text: string;
  model_id: string;
  suppressed_token_count: number | null;
  segments: TranscriptionSegment[] | null;
}

/** Categories of recoverable errors that can show a retry dialog. */
export type RecoverableErrorType =
  | "model_download"
  | "model_load"
  | "transcription"
  | "audio_device";

/** What kind of recovery action is possible for an error. */
export type RecoveryAction = "retry" | "user_action" | "permanent";

/**
 * Event payload from the backend for recoverable errors.
 * The frontend should show an ErrorDialog with retry/dismiss options.
 */
export interface RecoverableErrorEvent {
  /** Unique ID for this error occurrence (for dedup/tracking retry count) */
  error_id: string;
  /** Category of the error */
  error_type: RecoverableErrorType;
  /** Whether retry is possible and what kind */
  recovery_action: RecoveryAction;
  /** User-friendly error message */
  message: string;
  /** Optional i18n key for the message */
  message_key?: string;
  /** Optional i18n interpolation parameters as JSON string */
  message_params?: string;
  /** Additional context as JSON string */
  context?: string;
  /** Tauri command name to invoke on retry */
  retry_command?: string;
  /** JSON-encoded arguments for the retry command */
  retry_args?: string;
  /** Technical error detail for "Show Details" toggle */
  technical_detail?: string;
}
