use crate::{TranscriptionResult, TranscriptionSegment};

/// Default separator for merging chunk texts.
pub const DEFAULT_MERGE_SEPARATOR: &str = " ";

/// Merge chunk results into a single result.
///
/// Text is joined with `separator` (default `" "`). Use `""` for
/// languages that don't use space separators (Chinese, Japanese, etc.).
/// Segments are concatenated (timestamps should already be adjusted to
/// session-relative time by the caller).
/// `suppressed_token_count` is summed across all chunks.
pub fn merge_sequential(results: &[TranscriptionResult]) -> TranscriptionResult {
    merge_sequential_with_separator(results, DEFAULT_MERGE_SEPARATOR)
}

/// Merge chunk results with a custom separator between chunk texts.
pub fn merge_sequential_with_separator(
    results: &[TranscriptionResult],
    separator: &str,
) -> TranscriptionResult {
    let text = results
        .iter()
        .map(|r| r.text.trim())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(separator);

    let segments = {
        let all: Vec<TranscriptionSegment> = results
            .iter()
            .filter_map(|r| r.segments.as_ref())
            .flatten()
            .cloned()
            .collect();
        if all.is_empty() {
            None
        } else {
            Some(all)
        }
    };

    // Sum suppressed_token_count across all chunks.
    let mut total_suppressed: usize = 0;
    let mut any_populated = false;
    for r in results {
        if let Some(count) = r.suppressed_token_count {
            total_suppressed += count;
            any_populated = true;
        }
    }
    let suppressed_token_count = if any_populated {
        Some(total_suppressed)
    } else {
        None
    };

    TranscriptionResult {
        text,
        segments,
        suppressed_token_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_empty() {
        let result = merge_sequential(&[]);
        assert_eq!(result.text, "");
        assert!(result.segments.is_none());
    }

    #[test]
    fn merge_single() {
        let results = vec![TranscriptionResult {
            text: "hello world".to_string(),
            segments: Some(vec![TranscriptionSegment {
                start: 0.0,
                end: 1.0,
                text: "hello world".to_string(),
            }]),
            suppressed_token_count: None,
        }];
        let merged = merge_sequential(&results);
        assert_eq!(merged.text, "hello world");
        assert_eq!(merged.segments.unwrap().len(), 1);
    }

    #[test]
    fn merge_multiple_texts() {
        let results = vec![
            TranscriptionResult {
                text: "hello".to_string(),
                segments: None,
                suppressed_token_count: None,
            },
            TranscriptionResult {
                text: "world".to_string(),
                segments: None,
                suppressed_token_count: None,
            },
        ];
        let merged = merge_sequential(&results);
        assert_eq!(merged.text, "hello world");
        assert!(merged.segments.is_none());
    }

    #[test]
    fn merge_skips_empty_text() {
        let results = vec![
            TranscriptionResult {
                text: "hello".to_string(),
                segments: None,
                suppressed_token_count: None,
            },
            TranscriptionResult {
                text: "  ".to_string(),
                segments: None,
                suppressed_token_count: None,
            },
            TranscriptionResult {
                text: "world".to_string(),
                segments: None,
                suppressed_token_count: None,
            },
        ];
        let merged = merge_sequential(&results);
        assert_eq!(merged.text, "hello world");
    }

    #[test]
    fn merge_concatenates_segments() {
        let results = vec![
            TranscriptionResult {
                text: "hello".to_string(),
                segments: Some(vec![TranscriptionSegment {
                    start: 0.0,
                    end: 1.0,
                    text: "hello".to_string(),
                }]),
                suppressed_token_count: None,
            },
            TranscriptionResult {
                text: "world".to_string(),
                segments: Some(vec![TranscriptionSegment {
                    start: 5.0,
                    end: 6.0,
                    text: "world".to_string(),
                }]),
                suppressed_token_count: None,
            },
        ];
        let merged = merge_sequential(&results);
        let segs = merged.segments.unwrap();
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].start, 0.0);
        assert_eq!(segs[1].start, 5.0);
    }

    #[test]
    fn merge_trims_whitespace() {
        let results = vec![
            TranscriptionResult {
                text: "  hello  ".to_string(),
                segments: None,
                suppressed_token_count: None,
            },
            TranscriptionResult {
                text: "  world  ".to_string(),
                segments: None,
                suppressed_token_count: None,
            },
        ];
        let merged = merge_sequential(&results);
        assert_eq!(merged.text, "hello world");
    }
}
