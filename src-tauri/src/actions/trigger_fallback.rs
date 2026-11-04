//! Trigger-phrase fallback — shared detection logic for the gated "the user
//! said a trigger phrase but the primary model mangled the opening" recovery
//! path. The audio rescan and the call-site wiring live in router.rs.
//!
//! Pipeline contract (each step gates the next, which is what keeps the
//! rewrite safe even though the variant search window is generous):
//!   1. `detect_canonical_trigger` on the primary transcription — if a
//!      canonical phrase is already there, there is nothing to recover.
//!   2. `suspicious_opening` on the primary — cheap lexical gate before
//!      spending a model scan.
//!   3. The recorded opening (`trigger_scan_seconds` of audio) is re-scanned
//!      with the configured scan model, producing a candidate;  
//!      `detect_canonical_trigger` on the candidate decides.
//!   4. `canonicalize_leading_trigger` rewrites the primary so downstream
//!      consumers (e.g. the router's write-for-me fast path) see a clean,
//!      canonical opening.

/// Single-word mishearings of "Minion" that STT models commonly produce,
/// lowercase. The canonical word itself is included so
/// `canonicalize_leading_trigger` is idempotent and can also recover a
/// dropped "Hey" ("minion, write me …").
pub const MINION_VARIANTS: &[&str] = &["minion", "minions", "million", "millions", "milion"];

/// Single-word mishearings of "Assistant", lowercase. Same canonical-word
/// inclusion rationale as [`MINION_VARIANTS`].
pub const ASSISTANT_VARIANTS: &[&str] = &[
    "assistant",
    "assistants",
    "assistance",
    "asistant",
    "assistent",
];

/// How many leading words are searched for a trigger-variant token. The
/// trigger is "Hey <name>", so at most one word precedes the name — but
/// mishearings can also insert a short filler ("a minion", "the mission"),
/// hence a small window instead of exactly two.
const OPENING_WINDOW_WORDS: usize = 4;

/// True when `word` (already normalized) is a known trigger-variant token.
fn is_variant(word: &str) -> bool {
    MINION_VARIANTS.contains(&word) || ASSISTANT_VARIANTS.contains(&word)
}

