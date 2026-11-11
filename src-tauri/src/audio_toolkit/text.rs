use natural::phonetics::soundex;
use once_cell::sync::Lazy;
use regex::Regex;
use strsim::levenshtein;

use super::accent_map::TRUNCATED_WORDS;
use super::spelling_dictionaries::{convert_us_to_british_with_dict, SpellingDictionary};
use crate::settings::{CustomWord, WordCorrectionMode, WordReplacement};

/// Builds an n-gram string by cleaning and concatenating words
///
/// Strips punctuation from each word, lowercases, and joins without spaces.
/// This allows matching "Charge B" against "ChargeBee".
fn build_ngram(words: &[&str]) -> String {
    words
        .iter()
        .map(|w| build_match_key(w))
        .collect::<Vec<_>>()
        .concat()
}

fn build_match_key(word: &str) -> String {
    word.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

struct CustomWordMatchKey {
    word_index: usize,
    key: String,
}

fn build_custom_word_match_keys(word: &str, word_index: usize) -> Vec<CustomWordMatchKey> {
    let primary_key = build_match_key(word);
    let mut keys = Vec::with_capacity(2);

    // The fallback matcher is intentionally limited to ASCII terms. Its
    // whitespace tokenization and Soundex scoring are not suitable for CJK
    // scripts. Unicode custom words remain available to models that accept
    // them as native decode prompts; they are simply skipped by this fallback.
    if is_supported_fuzzy_key(&primary_key) {
        keys.push(CustomWordMatchKey {
            word_index,
            key: primary_key.clone(),
        });
    }

    if word.contains('&') {
        let expanded_key = build_match_key(&word.replace('&', " and "));
        if is_supported_fuzzy_key(&expanded_key) && expanded_key != primary_key {
            keys.push(CustomWordMatchKey {
                word_index,
                key: expanded_key,
            });
        }
    }

    keys
}

fn is_supported_fuzzy_key(key: &str) -> bool {
    !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric())
}

fn supports_soundex(key: &str) -> bool {
    !key.is_empty() && key.chars().all(|c| c.is_ascii_alphabetic())
}

/// Finds the best matching custom word for a candidate string
///
/// Uses Levenshtein distance and Soundex phonetic matching to find
/// the best match above the given threshold.
///
/// # Arguments
/// * `candidate` - The cleaned/lowercased candidate string to match
/// * `custom_words` - Original custom words (for returning the replacement)
/// * `custom_word_match_keys` - Normalized custom-word keys for comparison
/// * `threshold` - Maximum similarity score to accept
///
/// # Returns
/// The best matching custom word and its score, if any match was found
fn find_best_match<'a>(
    candidate: &str,
    custom_words: &'a [String],
    custom_word_match_keys: &[CustomWordMatchKey],
    threshold: f64,
) -> Option<(&'a String, f64)> {
    if !is_supported_fuzzy_key(candidate) || candidate.chars().count() > 50 {
        return None;
    }

    let mut best_match: Option<&String> = None;
    let mut best_score = f64::MAX;

    for custom_word_key in custom_word_match_keys {
        // Skip if lengths are too different (optimization + prevents over-matching)
        // Use percentage-based check: max 25% length difference (prevents n-grams from
        // matching significantly shorter custom words, e.g., "openaigpt" vs "openai")
        let candidate_len = candidate.chars().count();
        let custom_word_len = custom_word_key.key.chars().count();
        let len_diff = candidate_len.abs_diff(custom_word_len) as f64;
        let max_len = candidate_len.max(custom_word_len) as f64;
        let max_allowed_diff = (max_len * 0.25).max(2.0); // At least 2 chars difference allowed
        if len_diff > max_allowed_diff {
            continue;
        }

        // Calculate Levenshtein distance (normalized by length)
        let levenshtein_dist = levenshtein(candidate, &custom_word_key.key);
        let levenshtein_score = if max_len > 0.0 {
            levenshtein_dist as f64 / max_len
        } else {
            1.0
        };

        // Soundex is an English/ASCII phonetic algorithm. Numeric terms can
        // still use edit distance, but must not receive a phonetic boost.
        let phonetic_match = supports_soundex(candidate)
            && supports_soundex(&custom_word_key.key)
            && soundex(candidate, &custom_word_key.key);

        // Combine scores: favor phonetic matches, but also consider string similarity
        let combined_score = if phonetic_match {
            levenshtein_score * 0.3 // Give significant boost to phonetic matches
        } else {
            levenshtein_score
        };

        // Accept if the score is good enough (configurable threshold)
        if combined_score < threshold && combined_score < best_score {
            best_match = Some(&custom_words[custom_word_key.word_index]);
            best_score = combined_score;
        }
    }

    best_match.map(|m| (m, best_score))
}

/// Applies custom word corrections to transcribed text using fuzzy matching
///
/// This function corrects words in the input text by finding the best matches
/// from a list of custom words using a combination of:
/// - Levenshtein distance for string similarity
/// - Soundex phonetic matching for pronunciation similarity
/// - N-gram matching for multi-word speech artifacts (e.g., "Charge B" -> "ChargeBee")
///
/// # Arguments
/// * `text` - The input text to correct
/// * `custom_words` - List of custom words to match against
/// * `threshold` - Maximum similarity score to accept (0.0 = exact match, 1.0 = any match)
///
/// # Returns
/// The corrected text with custom words applied
pub fn apply_custom_words(text: &str, custom_words: &[String], threshold: f64) -> String {
    if custom_words.is_empty() {
        return text.to_string();
    }

    // Pre-compute normalized comparison keys to avoid repeated allocations.
    let custom_word_match_keys: Vec<CustomWordMatchKey> = custom_words
        .iter()
        .enumerate()
        .flat_map(|(index, word)| build_custom_word_match_keys(word, index))
        .collect();

    let words: Vec<&str> = text.split_whitespace().collect();
    let mut result = Vec::new();
    let mut i = 0;

    while i < words.len() {
        let mut best_match: Option<(usize, &String, f64)> = None;

        // Consider n-grams up to three words and choose the closest match. A
        // longest-first match can consume a following ordinary word when both
        // candidates happen to share a Soundex code (for example,
        // "Charge B, che" matching "ChargeBee").
        for n in (1..=3).rev() {
            if i + n > words.len() {
                continue;
            }

            let ngram_words = &words[i..i + n];
            // Do not consume across a punctuation boundary. In
            // "Charge B, che", the comma closes the candidate at "B,".
            if ngram_words[..n.saturating_sub(1)]
                .iter()
                .any(|word| !extract_punctuation(word).1.is_empty())
            {
                continue;
            }
            let ngram = build_ngram(ngram_words);

            if let Some((replacement, score)) =
                find_best_match(&ngram, custom_words, &custom_word_match_keys, threshold)
            {
                let is_better = best_match
                    .as_ref()
                    .is_none_or(|(_, _, best_score)| score < *best_score);
                if is_better {
                    best_match = Some((n, replacement, score));
                }
            }
        }

        if let Some((n, replacement, _)) = best_match {
            let ngram_words = &words[i..i + n];
            // Extract punctuation from first and last words of the n-gram.
            let (prefix, _) = extract_punctuation(ngram_words[0]);
            let (_, suffix) = extract_punctuation(ngram_words[n - 1]);

            // Preserve case from first word.
            let corrected = preserve_case_pattern(ngram_words[0], replacement);

            result.push(format!("{}{}{}", prefix, corrected, suffix));
            i += n;
        } else {
            result.push(words[i].to_string());
            i += 1;
        }
    }

    result.join(" ")
}

/// A comparison target for advanced custom word matching.
///
/// Each entry pairs a normalized comparison form (lowercase, spaces removed)
/// with the canonical word it should be replaced with.
struct AdvancedComparisonEntry<'a> {
    /// The canonical word to use as the replacement when matched.
    canonical: &'a String,
    /// The normalized comparison string (lowercase, spaces removed).
    normalized: String,
    /// The original form of this comparison target (for Soundex).
    /// For the word itself this is the lowered form; for pronunciations,
    /// this is the lowered pronunciation with spaces collapsed.
    lowered: String,
}

/// Applies advanced custom word corrections with pronunciation variants.
///
/// Works like `apply_custom_words`, but expands comparison targets to include
/// each word's pronunciation variants. When a pronunciation matches, the
/// transcribed text is replaced with the canonical `word`.
///
/// # Arguments
/// * `text` - The input text to correct
/// * `advanced_words` - List of custom words with optional pronunciations
/// * `threshold` - Maximum similarity score to accept (0.0 = exact match, 1.0 = any match)
///
/// # Returns
/// The corrected text with custom words applied
pub fn apply_advanced_custom_words(
    text: &str,
    advanced_words: &[CustomWord],
    threshold: f64,
) -> String {
    if advanced_words.is_empty() {
        return text.to_string();
    }

    // Build expanded comparison entries:
    // For each CustomWord, create entries for:
    //   1. The canonical word itself
    //   2. Each pronunciation variant
    // All entries point back to the canonical word as the replacement target.
    let mut entries: Vec<AdvancedComparisonEntry<'_>> = Vec::new();

    for cw in advanced_words {
        let word_lower = cw.word.to_lowercase();
        let word_nospace = word_lower.replace(' ', "");

        // Entry for the canonical word itself
        entries.push(AdvancedComparisonEntry {
            canonical: &cw.word,
            normalized: word_nospace.clone(),
            lowered: word_lower.clone(),
        });

        // Entries for each pronunciation
        for pron in &cw.pronunciations {
            let pron_lower = pron.to_lowercase();
            let pron_nospace = pron_lower.replace(' ', "");

            entries.push(AdvancedComparisonEntry {
                canonical: &cw.word,
                normalized: pron_nospace,
                lowered: pron_lower,
            });
        }
    }

    if entries.is_empty() {
        return text.to_string();
    }

    let words: Vec<&str> = text.split_whitespace().collect();
    let mut result = Vec::new();
    let mut i = 0;

    while i < words.len() {
        let mut matched = false;

        // Try n-grams from longest (3) to shortest (1) - greedy matching
        for n in (1..=3).rev() {
            if i + n > words.len() {
                continue;
            }

            let ngram_words = &words[i..i + n];
            let ngram = build_ngram(ngram_words);

            if ngram.is_empty() || ngram.len() > 50 {
                continue;
            }

            // Find the best match across all comparison entries
            let mut best_replacement: Option<&String> = None;
            let mut best_score = f64::MAX;

            for entry in &entries {
                // Skip if lengths are too different
                let len_diff = (ngram.len() as i32 - entry.normalized.len() as i32).abs() as f64;
                let max_len = ngram.len().max(entry.normalized.len()) as f64;
                let max_allowed_diff = (max_len * 0.25).max(2.0);
                if len_diff > max_allowed_diff {
                    continue;
                }

                // Calculate Levenshtein distance (normalized by length)
                let levenshtein_dist = levenshtein(&ngram, &entry.normalized);
                let max_len = ngram.len().max(entry.normalized.len()) as f64;
                let levenshtein_score = if max_len > 0.0 {
                    levenshtein_dist as f64 / max_len
                } else {
                    1.0
                };

                // Calculate phonetic similarity using Soundex
                let phonetic_match = soundex(&ngram, &entry.lowered);

                // Combine scores: favor phonetic matches
                let combined_score = if phonetic_match {
                    levenshtein_score * 0.3
                } else {
                    levenshtein_score
                };

                if combined_score < threshold && combined_score < best_score {
                    best_replacement = Some(entry.canonical);
                    best_score = combined_score;
                }
            }

            if let Some(replacement) = best_replacement {
                // Extract punctuation from first and last words of the n-gram
                let (prefix, _) = extract_punctuation(ngram_words[0]);
                let (_, suffix) = extract_punctuation(ngram_words[n - 1]);

                // Preserve case from first word
                let corrected = preserve_case_pattern(ngram_words[0], replacement);

                result.push(format!("{}{}{}", prefix, corrected, suffix));
                i += n;
                matched = true;
                break;
            }
        }

        if !matched {
            result.push(words[i].to_string());
            i += 1;
        }
    }

    result.join(" ")
}

/// Applies exact word replacements to transcribed text.
///
/// This is a simpler mode than pronunciation matching: when a mistranslation
/// appears in the transcript (respecting word boundaries), it is replaced
/// with the correction.
///
/// # Arguments
/// * `text` - The input text to correct
/// * `replacements` - List of word replacement rules (mistranslation → correction)
///
/// # Returns
/// The corrected text with replacements applied
///
/// # Examples
/// - "open a i" → "OpenAI" (with replacement {mistranslation: "open a i", correction: "OpenAI"})
/// - "the" will NOT match within "there" or "them" (word boundary enforcement)
/// - Multi-word mistranslations are supported: "charge b" → "ChargeBee"
pub fn apply_word_replacements(
    text: &str,
    replacements: &[crate::settings::WordReplacement],
) -> String {
    if replacements.is_empty() {
        return text.to_string();
    }

    // Pre-process replacements: lowercase for matching, store original correction
    let replacement_map: Vec<(&str, &str, String)> = replacements
        .iter()
        .map(|r| {
            let mistranslation_lower = r.mistranslation.to_lowercase();
            (
                r.mistranslation.as_str(),
                r.correction.as_str(),
                mistranslation_lower,
            )
        })
        .collect();

    let words: Vec<&str> = text.split_whitespace().collect();
    let mut result = Vec::new();
    let mut i = 0;

    while i < words.len() {
        let mut matched = false;

        // Try to match each replacement (from longest to shortest to prefer longer matches)
        // Sort by mistranslation word count would be expensive, so we try all and take the best match
        for (_mistranslation_orig, correction, mistranslation_lower) in &replacement_map {
            // Count words in the mistranslation
            let mistranslation_words: Vec<&str> = mistranslation_lower.split_whitespace().collect();
            let num_words = mistranslation_words.len();

            if i + num_words > words.len() {
                continue;
            }

            // Check if the next N words match the mistranslation
            let ngram_words = &words[i..i + num_words];

            // Build the n-gram for comparison (lowercase, punctuation-stripped)
            let ngram: String = ngram_words
                .iter()
                .map(|w| {
                    w.chars()
                        .filter(|c| c.is_alphanumeric())
                        .collect::<String>()
                        .to_lowercase()
                })
                .collect::<Vec<_>>()
                .join("");

            // Also build with spaces for word-boundary matching
            let ngram_with_spaces: String = ngram_words
                .iter()
                .map(|w| {
                    w.chars()
                        .filter(|c| c.is_alphanumeric())
                        .collect::<String>()
                        .to_lowercase()
                })
                .collect::<Vec<_>>()
                .join(" ");

            // The mistranslation might be a single word or multiple words
            let mistranslation_normalized = mistranslation_words.join("");

            // Match if:
            // 1. Single word: exact alphanumeric match
            // 2. Multiple words: exact sequence match
            let is_match = if num_words == 1 {
                // Single word: must match exactly (with punctuation stripped)
                ngram == mistranslation_normalized
            } else {
                // Multiple words: each word must match exactly
                ngram_with_spaces == mistranslation_words.join(" ")
            };

            if is_match {
                // Extract punctuation from first and last words
                let (prefix, _) = extract_punctuation(ngram_words[0]);
                let (_, suffix) = extract_punctuation(ngram_words[num_words - 1]);

                // Preserve case from first word
                let corrected = preserve_case_pattern(ngram_words[0], correction);

                result.push(format!("{}{}{}", prefix, corrected, suffix));
                i += num_words;
                matched = true;
                break;
            }
        }

        if !matched {
            result.push(words[i].to_string());
            i += 1;
        }
    }

    result.join(" ")
}

fn preserve_case_pattern(original: &str, replacement: &str) -> String {
    if original.chars().all(|c| c.is_uppercase()) {
        replacement.to_uppercase()
    } else if original.chars().next().map_or(false, |c| c.is_uppercase()) {
        let mut chars: Vec<char> = replacement.chars().collect();
        if let Some(first_char) = chars.get_mut(0) {
            *first_char = first_char.to_uppercase().next().unwrap_or(*first_char);
        }
        chars.into_iter().collect()
    } else {
        replacement.to_string()
    }
}

/// Extracts punctuation prefix and suffix from a word.
/// Returns (prefix, suffix) where prefix is leading non-alphanumeric characters
/// and suffix is trailing non-alphanumeric characters.
///
/// Uses byte indices from char_indices() to correctly handle multi-byte UTF-8
/// characters (CJK, emoji, etc.) — plain char counting would produce invalid
/// byte offsets that panic on non-ASCII text.
fn extract_punctuation(word: &str) -> (&str, &str) {
    // Find byte offset where alphanumeric content starts
    let prefix_end = word
        .char_indices()
        .take_while(|(_, c)| !c.is_alphanumeric())
        .map(|(i, c)| i + c.len_utf8())
        .last()
        .unwrap_or(0);

    // Find byte offset where alphanumeric content ends
    let suffix_start = word
        .char_indices()
        .rev()
        .take_while(|(_, c)| !c.is_alphanumeric())
        .map(|(i, _)| i)
        .last()
        .unwrap_or(word.len());

    let prefix = if prefix_end > 0 {
        &word[..prefix_end]
    } else {
        ""
    };

    let suffix = if suffix_start < word.len() {
        &word[suffix_start..]
    } else {
        ""
    };

    (prefix, suffix)
}

/// Evidence for the language of the text being cleaned.
///
/// This intentionally describes the transcription output, not Handy's UI
/// language. Unknown output languages fail closed: built-in filler removal is
/// skipped rather than applying a language profile speculatively.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutputLanguageEvidence {
    UserSelected(String),
    ModelConstrained(String),
    /// The transcription model itself identified the language (audio-based
    /// LID, e.g. Whisper in auto mode).
    ModelDetected(String),
    /// Detected from the transcribed text with high confidence, constrained to
    /// the model's supported languages. Weakest accepted evidence.
    TextDetected(String),
    TranslatedToEnglish,
    Unknown,
}

impl OutputLanguageEvidence {
    pub(crate) fn language(&self) -> Option<&str> {
        match self {
            Self::UserSelected(language)
            | Self::ModelConstrained(language)
            | Self::ModelDetected(language)
            | Self::TextDetected(language) => Some(language),
            Self::TranslatedToEnglish => Some("en"),
            Self::Unknown => None,
        }
    }
}

