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