/// Lowercase `token` with leading/trailing punctuation removed. Interior
/// characters (apostrophes, hyphens) are preserved; pure-punctuation tokens
/// normalize to the empty string, which never matches a variant.
fn normalize_token(token: &str) -> String {
    token
        .trim()
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

/// Cheap lexical gate: does the transcription open with something that *could*
/// be a misheard trigger phrase? Checks whether any of the first
/// [`OPENING_WINDOW_WORDS`] words (lowercased, punctuation-stripped) is an
/// exact variant token. False positives are acceptable — they only trigger a
/// rescan, which step 3's canonical check then arbitrates — so callers must
/// run `detect_canonical_trigger` on the primary first to skip already-clean
/// transcriptions.
pub fn suspicious_opening(text: &str) -> bool {
    text.split_whitespace()
        .take(OPENING_WINDOW_WORDS)
        .any(|token| is_variant(&normalize_token(token)))
}

/// Returns the configured canonical phrase (trimmed, as configured) that
/// `candidate` starts with, case-insensitively, or `None`. Leading
/// punctuation/whitespace on the candidate is tolerated. The first matching
/// phrase in `phrases` order wins, so the configured order is the priority
/// order. Empty phrase entries are skipped.
pub fn detect_canonical_trigger(candidate: &str, phrases: &[String]) -> Option<String> {
    // Whisper (and other local decoders) often emit punctuation inside or
    // immediately after the trigger phrase ("Hey, Minion,", "Hey... Minion").
    // Normalize both sides by dropping non-alphanumeric/non-space characters
    // before the prefix test so those candidates are still recognized.
    let normalized_candidate: String = candidate
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let low = normalized_candidate.to_lowercase();
    phrases.iter().find_map(|phrase| {
        let p = phrase.trim();
        if p.is_empty() {
            return None;
        }
        let normalized_phrase: String = p
            .chars()
            .filter(|c| c.is_alphanumeric() || c.is_whitespace())
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if normalized_phrase.is_empty() || !low.starts_with(&normalized_phrase.to_lowercase()) {
            None
        } else {
            Some(p.to_string())
        }
    })
}

/// Rewrite `primary` so it opens with the canonical `canonical` phrase instead
/// of the misheard variant: finds the first variant token within the first
/// [`OPENING_WINDOW_WORDS`] words and replaces everything from the start of
/// `primary` through that token with `canonical`, preserving the token's
/// trailing punctuation and all following text.
///
/// Returns `None` when no variant token is found in the window — this
/// function never prepends a trigger to text that does not contain one.
/// Leading whitespace of `primary` is dropped (the result starts with
/// `canonical`).
///
/// Safety contract: only call this after `detect_canonical_trigger` matched a
/// scan candidate, i.e. with hard evidence the recording actually opens with
/// the trigger. The generous window alone is not evidence.
pub fn canonicalize_leading_trigger(primary: &str, canonical: &str) -> Option<String> {
    let canonical = canonical.trim();
    if canonical.is_empty() {
        return None;
    }

    // Manual scanner so byte offsets stay exact even with collapsed or
    // unusual whitespace: `consumed` is the byte offset in `primary` where the
    // current token starts.
    let mut rest = primary;
    let mut consumed = 0usize;

    for _ in 0..OPENING_WINDOW_WORDS {
        let leading_ws = rest.len() - rest.trim_start().len();
        consumed += leading_ws;
        rest = &rest[leading_ws..];
        if rest.is_empty() {
            return None;
        }

        let token_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let token = &rest[..token_end];
        let core = normalize_token(token);
        if is_variant(&core) {
            // Offset of the core inside the token: skip its leading
            // punctuation. The replacement ends right after the core, so the
            // token's trailing punctuation (e.g. the comma in "hey million,
            // write …") and everything after it are preserved.
            let leading_punct = token.len()
                - token
                    .trim_start_matches(|c: char| !c.is_alphanumeric())
                    .len();
            let replace_end = consumed + leading_punct + core.len();
            return Some(format!("{}{}", canonical, &primary[replace_end..]));
        }

        consumed += token_end;
        rest = &rest[token_end..];
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phrases() -> Vec<String> {
        vec!["Hey Minion".to_string(), "Hey Assistant".to_string()]
    }

    // ── constants ──

    #[test]
    fn variants_are_lowercase_single_words() {
        for v in MINION_VARIANTS.iter().chain(ASSISTANT_VARIANTS.iter()) {
            assert!(!v.is_empty());
            assert_eq!(*v, v.to_lowercase());
            assert!(!v.contains(|c: char| c.is_whitespace() || !c.is_alphanumeric()));
        }
    }

    // ── suspicious_opening ──

    #[test]
    fn suspicious_flags_misheard_minion() {
        assert!(suspicious_opening("hey million write an email to my boss"));
        assert!(suspicious_opening("a million right me an email"));
        assert!(suspicious_opening("Hey Minions, draft a tweet"));
    }

    #[test]
    fn suspicious_flags_variant_with_leading_punctuation() {
        // Trailing comma is stripped from the token before matching.
        assert!(suspicious_opening("hey million, write for me an email"));
    }

    #[test]
    fn suspicious_ignores_unrelated_openings() {
        assert!(!suspicious_opening("please write an email to my boss"));
        assert!(!suspicious_opening("can you draft something quick"));
        assert!(!suspicious_opening(""));
        assert!(!suspicious_opening("   "));
    }

    #[test]
    fn suspicious_ignores_variant_beyond_window() {
        // "minion" is the 5th word — outside the 4-word window.
        assert!(!suspicious_opening(
            "please do not wake the minion up today"
        ));
    }

    // ── detect_canonical_trigger ──

    #[test]
    fn detects_canonical_case_insensitively() {
        assert_eq!(
            detect_canonical_trigger("HEY MINION write an email", &phrases()),
            Some("Hey Minion".to_string())
        );
        assert_eq!(
            detect_canonical_trigger("hey assistant draft a tweet", &phrases()),
            Some("Hey Assistant".to_string())
        );
    }

    #[test]
    fn configured_order_wins() {
        // A candidate can only start with one phrase, but the first
        // configured match must win regardless of list order.
        let reversed = vec!["Hey Assistant".to_string(), "Hey Minion".to_string()];
        assert_eq!(
            detect_canonical_trigger("hey assistant go", &reversed),
            Some("Hey Assistant".to_string())
        );
    }

    #[test]
    fn tolerates_leading_punctuation_and_whitespace() {
        assert_eq!(
            detect_canonical_trigger("  ... Hey Minion, write", &phrases()),
            Some("Hey Minion".to_string())
        );
    }

    #[test]
    fn returns_none_for_plain_request_or_empty_phrases() {
        assert!(detect_canonical_trigger("write an email please", &phrases()).is_none());
        assert!(detect_canonical_trigger("hey minion", &[]).is_none());
        // Blank configured entries are skipped, not matched.
        let blanks = vec![String::new(), "   ".to_string()];
        assert!(detect_canonical_trigger("hey minion write", &blanks).is_none());
    }

    // ── canonicalize_leading_trigger ──

    #[test]
    fn rewrites_misheard_minion() {
        assert_eq!(
            canonicalize_leading_trigger("hey million write an email", "Hey Minion").unwrap(),
            "Hey Minion write an email"
        );
    }

    #[test]
    fn preserves_following_punctuation_and_whitespace() {
        assert_eq!(
            canonicalize_leading_trigger("hey million, write for me an email", "Hey Minion")
                .unwrap(),
            "Hey Minion, write for me an email"
        );
        assert_eq!(
            canonicalize_leading_trigger("hey assistance   draft a tweet", "Hey Assistant")
                .unwrap(),
            "Hey Assistant   draft a tweet"
        );
    }

    #[test]
    fn handles_dropped_hey() {
        assert_eq!(
            canonicalize_leading_trigger("minion write an email", "Hey Minion").unwrap(),
            "Hey Minion write an email"
        );
    }

    #[test]
    fn returns_none_without_variant_and_never_prepends() {
        assert!(canonicalize_leading_trigger("please write an email", "Hey Minion").is_none());
        assert!(canonicalize_leading_trigger("", "Hey Minion").is_none());
        assert!(canonicalize_leading_trigger("hey million write", "").is_none());
    }

    #[test]
    fn ignores_variant_outside_window() {
        // "minion" is the 5th word — must not rewrite the sentence head.
        assert!(
            canonicalize_leading_trigger("please do not wake the minion up", "Hey Minion")
                .is_none()
        );
    }

    #[test]
    fn is_idempotent_on_canonical_text() {
        let once = canonicalize_leading_trigger("hey minion write an email", "Hey Minion").unwrap();
        assert_eq!(once, "Hey Minion write an email");
        let twice = canonicalize_leading_trigger(&once, "Hey Minion").unwrap();
        assert_eq!(twice, once);
    }
}