/// Filler tokens that are not lexical words in any language Handy's models can
/// output, so removing them cannot corrupt text regardless of the (possibly
/// unknown) output language. Kept deliberately conservative: anything that is a
/// real word somewhere ("um" pt/de, "ha" es, "ah"/"eh" interjections, "mm"
/// millimetres) belongs in the language-gated lists instead.
const UNIVERSAL_FILLER_WORDS: &[&str] = &[
    "uh", "uhm", "umm", "uhh", "uhhh", "ehh", "ehm", "ahm", "hmm", "hm", "mmm", "хм", "ммм",
];

/// Filler words that are only safe to remove with evidence for the output
/// language, because the same token is a real word elsewhere (e.g. Portuguese
/// "um" = "a/an", German "um" = "at/around", Spanish "ha" = "has").
fn gated_filler_words_for_language(lang: &str) -> &'static [&'static str] {
    let base_lang = lang.split(&['-', '_'][..]).next().unwrap_or(lang);

    match base_lang {
        "en" => &["um", "ah", "eh"],
        "de" => &["äh", "ähm"],
        "fr" => &["euh"],
        _ => &[],
    }
}

static MULTI_SPACE_PATTERN: Lazy<Regex> = Lazy::new(|| Regex::new(r"\s{2,}").unwrap());

/// Removes word fragment overlaps from transcription output.
///
/// CTC/Transducer models like Parakeet can emit overlapping tokens at boundaries,
/// producing output like "it wa was a" where "wa" is a fragment of "was".
/// This function detects when a word is a prefix (case-insensitive) of the next word
/// and removes the fragment, with strict safeguards against removing legitimate words.
///
/// A word is considered a **fragment artifact** (and removed) only when ALL of:
/// 1. It is a case-insensitive prefix of the next word
/// 2. The current word is no longer than MAX_FRAGMENT_LEN characters (≤ 3)
///    (Single-char words also qualify — they're protected by COMMON_WORDS instead)
/// 3. The next word extends the current word by at most MAX_FRAGMENT_EXTENSION chars (≤ 5)
///    (Single-char fragments skip this check — a 1-char prefix of a longer word is
///    almost certainly CTC noise)
/// 4. It has at least 1 alphabetic character (but "a" and "I" are protected by COMMON_WORDS)
/// 5. It is NOT a common English word or short prefix (see COMMON_WORDS list)
///
/// **Staircase detection**: When a fragment is removed, the function also checks if
/// the previous kept word forms a "staircase" pattern — both are prefixes of the same
/// target word. Example: "can c candles" → both "can" and "c" prefix "candles" →
/// remove both → "candles". The previous word's COMMON_WORDS protection is relaxed
/// in this case because two consecutive prefix words pointing to the same target is
/// extremely unlikely in natural language but common in CTC artifacts.
///
/// Examples:
/// - "it wa was a" → "it was a"  ("wa" len=2, ext=1, not common → fragment)
/// - "can c candles" → "candles"  (staircase: "can"+"c" both prefix "candles")
/// - "c candles" → "candles"  ("c" len=1, single-char fragment)
/// - "for forget" → "for forget"  ("for" is common, kept)
/// - "can cancel" → "can cancel"  ("can" is common, no staircase → kept)
/// - "pro process" → "pro process"  ("pro" is in COMMON_WORDS, kept)
/// - "my mac machine" → "my mac machine"  ("mac" is in COMMON_WORDS, kept)
pub(crate) fn dedup_word_fragments(text: &str) -> String {
    // Common English words and short prefixes that should never be treated as
    // fragments, even if they are a prefix of the following word.
    //
    // This list is comprehensive for words up to 3 characters that appear
    // independently in English transcription. CTC artifacts are typically
    // 2-3 character non-words like "wa", "thi", "sta", "wan" that are NOT
    // in this list. If a short word IS in this list, it is protected regardless
    // of how much the next word extends it.
    //
    // Categories:
    // 1. Function words (the, to, can, for, etc.) — never CTC artifacts
    // 2. Common content words (day, way, run, etc.) — real words, not fragments
    // 3. Common abbreviations/prefixes (pro, con, sub, pre, etc.) — used independently
    // 4. Short words that are productive prefixes of longer words (add→adding, etc.)
    const COMMON_WORDS: &[&str] = &[
        // --- 1-letter function words ---
        "a", "i", // --- 2-letter function words ---
        "an", "as", "at", "be", "by", "do", "go", "he", "if", "in", "is", "it", "me", "my", "no",
        "of", "on", "or", "so", "to", "up", "us", "we", "am",
        // --- 2-letter common words (also productive prefixes) ---
        // These are real English words that are prefixes of longer words and must
        // NOT be treated as CTC artifacts. Without these, removing MAX_FRAGMENT_EXTENSION
        // would cause regressions (e.g., "re really" → "really" removing "re").
        "re", "ex", "un", "im", "de", "bi", "oh", "ah", "ha", "ad", "lo", "mo", "ma", "pa", "id",
        "ed", "al", "bo", "fa", "sa", "sh", "ta", "ob", "op", "aw", "en", "er", "es", "hi", "ho",
        "ok", "uh", "um", "ya", "ye", "yo", // --- 3-letter function words ---
        "and", "any", "are", "but", "can", "did", "for", "get", "had", "has", "her", "him", "his",
        "how", "its", "let", "may", "not", "now", "one", "our", "out", "own", "she", "the", "too",
        "use", "was", "way", "who", "why", "yes", "yet", "you",
        // --- 3-letter common words (also productive prefixes) ---
        "add", "age", "ago", "aid", "air", "all", "arm", "art", "ask", "bad", "bag", "ban", "bat",
        "bed", "big", "bit", "bow", "box", "boy", "bug", "buy", "cab", "cap", "car", "cat", "cut",
        "day", "die", "dig", "dim", "dip", "doc", "dog", "dot", "dry", "ear", "eat", "egg", "end",
        "era", "eve", "eye", "fan", "far", "fat", "few", "fig", "fin", "fix", "fly", "fog", "fun",
        "gap", "gas", "gin", "got", "gun", "gut", "ham", "hat", "hid", "hip", "hit", "hog", "hop",
        "hot", "hug", "ice", "ill", "imp", "ink", "inn", "ins", "ion", "ire", "jam", "jar", "jet",
        "job", "jog", "joy", "key", "kid", "kit", "lab", "lap", "law", "lay", "led", "leg", "lie",
        "lip", "lit", "log", "lot", "low", "mad", "man", "map", "mat", "met", "mid", "mix", "mob",
        "mod", "mop", "mud", "nap", "net", "new", "nod", "nor", "nut", "oak", "odd", "off", "oil",
        "old", "opt", "ore", "pad", "pan", "pat", "pay", "pen", "pet", "pie", "pig", "pin", "pit",
        "pod", "pop", "pot", "pro", "pub", "put", "rag", "ram", "ran", "rat", "raw", "ray", "ref",
        "rep", "rib", "rid", "rig", "rim", "rip", "rob", "rod", "rot", "row", "rub", "rug", "run",
        "rut", "sad", "sat", "saw", "say", "sea", "see", "set", "sew", "shy", "sin", "sir", "sit",
        "six", "ski", "sky", "sob", "son", "sow", "spa", "spy", "sub", "sum", "sun", "tab", "tag",
        "tan", "tap", "tax", "tea", "ten", "tie", "tin", "tip", "toe", "ton", "top", "tow", "toy",
        "try", "tub", "van", "vat", "vet", "via", "vim", "vow", "war", "wax", "web", "wed", "wet",
        "win", "wit", "wok", "won", "woo", "wow", "zip", "zoo",
        // --- 2-letter abbreviations ---
        "st", // St (Saint/Street) → Street, St.
        // --- Abbreviations / prefixes commonly used in transcription ---
        "bar", "con", "dis", "pre", "per", "app", "co", "mac", "bus", "int", "sys", "sec", "tel",
        "org", "dev",
    ];

    // Maximum length of a word that can be considered a fragment artifact.
    // CTC fragments are typically 2-3 characters (e.g. "wa", "th", "co").
    // Real words can be any length, so we set a conservative cutoff.
    // Words of length 4+ are almost never CTC artifacts.
    const MAX_FRAGMENT_LEN: usize = 3;

    // Maximum number of additional characters in the next word beyond the
    // current word that still qualifies as a fragment overlap.
    // This provides a secondary safety net for 2-letter words not in COMMON_WORDS:
    // - "re" → "really" (ext=4): protected because "re" IS in COMMON_WORDS
    // - "wa" → "was" (ext=1): caught because ext=1 ≤ MAX_FRAGMENT_EXTENSION
    // - "sta" → "starting" (ext=5): caught because ext=5 ≤ MAX_FRAGMENT_EXTENSION
    // - "mac" → "machine" (ext=4): protected because "mac" IS in COMMON_WORDS
    // Without this limit, even with COMMON_WORDS, obscure 2-letter words could be
    // falsely removed. The limit provides defense-in-depth.
    const MAX_FRAGMENT_EXTENSION: usize = 5;

    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() < 2 {
        return text.to_string();
    }

    let mut result: Vec<String> = Vec::new();
    let mut i = 0;

    while i < words.len() {
        // Look ahead: if the current word is a prefix of the next word,
        // it might be a fragment that should be removed.
        let current_alpha: String = words[i].chars().filter(|c| c.is_alphabetic()).collect();
        let current_lower = current_alpha.to_lowercase();

        if i + 1 < words.len() && !current_alpha.is_empty() {
            let next_alpha: String = words[i + 1].chars().filter(|c| c.is_alphabetic()).collect();
            let next_lower = next_alpha.to_lowercase();

            // Only consider prefix match when next word is strictly longer
            let next_is_longer = next_alpha.len() > current_alpha.len();

            if next_is_longer {
                // Current word is a potential fragment if it's a case-insensitive
                // prefix of the next word
                let is_prefix = next_lower.starts_with(&current_lower);

                if is_prefix {
                    // Determine if the extension length check should be skipped.
                    // Single-letter fragments (like "c" before "candles") are almost certainly
                    // CTC artifacts — a genuine English word would have more characters.
                    // The only single-letter English words ("a", "I") are in COMMON_WORDS.
                    let is_single_char = current_alpha.len() == 1;
                    let extension = next_alpha.len() - current_alpha.len();
                    let extension_ok = is_single_char || extension <= MAX_FRAGMENT_EXTENSION;

                    // Current word is a fragment of the next word only when ALL conditions hold:
                    // 1. Current is short enough to be a CTC artifact (≤ MAX_FRAGMENT_LEN)
                    //    Single-char words bypass this check (they're caught by COMMON_WORDS instead)
                    // 2. Extension is within bounds (≤ MAX_FRAGMENT_EXTENSION)
                    //    Single-char words skip this check (see is_single_char above)
                    // 3. Current is NOT a common word/prefix
                    if (is_single_char || current_alpha.len() <= MAX_FRAGMENT_LEN)
                        && extension_ok
                        && !COMMON_WORDS.contains(&current_lower.as_str())
                    {
                        // Skip this word — it's a fragment of the next word
                        //
                        // Staircase detection: also remove the previous kept word if it
                        // forms a "staircase" with this fragment — i.e., both the previous
                        // kept word and this fragment are prefixes of the same target word.
                        //
                        // Example: "can c candles" → "can" and "c" both prefix "candles"
                        // → remove both → "candles"
                        //
                        // This is safe because two consecutive prefix words pointing to the
                        // same target is extremely unlikely in natural language but common
                        // in CTC artifacts. The key safety constraints are:
                        // - The previous word must be short (≤ MAX_FRAGMENT_LEN + 1 = 4 chars)
                        // - The previous word must be a genuine prefix (not just same first letter)
                        // - The previous word must NOT be a single-letter word (protects "a", "I")
                        // - The target word must be significantly longer than the previous word
                        //   (extension > MAX_FRAGMENT_EXTENSION), ensuring a real staircase
                        // - In a staircase, COMMON_WORDS protection is relaxed because two
                        //   consecutive prefixes pointing to the same target is CTC noise
                        if !result.is_empty() {
                            let prev_alpha: String = result
                                .last()
                                .unwrap()
                                .chars()
                                .filter(|c: &char| c.is_alphabetic())
                                .collect();
                            let prev_lower = prev_alpha.to_lowercase();

                            // Staircase: previous word is also a prefix of the same target
                            if !prev_alpha.is_empty()
                                && prev_alpha.len() >= 2  // Not a single-letter ("a", "I")
                                && prev_alpha.len() <= MAX_FRAGMENT_LEN + 1  // Short enough
                                && next_lower.starts_with(&prev_lower)
                            // Also prefixes target
                            {
                                // Remove the previous word — it's part of the staircase
                                result.pop();
                            }
                        }

                        i += 1;
                        continue;
                    }
                }
            }
        }

        result.push(words[i].to_string());
        i += 1;
    }

    result.join(" ")
}

/// Protected words that should NOT be deduplicated even if repeated.
/// These are commonly used in legitimate speech patterns (stuttering, emphasis, etc.).
const PROTECTED_WORDS: &[&str] = &[
    // Pronouns - common in stuttering/stammering
    "i", "you", "he", "she", "we", "they", "it", "me", "him", "her", "us", "them",
    // Articles
    "a", "an", "the", // Common intensifiers (often repeated for emphasis)
    "very", "so", "quite", "really", "too", "just", // Conjunctions
    "and", "or", "but", "if", "then", "because", "that", "which", // Common prepositions
    "to", "for", "with", "from", "in", "on", "at", "by",
    // Common verbs that might be repeated
    "is", "was", "are", "were", "be", "been", "have", "has", "had", "do", "does", "did",
    // Common adverbs
    "not", "now", "here", "there", "how", "why", "when", "where",
];

/// Checks if a word is in the protected list (case-insensitive).
fn is_protected_word(word: &str) -> bool {
    let word_lower = word.to_lowercase();
    PROTECTED_WORDS.contains(&word_lower.as_str())
}

/// Detects repeated consecutive words in text.
///
/// Returns a vector of (word, count) tuples for words that appear consecutively
/// (count >= 2). This helps detect transcription artifacts like "def def definitely"
/// or "the the house".
///
/// # Arguments
/// * `text` - The text to analyze
///
/// # Returns
/// Vector of (word, count) tuples for repeated consecutive words
pub fn detect_repeated_words(text: &str) -> Vec<(&str, usize)> {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return Vec::new();
    }

    let mut repetitions: Vec<(&str, usize)> = Vec::new();
    let mut i = 0;

    while i < words.len() {
        // Extract alphabetic part for comparison
        let current_alpha: String = words[i].chars().filter(|c| c.is_alphabetic()).collect();
        let current_lower = current_alpha.to_lowercase();

        if current_alpha.is_empty() {
            i += 1;
            continue;
        }

        // Count consecutive repetitions (case-insensitive, alphabetic only)
        let mut count = 1;
        while i + count < words.len() {
            let next_alpha: String = words[i + count]
                .chars()
                .filter(|c| c.is_alphabetic())
                .collect();
            let next_lower = next_alpha.to_lowercase();

            if next_alpha.is_empty() || next_lower != current_lower {
                break;
            }
            count += 1;
        }

        if count >= 2 {
            repetitions.push((words[i], count));
        }

        i += count;
    }

    repetitions
}

/// Removes repeated consecutive words based on suppression level.
///
/// Protected words (common pronouns, articles, intensifiers, etc.) are NEVER
/// deduplicated even if repeated, as this is legitimate speech pattern.
///
/// Level 0 (Off): No removal
/// Level 1 (Light): Remove 3+ repetitions to single instance (e.g., "def def def" -> "def")
/// Level 2 (Moderate): Remove 2+ repetitions to single instance (e.g., "def def" -> "def")
/// Level 3 (Aggressive): Same as Moderate
///
/// # Arguments
/// * `text` - The text to process
/// * `level` - Suppression level (0-3)
///
/// # Returns
/// The text with repetitions removed according to level
pub fn suppress_repeated_words(text: &str, level: u8) -> String {
    if level == 0 {
        return text.to_string();
    }

    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return text.to_string();
    }

    let min_repetitions = if level >= 2 { 2 } else { 3 };

    let mut result: Vec<String> = Vec::new();
    let mut i = 0;

    while i < words.len() {
        // Extract alphabetic part for comparison
        let current_alpha: String = words[i].chars().filter(|c| c.is_alphabetic()).collect();
        let current_lower = current_alpha.to_lowercase();

        if current_alpha.is_empty() {
            result.push(words[i].to_string());
            i += 1;
            continue;
        }

        // Count consecutive repetitions
        let mut count = 1;
        while i + count < words.len() {
            let next_alpha: String = words[i + count]
                .chars()
                .filter(|c| c.is_alphabetic())
                .collect();
            let next_lower = next_alpha.to_lowercase();

            if !next_alpha.is_empty() && next_lower == current_lower {
                count += 1;
            } else {
                break;
            }
        }

        // Check if this word is protected (should never be deduplicated)
        if is_protected_word(&current_lower) {
            // Keep all instances of protected words
            for j in 0..count {
                result.push(words[i + j].to_string());
            }
            i += count;
            continue;
        }

        // If repetitions meet threshold, keep only one
        if count >= min_repetitions {
            // Keep only one instance, preserving punctuation
            result.push(words[i].to_string());
            i += count;
        } else {
            // Keep all instances
            for j in 0..count {
                result.push(words[i + j].to_string());
            }
            i += count;
        }
    }

    result.join(" ")
}

/// Collapses repeated words (3+ repetitions) to a single instance.
/// E.g., "wh wh wh wh" -> "wh", "I I I I" -> "I"
fn collapse_stutters(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return text.to_string();
    }

    let mut result: Vec<&str> = Vec::new();
    let mut i = 0;

    while i < words.len() {
        let word = words[i];
        let word_lower = word.to_lowercase();

        if word_lower.chars().all(|c| c.is_alphabetic()) {
            // Count consecutive repetitions (case-insensitive)
            let mut count = 1;
            while i + count < words.len() && words[i + count].to_lowercase() == word_lower {
                count += 1;
            }

            // If 3+ repetitions, collapse to single instance
            if count >= 3 {
                result.push(word);
                i += count;
            } else {
                result.push(word);
                i += 1;
            }
        } else {
            result.push(word);
            i += 1;
        }
    }

    result.join(" ")
}

