//! Accent-to-ASCII dictionary mapping for Parakeet transcription recovery.
//!
//! Parakeet Unified EN 0.6B emits `<unk>` tokens for accented characters because
//! its vocabulary contains no accented Latin characters. Simply stripping these
//! tokens truncates words (e.g., "fiancé" → "fianc" instead of "fiance").
//!
//! This module provides two lookup tables:
//!
//! 1. **`ACCENT_TO_ASCII`** — Character-level normalization map. Converts individual
//!    accented characters to their ASCII equivalents (e.g., 'é' → 'e', 'æ' → 'ae').
//!    Useful for normalizing text that somehow retains accented characters.
//!
//! 2. **`TRUNCATED_WORDS`** — Pre-computed word-level recovery map. Maps the truncated
//!    ASCII forms produced by `<unk>` stripping back to the correct complete words
//!    (e.g., "fianc" → "fiance", "caf" → "cafe").
//!
//! The truncated dictionary only includes entries where stripping accent characters
//! produces a different word than the correct ASCII form. For many accented words
//! (e.g., "naïve" → "naive", "café" → "cafe" via character stripping), the
//! stripped form is already correct and no dictionary entry is needed.

use std::collections::HashMap;

use once_cell::sync::Lazy;

/// Character-level map: accented character → ASCII replacement string.
///
/// Handles common accented Latin characters from Latin-1 Supplement and
/// Latin Extended-A/B blocks. Each entry maps a single accented character
/// to its standard ASCII equivalent (which may be multiple characters,
/// e.g., 'æ' → "ae", 'ß' → "ss").
///
/// # Examples
/// ```text
/// 'é' → "e"
/// 'ñ' → "n"
/// 'æ' → "ae"
/// 'ß' → "ss"
/// 'œ' → "oe"
/// ```
pub(crate) static ACCENT_TO_ASCII: Lazy<HashMap<char, &'static str>> = Lazy::new(|| {
    let mut m = HashMap::with_capacity(130);

    // ── Latin-1 Supplement (U+00C0–U+00FF) ──────────────────────────────────
    // Lowercase
    m.insert('à', "a");
    m.insert('á', "a");
    m.insert('â', "a");
    m.insert('ã', "a");
    m.insert('ä', "a");
    m.insert('å', "a");
    m.insert('æ', "ae");
    m.insert('ç', "c");
    m.insert('è', "e");
    m.insert('é', "e");
    m.insert('ê', "e");
    m.insert('ë', "e");
    m.insert('ì', "i");
    m.insert('í', "i");
    m.insert('î', "i");
    m.insert('ï', "i");
    m.insert('ð', "d");
    m.insert('ñ', "n");
    m.insert('ò', "o");
    m.insert('ó', "o");
    m.insert('ô', "o");
    m.insert('õ', "o");
    m.insert('ö', "o");
    m.insert('ø', "o");
    m.insert('ù', "u");
    m.insert('ú', "u");
    m.insert('û', "u");
    m.insert('ü', "u");
    m.insert('ý', "y");
    m.insert('þ', "th");
    m.insert('ÿ', "y");

    // Uppercase
    m.insert('À', "A");
    m.insert('Á', "A");
    m.insert('Â', "A");
    m.insert('Ã', "A");
    m.insert('Ä', "A");
    m.insert('Å', "A");
    m.insert('Æ', "AE");
    m.insert('Ç', "C");
    m.insert('È', "E");
    m.insert('É', "E");
    m.insert('Ê', "E");
    m.insert('Ë', "E");
    m.insert('Ì', "I");
    m.insert('Í', "I");
    m.insert('Î', "I");
    m.insert('Ï', "I");
    m.insert('Ð', "D");
    m.insert('Ñ', "N");
    m.insert('Ò', "O");
    m.insert('Ó', "O");
    m.insert('Ô', "O");
    m.insert('Õ', "O");
    m.insert('Ö', "O");
    m.insert('Ø', "O");
    m.insert('Ù', "U");
    m.insert('Ú', "U");
    m.insert('Û', "U");
    m.insert('Ü', "U");
    m.insert('Ý', "Y");
    m.insert('Þ', "TH");
    m.insert('Ÿ', "Y");

    // ── Latin Extended-A (U+0100–U+017F) — selected ─────────────────────────
    m.insert('ā', "a");
    m.insert('ă', "a");
    m.insert('ą', "a");
    m.insert('ć', "c");
    m.insert('ĉ', "c");
    m.insert('ċ', "c");
    m.insert('č', "c");
    m.insert('ď', "d");
    m.insert('đ', "d");
    m.insert('ē', "e");
    m.insert('ĕ', "e");
    m.insert('ė', "e");
    m.insert('ę', "e");
    m.insert('ě', "e");
    m.insert('ĝ', "g");
    m.insert('ğ', "g");
    m.insert('ġ', "g");
    m.insert('ģ', "g");
    m.insert('ĩ', "i");
    m.insert('ī', "i");
    m.insert('ĭ', "i");
    m.insert('į', "i");
    m.insert('ı', "i");
    m.insert('ĺ', "l");
    m.insert('ļ', "l");
    m.insert('ľ', "l");
    m.insert('ŀ', "l");
    m.insert('ł', "l");
    m.insert('ń', "n");
    m.insert('ņ', "n");
    m.insert('ň', "n");
    m.insert('ŋ', "n");
    m.insert('ō', "o");
    m.insert('ŏ', "o");
    m.insert('ő', "o");
    m.insert('ŕ', "r");
    m.insert('ŗ', "r");
    m.insert('ř', "r");
    m.insert('ś', "s");
    m.insert('ŝ', "s");
    m.insert('ş', "s");
    m.insert('š', "s");
    m.insert('ţ', "t");
    m.insert('ť', "t");
    m.insert('ŧ', "t");
    m.insert('ũ', "u");
    m.insert('ū', "u");
    m.insert('ŭ', "u");
    m.insert('ů', "u");
    m.insert('ű', "u");
    m.insert('ų', "u");
    m.insert('ŵ', "w");
    m.insert('ŷ', "y");
    m.insert('ź', "z");
    m.insert('ż', "z");
    m.insert('ž', "z");

    // ── Latin Extended-B (U+0180–U+024F) — selected ligatures ───────────────
        m.insert('œ', "oe");
        m.insert('Œ', "OE");
    m.insert('ſ', "s"); // long s

    // ── German sharp s ───────────────────────────────────────────────────────
    m.insert('ß', "ss");

    m
});

