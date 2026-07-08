//! Centralized error types for the Handy application.
//!
//! Uses `thiserror` for structured error definitions with user-friendly messages.
//! Internal error propagation uses `anyhow`; conversion to `AppError` happens at
//! API boundaries (Tauri commands, manager public methods). `AppError` converts
//! to `String` via `From<AppError> for String` for Tauri command compatibility.

/// Unified error type for the Handy application.
/// Each variant corresponds to a domain area and carries enough context
/// for user-friendly error messages. The `#[error(...)]` attribute
/// provides the display message; `#[source]` marks the underlying cause
/// for error chaining.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    // ── Audio ──────────────────────────────────────────────────────────
    /// Errors from audio recording, device enumeration, or stream setup.
    #[error("Audio error: {message}")]
    Audio {
        message: String,
        #[source]
        source: anyhow::Error,
    },

    /// No audio input device available or selected.
    #[error("No audio input device available")]
    AudioNoDevice,

    // ── Transcription ──────────────────────────────────────────────────
    /// Errors from the transcription engine (model loading, inference, etc.).
    #[error("Transcription error: {message}")]
    Transcription {
        message: String,
        #[source]
        source: anyhow::Error,
    },

    /// The transcription engine panicked (e.g., segfault in native code).
    #[error("Transcription engine panicked: {0}. The model has been unloaded and will reload on next attempt.")]
    TranscriptionPanic(String),

    /// Another transcription is already in progress.
    #[error("Another transcription is in progress. Please wait and try again.")]
    TranscriptionBusy,

    /// Timed out waiting for model to load.
    #[error("Timed out waiting for model to load. Please try again.")]
    TranscriptionLoadTimeout,

    /// Model not loaded when transcription was requested.
    #[error("Model is not loaded for transcription.")]
    ModelNotLoaded,

    // ── Model ───────────────────────────────────────────────────────────
    /// Model not found in the available models list.
    #[error("Model not found: {0}")]
    ModelNotFound(String),

    /// Model exists but has not been downloaded yet.
    #[error("Model not downloaded: {0}")]
    ModelNotDownloaded(String),

    /// Download verification (SHA256) failed.
    #[error("Download verification failed for model {model_id}: file is corrupt. Please retry.")]
    ModelVerificationFailed {
        model_id: String,
        #[source]
        source: anyhow::Error,
    },

    /// Network or I/O error during model download.
    #[error("Failed to download model {model_id}: {message}")]
    ModelDownloadFailed {
        model_id: String,
        message: String,
        #[source]
        source: anyhow::Error,
    },

    /// Download was cancelled by the user.
    #[error("Download cancelled for: {0}")]
    ModelDownloadCancelled(String),

    /// Failed to extract a downloaded model archive.
    #[error("Failed to extract model {model_id}: {message}")]
    ModelExtractionFailed {
        model_id: String,
        message: String,
        #[source]
        source: anyhow::Error,
    },

    /// Failed to load a model into memory for transcription.
    #[error("Failed to load {engine} model {model_id}: {message}")]
    ModelLoadFailed {
        engine: String,
        model_id: String,
        message: String,
        #[source]
        source: anyhow::Error,
    },

    /// No model files found to delete.
    #[error("No model files found to delete")]
    ModelNoFilesToDelete,

    /// Model is currently downloading and cannot be used.
    #[error("Model is currently downloading: {0}")]
    ModelCurrentlyDownloading(String),

    /// Model file/directory not found on disk.
    #[error("Complete model {kind} not found: {model_id}")]
    ModelPathNotFound { kind: String, model_id: String },

    // ── Settings ────────────────────────────────────────────────────────
    /// Error persisting or loading application settings.
    #[error("Settings error: {0}")]
    Settings(String),

    // ── History / Database ──────────────────────────────────────────────
    /// SQLite or database-related errors.
    #[error("Database error: {message}")]
    Database {
        message: String,
        #[source]
        source: anyhow::Error,
    },

    // ── I/O ─────────────────────────────────────────────────────────────
    /// General file-system or I/O errors.
    #[error("I/O error: {message}")]
    Io {
        message: String,
        #[source]
        source: std::io::Error,
    },

    /// Path resolution error.
    #[error("Failed to resolve path: {0}")]
    PathResolution(String),

    // ── Catch-all ───────────────────────────────────────────────────────
    /// Errors that don't fit a specific category.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

// ── Conversions from underlying error types ─────────────────────────

impl From<std::io::Error> for AppError {
    fn from(err: std::io::Error) -> Self {
        AppError::Io {
            message: err.to_string(),
            source: err,
        }
    }
}

impl From<rusqlite::Error> for AppError {
    fn from(err: rusqlite::Error) -> Self {
        AppError::Database {
            message: err.to_string(),
            source: anyhow::anyhow!("{}", err),
        }
    }
}

impl From<reqwest::Error> for AppError {
    fn from(err: reqwest::Error) -> Self {
        AppError::Other(anyhow::anyhow!("{}", err))
    }
}

// ── Conversion to String for Tauri command boundary ─────────────────
//
// Tauri commands return `Result<T, String>`. This impl lets us write
// `result.map_err(AppError::from)?` or `?.to_string()` at the command
// boundary without boilerplate.

impl From<AppError> for String {
    fn from(err: AppError) -> String {
        err.to_string()
    }
}

// ── Convenience constructors ────────────────────────────────────────

impl AppError {
    /// Create an audio error from a message and any underlying error.
    pub fn audio(msg: impl Into<String>, source: anyhow::Error) -> Self {
        AppError::Audio {
            message: msg.into(),
            source,
        }
    }

    /// Create a transcription error from a message and any underlying error.
    pub fn transcription(msg: impl Into<String>, source: anyhow::Error) -> Self {
        AppError::Transcription {
            message: msg.into(),
            source,
        }
    }

    /// Create a model-load error with the engine name, model id, and underlying error.
    pub fn model_load(
        engine: impl Into<String>,
        model_id: impl Into<String>,
        message: impl Into<String>,
        source: anyhow::Error,
    ) -> Self {
        AppError::ModelLoadFailed {
            engine: engine.into(),
            model_id: model_id.into(),
            message: message.into(),
            source,
        }
    }

    /// Create a download failure error.
    pub fn model_download(
        model_id: impl Into<String>,
        message: impl Into<String>,
        source: anyhow::Error,
    ) -> Self {
        AppError::ModelDownloadFailed {
            model_id: model_id.into(),
            message: message.into(),
            source,
        }
    }

    /// Create a model extraction failure error.
    pub fn model_extraction(
        model_id: impl Into<String>,
        message: impl Into<String>,
        source: anyhow::Error,
    ) -> Self {
        AppError::ModelExtractionFailed {
            model_id: model_id.into(),
            message: message.into(),
            source,
        }
    }

    /// Create a database error from a message and any underlying error.
    pub fn database(msg: impl Into<String>, source: anyhow::Error) -> Self {
        AppError::Database {
            message: msg.into(),
            source,
        }
    }

    /// Create a settings error.
    pub fn settings(msg: impl Into<String>) -> Self {
        AppError::Settings(msg.into())
    }

    /// Create an I/O error with context.
    pub fn io(msg: impl Into<String>, source: std::io::Error) -> Self {
        AppError::Io {
            message: msg.into(),
            source,
        }
    }

    /// Create a path resolution error.
    pub fn path_resolution(msg: impl Into<String>) -> Self {
        AppError::PathResolution(msg.into())
    }
}

/// Result type alias used throughout the app for functions that can fail
/// with a structured `AppError`.
pub type AppResult<T> = Result<T, AppError>;