/// Whether a word appended to `kept` would open a sentence: nothing but
/// whitespace so far, or the last visible character ends a sentence.
fn opens_sentence(kept: &str) -> bool {
    kept.trim_end()
        .chars()
        .next_back()
        .is_none_or(|c| matches!(c, '.' | '!' | '?' | '…'))
}

/// Appends `segment` to `kept`. While `capital_owed` is set, the first
/// alphanumeric character of `segment` is uppercased and the debt is settled.
fn push_restoring_capital(kept: &mut String, segment: &str, capital_owed: &mut bool) {
    if *capital_owed {
        if let Some((index, first)) = segment.char_indices().find(|(_, c)| c.is_alphanumeric()) {
            *capital_owed = false;
            kept.push_str(&segment[..index]);
            kept.extend(first.to_uppercase());
            kept.push_str(&segment[index + first.len_utf8()..]);
            return;
        }
    }
    kept.push_str(segment);
}

/// Deletes every match of one filler pattern. A capitalized filler that opened
/// a sentence hands its capital to the word that takes its place, so
/// "Um, so I think" becomes "So I think" rather than "so I think".
fn remove_filler_matches(text: &str, pattern: &Regex) -> String {
    let mut kept = String::with_capacity(text.len());
    let mut resume = 0;
    let mut capital_owed = false;

    for filler in pattern.find_iter(text) {
        push_restoring_capital(&mut kept, &text[resume..filler.start()], &mut capital_owed);
        let capitalized = filler.as_str().starts_with(char::is_uppercase);
        capital_owed |= capitalized && opens_sentence(&kept);
        resume = filler.end();
    }
    push_restoring_capital(&mut kept, &text[resume..], &mut capital_owed);

    kept
}

/// Removes filler words from transcription output when enabled.
///
/// Built-in removal is two-tiered: [`UNIVERSAL_FILLER_WORDS`] apply regardless
/// of language evidence, while [`gated_filler_words_for_language`] tokens are
/// only removed when the output language is known. A custom list is an
/// explicit user override and replaces both tiers without requiring language
/// evidence. `Some(empty vec)` disables removal, preserving the legacy
/// power-user setting. The master toggle takes precedence over both built-in
/// and custom lists.
///
/// # Arguments
/// * `text` - The raw transcription text to filter
/// * `language` - Evidence for the language of the transcription output
/// * `custom_filler_words` - Optional user-provided filler word list. `Some(vec)` overrides
///   language defaults; `Some(empty vec)` disables filtering; `None` uses language defaults.
/// * `enabled` - Whether filler-word removal is enabled
///
/// # Returns
/// The text with configured filler words removed
pub fn remove_filler_words(
    text: &str,
    language: &OutputLanguageEvidence,
    custom_filler_words: &Option<Vec<String>>,
    enabled: bool,
) -> String {
    if !enabled {
        return text.to_string();
    }

    // Build filler patterns from custom list or the built-in tiers
    let patterns: Vec<Regex> = match custom_filler_words {
        Some(words) => words
            .iter()
            .filter_map(|word| Regex::new(&format!(r"(?i)\b{}\b[,.]?", regex::escape(word))).ok())
            .collect(),
        None => UNIVERSAL_FILLER_WORDS
            .iter()
            .chain(
                language
                    .language()
                    .map(gated_filler_words_for_language)
                    .unwrap_or_default(),
            )
            .map(|word| Regex::new(&format!(r"(?i)\b{}\b[,.]?", regex::escape(word))).unwrap())
            .collect(),
    };

    // Remove filler words
    let mut filtered = text.to_string();
    for pattern in &patterns {
        filtered = remove_filler_matches(&filtered, pattern);
    }

    filtered
}

/// Applies non-filler transcription cleanup.
///
/// Kept separate from [`remove_filler_words`] so disabling filler deletion
/// does not also disable the existing repeated-word and whitespace cleanup.
/// In addition to upstream's passes (stutter collapse, whitespace, trim), the
/// fork's word-fragment dedup runs here ("it wa was" -> "it was").
pub fn normalize_transcription_output(text: &str) -> String {
    let mut normalized = dedup_word_fragments(text);

    // Collapse repeated 1-2 letter words (stutter artifacts like "wh wh wh wh")
    normalized = collapse_stutters(&normalized);

    // Clean up multiple spaces to single space
    normalized = MULTI_SPACE_PATTERN
        .replace_all(&normalized, " ")
        .to_string();

    // Trim leading/trailing whitespace
    normalized.trim().to_string()
}

/// Converts US English spelling to British English.
///
/// Uses the DWYL dictionary by default (curated, human-verified, ~180 common pairs).
/// For speech-to-text, DWYL is recommended as it excludes archaic spellings
/// and semantic ambiguities.
///
/// # Arguments
/// * `text` - The input text with US English spelling
///
/// # Returns
/// Text with British English spelling applied
pub fn convert_us_to_british(text: &str) -> String {
    convert_us_to_british_with_dict(text, SpellingDictionary::Dwyl)
}

/// Converts spoken English number-word sequences into digit characters.
///
/// This is a deterministic, LLM-free transform. It only runs when the
/// application language starts with "en". Lone number words (e.g. "one" in
/// "I saw one bird") are intentionally left as words.
///
/// Supported forms include:
/// * Single-digit sequences: "zero zero four" → "004"
/// * Hyphenated compounds: "twenty-five" → "25"
/// * Scales: "one hundred", "two thousand and twenty six" → "100", "2026"
/// * Decimals: "three point five" → "3.5"
///
/// Punctuation attached to tokens is preserved on the converted result.
///
/// # Arguments
/// * `text` - The input text to process
/// * `app_language` - The application language code; only English variants convert
///
/// # Returns
/// Text with number-word runs replaced by digit strings
pub fn convert_number_words_to_digits(text: &str, app_language: &str) -> String {
    convert_number_words_to_digits_impl(text, app_language)
}

fn convert_number_words_to_digits_impl(text: &str, app_language: &str) -> String {
    // Gate on English language variants only.
    let base_lang = app_language
        .split(&['-', '_'][..])
        .next()
        .unwrap_or(app_language);
    if base_lang != "en" {
        return text.to_string();
    }

    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return text.to_string();
    }

    // Tokenize each whitespace token into (prefix, core, suffix) and split
    // hyphenated compounds into their constituent core words.
    let mut tokens: Vec<Token> = Vec::with_capacity(words.len());
    for word in &words {
        let (prefix, suffix) = extract_punctuation(word);
        let core_start = prefix.len();
        let core_end = word.len() - suffix.len();
        if core_start >= core_end {
            // No alphanumeric core; treat as a non-number word.
            tokens.push(Token {
                original: word,
                prefix,
                suffix,
                cores: vec![],
            });
            continue;
        }
        let core = &word[core_start..core_end];
        // Hyphenated compounds split into independent lowercase cores for matching.
        let cores: Vec<String> = core.to_lowercase().split('-').map(String::from).collect();
        tokens.push(Token {
            original: word,
            prefix,
            suffix,
            cores,
        });
    }

    // Identify maximal runs of number-word tokens. A token is part of a run if
    // every core inside it is a number word (or "and"), and the run as a whole
    // is not a single isolated digit word by itself.
    let mut result: Vec<String> = Vec::with_capacity(words.len());
    let mut i = 0;
    while i < tokens.len() {
        let token = &tokens[i];
        // Determine if this token is a candidate for a number run.
        let token_is_all_number_or_and = token
            .cores
            .iter()
            .all(|c| number_word_set_contains(c.as_str()));

        if !token_is_all_number_or_and {
            result.push(token.original.to_string());
            i += 1;
            continue;
        }

        // Try to extend a maximal run forward. All tokens in the run must be
        // fully composed of number words (or "and"). Hyphenated compounds are
        // allowed because all of their split cores are number words.
        let mut run_end = i + 1;
        while run_end < tokens.len() {
            let next = &tokens[run_end];
            let all_number_or_and = next
                .cores
                .iter()
                .all(|c| number_word_set_contains(c.as_str()));
            if all_number_or_and {
                run_end += 1;
            } else {
                break;
            }
        }
        let run = &tokens[i..run_end];

        // Clock-time handling: when a number-word run is immediately followed by
        // an AM/PM marker, format it as a time rather than as a plain integer.
        if let Some(marker_token) = tokens.get(run_end) {
            if let Some(marker) = am_pm_marker(marker_token) {
                // Look-behind guard: reject "two oh five pm" shapes where the
                // word just before the run is "oh" or "o".
                let preceded_by_oh = i > 0
                    && matches!(
                        tokens[i - 1].cores.as_slice(),
                        [s] if s == "oh" || s == "o"
                    );

                if !preceded_by_oh {
                    let flat_cores: Vec<&str> = run
                        .iter()
                        .flat_map(|t| t.cores.iter().map(String::as_str))
                        .collect();

                    if let Some(formatted_time) = try_format_clock_time(&flat_cores) {
                        let first = &run[0];
                        let replacement = format!(
                            "{}{} {}{}",
                            first.prefix, formatted_time, marker, marker_token.suffix
                        );
                        result.push(replacement);
                    } else {
                        // Invalid time shape before the marker: keep the entire
                        // run + marker as words rather than falling through to
                        // the additive number parser.
                        for token in &tokens[i..=run_end] {
                            result.push(token.original.to_string());
                        }
                    }
                } else {
                    // "oh"/"o" prefix means this is not a standalone time.
                    for token in &tokens[i..=run_end] {
                        result.push(token.original.to_string());
                    }
                }

                i = run_end + 1;
                continue;
            }
        }

        // Flatten the run into a single word sequence (hyphenated cores are
        // already split). We only keep real number-word cores, ignoring "and".
        // Build this early: it gates conversion for decimal runs where "point"
        // has no following number word (e.g. "one point" must stay as words).
        let flat_cores: Vec<&str> = run
            .iter()
            .flat_map(|t| t.cores.iter().map(String::as_str))
            .filter(|c| *c != "and")
            .collect();

        // Decide whether the run qualifies for conversion.
        // A run converts when it is either:
        //   - a single hyphenated compound token (e.g. "twenty-five"), or
        //   - a single token containing a scale word (e.g. "one hundred"), or
        //   - spans 2+ tokens (e.g. "zero zero four").
        // A run ending in "point" with no following number word remains words
        // because the decimal form requires at least one number word after
        // "point" (e.g. "one point" stays words, "one point two" converts).
        let has_trailing_bare_point = ends_with_bare_point(&flat_cores);
        let should_convert = if run.len() == 1 {
            let single = &run[0];
            (single.cores.len() > 1
                || single
                    .cores
                    .iter()
                    .any(|c| scale_word_set_contains(c.as_str())))
                && !has_trailing_bare_point
        } else {
            !has_trailing_bare_point
        };

        if should_convert {
            // Preserve leading punctuation from the first token and trailing
            // punctuation from the last token. Intermediate spaces collapse to a
            // single space between tokens, as the original whitespace was already
            // normalized by split_whitespace.
            let converted = convert_flat_number_words(&flat_cores);
            let first = &run[0];
            let last = &run[run.len() - 1];
            let replacement = format!("{}{}{}", first.prefix, converted, last.suffix);
            result.push(replacement);
            i = run_end;
        } else {
            // Keep the run as words. This handles lone digit words and runs where
            // "point" has no following number word (e.g. "one point").
            for t in run {
                result.push(t.original.to_string());
            }
            i = run_end;
        }
    }

    result.join(" ")
}

/// Internal representation of a whitespace token split for number-word conversion.
struct Token<'a> {
    original: &'a str,
    prefix: &'a str,
    suffix: &'a str,
    cores: Vec<String>,
}

/// The set of English words treated as number tokens for this conversion.
const NUMBER_WORDS: &[&str] = &[
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
    "twenty",
    "thirty",
    "forty",
    "fifty",
    "sixty",
    "seventy",
    "eighty",
    "ninety",
    "hundred",
    "thousand",
    "million",
    "billion",
    "and",
    "point",
];

/// Subset of number words that force conversion even in a single-token run.
///
/// These are scale words (hundred, thousand, million, billion). "point" is
/// intentionally omitted: a lone "point" should remain a word. A multi-token
/// run converts only when "point" is followed by at least one number word
/// (e.g. "three point five"), enforced by `ends_with_bare_point` in
/// `convert_number_words_to_digits_impl`.
const SCALE_WORDS: &[&str] = &["hundred", "thousand", "million", "billion"];

fn number_word_set_contains(word: &str) -> bool {
    NUMBER_WORDS.contains(&word)
}

fn scale_word_set_contains(word: &str) -> bool {
    SCALE_WORDS.contains(&word)
}

/// Returns the AM/PM marker label for a token, if it is one.
fn am_pm_marker(token: &Token) -> Option<&'static str> {
    if token.cores.len() != 1 {
        return None;
    }
    match token.cores[0].as_str() {
        "am" | "a.m" => Some("AM"),
        "pm" | "p.m" => Some("PM"),
        _ => None,
    }
}

/// Returns the hour value for a number-word core (1–12), if valid.
fn hour_word_value(core: &str) -> Option<u32> {
    match core {
        "one" => Some(1),
        "two" => Some(2),
        "three" => Some(3),
        "four" => Some(4),
        "five" => Some(5),
        "six" => Some(6),
        "seven" => Some(7),
        "eight" => Some(8),
        "nine" => Some(9),
        "ten" => Some(10),
        "eleven" => Some(11),
        "twelve" => Some(12),
        _ => None,
    }
}

/// Returns the unit/tens value for a number-word core (0–99 by tens), if valid.
fn unit_word_value(core: &str) -> Option<u32> {
    match core {
        "zero" => Some(0),
        "one" => Some(1),
        "two" => Some(2),
        "three" => Some(3),
        "four" => Some(4),
        "five" => Some(5),
        "six" => Some(6),
        "seven" => Some(7),
        "eight" => Some(8),
        "nine" => Some(9),
        "ten" => Some(10),
        "eleven" => Some(11),
        "twelve" => Some(12),
        "thirteen" => Some(13),
        "fourteen" => Some(14),
        "fifteen" => Some(15),
        "sixteen" => Some(16),
        "seventeen" => Some(17),
        "eighteen" => Some(18),
        "nineteen" => Some(19),
        "twenty" => Some(20),
        "thirty" => Some(30),
        "forty" => Some(40),
        "fifty" => Some(50),
        "sixty" => Some(60),
        "seventy" => Some(70),
        "eighty" => Some(80),
        "ninety" => Some(90),
        _ => None,
    }
}

/// Tries to format a flat sequence of number-word cores as a clock time.
///
/// Accepted shapes:
/// - hour only: "two" → "2"
/// - hour + minutes (one word, ≤ 59): "two ten" → "2:10"
/// - hour + tens + units: "two thirteen" → "2:13", "two twenty five" → "2:25"
///
/// Rejects "and", "point", scale words, invalid minutes, and unsupported shapes.
fn try_format_clock_time(cores: &[&str]) -> Option<String> {
    if cores.is_empty() {
        return None;
    }

    // Reject any core that is explicitly disallowed in a time phrase.
    if cores
        .iter()
        .any(|c| *c == "and" || *c == "point" || SCALE_WORDS.contains(c))
    {
        return None;
    }

    let hour = hour_word_value(cores[0])?;
    let minute_cores = &cores[1..];

    let minutes = match minute_cores.len() {
        0 => return Some(hour.to_string()),
        1 => {
            let v = unit_word_value(minute_cores[0])?;
            if v > 59 {
                return None;
            }
            v
        }
        2 => {
            let tens = unit_word_value(minute_cores[0])?;
            let units = unit_word_value(minute_cores[1])?;
            // First minute word must be a tens word (twenty..ninety).
            if !(20..=90).contains(&tens) || tens % 10 != 0 {
                return None;
            }
            // Second minute word must be a single digit (one..nine).
            if units > 9 {
                return None;
            }
            let sum = tens + units;
            if sum > 59 {
                return None;
            }
            sum
        }
        _ => return None,
    };

    Some(format!("{}:{:02}", hour, minutes))
}

/// Returns true when the flat core sequence ends in a bare "point" with no
/// number word following it.
///
/// Used at the run level to keep spoken trailing "point" in words
/// (e.g. "one point" stays "one point" rather than becoming "1.").
fn ends_with_bare_point(flat_cores: &[&str]) -> bool {
    flat_cores.last() == Some(&"point")
}

/// Converts a flat sequence of number-word cores into a digit string.
///
/// This handles two main forms:
/// 1. Pure single-digit sequences (only zero-nine) → emit one digit per token,
///    preserving leading zeros.
/// 2. All other valid number-word sequences → parse into an integer/decimal string.
fn convert_flat_number_words(cores: &[&str]) -> String {
    if cores.is_empty() {
        return String::new();
    }

    let all_single_digits = cores.iter().all(|c| SINGLE_DIGIT_WORDS.contains(c));
    if all_single_digits {
        return cores.iter().map(|c| word_to_digit(c)).collect::<String>();
    }

    // Check for a decimal point; if present, split and emit "int.frac".
    if let Some(pos) = cores.iter().position(|c| *c == "point") {
        let int_cores = &cores[..pos];
        let frac_cores = &cores[pos + 1..];
        let int_part = parse_number_word_sequence(int_cores);
        let frac_part: String = frac_cores
            .iter()
            .map(|c| word_to_digit(c))
            .collect::<String>();
        // Guard: a bare trailing "point" should not reach here because the run
        // level `ends_with_bare_point` check keeps those runs as words. Keeping
        // this branch defends against future direct callers of this function.
        if frac_part.is_empty() {
            return format!("{}.", int_part);
        }
        return format!("{}.{}", int_part, frac_part);
    }

    parse_number_word_sequence(cores)
}

/// Single digit words that map 1-to-1 to digit characters.
const SINGLE_DIGIT_WORDS: &[&str] = &[
    "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine",
];

/// Maps a single digit word to its digit character.
fn word_to_digit(word: &str) -> char {
    match word {
        "zero" => '0',
        "one" => '1',
        "two" => '2',
        "three" => '3',
        "four" => '4',
        "five" => '5',
        "six" => '6',
        "seven" => '7',
        "eight" => '8',
        "nine" => '9',
        _ => '0',
    }
}