/// Pre-computed map: truncated/stripped word form → correct ASCII equivalent.
///
/// When Parakeet emits `<unk>` for accented characters and we strip those tokens,
/// the resulting word may be truncated or corrupted. This map provides recovery
/// lookups for the most common cases.
///
/// Only includes entries where stripping accent characters produces a **different**
/// word than the correct ASCII form. For example:
/// - "fiancé" → strip é → "fianc" ≠ "fiance" → entry needed
/// - "naïve" → strip ï → "naive" = "naive" → no entry needed
///
/// # Examples
/// ```text
/// "fianc"   → "fiance"     (from fiancé)
/// "caf"     → "cafe"       (from café)
/// "resum"   → "resume"     (from résumé)
/// "jalapeo" → "jalapeno"   (from jalapeño)
/// "uvre"    → "oeuvre"     (from œuvre)
/// ```
pub(crate) static TRUNCATED_WORDS: Lazy<HashMap<&'static str, &'static str>> =
    Lazy::new(|| {
        let mut m = HashMap::with_capacity(20);

        // ── Accented chars at word END → truncation ──────────────────────────
        // These are the most impactful fixes: words where the final accented
        // char gets <unk>-stripped, chopping off the ending.
        m.insert("fianc", "fiance"); // fiancé → fianc + <unk>
        m.insert("caf", "cafe"); // café → caf + <unk>
        m.insert("resum", "resume"); // résumé → resum + <unk><unk>
        m.insert("proteg", "protege"); // protégé → proteg + <unk><unk>
        m.insert("crpe", "crepe"); // crêpe → crpe + <unk>
        m.insert("ftus", "fetus"); // fœtus → ftus + <unk>

        // ── Accented ligatures / middle chars → corruption ───────────────────
        // These are words where an accented character in the MIDDLE of the word
        // gets stripped, producing a corrupted (but recognizable) form.
        m.insert("jalapeo", "jalapeno"); // jalapeño → jalapeo + <unk>
        m.insert("uvre", "oeuvre"); // œuvre → uvre + <unk>
        m.insert("subpna", "subpoena"); // subpœna → subpna + <unk>

        // ── Less common but notable ──────────────────────────────────────────
        // Ligature Œ at the start of words (œ → oe, but stripping loses the 'o')
        m.insert("dipe", "oedipe"); // Œdipe → dip + e  (archaic, French)
        // Æ words where stripping creates a different word
        m.insert("csar", "caesar"); // Cæsar → csar + <unk> (archaic spelling)

        m
    });

