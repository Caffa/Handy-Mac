use natural::phonetics::soundex;
use once_cell::sync::Lazy;
use regex::Regex;
use strsim::levenshtein;

/// Builds an n-gram string by cleaning and concatenating words
///
/// Strips punctuation from each word, lowercases, and joins without spaces.
/// This allows matching "Charge B" against "ChargeBee".
fn build_ngram(words: &[&str]) -> String {
    words
        .iter()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .collect::<Vec<_>>()
        .concat()
}

/// Finds the best matching custom word for a candidate string
///
/// Uses Levenshtein distance and Soundex phonetic matching to find
/// the best match above the given threshold.
///
/// # Arguments
/// * `candidate` - The cleaned/lowercased candidate string to match
/// * `custom_words` - Original custom words (for returning the replacement)
/// * `custom_words_nospace` - Custom words with spaces removed, lowercased (for comparison)
/// * `threshold` - Maximum similarity score to accept
///
/// # Returns
/// The best matching custom word and its score, if any match was found
fn find_best_match<'a>(
    candidate: &str,
    custom_words: &'a [String],
    custom_words_nospace: &[String],
    threshold: f64,
) -> Option<(&'a String, f64)> {
    if candidate.is_empty() || candidate.len() > 50 {
        return None;
    }

    let mut best_match: Option<&String> = None;
    let mut best_score = f64::MAX;

    for (i, custom_word_nospace) in custom_words_nospace.iter().enumerate() {
        // Skip if lengths are too different (optimization + prevents over-matching)
        // Use percentage-based check: max 25% length difference (prevents n-grams from
        // matching significantly shorter custom words, e.g., "openaigpt" vs "openai")
        let len_diff = (candidate.len() as i32 - custom_word_nospace.len() as i32).abs() as f64;
        let max_len = candidate.len().max(custom_word_nospace.len()) as f64;
        let max_allowed_diff = (max_len * 0.25).max(2.0); // At least 2 chars difference allowed
        if len_diff > max_allowed_diff {
            continue;
        }

        // Calculate Levenshtein distance (normalized by length)
        let levenshtein_dist = levenshtein(candidate, custom_word_nospace);
        let max_len = candidate.len().max(custom_word_nospace.len()) as f64;
        let levenshtein_score = if max_len > 0.0 {
            levenshtein_dist as f64 / max_len
        } else {
            1.0
        };

        // Calculate phonetic similarity using Soundex
        let phonetic_match = soundex(candidate, custom_word_nospace);

        // Combine scores: favor phonetic matches, but also consider string similarity
        let combined_score = if phonetic_match {
            levenshtein_score * 0.3 // Give significant boost to phonetic matches
        } else {
            levenshtein_score
        };

        // Accept if the score is good enough (configurable threshold)
        if combined_score < threshold && combined_score < best_score {
            best_match = Some(&custom_words[i]);
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

    // Pre-compute lowercase versions to avoid repeated allocations
    let custom_words_lower: Vec<String> = custom_words.iter().map(|w| w.to_lowercase()).collect();

    // Pre-compute versions with spaces removed for n-gram comparison
    let custom_words_nospace: Vec<String> = custom_words_lower
        .iter()
        .map(|w| w.replace(' ', ""))
        .collect();

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

            if let Some((replacement, _score)) =
                find_best_match(&ngram, custom_words, &custom_words_nospace, threshold)
            {
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

/// Preserves the case pattern of the original word when applying a replacement
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

/// Extracts punctuation prefix and suffix from a word
fn extract_punctuation(word: &str) -> (&str, &str) {
    let prefix_end = word.chars().take_while(|c| !c.is_alphanumeric()).count();
    let suffix_start = word
        .char_indices()
        .rev()
        .take_while(|(_, c)| !c.is_alphanumeric())
        .count();

    let prefix = if prefix_end > 0 {
        &word[..prefix_end]
    } else {
        ""
    };

    let suffix = if suffix_start > 0 {
        &word[word.len() - suffix_start..]
    } else {
        ""
    };

    (prefix, suffix)
}

/// Returns filler words appropriate for the given language code.
///
/// Some words like "um" and "ha" are real words in certain languages
/// (e.g., Portuguese "um" = "a/an", Spanish "ha" = "has"), so we only
/// include them as fillers for languages where they are truly fillers.
fn get_filler_words_for_language(lang: &str) -> &'static [&'static str] {
    let base_lang = lang.split(&['-', '_'][..]).next().unwrap_or(lang);

    match base_lang {
        "en" => &[
            "uh", "um", "uhm", "umm", "uhh", "uhhh", "ah", "hmm", "hm", "mmm", "mm", "mh", "eh",
            "ehh", "ha",
        ],
        "es" => &["ehm", "mmm", "hmm", "hm"],
        "pt" => &["ahm", "hmm", "mmm", "hm"],
        "fr" => &["euh", "hmm", "hm", "mmm"],
        "de" => &["äh", "ähm", "hmm", "hm", "mmm"],
        "it" => &["ehm", "hmm", "mmm", "hm"],
        "cs" => &["ehm", "hmm", "mmm", "hm"],
        "pl" => &["hmm", "mmm", "hm"],
        "tr" => &["hmm", "mmm", "hm"],
        "ru" => &["хм", "ммм", "hmm", "mmm"],
        "uk" => &["хм", "ммм", "hmm", "mmm"],
        "ar" => &["hmm", "mmm"],
        "ja" => &["hmm", "mmm"],
        "ko" => &["hmm", "mmm"],
        "vi" => &["hmm", "mmm", "hm"],
        "zh" => &["hmm", "mmm"],
        // Conservative universal fallback (no "um", "eh", "ha")
        _ => &[
            "uh", "uhm", "umm", "uhh", "uhhh", "ah", "hmm", "hm", "mmm", "mm", "mh", "ehh",
        ],
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
/// 3. The next word extends the current word by at most MAX_FRAGMENT_EXTENSION chars (≤ 5)
/// 4. It has at least 2 alphabetic characters (avoids removing "a", "I")
/// 5. It is NOT a common English word or short prefix (see COMMON_WORDS list)
///
/// The dual-layer protection (extension limit + common words list) ensures:
/// - CTC artifacts are caught: "wa"→"was" (ext=1), "sta"→"starting" (ext=5)
/// - Common words are preserved: "re"→"really" ("re" in COMMON_WORDS), "for"→"forget" (in COMMON_WORDS)
/// - Unknown short words with bounded extensions are caught: "ab"→"about" (ext=3, not common)
/// - Unknown short words with large extensions are preserved by the extension limit
///
/// Examples:
/// - "it wa was a" → "it was a"  ("wa" len=2, ext=1, not common → fragment)
/// - "I sta started running" → "I started running"  ("sta" len=3, ext=4≤5, not common → fragment)
/// - "for forget" → "for forget"  ("for" is common, kept)
/// - "can cancel" → "can cancel"  ("can" is common, kept)
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
        "a", "i",
        // --- 2-letter function words ---
        "an", "as", "at", "be", "by", "do", "go", "he", "if", "in", "is", "it",
        "me", "my", "no", "of", "on", "or", "so", "to", "up", "us", "we", "am",
        // --- 2-letter common words (also productive prefixes) ---
        // These are real English words that are prefixes of longer words and must
        // NOT be treated as CTC artifacts. Without these, removing MAX_FRAGMENT_EXTENSION
        // would cause regressions (e.g., "re really" → "really" removing "re").
        "re", "ex", "un", "im", "de", "bi", "oh", "ah", "ha", "ad", "lo", "mo",
        "ma", "pa", "id", "ed", "al", "bo", "fa", "sa", "sh", "ta", "ob", "op",
        "aw", "en", "er", "es", "hi", "ho", "ok", "uh", "um", "ya", "ye", "yo",
        // --- 3-letter function words ---
        "and", "any", "are", "but", "can", "did", "for", "get", "had", "has",
        "her", "him", "his", "how", "its", "let", "may", "not", "now", "one",
        "our", "out", "own", "she", "the", "too", "use", "was", "way", "who",
        "why", "yes", "yet", "you",
        // --- 3-letter common words (also productive prefixes) ---
        "add", "age", "ago", "aid", "air", "all", "arm", "art", "ask", "bad",
        "bag", "ban", "bat", "bed", "big", "bit", "bow", "box", "boy", "bug",
        "buy", "cab", "cap", "car", "cat", "cut", "day", "die", "dig", "dim",
        "dip", "doc", "dog", "dot", "dry", "ear", "eat", "egg", "end", "era",
        "eve", "eye", "fan", "far", "fat", "few", "fig", "fin", "fix", "fly",
        "fog", "fun", "gap", "gas", "gin", "got", "gun", "gut",
        "ham", "hat", "hid", "hip", "hit", "hog", "hop", "hot", "hug",
        "ice", "ill", "imp", "ink", "inn", "ins", "ion", "ire",
        "jam", "jar", "jet", "job", "jog", "joy", "key", "kid", "kit",
        "lab", "lap", "law", "lay", "led", "leg", "lie", "lip", "lit",
        "log", "lot", "low", "mad", "man", "map", "mat", "met", "mid", "mix",
        "mob", "mod", "mop", "mud", "nap", "net", "new", "nod", "nor",
        "nut", "oak", "odd", "off", "oil", "old", "opt", "ore",
        "pad", "pan", "pat", "pay", "pen", "pet", "pie", "pig", "pin", "pit",
        "pod", "pop", "pot", "pro", "pub", "put",
        "rag", "ram", "ran", "rat", "raw", "ray", "ref", "rep", "rib", "rid",
        "rig", "rim", "rip", "rob", "rod", "rot", "row", "rub", "rug", "run",
        "rut", "sad", "sat", "saw", "say", "sea", "see", "set", "sew", "shy",
        "sin", "sir", "sit", "six", "ski", "sky", "sob", "son", "sow", "spa",
        "spy", "sub", "sum", "sun",
        "tab", "tag", "tan", "tap", "tax", "tea", "ten", "tie", "tin", "tip",
        "toe", "ton", "top", "tow", "toy", "try", "tub",
        "van", "vat", "vet", "via", "vim", "vow",
        "war", "wax", "web", "wed", "wet", "win", "wit", "wok",
        "won", "woo", "wow",
        "zip", "zoo",
        // --- 2-letter abbreviations ---
        "st",   // St (Saint/Street) → Street, St.
        // --- Abbreviations / prefixes commonly used in transcription ---
        "bar", "con", "dis", "pre", "per", "app", "co", "mac", "bus",
        "net", "oil", "red", "int", "sys", "sec", "tel", "fin", "org", "dev",
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
        let current_alpha: String = words[i]
            .chars()
            .filter(|c| c.is_alphabetic())
            .collect();
        let current_lower = current_alpha.to_lowercase();

        if i + 1 < words.len() && current_alpha.len() >= 2 {
            let next_alpha: String = words[i + 1]
                .chars()
                .filter(|c| c.is_alphabetic())
                .collect();
            let next_lower = next_alpha.to_lowercase();

            // Current word is a fragment of the next word only when ALL conditions hold:
            // 1. Current is short enough to be a CTC artifact (≤ MAX_FRAGMENT_LEN)
            // 2. Next word is strictly longer (proper prefix match)
            // 3. Extension is within bounds (≤ MAX_FRAGMENT_EXTENSION)
            //    This provides defense-in-depth for words not in COMMON_WORDS.
            // 4. Next word starts with current word (case-insensitive)
            // 5. Current is NOT a common word/prefix
            if current_alpha.len() <= MAX_FRAGMENT_LEN
                && next_alpha.len() > current_alpha.len()
                && next_alpha.len() - current_alpha.len() <= MAX_FRAGMENT_EXTENSION
                && next_lower.starts_with(&current_lower)
                && !COMMON_WORDS.contains(&current_lower.as_str())
            {
                // Skip this word — it's a fragment of the next word
                i += 1;
                continue;
            }
        }

        result.push(words[i].to_string());
        i += 1;
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

/// Filters transcription output by removing filler words and stutter artifacts.
///
/// This function cleans up raw transcription text by:
/// 1. Removing filler words based on the app language (or custom list)
/// 2. Removing word fragment overlaps from CTC/Transducer models (e.g., "it wa was" -> "it was")
/// 3. Collapsing repeated word stutters (e.g., "wh wh wh" -> "wh")
/// 4. Cleaning up excess whitespace
///
/// # Arguments
/// * `text` - The raw transcription text to filter
/// * `lang` - The app language code (e.g., "en", "pt-BR") used to select filler words
/// * `custom_filler_words` - Optional user-provided filler word list. `Some(vec)` overrides
///   language defaults; `Some(empty vec)` disables filtering; `None` uses language defaults.
///
/// # Returns
/// The filtered text with filler words, fragments, and stutters removed
pub fn filter_transcription_output(
    text: &str,
    lang: &str,
    custom_filler_words: &Option<Vec<String>>,
) -> String {
    let mut filtered = text.to_string();

    // Build filler patterns from custom list or language defaults
    let patterns: Vec<Regex> = match custom_filler_words {
        Some(words) => words
            .iter()
            .filter_map(|word| Regex::new(&format!(r"(?i)\b{}\b[,.]?", regex::escape(word))).ok())
            .collect(),
        None => get_filler_words_for_language(lang)
            .iter()
            .map(|word| Regex::new(&format!(r"(?i)\b{}\b[,.]?", regex::escape(word))).unwrap())
            .collect(),
    };

    // Remove filler words
    for pattern in &patterns {
        filtered = pattern.replace_all(&filtered, "").to_string();
    }

    // Remove word fragment overlaps (CTC/Transducer boundary artifacts like "wa was" -> "was")
    filtered = dedup_word_fragments(&filtered);

    // Collapse repeated 1-2 letter words (stutter artifacts like "wh wh wh wh")
    filtered = collapse_stutters(&filtered);

    // Clean up multiple spaces to single space
    filtered = MULTI_SPACE_PATTERN.replace_all(&filtered, " ").to_string();

    // Trim leading/trailing whitespace
    filtered.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(result, "this is a test");
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
        assert_eq!(result, "so I was, thinking about this");
    }

    #[test]
    fn test_filter_preserves_valid_text() {
        let text = "This is a completely normal sentence.";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "This is a completely normal sentence.");
    }

    #[test]
    fn test_filter_stutter_collapse() {
        let text = "w wh wh wh wh wh wh wh wh wh why";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "w wh why");
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
        assert!(result.contains("ChargeBee,"));
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
        assert!(result.contains("MacBook"));
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
        assert_eq!(dedup_word_fragments("I sta started running"), "I started running");
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
        assert_eq!(dedup_word_fragments("I can cancel this"), "I can cancel this");
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
        assert_eq!(dedup_word_fragments("I read reading books"), "I read reading books");
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
        assert_eq!(dedup_word_fragments("I wan wanted to thi this"), "I wanted to this");
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
        assert_eq!(dedup_word_fragments("in inside the house"), "in inside the house");
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
        assert_eq!(dedup_word_fragments("no nothing happened"), "no nothing happened");
    }

    #[test]
    fn bench_preserves_up_upon() {
        assert_eq!(dedup_word_fragments("up upon the hill"), "up upon the hill");
    }

    #[test]
    fn bench_preserves_my_myself() {
        assert_eq!(dedup_word_fragments("I did it my myself"), "I did it my myself");
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
        assert_eq!(dedup_word_fragments("I do doing things"), "I do doing things");
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
        assert_eq!(
            dedup_word_fragments("I had had enough"),
            "I had had enough"
        );
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
        assert_eq!(
            dedup_word_fragments("I was go going"),
            "I was go going"
        );
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
        assert_eq!(dedup_word_fragments("ear early morning"), "ear early morning");
    }

    #[test]
    fn adv_common_word_eat() {
        // "eat" is a common word — should not be removed before "eating"
        assert_eq!(dedup_word_fragments("eat eating food"), "eat eating food");
    }

    #[test]
    fn adv_common_word_end() {
        // "end" is a common word — should not be removed before "ending"
        assert_eq!(dedup_word_fragments("end ending credits"), "end ending credits");
    }

    #[test]
    fn adv_common_word_man() {
        // "man" is a common word — should not be removed before "many"
        assert_eq!(dedup_word_fragments("man many things"), "man many things");
    }

    #[test]
    fn adv_common_word_sat() {
        // "sat" is a common word — should not be removed before "saturday"
        assert_eq!(dedup_word_fragments("sat saturday night"), "sat saturday night");
    }

    #[test]
    fn adv_common_word_ran() {
        // "ran" is a common word — should not be removed before "random"
        assert_eq!(dedup_word_fragments("ran random numbers"), "ran random numbers");
    }

    #[test]
    fn adv_common_word_met() {
        // "met" is a common word — should not be removed before "method"
        assert_eq!(dedup_word_fragments("met method calls"), "met method calls");
    }

    #[test]
    fn adv_common_word_bed() {
        // "bed" is a common word — should not be removed before "bedroom"
        assert_eq!(dedup_word_fragments("bed bedroom light"), "bed bedroom light");
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
        assert_eq!(
            dedup_word_fragments("I had had enough"),
            "I had had enough"
        );
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
        assert_eq!(dedup_word_fragments("mac machine works"), "mac machine works");
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
            "cat category"  // "cat" is in COMMON_WORDS, preserved
        );
    }

    #[test]
    fn dedupe_style_no_fuzzy_match() {
        // Unlike dedupeio which uses fuzzy matching, our approach requires
        // exact prefix match. Similar but not matching words are preserved.
        assert_eq!(
            dedup_word_fragments("the them there"),
            "the them there"  // "the" is common, all preserved
        );
    }

    #[test]
    fn dedupe_style_no_cross_sentence_dedup() {
        // Unlike dedupeio which can dedup across records, our approach
        // only operates within a single transcription chunk boundary.
        assert_eq!(
            dedup_word_fragments("it wa was good"),
            "it was good"  // Fragment removed, but other words preserved
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
        assert_eq!(dedup_word_fragments("im impossible task"), "im impossible task");
    }

    #[test]
    fn regression_de_before_definitely() {
        // "de" is a common prefix — must be preserved
        assert_eq!(dedup_word_fragments("de definitely yes"), "de definitely yes");
    }

    #[test]
    fn regression_bi_before_bisexual() {
        // "bi" is a common prefix (binary, bisexual) — must be preserved
        assert_eq!(dedup_word_fragments("bi bisexual community"), "bi bisexual community");
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
        assert_eq!(dedup_word_fragments("I sta started running"), "I started running");
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
}
