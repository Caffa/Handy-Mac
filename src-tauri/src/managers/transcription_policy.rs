//! How audio is decoded: the one place the model's traits and the settings
//! choose between streaming, long-form chunking and a single run.
//!
//! Fork note: upstream lands this at `managers/transcription/policy.rs`; the
//! fork keeps it a sibling file because CI swaps `transcription.rs` for a
//! mock wholesale, and a module inside that file would vanish under the swap.

use crate::engine_supervisor::LoadedInfo;

/// What a model can do that decides how its audio is decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelTraits {
    /// Decodes audio incrementally as it is recorded.
    pub streaming: bool,
    /// Keeps its accuracy on audio longer than its native window in one run
    /// (whisper today).
    pub long_form: bool,
}

impl ModelTraits {
    /// The loaded model's own traits: the authority once a model is loaded.
    pub fn of(info: &LoadedInfo) -> Self {
        Self {
            streaming: info.capabilities.supports_streaming,
            long_form: info.supports_long_form,
        }
    }
}

/// How a recording is decoded while it is still going.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveDecode {
    /// The model decodes the audio as it arrives.
    Stream,
    /// Long audio is cut at pauses and each chunk decoded once its cut is
    /// certain.
    Chunked,
    /// Nothing until the recording stops; then it is decoded in batch.
    None,
}

/// How a whole clip is decoded: a recording without a live result, a
/// re-transcribed history entry, or a file from the CLI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatchDecode {
    /// Cut at pauses into chunks of about 30 s, each decoded in turn; a clip
    /// of 40 s or less is one chunk.
    Chunked,
    /// One run.
    Whole,
}

/// Streaming wins: a streaming model handles long audio itself. Otherwise
/// long audio is chunked unless the model keeps its accuracy on it.
pub fn live_decode(model: ModelTraits, long_form_chunking: bool) -> LiveDecode {
    if model.streaming {
        LiveDecode::Stream
    } else if long_form_chunking && !model.long_form {
        LiveDecode::Chunked
    } else {
        LiveDecode::None
    }
}

/// As live, except that a streaming model takes a whole clip in one run, as
/// it did before long-form chunking existed.
pub fn batch_decode(model: ModelTraits, long_form_chunking: bool) -> BatchDecode {
    if long_form_chunking && !model.long_form && !model.streaming {
        BatchDecode::Chunked
    } else {
        BatchDecode::Whole
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn traits(streaming: bool, long_form: bool) -> ModelTraits {
        ModelTraits {
            streaming,
            long_form,
        }
    }

    #[test]
    fn streaming_models_stream_and_take_whole_clips() {
        let model = traits(true, false);
        assert_eq!(live_decode(model, true), LiveDecode::Stream);
        assert_eq!(live_decode(model, false), LiveDecode::Stream);
        assert_eq!(batch_decode(model, true), BatchDecode::Whole);
    }

    #[test]
    fn long_form_models_never_chunk() {
        let model = traits(false, true);
        assert_eq!(live_decode(model, true), LiveDecode::None);
        assert_eq!(batch_decode(model, true), BatchDecode::Whole);
    }

    #[test]
    fn ordinary_models_chunk_only_when_the_setting_is_on() {
        let model = traits(false, false);
        assert_eq!(live_decode(model, true), LiveDecode::Chunked);
        assert_eq!(live_decode(model, false), LiveDecode::None);
        assert_eq!(batch_decode(model, true), BatchDecode::Chunked);
        assert_eq!(batch_decode(model, false), BatchDecode::Whole);
    }
}