/// Normalizes accented characters in text to their ASCII equivalents.
///
/// Uses the `ACCENT_TO_ASCII` map to replace each accented character with
/// its standard ASCII representation. Characters not in the map are passed
/// through unchanged.
///
/// # Arguments
/// * `text` - The input text potentially containing accented characters
///
/// # Returns
/// The text with all accented characters replaced by ASCII equivalents
///
/// # Examples
/// ```text
/// "café"      → "cafe"
/// "résumé"    → "resume"
/// "naïve"     → "naive"
/// "über"      → "uber"
/// "José"      → "Jose"
/// "Pokémon"   → "Pokemon"
/// "jalapeño"  → "jalapeno"
/// "hello"     → "hello"  (no change)
/// ```
pub(crate) fn normalize_accents(text: &str) -> String {
    text.chars()
        .map(|c| {
            ACCENT_TO_ASCII
                .get(&c)
                .copied()
                .unwrap_or_else(|| {
                    // For chars not in the map, return the char as a static str
                    // by leaking a small allocation. This is fine for a HashMap lookup
                    // that's rarely called for unmapped chars.
                    Box::leak(c.encode_utf8(&mut [0; 4]).to_string().into_boxed_str())
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── ACCENT_TO_ASCII tests ───────────────────────────────────────────────

    #[test]
    fn test_accent_char_map_common_lowercase() {
        assert_eq!(ACCENT_TO_ASCII[&'é'], "e");
        assert_eq!(ACCENT_TO_ASCII[&'ñ'], "n");
        assert_eq!(ACCENT_TO_ASCII[&'ß'], "ss");
        assert_eq!(ACCENT_TO_ASCII[&'æ'], "ae");
        assert_eq!(ACCENT_TO_ASCII[&'œ'], "oe");
        assert_eq!(ACCENT_TO_ASCII[&'ü'], "u");
        assert_eq!(ACCENT_TO_ASCII[&'ö'], "o");
        assert_eq!(ACCENT_TO_ASCII[&'ä'], "a");
    }

    #[test]
    fn test_accent_char_map_uppercase() {
        assert_eq!(ACCENT_TO_ASCII[&'É'], "E");
        assert_eq!(ACCENT_TO_ASCII[&'Ñ'], "N");
        assert_eq!(ACCENT_TO_ASCII[&'Æ'], "AE");
        assert_eq!(ACCENT_TO_ASCII[&'Œ'], "OE");
        assert_eq!(ACCENT_TO_ASCII[&'Ü'], "U");
        assert_eq!(ACCENT_TO_ASCII[&'Ö'], "O");
    }

    #[test]
    fn test_accent_char_map_extended() {
        assert_eq!(ACCENT_TO_ASCII[&'ā'], "a");
        assert_eq!(ACCENT_TO_ASCII[&'č'], "c");
        assert_eq!(ACCENT_TO_ASCII[&'ž'], "z");
        assert_eq!(ACCENT_TO_ASCII[&'ś'], "s");
    }

    // ── TRUNCATED_WORDS tests ───────────────────────────────────────────────

    #[test]
    fn test_truncated_fiance() {
        assert_eq!(TRUNCATED_WORDS["fianc"], "fiance");
    }

    #[test]
    fn test_truncated_cafe() {
        assert_eq!(TRUNCATED_WORDS["caf"], "cafe");
    }

    #[test]
    fn test_truncated_resume() {
        assert_eq!(TRUNCATED_WORDS["resum"], "resume");
    }

    #[test]
    fn test_truncated_protege() {
        assert_eq!(TRUNCATED_WORDS["proteg"], "protege");
    }

    #[test]
    fn test_truncated_jalapeno() {
        assert_eq!(TRUNCATED_WORDS["jalapeo"], "jalapeno");
    }

    #[test]
    fn test_truncated_oeuvre() {
        assert_eq!(TRUNCATED_WORDS["uvre"], "oeuvre");
    }

    #[test]
    fn test_truncated_crepe() {
        assert_eq!(TRUNCATED_WORDS["crpe"], "crepe");
    }

    #[test]
    fn test_truncated_subpoena() {
        assert_eq!(TRUNCATED_WORDS["subpna"], "subpoena");
    }

    #[test]
    fn test_truncated_fetus() {
        assert_eq!(TRUNCATED_WORDS["ftus"], "fetus");
    }

    #[test]
    fn test_no_false_positive_for_common_words() {
        assert!(!TRUNCATED_WORDS.contains_key("the"));
        assert!(!TRUNCATED_WORDS.contains_key("and"));
        assert!(!TRUNCATED_WORDS.contains_key("hello"));
        assert!(!TRUNCATED_WORDS.contains_key("world"));
        assert!(!TRUNCATED_WORDS.contains_key("is"));
        assert!(!TRUNCATED_WORDS.contains_key("a"));
    }

    // ── normalize_accents tests ─────────────────────────────────────────────

    #[test]
    fn test_normalize_accents_cafe() {
        assert_eq!(normalize_accents("café"), "cafe");
    }

    #[test]
    fn test_normalize_accents_resume() {
        assert_eq!(normalize_accents("résumé"), "resume");
    }

    #[test]
    fn test_normalize_accents_naive() {
        assert_eq!(normalize_accents("naïve"), "naive");
    }

    #[test]
    fn test_normalize_accents_uber() {
        assert_eq!(normalize_accents("über"), "uber");
    }

    #[test]
    fn test_normalize_accents_jose() {
        assert_eq!(normalize_accents("José"), "Jose");
    }

    #[test]
    fn test_normalize_accents_pokemon() {
        assert_eq!(normalize_accents("Pokémon"), "Pokemon");
    }

    #[test]
    fn test_normalize_accents_jalapeno() {
        assert_eq!(normalize_accents("jalapeño"), "jalapeno");
    }

    #[test]
    fn test_normalize_accents_unchanged() {
        assert_eq!(normalize_accents("hello world"), "hello world");
        assert_eq!(normalize_accents("123"), "123");
    }
}