/// Parses a sequence of number-word cores into a conventional integer string.
///
/// The algorithm accumulates units into `current`, and when a scale word is
/// encountered multiplies `current` by the scale and adds it to `total`. This
/// correctly handles forms like "two thousand and twenty six" and "one hundred
/// twenty three million".
fn parse_number_word_sequence(cores: &[&str]) -> String {
    let mut total: u64 = 0;
    let mut current: u64 = 0;

    for word in cores {
        match *word {
            "zero" => current += 0,
            "one" => current += 1,
            "two" => current += 2,
            "three" => current += 3,
            "four" => current += 4,
            "five" => current += 5,
            "six" => current += 6,
            "seven" => current += 7,
            "eight" => current += 8,
            "nine" => current += 9,
            "ten" => current += 10,
            "eleven" => current += 11,
            "twelve" => current += 12,
            "thirteen" => current += 13,
            "fourteen" => current += 14,
            "fifteen" => current += 15,
            "sixteen" => current += 16,
            "seventeen" => current += 17,
            "eighteen" => current += 18,
            "nineteen" => current += 19,
            "twenty" => current += 20,
            "thirty" => current += 30,
            "forty" => current += 40,
            "fifty" => current += 50,
            "sixty" => current += 60,
            "seventy" => current += 70,
            "eighty" => current += 80,
            "ninety" => current += 90,
            "hundred" => {
                if current == 0 {
                    current = 1;
                }
                current *= 100;
            }
            "thousand" => {
                if current == 0 {
                    current = 1;
                }
                current *= 1000;
                total += current;
                current = 0;
            }
            "million" => {
                if current == 0 {
                    current = 1;
                }
                current *= 1_000_000;
                total += current;
                current = 0;
            }
            "billion" => {
                if current == 0 {
                    current = 1;
                }
                current *= 1_000_000_000;
                total += current;
                current = 0;
            }
            _ => {}
        }
    }

    total += current;
    total.to_string()
}

/// Extracts leading punctuation, core word, and trailing punctuation from a word.
#[allow(dead_code)]
fn extract_word_parts(word: &str) -> (String, &str, String) {
    let chars: Vec<char> = word.chars().collect();

    // Find start of core word (first alphanumeric)
    let start = chars.iter().position(|c| c.is_alphanumeric()).unwrap_or(0);

    // Find end of core word (last alphanumeric)
    let end = chars.iter().rposition(|c| c.is_alphanumeric()).unwrap_or(0);

    // Handle edge case: no alphanumeric characters
    if start > end || chars.iter().all(|c| !c.is_alphanumeric()) {
        return (word.to_string(), "", String::new());
    }

    // Convert to byte indices
    let start_byte: usize = chars[..start].iter().map(|c| c.len_utf8()).sum();
    let core_len: usize = chars[start..=end].iter().map(|c| c.len_utf8()).sum();
    let end_byte = start_byte + core_len;

    let prefix: String = chars[..start].iter().collect();
    let core = &word[start_byte..end_byte];
    let suffix: String = chars[end + 1..].iter().collect();

    (prefix, core, suffix)
}

/// Replaces `<unk>` tokens with accent-aware word recovery.
///
/// Models like Parakeet emit `<unk>` for accented characters. Instead of just
/// stripping them (which truncates words like "fiancé" → "fianc"), this function
/// tries to recover the correct ASCII equivalent using a dictionary lookup.
///
/// # Algorithm
/// 1. Strip all `<unk>` tokens from the text
/// 2. For each word, extract the core alphanumeric part (preserving punctuation)
/// 3. Look up the lowercased core in `TRUNCATED_WORDS`
/// 4. If found, replace with the correct ASCII form (preserving case pattern)
///
/// # Arguments
/// * `text` - The raw transcription text containing `<unk>` tokens
///
/// # Returns
/// The text with `<unk>` tokens replaced by recovered ASCII words where possible
///
/// # Examples
/// ```text
/// "my fianc is great"     → "my fiance is great"
/// "I love caf con leche"   → "I love cafe con leche"
/// "the jalapeo is hot"     → "the jalapeno is hot"
/// ```
fn replace_unk_with_accent_recovery(text: &str) -> String {
    // Step 1: Strip <unk> tokens
    let stripped = text.replace("<unk>", "");

    // Step 2: Try to recover truncated words using dictionary
    let words: Vec<&str> = stripped.split_whitespace().collect();
    let mut result = Vec::with_capacity(words.len());

    for word in &words {
        let (prefix, suffix) = extract_punctuation(word);

        // Get the core alphanumeric part for dictionary lookup
        let core_start = prefix.len();
        let core_end = word.len() - suffix.len();

        if core_start >= core_end {
            // No alphanumeric content, keep as-is
            result.push(word.to_string());
            continue;
        }

        let core = &word[core_start..core_end];
        let core_lower = core.to_lowercase();

        if let Some(&replacement) = TRUNCATED_WORDS.get(core_lower.as_str()) {
            let corrected = preserve_case_pattern(core, replacement);
            result.push(format!("{}{}{}", prefix, corrected, suffix));
        } else {
            result.push(word.to_string());
        }
    }

    result.join(" ")
}

/// Processes transcription text through the full correction pipeline in a single call.
///
/// This function combines all text processing steps that were previously done as
/// separate passes (word correction, filler removal, spelling conversion, repetition
/// suppression) into a single orchestrated call. Each transformation is still applied
/// sequentially, but the function avoids creating unnecessary intermediate string copies
/// by reusing the same buffer and skipping no-op transformations.
///
/// # Arguments
/// * `text` - The raw transcription text to process
/// * `word_correction_mode` - Which word correction algorithm to use
/// * `custom_words` - Simple custom words for `WordBias` mode
/// * `advanced_custom_words` - Custom words with pronunciations for `Pronunciation` mode
/// * `word_replacements` - Exact word replacements for `Replacement` mode
/// * `word_correction_threshold` - Similarity threshold for fuzzy matching (0.0–1.0)
/// * `output_language` - Evidence for the language of the output text; gates
///   built-in filler removal (fails closed on `Unknown`). Replaces the old
///   `UserSelected(app_language)` stand-in.
/// * `app_language` - UI language fallback for the number-words-to-digits gate
///   when the output language is unknown
/// * `custom_filler_words` - Optional user-provided filler word list overriding language defaults
/// * `filler_word_removal_enabled` - Master toggle for filler-word removal (built-in and custom)
/// * `convert_us_to_british` - Whether to apply US→British spelling conversion
/// * `convert_number_words_to_digits` - Whether to convert spoken English number words to digits
/// * `spelling_dictionary` - Which spelling dictionary to use for conversion
/// * `repetition_suppression_level` - Repetition suppression level (0=off, 1=light, 2+=moderate)
///
/// # Returns
/// The fully processed transcription text
pub fn process_transcription_text(
    text: &str,
    word_correction_mode: WordCorrectionMode,
    custom_words: &[String],
    advanced_custom_words: &[CustomWord],
    word_replacements: &[WordReplacement],
    word_correction_threshold: f64,
    // True when the custom words already reached the model (vocabulary slot /
    // initial prompt), so fuzzy correction must not re-apply them.
    custom_words_prompted: bool,
    output_language: &OutputLanguageEvidence,
    app_language: &str,
    custom_filler_words: &Option<Vec<String>>,
    filler_word_removal_enabled: bool,
    convert_us_to_british: bool,
    convert_number_words_to_digits: bool,
    spelling_dictionary: SpellingDictionary,
    repetition_suppression_level: u8,
) -> String {
    // Step 0: Replace <unk> tokens with accent-aware recovery.
    // Models like Parakeet emit <unk> for accented characters. Instead of just
    // stripping them (which truncates words like "fiancé" → "fianc"), we try to
    // recover the correct ASCII equivalent using a dictionary lookup.
    let text = replace_unk_with_accent_recovery(&text);

    // Step 1: Word correction.
    // Determine whether custom words exist (for skipping models that already
    // received them as vocabulary / prompt hints) and apply the appropriate
    // correction mode.
    let has_words = !custom_words.is_empty() || !advanced_custom_words.is_empty();

    // Replacement mode always applies, even when the model was prompted.
    // WordBias/Pronunciation only apply for models that were not prompted.
    let mut result = match word_correction_mode {
        WordCorrectionMode::Replacement if !word_replacements.is_empty() => {
            apply_word_replacements(&text, word_replacements)
        }
        _ if has_words && !custom_words_prompted => match word_correction_mode {
            WordCorrectionMode::WordBias => {
                apply_custom_words(&text, custom_words, word_correction_threshold)
            }
            WordCorrectionMode::Pronunciation => {
                apply_advanced_custom_words(&text, advanced_custom_words, word_correction_threshold)
            }
            WordCorrectionMode::Replacement => text.to_string(),
        },
        _ => text.to_string(),
    };

    // Step 2: Filler word removal and stutter cleanup.
    // Filler deletion is gated by the master toggle; the remaining cleanup
    // passes (fragment dedup, stutter collapse, whitespace) always run.
    // The evidence chain is resolved by the transcription client (model
    // detected / user-selected / text-detected); `Unknown` fails closed:
    // built-in gated fillers are skipped rather than applied speculatively.
    let filtered = remove_filler_words(
        &result,
        output_language,
        custom_filler_words,
        filler_word_removal_enabled,
    );
    // filter passes reuse the buffer by assigning back into `result`.
    result = normalize_transcription_output(&filtered);

    // Step 3: US → British spelling conversion (optional).
    if convert_us_to_british {
        result = convert_us_to_british_with_dict(&result, spelling_dictionary);
    }

    // Step 4: Repetition suppression.
    if repetition_suppression_level > 0 {
        result = suppress_repeated_words(&result, repetition_suppression_level);
    }

    // Step 5: Convert number words to digits (after repetition suppression so
    // repeated digit words like "zero zero zero" are not collapsed before they
    // can be converted). Gated on the output language when known, with the UI
    // language as the fallback for unknown output.
    if convert_number_words_to_digits {
        let number_language = output_language.language().unwrap_or(app_language);
        result = convert_number_words_to_digits_impl(&result, number_language);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test-local stand-in for the deleted `filter_transcription_output`
    /// wrapper (its last production callers left with the Phase 6 evidence
    /// chain): filler removal with always-on deletion under UserSelected
    /// evidence, then the shared normalization passes.
    fn filter_transcription_output(
        text: &str,
        lang: &str,
        custom_filler_words: &Option<Vec<String>>,
    ) -> String {
        let language = OutputLanguageEvidence::UserSelected(lang.to_string());
        let filtered = remove_filler_words(text, &language, custom_filler_words, true);
        normalize_transcription_output(&filtered)
    }

    #[test]
    fn test_apply_custom_words_exact_match() {
        let text = "hello world";
        let custom_words = vec!["Hello".to_string(), "World".to_string()];
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert_eq!(result, "Hello World");
    }

    #[test]
    fn test_apply_custom_words_fuzzy_match() {
        let text = "helo wrold";
        let custom_words = vec!["hello".to_string(), "world".to_string()];
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_preserve_case_pattern() {
        assert_eq!(preserve_case_pattern("HELLO", "world"), "WORLD");
        assert_eq!(preserve_case_pattern("Hello", "world"), "World");
        assert_eq!(preserve_case_pattern("hello", "WORLD"), "WORLD");
    }

    #[test]
    fn test_extract_punctuation() {
        assert_eq!(extract_punctuation("hello"), ("", ""));
        assert_eq!(extract_punctuation("!hello?"), ("!", "?"));
        assert_eq!(extract_punctuation("...hello..."), ("...", "..."));
        // CJK punctuation — must use byte offsets, not char counts, to avoid panics
        assert_eq!(extract_punctuation("「你好」"), ("「", "」"));
        // Emoji punctuation (multi-byte)
        assert_eq!(extract_punctuation("¡Hola!"), ("¡", "!"));
    }

    #[test]
    fn test_empty_custom_words() {
        let text = "hello world";
        let custom_words = vec![];
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_filter_filler_words() {
        let text = "So uhm I was thinking uh about this";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "So I was thinking about this");
    }

    #[test]
    fn test_filter_filler_words_case_insensitive() {
        let text = "UHM this is UH a test";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "This is a test");
    }

    #[test]
    fn test_filter_filler_words_with_punctuation() {
        let text = "Well, uhm, I think, uh. that's right";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "Well, I think, that's right");
    }

    #[test]
    fn test_filter_cleans_whitespace() {
        let text = "Hello    world   test";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "Hello world test");
    }

    #[test]
    fn test_filter_trims() {
        let text = "  Hello world  ";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "Hello world");
    }

    #[test]
    fn test_filter_combined() {
        let text = "  Uhm, so I was, uh, thinking about this  ";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "So I was, thinking about this");
    }

    #[test]
    fn test_filter_leading_filler_keeps_sentence_capital() {
        let result = filter_transcription_output("Um, so I think we should ship it.", "en", &None);
        assert_eq!(result, "So I think we should ship it.");

        let result = filter_transcription_output("That works. Um, let me check.", "en", &None);
        assert_eq!(result, "That works. Let me check.");

        // Mid-sentence there is no capital to hand over.
        let result = filter_transcription_output("He said, Um, not today.", "en", &None);
        assert_eq!(result, "He said, not today.");
    }

    #[test]
    fn test_filter_preserves_valid_text() {
        let text = "This is a completely normal sentence.";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "This is a completely normal sentence.");
    }

    #[test]
    fn test_filter_stutter_collapse() {
        // "w" is now correctly removed as a single-char fragment of "wh",
        // leaving "wh wh ..." which collapses to "wh why"
        let text = "w wh wh wh wh wh wh wh wh wh why";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "wh why");
    }

    #[test]
    fn test_filter_stutter_short_words() {
        let text = "I I I I think so so so so";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "I think so");
    }

    #[test]
    fn test_filter_stutter_longer_words() {
        let text = "Check data doc doc doc doc documentation.";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "Check data doc documentation.");
    }

    #[test]
    fn test_filter_stutter_mixed_case() {
        let text = "No NO no NO no";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "No");
    }

    #[test]
    fn test_filter_stutter_preserves_two_repetitions() {
        let text = "no no is fine";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "no no is fine");
    }

    #[test]
    fn test_filter_english_removes_um() {
        let text = "um I think um this is good";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "I think this is good");
    }

    #[test]
    fn test_filter_portuguese_preserves_um() {
        // "um" means "a/an" in Portuguese
        let text = "um gato bonito";
        let result = filter_transcription_output(text, "pt", &None);
        assert_eq!(result, "um gato bonito");
    }

    #[test]
    fn test_filter_spanish_preserves_ha() {
        // "ha" means "has" in Spanish
        let text = "ha sido un buen día";
        let result = filter_transcription_output(text, "es", &None);
        assert_eq!(result, "ha sido un buen día");
    }

    #[test]
    fn test_filter_language_code_with_region() {
        // "pt-BR" should normalize to "pt"
        let text = "um gato bonito";
        let result = filter_transcription_output(text, "pt-BR", &None);
        assert_eq!(result, "um gato bonito");
    }

    #[test]
    fn test_filter_custom_filler_words_override() {
        let custom = Some(vec!["okay".to_string(), "right".to_string()]);
        let text = "okay so I think right this works";
        let result = filter_transcription_output(text, "en", &custom);
        assert_eq!(result, "so I think this works");
    }

    #[test]
    fn test_filter_custom_filler_words_empty_disables() {
        let custom = Some(vec![]);
        let text = "So uhm I was thinking uh about this";
        let result = filter_transcription_output(text, "en", &custom);
        // No filler words removed since custom list is empty
        assert_eq!(result, "So uhm I was thinking uh about this");
    }

    #[test]
    fn test_filter_unknown_language_uses_fallback() {
        let text = "uh I think uhm this works";
        let result = filter_transcription_output(text, "xx", &None);
        assert_eq!(result, "I think this works");
    }

    #[test]
    fn test_filter_unknown_evidence_removes_universal_keeps_gated() {
        let filtered = remove_filler_words(
            "uhh bueno hmm creo que um ha llegado",
            &OutputLanguageEvidence::Unknown,
            &None,
            true,
        );
        assert_eq!(
            normalize_transcription_output(&filtered),
            "bueno creo que um ha llegado"
        );

        let cyrillic = remove_filler_words(
            "хм я думаю ммм это работает",
            &OutputLanguageEvidence::Unknown,
            &None,
            true,
        );
        assert_eq!(
            normalize_transcription_output(&cyrillic),
            "я думаю это работает"
        );
    }

    #[test]
    fn test_filter_german_gated_fillers_require_evidence() {
        let text = "äh ich glaube ähm das passt";

        let unknown = remove_filler_words(text, &OutputLanguageEvidence::Unknown, &None, true);
        assert_eq!(normalize_transcription_output(&unknown), text);

        let result = filter_transcription_output(text, "de", &None);
        assert_eq!(result, "ich glaube das passt");
    }

    #[test]
    fn test_filter_preserves_millimetre_unit() {
        // "mm" was removed from the filler lists because it eats units.
        let text = "the screw is 5 mm long";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "the screw is 5 mm long");
    }

    #[test]
    fn test_filter_detected_evidence_unlocks_gated_fillers() {
        let model = remove_filler_words(
            "um I think this works",
            &OutputLanguageEvidence::ModelDetected("en".to_string()),
            &None,
            true,
        );
        assert_eq!(normalize_transcription_output(&model), "I think this works");

        let text = remove_filler_words(
            "euh je pense que ça marche",
            &OutputLanguageEvidence::TextDetected("fr".to_string()),
            &None,
            true,
        );
        assert_eq!(
            normalize_transcription_output(&text),
            "je pense que ça marche"
        );
    }

    #[test]
    fn test_filter_master_toggle_disables_custom_and_builtin_removal() {
        let text = "um customword I think";
        let language = OutputLanguageEvidence::UserSelected("en".to_string());
        let custom = Some(vec!["customword".to_string()]);

        let result = remove_filler_words(text, &language, &custom, false);

        assert_eq!(result, text);
    }

    #[test]
    fn test_filter_custom_words_apply_without_language_evidence() {
        let custom = Some(vec!["customword".to_string()]);
        let text = "customword should be removed but um should remain";

        let filtered = remove_filler_words(text, &OutputLanguageEvidence::Unknown, &custom, true);
        let result = normalize_transcription_output(&filtered);

        assert_eq!(result, "should be removed but um should remain");
    }

    #[test]
    fn test_filter_fallback_does_not_remove_um() {
        // Fallback (unknown language) should not remove "um" since it's a real word in some languages
        let text = "um I think this works";
        let result = filter_transcription_output(text, "xx", &None);
        assert_eq!(result, "um I think this works");
    }

    #[test]
    fn test_apply_custom_words_ngram_two_words() {
        let text = "il cui nome è Charge B, che permette";
        let custom_words = vec!["ChargeBee".to_string()];
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert!(result.contains("ChargeBee,"), "unexpected result: {result}");
        assert!(!result.contains("Charge B"));
    }

    #[test]
    fn test_apply_custom_words_ngram_three_words() {
        let text = "use Chat G P T for this";
        let custom_words = vec!["ChatGPT".to_string()];
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert!(result.contains("ChatGPT"));
    }

    #[test]
    fn test_apply_custom_words_prefers_longer_ngram() {
        let text = "Open AI GPT model";
        let custom_words = vec!["OpenAI".to_string(), "GPT".to_string()];
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert_eq!(result, "OpenAI GPT model");
    }

    #[test]
    fn test_apply_custom_words_ngram_preserves_case() {
        let text = "CHARGE B is great";
        let custom_words = vec!["ChargeBee".to_string()];
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert!(result.contains("CHARGEBEE"));
    }

    #[test]
    fn test_apply_custom_words_ngram_with_spaces_in_custom() {
        // Custom word with space should also match against split words
        let text = "using Mac Book Pro";
        let custom_words = vec!["MacBook Pro".to_string()];
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert_eq!(result, "using MacBook Pro");
    }

    #[test]
    fn test_apply_custom_words_trailing_number_not_doubled() {
        // Verify that trailing non-alpha chars (like numbers) aren't double-counted
        // between build_ngram stripping them and extract_punctuation capturing them
        let text = "use GPT4 for this";
        let custom_words = vec!["GPT-4".to_string()];
        let result = apply_custom_words(text, &custom_words, 0.5);
        // Should NOT produce "GPT-44" (double-counting the trailing 4)
        assert!(
            !result.contains("GPT-44"),
            "got double-counted result: {}",
            result
        );
    }

    #[test]
    fn test_apply_custom_words_matches_ampersand_word() {
        let text = "send it to RD for review";
        let custom_words = vec!["R&D".to_string()];
        let result = apply_custom_words(text, &custom_words, 0.18);
        assert_eq!(result, "send it to R&D for review");
    }

    #[test]
    fn test_apply_custom_words_matches_spoken_ampersand_word() {
        let text = "send it to R and D for review";
        let custom_words = vec!["R&D".to_string()];
        let result = apply_custom_words(text, &custom_words, 0.18);
        assert_eq!(result, "send it to R&D for review");
    }

    #[test]
    fn test_apply_custom_words_preserves_ampersand_word() {
        let text = "send it to R&D for review";
        let custom_words = vec!["R&D".to_string()];
        let result = apply_custom_words(text, &custom_words, 0.18);
        assert_eq!(result, "send it to R&D for review");
    }

    #[test]
    fn test_apply_custom_words_handles_unicode_punctuation() {
        let text = "「Handee。」";
        let custom_words = vec!["Handy".to_string()];
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert_eq!(result, "「Handy。」");
    }

    #[test]
    fn test_apply_custom_words_skips_cjk_fuzzy_matching() {
        let text = "你好。";
        let custom_words = vec!["你号".to_string()];
        let result = apply_custom_words(text, &custom_words, 1.0);
        assert_eq!(result, text);
    }

    // --- dedup_word_fragments tests ---

    #[test]
    fn test_dedup_word_fragments_basic() {
        // "wa" is a prefix of "was" — classic CTC fragment overlap
        let result = dedup_word_fragments("it wa was a");
        assert_eq!(result, "it was a");
    }

    #[test]
    fn test_dedup_word_fragments_user_reported_case() {
        // The exact pattern from the bug report: "it wa was a"
        let result = dedup_word_fragments("it wa was a");
        assert_eq!(result, "it was a");
    }

    #[test]
    fn test_dedup_word_fragments_no_overlap() {
        // No fragment overlap — should be unchanged
        let result = dedup_word_fragments("it was a good day");
        assert_eq!(result, "it was a good day");
    }

    #[test]
    fn test_dedup_word_fragments_case_insensitive() {
        // Case-insensitive prefix matching: "Wa" is prefix of "was"
        let result = dedup_word_fragments("it Wa was a");
        assert_eq!(result, "it was a");
    }

    #[test]
    fn test_dedup_word_fragments_with_punctuation() {
        // Fragment with punctuation preserved on the next word
        let result = dedup_word_fragments("it wa was, a");
        assert_eq!(result, "it was, a");
    }

    #[test]
    fn test_dedup_word_fragments_single_letter_unchanged() {
        // Single-letter words (like "I") should not be treated as fragments
        // even if the next word starts with the same letter
        let result = dedup_word_fragments("I Iceland");
        assert_eq!(result, "I Iceland");
    }

    #[test]
    fn test_dedup_word_fragments_same_length_not_removed() {
        // "want" and "want" are same length — "want" is NOT a fragment of "want"
        let result = dedup_word_fragments("I want want this");
        assert_eq!(result, "I want want this");
    }

    #[test]
    fn test_dedup_word_fragments_empty_string() {
        let result = dedup_word_fragments("");
        assert_eq!(result, "");
    }

    #[test]
    fn test_dedup_word_fragments_single_word() {
        let result = dedup_word_fragments("hello");
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_dedup_word_fragments_two_words_no_overlap() {
        let result = dedup_word_fragments("hello world");
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_dedup_word_fragments_multiple_overlaps() {
        // Multiple fragment overlaps in one sentence
        let result = dedup_word_fragments("I wan wanted to thi this");
        assert_eq!(result, "I wanted to this");
    }

    #[test]
    fn test_dedup_word_fragments_not_a_prefix() {
        // "the" is a common word — even though it's a prefix of "then", it should be kept
        let result = dedup_word_fragments("the then");
        assert_eq!(result, "the then");
    }

    #[test]
    fn test_dedup_word_fragments_independent_words() {
        // "can" is NOT a prefix of "be" — no overlap, both kept
        let result = dedup_word_fragments("it can be done");
        assert_eq!(result, "it can be done");
    }

    #[test]
    fn test_dedup_word_fragments_common_word_preserved() {
        // "can" is a common word and a prefix of "cancel" — should NOT be removed
        let result = dedup_word_fragments("I can cancel this");
        assert_eq!(result, "I can cancel this");
    }

    #[test]
    fn test_dedup_word_fragments_for_preserved() {
        // "for" is a common word and a prefix of "forget" — should NOT be removed
        let result = dedup_word_fragments("for forget");
        assert_eq!(result, "for forget");
    }

    #[test]
    fn test_dedup_word_fragments_combined_with_filter() {
        // Test that dedup_word_fragments integrates correctly in the full pipeline
        // "it wa was a" -> after filler removal -> after fragment dedup -> after stutter collapse
        let result = filter_transcription_output("it wa was a", "en", &None);
        assert_eq!(result, "it was a");
    }

    #[test]
    fn test_dedup_word_fragments_combined_with_stutter() {
        // Both fragment overlap AND stutter in same text
        let result = filter_transcription_output("I I I wan wanted to go", "en", &None);
        assert_eq!(result, "I wanted to go");
    }

    #[test]
    fn test_dedup_word_fragments_to_preserved() {
        // "to" is a common word and a prefix of "today" — should NOT be removed
        let result = dedup_word_fragments("go to today");
        assert_eq!(result, "go to today");
    }

    #[test]
    fn test_dedup_word_fragments_longer_fragment() {
        // "understand" is not a prefix of anything after it
        let result = dedup_word_fragments("I understand this");
        assert_eq!(result, "I understand this");
    }

    // --- Regression tests: legitimate words must NOT be removed ---

    #[test]
    fn test_dedup_word_fragments_preserves_pro() {
        // "pro" is a common abbreviation/word that prefixes "process", "procedure", etc.
        // It must NOT be removed even though it's a prefix.
        let result = dedup_word_fragments("pro process");
        assert_eq!(result, "pro process");
    }

    #[test]
    fn test_dedup_word_fragments_preserves_con() {
        // "con" is a common word that prefixes "control", "continue", etc.
        let result = dedup_word_fragments("con control");
        assert_eq!(result, "con control");
    }

    #[test]
    fn test_dedup_word_fragments_preserves_sub() {
        // "sub" is a common word that prefixes "substance", "substantial", etc.
        let result = dedup_word_fragments("sub substance");
        assert_eq!(result, "sub substance");
    }

    #[test]
    fn test_dedup_word_fragments_preserves_pre() {
        // "pre" is a common word that prefixes "pretty", "prevent", etc.
        let result = dedup_word_fragments("pre pretty");
        assert_eq!(result, "pre pretty");
    }

    #[test]
    fn test_dedup_word_fragments_preserves_per() {
        // "per" is a common word that prefixes "percent", "person", etc.
        let result = dedup_word_fragments("per percent");
        assert_eq!(result, "per percent");
    }

    #[test]
    fn test_dedup_word_fragments_preserves_long_prefix_words() {
        // Words longer than 3 chars should never be considered fragments,
        // even if they're a prefix of the next word.
        // "under" (5 chars) before "understand" must be kept.
        let result = dedup_word_fragments("under understand");
        assert_eq!(result, "under understand");
    }

    #[test]
    fn test_dedup_word_fragments_st_preserved() {
        // "St" is an abbreviation for Saint/Street, should not be removed before "Street"
        let result = dedup_word_fragments("St Street");
        assert_eq!(result, "St Street");
    }

    #[test]
    fn test_dedup_word_fragments_wan_wanted() {
        // "wan" → "wanted" is a legitimate CTC fragment overlap (ext by 3, ≤ MAX 3)
        let result = dedup_word_fragments("I wan wanted to go");
        assert_eq!(result, "I wanted to go");
    }

    #[test]
    fn test_dedup_word_fragments_thi_this() {
        // "thi" → "this" is a fragment overlap (ext by 1)
        let result = dedup_word_fragments("thi this is it");
        assert_eq!(result, "this is it");
    }

    #[test]
    fn test_dedup_word_fragments_co_company() {
        // "co" before "company" — "co" is in COMMON_WORDS, must be kept
        let result = dedup_word_fragments("co company");
        assert_eq!(result, "co company");
    }

    // ============================================================
    // COMPREHENSIVE BENCHMARK — Fragment dedup at chunking boundaries
    //
    // Golden cases (MUST PASS):
    //   "I wa was going" → "I was going"
    //   "my mac machine" → "my mac machine" (unchanged)
    //
    // These tests go far beyond the golden cases to avoid overfitting.
    // ============================================================

    // --- GOLDEN CASES ---

    #[test]
    fn bench_golden_fragment_removal() {
        assert_eq!(dedup_word_fragments("I wa was going"), "I was going");
    }

    #[test]
    fn bench_golden_no_false_positive() {
        assert_eq!(dedup_word_fragments("my mac machine"), "my mac machine");
    }

    // --- SIMPLE FRAGMENT OVERLAPS (CTC artifacts) ---

    #[test]
    fn bench_fragment_thi_this() {
        assert_eq!(dedup_word_fragments("thi this is it"), "this is it");
    }

    #[test]
    fn bench_fragment_wan_wanted() {
        assert_eq!(dedup_word_fragments("I wan wanted to go"), "I wanted to go");
    }

    #[test]
    fn bench_fragment_sta_started() {
        // "sta" is a 3-char prefix of "start", extension = 2
        assert_eq!(
            dedup_word_fragments("I sta started running"),
            "I started running"
        );
    }

    #[test]
    fn bench_fragment_sta_starting() {
        assert_eq!(dedup_word_fragments("sta starting now"), "starting now");
    }

    // --- FALSE POSITIVE PROTECTION ---

    #[test]
    fn bench_preserves_for_forget() {
        assert_eq!(dedup_word_fragments("for forget"), "for forget");
    }

    #[test]
    fn bench_preserves_can_cancel() {
        assert_eq!(
            dedup_word_fragments("I can cancel this"),
            "I can cancel this"
        );
    }

    #[test]
    fn bench_preserves_the_then() {
        assert_eq!(dedup_word_fragments("the then"), "the then");
    }

    #[test]
    fn bench_preserves_to_today() {
        assert_eq!(dedup_word_fragments("go to today"), "go to today");
    }

    #[test]
    fn bench_preserves_pro_process() {
        assert_eq!(dedup_word_fragments("pro process"), "pro process");
    }

    #[test]
    fn bench_preserves_con_control() {
        assert_eq!(dedup_word_fragments("con control"), "con control");
    }

    #[test]
    fn bench_preserves_sub_substance() {
        assert_eq!(dedup_word_fragments("sub substance"), "sub substance");
    }

    #[test]
    fn bench_preserves_per_percent() {
        assert_eq!(dedup_word_fragments("per percent"), "per percent");
    }

    #[test]
    fn bench_preserves_pre_pretty() {
        assert_eq!(dedup_word_fragments("pre pretty"), "pre pretty");
    }

    #[test]
    fn bench_preserves_app_application() {
        assert_eq!(dedup_word_fragments("app application"), "app application");
    }

    #[test]
    fn bench_preserves_run_running() {
        assert_eq!(dedup_word_fragments("I run running"), "I run running");
    }

    #[test]
    fn bench_preserves_win_windows() {
        assert_eq!(dedup_word_fragments("win windows"), "win windows");
    }

    #[test]
    fn bench_preserves_hot_hotel() {
        assert_eq!(dedup_word_fragments("hot hotel"), "hot hotel");
    }

    #[test]
    fn bench_preserves_key_keyboard() {
        assert_eq!(dedup_word_fragments("key keyboard"), "key keyboard");
    }

    #[test]
    fn bench_preserves_car_careful() {
        assert_eq!(dedup_word_fragments("car careful"), "car careful");
    }

    #[test]
    fn bench_preserves_son_song() {
        assert_eq!(dedup_word_fragments("my son song"), "my son song");
    }

    #[test]
    fn bench_preserves_sit_sitting() {
        assert_eq!(dedup_word_fragments("sit sitting"), "sit sitting");
    }

    #[test]
    fn bench_preserves_bar_barely() {
        assert_eq!(dedup_word_fragments("bar barely"), "bar barely");
    }

    #[test]
    fn bench_preserves_bus_business() {
        assert_eq!(dedup_word_fragments("the bus business"), "the bus business");
    }

    #[test]
    fn bench_preserves_day_daylight() {
        assert_eq!(dedup_word_fragments("day daylight"), "day daylight");
    }

    // --- LENGTH-BASED PROTECTION ---

    #[test]
    fn bench_preserves_four_char_word() {
        assert_eq!(
            dedup_word_fragments("I read reading books"),
            "I read reading books"
        );
    }

    #[test]
    fn bench_preserves_five_char_word() {
        assert_eq!(dedup_word_fragments("under understand"), "under understand");
    }

    #[test]
    fn bench_preserves_six_char_word() {
        assert_eq!(dedup_word_fragments("record recording"), "record recording");
    }

    // --- CASE VARIATIONS ---

    #[test]
    fn bench_case_insensitive_wa_Was() {
        assert_eq!(dedup_word_fragments("it wa Was a"), "it Was a");
    }

    #[test]
    fn bench_case_insensitive_Wa_was() {
        assert_eq!(dedup_word_fragments("it Wa was a"), "it was a");
    }

    #[test]
    fn bench_case_insensitive_WA_WAS() {
        assert_eq!(dedup_word_fragments("I WA WAS going"), "I WAS going");
    }

    // --- PUNCTUATION ---

    #[test]
    fn bench_fragment_with_trailing_punctuation() {
        assert_eq!(dedup_word_fragments("it wa was, a"), "it was, a");
    }

    #[test]
    fn bench_preserves_words_with_punctuation() {
        assert_eq!(dedup_word_fragments("for. forget"), "for. forget");
    }

    // --- MULTI-FRAGMENT CHAINS ---

    #[test]
    fn bench_two_fragments_in_sentence() {
        assert_eq!(
            dedup_word_fragments("I wan wanted to thi this"),
            "I wanted to this"
        );
    }

    #[test]
    fn bench_fragment_at_start() {
        assert_eq!(dedup_word_fragments("wa was going"), "was going");
    }

    #[test]
    fn bench_consecutive_fragments() {
        assert_eq!(dedup_word_fragments("wa was thi this"), "was this");
    }

    // --- EXTENSION LENGTH BOUNDARIES ---

    #[test]
    fn bench_extension_exact_limit_3() {
        // "wan" (3 chars) → "wanted" (6 chars), extension = 3 == MAX_FRAGMENT_EXTENSION
        assert_eq!(dedup_word_fragments("I wan wanted"), "I wanted");
    }

    #[test]
    fn bench_extension_1_char() {
        assert_eq!(dedup_word_fragments("it wa was"), "it was");
    }

    #[test]
    fn bench_extension_2_chars() {
        assert_eq!(dedup_word_fragments("sta starting"), "starting");
    }

    // --- COMMON WORD PROTECTION ---

    #[test]
    fn bench_preserves_in_inside() {
        assert_eq!(
            dedup_word_fragments("in inside the house"),
            "in inside the house"
        );
    }

    #[test]
    fn bench_preserves_on_only() {
        assert_eq!(dedup_word_fragments("on only one"), "on only one");
    }

    #[test]
    fn bench_preserves_or_order() {
        assert_eq!(dedup_word_fragments("or order them"), "or order them");
    }

    #[test]
    fn bench_preserves_so_some() {
        assert_eq!(dedup_word_fragments("so some people"), "so some people");
    }

    #[test]
    fn bench_preserves_no_nothing() {
        assert_eq!(
            dedup_word_fragments("no nothing happened"),
            "no nothing happened"
        );
    }

    #[test]
    fn bench_preserves_up_upon() {
        assert_eq!(dedup_word_fragments("up upon the hill"), "up upon the hill");
    }

    #[test]
    fn bench_preserves_my_myself() {
        assert_eq!(
            dedup_word_fragments("I did it my myself"),
            "I did it my myself"
        );
    }

    #[test]
    fn bench_preserves_be_been() {
        assert_eq!(dedup_word_fragments("I be been there"), "I be been there");
    }

    #[test]
    fn bench_preserves_he_here() {
        assert_eq!(dedup_word_fragments("he here comes"), "he here comes");
    }

    #[test]
    fn bench_preserves_do_doing() {
        assert_eq!(
            dedup_word_fragments("I do doing things"),
            "I do doing things"
        );
    }

    // --- REMOVES NON-COMMON SHORT FRAGMENTS ---

    #[test]
    fn bench_removes_ab_about() {
        // "ab" (2 chars) → "about" (5 chars), extension = 3
        // "ab" is not a common word, so it's a fragment
        assert_eq!(dedup_word_fragments("ab about that"), "about that");
    }

    // --- REAL-WORLD PATTERNS ---

    #[test]
    fn bench_real_world_wa_was_sentence() {
        assert_eq!(
            dedup_word_fragments("I wa was going to the store"),
            "I was going to the store"
        );
    }

    #[test]
    fn bench_real_world_thi_this_sentence() {
        assert_eq!(
            dedup_word_fragments("thi this is what I mean"),
            "this is what I mean"
        );
    }

    #[test]
    fn bench_real_world_fragment_in_middle() {
        assert_eq!(
            dedup_word_fragments("I went wa was walking down the street"),
            "I went was walking down the street"
        );
    }

    #[test]
    fn bench_real_world_multiple_fragments() {
        assert_eq!(
            dedup_word_fragments("it wa was sunny and thi this is great"),
            "it was sunny and this is great"
        );
    }

    #[test]
    fn bench_real_world_preserves_legitimate_repetition() {
        assert_eq!(dedup_word_fragments("I had had enough"), "I had had enough");
    }

    #[test]
    fn bench_preserves_sentence_without_fragments() {
        assert_eq!(
            dedup_word_fragments("the quick brown fox jumps over the lazy dog"),
            "the quick brown fox jumps over the lazy dog"
        );
    }

    // --- STRESS TESTS ---

    #[test]
    fn bench_all_fragments_removed() {
        assert_eq!(dedup_word_fragments("wa was thi this"), "was this");
    }

    #[test]
    fn bench_numbers_and_symbols_preserved() {
        assert_eq!(dedup_word_fragments("I wa was #1!"), "I was #1!");
    }

    #[test]
    fn bench_mixed_fragment_and_normal() {
        assert_eq!(
            dedup_word_fragments("hello wa was world"),
            "hello was world"
        );
    }

    #[test]
    fn bench_preserves_mac_machinery() {
        assert_eq!(
            dedup_word_fragments("the mac machinery works"),
            "the mac machinery works"
        );
    }

    // --- SUFFIX OVERLAP (current algorithm handles prefix only) ---

    #[test]
    fn bench_suffix_overlap_go_going() {
        // "go" is a common word so preserved regardless
        assert_eq!(dedup_word_fragments("I was go going"), "I was go going");
    }

    // --- EDGE CASES ---

    #[test]
    fn bench_same_word_repeated() {
        assert_eq!(dedup_word_fragments("the the"), "the the");
    }

    #[test]
    fn bench_fragment_at_end_stays() {
        // "wa" at end with nothing after it is not a fragment
        assert_eq!(dedup_word_fragments("I was going wa"), "I was going wa");
    }

    // --- ANTI-OVERFITTING: ADVERSARIAL TEST CASES ---
    // These test cases are designed to break naive implementations that
    // only optimize for the golden cases ("wa was" → "was" and "mac machine" → unchanged).

    #[test]
    fn adv_uncommon_3letter_fragment() {
        // "sta" is an uncommon 3-letter sequence that IS a CTC artifact
        assert_eq!(dedup_word_fragments("I sta started"), "I started");
    }

    #[test]
    fn adv_fragment_with_long_extension() {
        // "sta" → "starting" extends by 5 — must be caught regardless of extension length
        assert_eq!(dedup_word_fragments("sta starting now"), "starting now");
    }

    #[test]
    fn adv_common_word_not_fragment() {
        // "add" is a common word — should not be removed before "adding"
        assert_eq!(dedup_word_fragments("add adding more"), "add adding more");
    }

    #[test]
    fn adv_common_word_bad() {
        // "bad" is a common word — should not be removed before "badly"
        assert_eq!(dedup_word_fragments("bad badly done"), "bad badly done");
    }

    #[test]
    fn adv_common_word_ear() {
        // "ear" is a common word — should not be removed before "early"
        assert_eq!(
            dedup_word_fragments("ear early morning"),
            "ear early morning"
        );
    }

    #[test]
    fn adv_common_word_eat() {
        // "eat" is a common word — should not be removed before "eating"
        assert_eq!(dedup_word_fragments("eat eating food"), "eat eating food");
    }

    #[test]
    fn adv_common_word_end() {
        // "end" is a common word — should not be removed before "ending"
        assert_eq!(
            dedup_word_fragments("end ending credits"),
            "end ending credits"
        );
    }

    #[test]
    fn adv_common_word_man() {
        // "man" is a common word — should not be removed before "many"
        assert_eq!(dedup_word_fragments("man many things"), "man many things");
    }

    #[test]
    fn adv_common_word_sat() {
        // "sat" is a common word — should not be removed before "saturday"
        assert_eq!(
            dedup_word_fragments("sat saturday night"),
            "sat saturday night"
        );
    }

    #[test]
    fn adv_common_word_ran() {
        // "ran" is a common word — should not be removed before "random"
        assert_eq!(
            dedup_word_fragments("ran random numbers"),
            "ran random numbers"
        );
    }

    #[test]
    fn adv_common_word_met() {
        // "met" is a common word — should not be removed before "method"
        assert_eq!(dedup_word_fragments("met method calls"), "met method calls");
    }

    #[test]
    fn adv_common_word_bed() {
        // "bed" is a common word — should not be removed before "bedroom"
        assert_eq!(
            dedup_word_fragments("bed bedroom light"),
            "bed bedroom light"
        );
    }

    #[test]
    fn adv_st_before_street() {
        // "St" before "Street" — common abbreviation, must preserve
        assert_eq!(dedup_word_fragments("St Street corner"), "St Street corner");
    }

    #[test]
    fn adv_common_word_put() {
        // "put" is a common word — should not be removed before "putting"
        assert_eq!(dedup_word_fragments("put putting away"), "put putting away");
    }

    #[test]
    fn adv_fragment_thi_various() {
        // "thi" is NOT a common word, should be removed before "this"
        assert_eq!(dedup_word_fragments("thi this is it"), "this is it");
    }

    #[test]
    fn adv_fragment_ab_about() {
        // "ab" is NOT a common word, should be removed before "about"
        assert_eq!(dedup_word_fragments("ab about that"), "about that");
    }

    #[test]
    fn adv_common_word_forget() {
        // "for" followed by "forget" — must preserve "for"
        assert_eq!(dedup_word_fragments("for forget it"), "for forget it");
    }

    #[test]
    fn adv_real_world_transcription() {
        // A real Parakeet V2 artifact pattern
        assert_eq!(
            dedup_word_fragments("I wa was going to the store and thi this is great"),
            "I was going to the store and this is great"
        );
    }

    #[test]
    fn adv_preserves_legitimate_repetition() {
        // Someone actually saying the same word twice is NOT a fragment
        assert_eq!(dedup_word_fragments("I had had enough"), "I had had enough");
    }

    #[test]
    fn adv_preserves_4_plus_letter_words() {
        // Words 4+ chars are never CTC fragments, even if they're a prefix
        assert_eq!(
            dedup_word_fragments("I read reading books"),
            "I read reading books"
        );
    }

    #[test]
    fn adv_preserves_5_letter_word() {
        assert_eq!(
            dedup_word_fragments("under understand this"),
            "under understand this"
        );
    }

    #[test]
    fn adv_fragment_wa_various_contexts() {
        // Golden case in different contexts
        assert_eq!(dedup_word_fragments("wa was there"), "was there");
        assert_eq!(dedup_word_fragments("he wa was there"), "he was there");
        assert_eq!(dedup_word_fragments("I wa was going"), "I was going");
    }

    #[test]
    fn adv_mac_machine_various_contexts() {
        // Golden false-positive case in different contexts
        assert_eq!(dedup_word_fragments("my mac machine"), "my mac machine");
        assert_eq!(dedup_word_fragments("the mac machine"), "the mac machine");
        assert_eq!(
            dedup_word_fragments("mac machine works"),
            "mac machine works"
        );
    }

    // --- DEDUPEIO-STYLE COMPARISON TESTS ---
    // These test whether our approach handles patterns that a database-deduplication
    // library like dedupeio/dedupe would handle differently (clustering-based).
    // Our approach is simpler but correct for the specific CTC fragment use case.

    #[test]
    fn dedupe_style_no_cluster_overlap() {
        // Unlike dedupeio which clusters similar records, our approach
        // only handles immediate prefix overlap at word boundaries.
        // This is correct for CTC artifacts — we don't want to merge similar words.
        assert_eq!(
            dedup_word_fragments("cat category"),
            "cat category" // "cat" is in COMMON_WORDS, preserved
        );
    }

    #[test]
    fn dedupe_style_no_fuzzy_match() {
        // Unlike dedupeio which uses fuzzy matching, our approach requires
        // exact prefix match. Similar but not matching words are preserved.
        assert_eq!(
            dedup_word_fragments("the them there"),
            "the them there" // "the" is common, all preserved
        );
    }

    #[test]
    fn dedupe_style_no_cross_sentence_dedup() {
        // Unlike dedupeio which can dedup across records, our approach
        // only operates within a single transcription chunk boundary.
        assert_eq!(
            dedup_word_fragments("it wa was good"),
            "it was good" // Fragment removed, but other words preserved
        );
    }

    // === REGRESSION TESTS ===
    // These test for regressions found during optimization.
    // Removing MAX_FRAGMENT_EXTENSION caused these false positives.
    // With MAX_FRAGMENT_EXTENSION=5 + expanded COMMON_WORDS, they should pass.

    #[test]
    fn regression_re_before_really() {
        // "re" is a common word (regarding, musical note) — must be preserved
        assert_eq!(dedup_word_fragments("re really good"), "re really good");
    }

    #[test]
    fn regression_ex_before_example() {
        // "ex" is a common word (former) — must be preserved
        assert_eq!(dedup_word_fragments("ex example shown"), "ex example shown");
    }

    #[test]
    fn regression_un_before_until() {
        // "un" is a common prefix — must be preserved
        assert_eq!(dedup_word_fragments("un until now"), "un until now");
    }

    #[test]
    fn regression_im_before_impossible() {
        // "im" is a common prefix — must be preserved
        assert_eq!(
            dedup_word_fragments("im impossible task"),
            "im impossible task"
        );
    }

    #[test]
    fn regression_de_before_definitely() {
        // "de" is a common prefix — must be preserved
        assert_eq!(
            dedup_word_fragments("de definitely yes"),
            "de definitely yes"
        );
    }

    #[test]
    fn regression_bi_before_bisexual() {
        // "bi" is a common prefix (binary, bisexual) — must be preserved
        assert_eq!(
            dedup_word_fragments("bi bisexual community"),
            "bi bisexual community"
        );
    }

    #[test]
    fn regression_ha_before_happens() {
        // "ha" is a filler word ("ha!") — must be preserved
        assert_eq!(dedup_word_fragments("ha happens often"), "ha happens often");
    }

    #[test]
    fn regression_oh_before_obviously() {
        // "oh" is a common interjection — must be preserved
        assert_eq!(dedup_word_fragments("oh obviously not"), "oh obviously not");
    }

    #[test]
    fn regression_lo_before_lower() {
        // "lo" (as in "lo and behold") — must be preserved
        assert_eq!(dedup_word_fragments("lo lower prices"), "lo lower prices");
    }

    #[test]
    fn regression_sta_before_starting() {
        // This was the original failure case — must still pass
        assert_eq!(dedup_word_fragments("sta starting now"), "starting now");
    }

    #[test]
    fn regression_sta_before_started() {
        // Another original failure case — must still pass
        assert_eq!(
            dedup_word_fragments("I sta started running"),
            "I started running"
        );
    }

    #[test]
    fn regression_extension_boundary() {
        // "pro" → "process" extends by 4 — within MAX_FRAGMENT_EXTENSION=5
        // But "pro" is in COMMON_WORDS, so it should be preserved regardless
        assert_eq!(dedup_word_fragments("pro process"), "pro process");
    }

    #[test]
    fn regression_extension_exactly_5() {
        // "sta" → "starting" extends by 5 — exactly MAX_FRAGMENT_EXTENSION
        assert_eq!(dedup_word_fragments("sta starting"), "starting");
    }

    #[test]
    fn regression_unknown_word_over_extension() {
        // Unknown 3-letter word with extension > 5 should be preserved by extension limit.
        // "fra" → "fraction" extends by 5, which equals MAX_FRAGMENT_EXTENSION.
        // This should be removed since ext=5≤5 and "fra" is not in COMMON_WORDS.
        assert_eq!(dedup_word_fragments("fra fraction"), "fraction");
    }

    // === STAIRCASE DETECTION TESTS ===
    // These test the new staircase detection that handles CTC artifacts
    // where multiple consecutive prefix words point to the same target.

    #[test]
    fn staircase_can_c_candles() {
        // The user-reported bug: "can c candles" → "candles"
        // Both "can" and "c" are prefixes of "candles"
        assert_eq!(
            dedup_word_fragments(
                "Well, perhaps not going without can c candles because that was insanity."
            ),
            "Well, perhaps not going without candles because that was insanity."
        );
    }

    #[test]
    fn staircase_simple() {
        // Simplified staircase: "can c candles" → "candles"
        assert_eq!(dedup_word_fragments("can c candles"), "candles");
    }

    #[test]
    fn staircase_with_other_words() {
        // Staircase in sentence context
        assert_eq!(
            dedup_word_fragments("we need can c candles for the dinner"),
            "we need candles for the dinner"
        );
    }

    #[test]
    fn single_char_fragment_removed() {
        // "c" before "candles" is a single-char CTC artifact — should be removed
        assert_eq!(dedup_word_fragments("we need c candles"), "we need candles");
    }

    #[test]
    fn single_char_a_protected() {
        // "a" before "apple" is protected by COMMON_WORDS
        assert_eq!(dedup_word_fragments("I have a apple"), "I have a apple");
    }

    #[test]
    fn single_char_I_protected() {
        // "I" before "Iceland" is protected by COMMON_WORDS
        assert_eq!(dedup_word_fragments("I Iceland trip"), "I Iceland trip");
    }

    #[test]
    fn single_char_w_before_wh_removed() {
        // "w" before "wh" is a CTC artifact → removed
        // Then "wh" before "why" is also a fragment (2 chars, not common) → also removed
        // Result: just "why"
        assert_eq!(dedup_word_fragments("w wh why"), "why");
    }

    #[test]
    fn staircase_not_triggered_for_can_cancel() {
        // "can" before "cancel" — no staircase (no intermediate fragment)
        // "can" is in COMMON_WORDS → kept
        assert_eq!(
            dedup_word_fragments("I can cancel this"),
            "I can cancel this"
        );
    }

    #[test]
    fn staircase_not_triggered_for_in_inside() {
        // No staircase here — just common word "in" before "inside"
        assert_eq!(
            dedup_word_fragments("in inside the house"),
            "in inside the house"
        );
    }

    #[test]
    fn staircase_preserves_single_letter_before() {
        // "a c candles" → "a" is single-letter, protected → "a candles"
        assert_eq!(
            dedup_word_fragments("I need a c candles"),
            "I need a candles"
        );
    }

    #[test]
    fn full_pipeline_can_c_candles() {
        // Full pipeline test: filter_transcription_output with the reported bug
        assert_eq!(
            filter_transcription_output(
                "Well, perhaps not going without can c candles because that was insanity.",
                "en",
                &None
            ),
            "Well, perhaps not going without candles because that was insanity."
        );
    }

    #[test]
    fn staircase_single_char_w_before_why() {
        // Dedup removes "w", then stutter collapse handles "wh why" → wait, "wh" is not a stutter of "why"
        // "w" is removed as fragment of "wh", leaving "wh why"
        // "wh" is not a prefix of "why" (it's "wh" vs "why" — "wh" IS a prefix!)
        // Actually "wh" (2 chars) before "why" (3 chars) → "wh" not in COMMON_WORDS → removed
        assert_eq!(dedup_word_fragments("w wh why"), "why");
    }

    #[test]
    fn staircase_partial() {
        // Only a single-char fragment, no staircase
        assert_eq!(
            dedup_word_fragments("the c candles are bright"),
            "the candles are bright"
        );
    }

    #[test]
    fn staircase_with_punctuation() {
        // Staircase with comma
        assert_eq!(
            dedup_word_fragments("Well, can c candles, please"),
            "Well, candles, please"
        );
    }

    #[test]
    fn single_char_preserves_non_prefix() {
        // Single char "x" before "ray" — not a prefix, kept
        assert_eq!(
            dedup_word_fragments("the x ray machine"),
            "the x ray machine"
        );
    }

    #[test]
    fn single_char_fragment_with_long_word() {
        // Single char "c" before very long word — still removed (skip extension check)
        assert_eq!(dedup_word_fragments("c chromatography"), "chromatography");
    }

    // --- Advanced Custom Words Tests ---

    #[test]
    fn test_advanced_custom_words_exact_match() {
        use crate::settings::CustomWord;
        let text = "hello world";
        let words = vec![
            CustomWord {
                word: "Hello".to_string(),
                pronunciations: vec![],
            },
            CustomWord {
                word: "World".to_string(),
                pronunciations: vec![],
            },
        ];
        let result = apply_advanced_custom_words(text, &words, 0.5);
        assert_eq!(result, "Hello World");
    }

    #[test]
    fn test_advanced_custom_words_with_pronunciation() {
        use crate::settings::CustomWord;
        let text = "il cui nome è Charge B, che permette";
        let words = vec![CustomWord {
            word: "ChargeBee".to_string(),
            pronunciations: vec!["charge b".to_string(), "charge bee".to_string()],
        }];
        let result = apply_advanced_custom_words(text, &words, 0.5);
        assert!(result.contains("ChargeBee"), "got: {}", result);
        assert!(!result.contains("Charge B"), "got: {}", result);
    }

    #[test]
    fn test_advanced_custom_words_pronunciation_replaces_with_canonical() {
        use crate::settings::CustomWord;
        // When a pronunciation matches, the transcript should be replaced with the canonical word
        // Use comma to prevent 3-gram from swallowing the next word
        let text = "I use charge bee, for payments";
        let words = vec![CustomWord {
            word: "ChargeBee".to_string(),
            pronunciations: vec!["charge b".to_string(), "charge bee".to_string()],
        }];
        let result = apply_advanced_custom_words(text, &words, 0.5);
        assert!(result.contains("ChargeBee"), "got: {}", result);
    }

    #[test]
    fn test_advanced_custom_words_kubernetes() {
        use crate::settings::CustomWord;
        let text = "deploy on koober netty cluster";
        let words = vec![CustomWord {
            word: "Kubernetes".to_string(),
            pronunciations: vec!["koober netty".to_string(), "koober nay".to_string()],
        }];
        let result = apply_advanced_custom_words(text, &words, 0.5);
        assert!(result.contains("Kubernetes"), "got: {}", result);
    }

    #[test]
    fn test_advanced_custom_words_empty() {
        use crate::settings::CustomWord;
        let text = "hello world";
        let words: Vec<CustomWord> = vec![];
        let result = apply_advanced_custom_words(text, &words, 0.5);
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_advanced_custom_words_no_pronunciations() {
        use crate::settings::CustomWord;
        // Words without pronunciations should work like simple custom words
        let text = "helo wrold";
        let words = vec![
            CustomWord {
                word: "hello".to_string(),
                pronunciations: vec![],
            },
            CustomWord {
                word: "world".to_string(),
                pronunciations: vec![],
            },
        ];
        let result = apply_advanced_custom_words(text, &words, 0.5);
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_advanced_custom_words_preserves_case() {
        use crate::settings::CustomWord;
        let text = "CHARGE B is great";
        let words = vec![CustomWord {
            word: "ChargeBee".to_string(),
            pronunciations: vec!["charge b".to_string()],
        }];
        let result = apply_advanced_custom_words(text, &words, 0.5);
        assert!(result.contains("CHARGEBEE"), "got: {}", result);
    }

    #[test]
    fn test_advanced_custom_words_chatgpt() {
        use crate::settings::CustomWord;
        let text = "use Chat G P T for this";
        let words = vec![CustomWord {
            word: "ChatGPT".to_string(),
            pronunciations: vec!["chat g p t".to_string(), "chat gpt".to_string()],
        }];
        let result = apply_advanced_custom_words(text, &words, 0.5);
        assert!(result.contains("ChatGPT"), "got: {}", result);
    }

    // === NUMBER WORDS TO DIGITS CONVERSION TESTS ===

    #[test]
    fn test_convert_number_words_zero_zero_four() {
        assert_eq!(
            convert_number_words_to_digits("zero zero four", "en"),
            "004"
        );
    }

    #[test]
    fn test_convert_number_words_zero_zero_four_capitalized() {
        assert_eq!(
            convert_number_words_to_digits("Zero Zero Four", "en"),
            "004"
        );
    }

    #[test]
    fn test_convert_number_words_twenty_five() {
        assert_eq!(convert_number_words_to_digits("twenty-five", "en"), "25");
    }

    #[test]
    fn test_convert_number_words_one_hundred() {
        assert_eq!(convert_number_words_to_digits("one hundred", "en"), "100");
    }

    #[test]
    fn test_convert_number_words_two_thousand_and_twenty_six() {
        assert_eq!(
            convert_number_words_to_digits("two thousand and twenty six", "en"),
            "2026"
        );
    }

    #[test]
    fn test_convert_number_words_three_point_five() {
        assert_eq!(
            convert_number_words_to_digits("three point five", "en"),
            "3.5"
        );
    }

    #[test]
    fn test_convert_number_words_bare_point_unchanged() {
        assert_eq!(convert_number_words_to_digits("point", "en"), "point");
        assert_eq!(convert_number_words_to_digits("Point.", "en"), "Point.");
        assert_eq!(
            convert_number_words_to_digits("a talking point", "en"),
            "a talking point"
        );
    }

    #[test]
    fn test_convert_number_words_trailing_point_unchanged() {
        // A spoken sequence ending in "point" with no following number word
        // must stay in words, preserving attached punctuation and capitalization.
        assert_eq!(
            convert_number_words_to_digits("one point", "en"),
            "one point"
        );
        assert_eq!(
            convert_number_words_to_digits("three point.", "en"),
            "three point."
        );
        assert_eq!(
            convert_number_words_to_digits("One point.", "en"),
            "One point."
        );
        // Repeated "point" at the end (or after a valid decimal start) must
        // keep the whole run as words.
        assert_eq!(
            convert_number_words_to_digits("one point two point", "en"),
            "one point two point"
        );
        assert_eq!(
            convert_number_words_to_digits("one point point", "en"),
            "one point point"
        );
    }

    #[test]
    fn test_convert_number_words_point_with_following_number_converts() {
        // Guard against over-rejection: decimals with a number after "point"
        // must still convert.
        assert_eq!(convert_number_words_to_digits("one point two", "en"), "1.2");
        assert_eq!(
            convert_number_words_to_digits("one hundred point five", "en"),
            "100.5"
        );
    }

    #[test]
    fn test_convert_number_words_lone_word_unchanged() {
        assert_eq!(
            convert_number_words_to_digits("I saw one bird", "en"),
            "I saw one bird"
        );
        assert_eq!(
            convert_number_words_to_digits("two apples", "en"),
            "two apples"
        );
    }

    #[test]
    fn test_convert_number_words_non_english_unchanged() {
        // Spanish text with a number word should remain unchanged.
        assert_eq!(
            convert_number_words_to_digits("tengo cinco manzanas", "es"),
            "tengo cinco manzanas"
        );
    }

    #[test]
    fn test_convert_number_words_case_insensitive() {
        assert_eq!(convert_number_words_to_digits("Twenty-Five", "en"), "25");
        assert_eq!(convert_number_words_to_digits("ONE HUNDRED", "en"), "100");
    }

    #[test]
    fn test_convert_number_words_punctuation() {
        assert_eq!(
            convert_number_words_to_digits("zero, zero, four", "en"),
            "004"
        );
        assert_eq!(
            convert_number_words_to_digits("(zero zero four)", "en"),
            "(004)"
        );
    }

    #[test]
    fn test_convert_number_words_mixed_sentence() {
        assert_eq!(
            convert_number_words_to_digits(
                "call zero zero four then twenty-five for room one hundred",
                "en"
            ),
            "call 004 then 25 for room 100"
        );
    }

    #[test]
    fn test_convert_number_words_large_numbers() {
        assert_eq!(
            convert_number_words_to_digits(
                "one million two hundred thirty four thousand five hundred sixty seven",
                "en"
            ),
            "1234567"
        );
    }

    #[test]
    fn test_convert_number_words_decimal_sequence() {
        assert_eq!(
            convert_number_words_to_digits("one hundred twenty three point four five six", "en"),
            "123.456"
        );
    }

    #[test]
    fn test_convert_number_words_empty_input() {
        assert_eq!(convert_number_words_to_digits("", "en"), "");
    }

    #[test]
    fn test_convert_number_words_only_lone_digits_unchanged() {
        // Each isolated single digit word stays as a word because we only
        // convert runs that span multiple tokens, hyphenated compounds, or
        // contain a scale/point word.
        assert_eq!(
            convert_number_words_to_digits("one plus one equals two", "en"),
            "one plus one equals two"
        );
    }

    #[test]
    fn test_convert_number_words_app_language_en_us() {
        assert_eq!(convert_number_words_to_digits("twenty-five", "en-US"), "25");
    }

    #[test]
    fn test_convert_number_words_zero_alone_unchanged() {
        assert_eq!(convert_number_words_to_digits("zero", "en"), "zero");
    }

    #[test]
    fn test_convert_number_words_clock_times_accept() {
        assert_eq!(
            convert_number_words_to_digits("two ten pm", "en"),
            "2:10 PM"
        );
        assert_eq!(
            convert_number_words_to_digits("eleven forty five pm", "en"),
            "11:45 PM"
        );
        assert_eq!(
            convert_number_words_to_digits("two thirty am", "en"),
            "2:30 AM"
        );
        assert_eq!(
            convert_number_words_to_digits("twelve thirty am", "en"),
            "12:30 AM"
        );
        assert_eq!(
            convert_number_words_to_digits("two five pm", "en"),
            "2:05 PM"
        );
        assert_eq!(
            convert_number_words_to_digits("one twenty five pm", "en"),
            "1:25 PM"
        );
        assert_eq!(
            convert_number_words_to_digits("two thirteen pm", "en"),
            "2:13 PM"
        );
        assert_eq!(
            convert_number_words_to_digits("two zero pm", "en"),
            "2:00 PM"
        );
    }

    #[test]
    fn test_convert_number_words_clock_times_hour_only() {
        assert_eq!(convert_number_words_to_digits("two pm", "en"), "2 PM");
        assert_eq!(convert_number_words_to_digits("twelve am", "en"), "12 AM");
        assert_eq!(
            convert_number_words_to_digits("meet at nine am sharp", "en"),
            "meet at 9 AM sharp"
        );
    }

    #[test]
    fn test_convert_number_words_clock_times_case_and_punctuation() {
        assert_eq!(
            convert_number_words_to_digits("Two Ten Pm", "en"),
            "2:10 PM"
        );
        assert_eq!(
            convert_number_words_to_digits("two ten pm.", "en"),
            "2:10 PM."
        );
        assert_eq!(
            convert_number_words_to_digits("(two ten pm)", "en"),
            "(2:10 PM)"
        );
        assert_eq!(
            convert_number_words_to_digits("two ten p.m.", "en"),
            "2:10 PM."
        );
        assert_eq!(
            convert_number_words_to_digits("noon at two ten pm is late", "en"),
            "noon at 2:10 PM is late"
        );
    }

    #[test]
    fn test_convert_number_words_clock_times_rejected() {
        assert_eq!(
            convert_number_words_to_digits("thirteen pm", "en"),
            "thirteen pm"
        );
        assert_eq!(
            convert_number_words_to_digits("two ninety pm", "en"),
            "two ninety pm"
        );
        assert_eq!(
            convert_number_words_to_digits("two sixty pm", "en"),
            "two sixty pm"
        );
        assert_eq!(
            convert_number_words_to_digits("two ninety five pm", "en"),
            "two ninety five pm"
        );
        assert_eq!(
            convert_number_words_to_digits("five hundred pm", "en"),
            "five hundred pm"
        );
        assert_eq!(
            convert_number_words_to_digits("two and ten pm", "en"),
            "two and ten pm"
        );
        assert_eq!(
            convert_number_words_to_digits("three point five pm", "en"),
            "three point five pm"
        );
        assert_eq!(
            convert_number_words_to_digits("twenty pm", "en"),
            "twenty pm"
        );
        assert_eq!(
            convert_number_words_to_digits("two oh five pm", "en"),
            "two oh five pm"
        );
        assert_eq!(
            convert_number_words_to_digits("oh five pm", "en"),
            "oh five pm"
        );
    }

    #[test]
    fn test_convert_number_words_clock_times_non_time_unchanged() {
        assert_eq!(convert_number_words_to_digits("two ten", "en"), "12");
        assert_eq!(
            convert_number_words_to_digits("call zero zero four then two ten pm", "en"),
            "call 004 then 2:10 PM"
        );
    }

    // === US TO BRITISH ENGLISH CONVERSION TESTS ===

    #[test]
    fn test_us_to_british_color() {
        assert_eq!(convert_us_to_british("color"), "colour");
        assert_eq!(convert_us_to_british("colors"), "colours");
        assert_eq!(
            convert_us_to_british("The color is red"),
            "The colour is red"
        );
    }

    #[test]
    fn test_us_to_british_analyze() {
        assert_eq!(convert_us_to_british("analyze"), "analyse");
        assert_eq!(convert_us_to_british("analyzed"), "analysed");
        assert_eq!(convert_us_to_british("analyzing"), "analysing");
        assert_eq!(
            convert_us_to_british("Please analyze the data"),
            "Please analyse the data"
        );
    }

    #[test]
    fn test_us_to_british_center() {
        assert_eq!(convert_us_to_british("center"), "centre");
        assert_eq!(convert_us_to_british("centers"), "centres");
        assert_eq!(convert_us_to_british("centered"), "centred");
        assert_eq!(
            convert_us_to_british("The center of the circle"),
            "The centre of the circle"
        );
    }

    #[test]
    fn test_us_to_british_defense() {
        assert_eq!(convert_us_to_british("defense"), "defence");
        assert_eq!(convert_us_to_british("defenses"), "defences");
        assert_eq!(
            convert_us_to_british("The defense was strong"),
            "The defence was strong"
        );
    }

    #[test]
    fn test_us_to_british_dialog_not_converted() {
        // dialog/dialogue are DIFFERENT WORDS in British English:
        // - dialog: UI element in computing (valid in British English)
        // - dialogue: conversation
        // Both exist with different meanings, so NO auto-conversion
        assert_eq!(convert_us_to_british("dialog"), "dialog");
        assert_eq!(convert_us_to_british("dialogs"), "dialogs");
        assert_eq!(
            convert_us_to_british("Click the dialog box"),
            "Click the dialog box"
        );
        // But "dialogue" (already British) should remain unchanged
        assert_eq!(convert_us_to_british("dialogue"), "dialogue");
        assert_eq!(convert_us_to_british("dialogues"), "dialogues");
    }

    #[test]
    fn test_us_to_british_traveled() {
        assert_eq!(convert_us_to_british("traveled"), "travelled");
        assert_eq!(convert_us_to_british("traveler"), "traveller");
        assert_eq!(convert_us_to_british("travelers"), "travellers");
        assert_eq!(
            convert_us_to_british("I traveled to London"),
            "I travelled to London"
        );
    }

    #[test]
    fn test_us_to_british_organize() {
        assert_eq!(convert_us_to_british("organize"), "organise");
        assert_eq!(convert_us_to_british("organized"), "organised");
        assert_eq!(convert_us_to_british("organizing"), "organising");
        assert_eq!(convert_us_to_british("organization"), "organisation");
        assert_eq!(convert_us_to_british("organizations"), "organisations");
    }

    #[test]
    fn test_us_to_british_case_preservation() {
        assert_eq!(convert_us_to_british("Color"), "Colour");
        assert_eq!(convert_us_to_british("COLOR"), "COLOUR");
        assert_eq!(
            convert_us_to_british("Analyze the Data"),
            "Analyse the Data"
        );
        assert_eq!(convert_us_to_british("THE CENTER"), "THE CENTRE");
    }

    #[test]
    fn test_us_to_british_punctuation_preserved() {
        assert_eq!(convert_us_to_british("color!"), "colour!");
        assert_eq!(convert_us_to_british("color."), "colour.");
        assert_eq!(convert_us_to_british("(color)"), "(colour)");
        assert_eq!(convert_us_to_british("\"color\""), "\"colour\"");
    }

    #[test]
    fn test_us_to_british_mixed_sentence() {
        assert_eq!(
            convert_us_to_british("The color of the center was analyzed"),
            "The colour of the centre was analysed"
        );
        // Note: "dialog" is NOT converted because it's a valid British computing term (UI element)
        assert_eq!(
            convert_us_to_british("I organized the dialog and traveled around"),
            "I organised the dialog and travelled around"
        );
    }

    #[test]
    fn test_us_to_british_semantic_ambiguities_preserved() {
        // Words where BOTH American and British forms exist with different meanings
        // should NOT be auto-converted as context determines which word to use.

        // check/cheque: check (verify) vs cheque (payment)
        assert_eq!(convert_us_to_british("check"), "check");
        assert_eq!(convert_us_to_british("checks"), "checks");
        assert_eq!(convert_us_to_british("checked"), "checked");
        assert_eq!(convert_us_to_british("checking"), "checking");
        assert_eq!(
            convert_us_to_british("I need to check my email"),
            "I need to check my email"
        );

        // program/programme: program (computer software) vs programme (TV show/schedule)
        assert_eq!(convert_us_to_british("program"), "program");
        assert_eq!(convert_us_to_british("programs"), "programs");
        assert_eq!(convert_us_to_british("programmed"), "programmed");
        assert_eq!(convert_us_to_british("programming"), "programming");
        assert_eq!(
            convert_us_to_british("I installed the program"),
            "I installed the program"
        );

        // tire/tyre: tire (to become weary) vs tyre (wheel)
        assert_eq!(convert_us_to_british("tire"), "tire");
        assert_eq!(convert_us_to_british("tires"), "tires");
        assert_eq!(convert_us_to_british("tired"), "tired");
        assert_eq!(convert_us_to_british("tiring"), "tiring");
        assert_eq!(convert_us_to_british("I began to tire"), "I began to tire");

        // catalog/catalogue: catalog (database) vs catalogue (traditional)
        assert_eq!(convert_us_to_british("catalog"), "catalog");
        assert_eq!(convert_us_to_british("catalogs"), "catalogs");
        assert_eq!(convert_us_to_british("cataloged"), "cataloged");
        assert_eq!(convert_us_to_british("cataloging"), "cataloging");

        // dialog/dialogue: dialog (UI element) vs dialogue (conversation)
        assert_eq!(convert_us_to_british("dialog"), "dialog");
        assert_eq!(convert_us_to_british("dialogs"), "dialogs");
        assert_eq!(
            convert_us_to_british("Click the dialog box"),
            "Click the dialog box"
        );

        // meter/metre: measuring device vs unit of length
        assert_eq!(convert_us_to_british("meter"), "meter");
        assert_eq!(convert_us_to_british("meters"), "meters");
        assert_eq!(convert_us_to_british("parking meter"), "parking meter");

        // draft/draught: preliminary version vs air current / beer / board game
        assert_eq!(convert_us_to_british("draft"), "draft");
        assert_eq!(convert_us_to_british("drafts"), "drafts");
        assert_eq!(convert_us_to_british("bank draft"), "bank draft");

        // disk/disc: computing storage vs optical media / body disc
        assert_eq!(convert_us_to_british("disk"), "disk");
        assert_eq!(convert_us_to_british("hard disk"), "hard disk");

        // license/licence: US noun+verb vs UK noun only
        assert_eq!(convert_us_to_british("license"), "license");
        assert_eq!(convert_us_to_british("licenses"), "licenses");
        assert_eq!(convert_us_to_british("licensed"), "licensed");
        assert_eq!(
            convert_us_to_british("I need to license the software"),
            "I need to license the software"
        );

        // font/fount: typeface vs source/fountain
        assert_eq!(convert_us_to_british("font"), "font");
        assert_eq!(convert_us_to_british("fonts"), "fonts");
        assert_eq!(convert_us_to_british("Change the font"), "Change the font");

        // dependent/dependant: adjective vs noun in British English
        assert_eq!(convert_us_to_british("dependent"), "dependent");
        assert_eq!(
            convert_us_to_british("dependent on you"),
            "dependent on you"
        );

        // naught/nought: nothing vs digit zero
        assert_eq!(convert_us_to_british("naught"), "naught");
        assert_eq!(convert_us_to_british("all for naught"), "all for naught");
    }

    #[test]
    fn test_us_to_british_story_not_converted() {
        // "story" and "storey" are DIFFERENT words in British English:
        // - story (noun): a narrative or account
        // - storey (noun): a floor/level of a building
        // They should NOT be auto-converted as context determines which to use.
        assert_eq!(convert_us_to_british("story"), "story");
        assert_eq!(convert_us_to_british("stories"), "stories");
        assert_eq!(convert_us_to_british("storyteller"), "storyteller");
        assert_eq!(convert_us_to_british("Tell me a story"), "Tell me a story");
        // But "storey" (already British) should remain unchanged
        assert_eq!(convert_us_to_british("storey"), "storey");
        assert_eq!(convert_us_to_british("storeys"), "storeys");
    }

    #[test]
    fn test_us_to_british_other_false_friends_not_converted() {
        // gage/gauge: pledge vs measuring instrument
        assert_eq!(convert_us_to_british("gage"), "gage");
        assert_eq!(convert_us_to_british("gages"), "gages");

        // potter/putter: pottery maker vs golf club
        assert_eq!(convert_us_to_british("potter"), "potter");
        assert_eq!(convert_us_to_british("potters"), "potters");

        // prize/prise: award vs lever open
        assert_eq!(convert_us_to_british("prize"), "prize");
        assert_eq!(convert_us_to_british("prizes"), "prizes");
    }

    #[test]
    fn test_us_to_british_check_not_converted() {
        // "check" and "cheque" are DIFFERENT words in British English:
        // - check (verb/noun): to verify, examine, a pattern, a bill
        // - cheque (noun): specifically a written payment order
        // They should NOT be auto-converted as context determines which to use.
        assert_eq!(convert_us_to_british("check"), "check");
        assert_eq!(
            convert_us_to_british("I need to check my email"),
            "I need to check my email"
        );
        assert_eq!(convert_us_to_british("checks"), "checks");
        assert_eq!(convert_us_to_british("checked"), "checked");
        assert_eq!(convert_us_to_british("checking"), "checking");
        // But "cheque" (already British) should remain unchanged
        assert_eq!(convert_us_to_british("cheque"), "cheque");
        assert_eq!(convert_us_to_british("cheques"), "cheques");
    }

    #[test]
    fn test_us_to_british_no_change_for_british() {
        // Already British spelling should remain unchanged
        assert_eq!(convert_us_to_british("colour"), "colour");
        assert_eq!(convert_us_to_british("centre"), "centre");
        assert_eq!(convert_us_to_british("analyse"), "analyse");
        assert_eq!(convert_us_to_british("defence"), "defence");
        assert_eq!(convert_us_to_british("dialogue"), "dialogue");
    }

    #[test]
    fn test_us_to_british_no_change_for_unrelated() {
        // Words not in the conversion list should remain unchanged
        assert_eq!(convert_us_to_british("hello"), "hello");
        assert_eq!(convert_us_to_british("world"), "world");
        assert_eq!(convert_us_to_british("hello world"), "hello world");
        assert_eq!(
            convert_us_to_british("the quick brown fox"),
            "the quick brown fox"
        );
    }

    #[test]
    fn test_us_to_british_gray_grey() {
        assert_eq!(convert_us_to_british("gray"), "grey");
        assert_eq!(convert_us_to_british("grayer"), "greyer");
        assert_eq!(convert_us_to_british("The gray sky"), "The grey sky");
    }

    #[test]
    fn test_us_to_british_mixed_words() {
        // Multiple conversions in one sentence
        assert_eq!(
            convert_us_to_british("I realized the color was wrong after I organized it"),
            "I realised the colour was wrong after I organised it"
        );
    }

    // --- detect_repeated_words tests ---

    #[test]
    fn test_detect_repeated_words_basic() {
        let result = detect_repeated_words("def def definitely");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], ("def", 2));
    }

    #[test]
    fn test_detect_repeated_words_three_reps() {
        // Note: "the" is a protected word, so it won't be suppressed
        // but detect_repeated_words still reports it for informational purposes
        let result = detect_repeated_words("the the the house");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], ("the", 3));
    }

    #[test]
    fn test_detect_repeated_words_no_reps() {
        let result = detect_repeated_words("hello world test");
        assert!(result.is_empty());
    }

    #[test]
    fn test_detect_repeated_words_case_insensitive() {
        let result = detect_repeated_words("The THE the house");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], ("The", 3));
    }

    #[test]
    fn test_detect_repeated_words_multiple_groups() {
        let result = detect_repeated_words("def def the the test");
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], ("def", 2));
        assert_eq!(result[1], ("the", 2));
    }

    #[test]
    fn test_detect_repeated_words_with_punctuation() {
        let result = detect_repeated_words("def, def. definitely");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], ("def,", 2));
    }

    // --- suppress_repeated_words tests ---

    #[test]
    fn test_suppress_repeated_words_off() {
        let text = "def def definitely";
        let result = suppress_repeated_words(text, 0);
        assert_eq!(result, "def def definitely");
    }

    #[test]
    fn test_suppress_repeated_words_light() {
        let text = "def def def definitely";
        let result = suppress_repeated_words(text, 1);
        assert_eq!(result, "def definitely");
    }

    #[test]
    fn test_suppress_repeated_words_moderate() {
        let text = "def def definitely";
        let result = suppress_repeated_words(text, 2);
        assert_eq!(result, "def definitely");
    }

    #[test]
    fn test_suppress_repeated_words_preserves_two() {
        let text = "no no is fine";
        // Level 1 requires 3+ reps, so "no no" (2 reps) is preserved
        let result = suppress_repeated_words(text, 1);
        assert_eq!(result, "no no is fine");
    }

    #[test]
    fn test_suppress_repeated_words_with_punctuation() {
        let text = "def, def. definitely";
        let result = suppress_repeated_words(text, 2);
        assert_eq!(result, "def, definitely");
    }

    #[test]
    fn test_suppress_repeated_words_empty() {
        let result = suppress_repeated_words("", 2);
        assert_eq!(result, "");
    }

    #[test]
    fn test_suppress_repeated_words_no_match() {
        let text = "hello world test";
        let result = suppress_repeated_words(text, 2);
        assert_eq!(result, "hello world test");
    }

    // --- protected words tests ---

    #[test]
    fn test_suppress_protected_pronoun() {
        // "I I I" should NOT be suppressed - it's a protected pronoun
        let text = "I I I think so";
        let result = suppress_repeated_words(text, 2);
        assert_eq!(result, "I I I think so");
    }

    #[test]
    fn test_suppress_protected_article() {
        // "the the" should NOT be suppressed - "the" is a protected article
        let text = "the the table";
        let result = suppress_repeated_words(text, 2);
        assert_eq!(result, "the the table");
    }

    #[test]
    fn test_suppress_protected_intensifier() {
        // "very very" should NOT be suppressed - legitimate emphasis
        let text = "very very good";
        let result = suppress_repeated_words(text, 2);
        assert_eq!(result, "very very good");
    }

    #[test]
    fn test_suppress_protected_conjunction() {
        // "and and" should NOT be suppressed
        let text = "and and then";
        let result = suppress_repeated_words(text, 2);
        assert_eq!(result, "and and then");
    }

    #[test]
    fn test_suppress_non_protected_word() {
        // "def def" SHOULD be suppressed - not a protected word
        let text = "def def definitely";
        let result = suppress_repeated_words(text, 2);
        assert_eq!(result, "def definitely");
    }

    #[test]
    fn test_suppress_mixed_protected_and_nonprotected() {
        // Protected "I I" stays, non-protected "def def" gets cleaned
        let text = "I I think def def definitely";
        let result = suppress_repeated_words(text, 2);
        assert_eq!(result, "I I think def definitely");
    }

    #[test]
    fn test_suppress_protected_case_insensitive() {
        // Protected words should work case-insensitively
        let text = "THE THE table";
        let result = suppress_repeated_words(text, 2);
        assert_eq!(result, "THE THE table");
    }

    #[test]
    fn test_is_protected_word() {
        assert!(is_protected_word("i"));
        assert!(is_protected_word("I"));
        assert!(is_protected_word("the"));
        assert!(is_protected_word("very"));
        assert!(is_protected_word("and"));
        assert!(!is_protected_word("def"));
    }

    // ========== apply_word_replacements tests ==========

    #[test]
    fn test_apply_word_replacements_single_word() {
        let replacements = vec![crate::settings::WordReplacement {
            mistranslation: "definately".to_string(),
            correction: "definitely".to_string(),
        }];
        let text = "I will definately do it";
        let result = apply_word_replacements(text, &replacements);
        assert_eq!(result, "I will definitely do it");
    }

    #[test]
    fn test_apply_word_replacements_case_insensitive() {
        let replacements = vec![crate::settings::WordReplacement {
            mistranslation: "openai".to_string(),
            correction: "OpenAI".to_string(),
        }];
        let text = "I use openai for work";
        let result = apply_word_replacements(text, &replacements);
        assert_eq!(result, "I use OpenAI for work");
    }

    #[test]
    fn test_apply_word_replacements_preserve_case_upper() {
        let replacements = vec![crate::settings::WordReplacement {
            mistranslation: "openai".to_string(),
            correction: "OpenAI".to_string(),
        }];
        let text = "I USE OPENAI FOR WORK";
        let result = apply_word_replacements(text, &replacements);
        assert_eq!(result, "I USE OPENAI FOR WORK");
    }

    #[test]
    fn test_apply_word_replacements_preserve_case_title() {
        let replacements = vec![crate::settings::WordReplacement {
            mistranslation: "chargebee".to_string(),
            correction: "ChargeBee".to_string(),
        }];
        let text = "Chargebee is great";
        let result = apply_word_replacements(text, &replacements);
        assert_eq!(result, "ChargeBee is great");
    }

    #[test]
    fn test_apply_word_replacements_multi_word() {
        let replacements = vec![crate::settings::WordReplacement {
            mistranslation: "open a i".to_string(),
            correction: "OpenAI".to_string(),
        }];
        let text = "I use open a i every day";
        let result = apply_word_replacements(text, &replacements);
        assert_eq!(result, "I use OpenAI every day");
    }

    #[test]
    fn test_apply_word_replacements_multi_word_with_punctuation() {
        let replacements = vec![crate::settings::WordReplacement {
            mistranslation: "charge b".to_string(),
            correction: "ChargeBee".to_string(),
        }];
        let text = "I use charge b, it's great.";
        let result = apply_word_replacements(text, &replacements);
        assert_eq!(result, "I use ChargeBee, it's great.");
    }

    #[test]
    fn test_apply_word_replacements_word_boundary() {
        let replacements = vec![crate::settings::WordReplacement {
            mistranslation: "the".to_string(),
            correction: "a".to_string(),
        }];
        // "the" should NOT match within "there" or "them"
        let text = "the cat is there with them";
        let result = apply_word_replacements(text, &replacements);
        assert_eq!(result, "a cat is there with them");
    }

    #[test]
    fn test_apply_word_replacements_multiple_replacements() {
        let replacements = vec![
            crate::settings::WordReplacement {
                mistranslation: "definately".to_string(),
                correction: "definitely".to_string(),
            },
            crate::settings::WordReplacement {
                mistranslation: "seperate".to_string(),
                correction: "separate".to_string(),
            },
        ];
        let text = "They are definately seperate things";
        let result = apply_word_replacements(text, &replacements);
        assert_eq!(result, "They are definitely separate things");
    }

    #[test]
    fn test_apply_word_replacements_punctuation() {
        let replacements = vec![crate::settings::WordReplacement {
            mistranslation: "openai".to_string(),
            correction: "OpenAI".to_string(),
        }];
        let text = "OpenAI's API is great.";
        let result = apply_word_replacements(text, &replacements);
        // Should match "OpenAI" part but preserve punctuation
        assert_eq!(result, "OpenAI's API is great.");
    }

    #[test]
    fn test_apply_word_replacements_empty_replacements() {
        let replacements: Vec<crate::settings::WordReplacement> = vec![];
        let text = "no replacements here";
        let result = apply_word_replacements(text, &replacements);
        assert_eq!(result, "no replacements here");
    }

    #[test]
    fn test_apply_word_replacements_no_match() {
        let replacements = vec![crate::settings::WordReplacement {
            mistranslation: "chargebee".to_string(),
            correction: "ChargeBee".to_string(),
        }];
        let text = "nothing to replace here";
        let result = apply_word_replacements(text, &replacements);
        assert_eq!(result, "nothing to replace here");
    }

    #[test]
    fn test_apply_word_replacements_overlapping() {
        let replacements = vec![
            crate::settings::WordReplacement {
                mistranslation: "open a i".to_string(),
                correction: "OpenAI".to_string(),
            },
            crate::settings::WordReplacement {
                mistranslation: "openai".to_string(),
                correction: "OpenAI".to_string(),
            },
        ];
        // Should prefer longer match
        let text = "I use open a i and openai both";
        let result = apply_word_replacements(text, &replacements);
        assert_eq!(result, "I use OpenAI and OpenAI both");
    }

    // --- replace_unk_with_accent_recovery tests ---

    #[test]
    fn test_recovery_fiance() {
        // fiancé → fianc + <unk> → should recover "fiance"
        assert_eq!(
            replace_unk_with_accent_recovery("my fianc is great"),
            "my fiance is great"
        );
    }

    #[test]
    fn test_recovery_cafe() {
        // café → caf + <unk> → should recover "cafe"
        assert_eq!(
            replace_unk_with_accent_recovery("I love caf con leche"),
            "I love cafe con leche"
        );
    }

    #[test]
    fn test_recovery_resume() {
        // résumé → resum + <unk><unk> → should recover "resume"
        assert_eq!(
            replace_unk_with_accent_recovery("send me your resum"),
            "send me your resume"
        );
    }

    #[test]
    fn test_recovery_jalapeno() {
        // jalapeño → jalapeo + <unk> → should recover "jalapeno"
        assert_eq!(
            replace_unk_with_accent_recovery("the jalapeo is hot"),
            "the jalapeno is hot"
        );
    }

    #[test]
    fn test_recovery_oeuvre() {
        // œuvre → uvre + <unk> → should recover "oeuvre"
        assert_eq!(
            replace_unk_with_accent_recovery("the uvre was magnificent"),
            "the oeuvre was magnificent"
        );
    }

    #[test]
    fn test_recovery_with_punctuation() {
        // fiancé! → fianc + <unk>! → should recover "fiance!"
        assert_eq!(
            replace_unk_with_accent_recovery("my fianc is great!"),
            "my fiance is great!"
        );
    }

    #[test]
    fn test_recovery_preserves_case() {
        // Fianc → should become "Fiance" (capital F preserved)
        assert_eq!(
            replace_unk_with_accent_recovery("Fianc is a French word"),
            "Fiance is a French word"
        );
    }

    #[test]
    fn test_recovery_all_caps() {
        // FIANC → should become "FIANCE" (all caps preserved)
        assert_eq!(
            replace_unk_with_accent_recovery("FIANC is not a word"),
            "FIANCE is not a word"
        );
    }

    #[test]
    fn test_recovery_no_unk() {
        // Text without <unk> should pass through unchanged (no truncation)
        assert_eq!(
            replace_unk_with_accent_recovery("hello world"),
            "hello world"
        );
    }

    #[test]
    fn test_recovery_empty_string() {
        assert_eq!(replace_unk_with_accent_recovery(""), "");
    }

    #[test]
    fn test_recovery_only_unk() {
        assert_eq!(replace_unk_with_accent_recovery("<unk>"), "");
        assert_eq!(replace_unk_with_accent_recovery("<unk> <unk>"), "");
    }

    #[test]
    fn test_recovery_mixed_unk_and_normal() {
        // split_whitespace collapses multiple spaces
        assert_eq!(
            replace_unk_with_accent_recovery("hello <unk> world"),
            "hello world"
        );
    }

    #[test]
    fn test_recovery_crepe() {
        assert_eq!(
            replace_unk_with_accent_recovery("I ordered a crpe"),
            "I ordered a crepe"
        );
    }

    #[test]
    fn test_recovery_protege() {
        assert_eq!(
            replace_unk_with_accent_recovery("she is my proteg"),
            "she is my protege"
        );
    }

    #[test]
    fn test_recovery_subpoena() {
        assert_eq!(
            replace_unk_with_accent_recovery("the subpna was issued"),
            "the subpoena was issued"
        );
    }

    #[test]
    fn test_recovery_fetus() {
        assert_eq!(
            replace_unk_with_accent_recovery("the ftus is healthy"),
            "the fetus is healthy"
        );
    }

    #[test]
    fn test_recovery_sentence_context() {
        // Full sentence with multiple accented words
        assert_eq!(
            replace_unk_with_accent_recovery("my fianc picked up a crpe from the caf"),
            "my fiance picked up a crepe from the cafe"
        );
    }

    #[test]
    fn test_recovery_unk_between_words() {
        // <unk> in the middle of a word (Parakeet sometimes does this)
        assert_eq!(
            replace_unk_with_accent_recovery("the jalape<unk>o pepper"),
            "the jalapeno pepper"
        );
    }

    #[test]
    fn test_recovery_preserves_surrounding_punctuation() {
        assert_eq!(
            replace_unk_with_accent_recovery("(fianc) is great"),
            "(fiance) is great"
        );
    }

    #[test]
    fn test_recovery_unknown_word_unchanged() {
        // Words not in the dictionary should pass through unchanged
        assert_eq!(
            replace_unk_with_accent_recovery("xyznotaword"),
            "xyznotaword"
        );
    }

    #[test]
    fn test_recovery_numbers_unchanged() {
        // split_whitespace collapses multiple spaces
        assert_eq!(replace_unk_with_accent_recovery("123 <unk> 456"), "123 456");
    }
}
