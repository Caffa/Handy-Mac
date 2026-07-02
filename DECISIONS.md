# Architecture Decisions

## 2026-06-25: Spelling Dictionary Source for US-to-British Conversion

### Status

**Accepted**

### Context

The Handy app provides US-to-British English spelling conversion for speech-to-text transcriptions. The previous implementation used the `varcon` crate (v1.0.5), which is based on the VARCON project (v2020.12.07).

### Problem

The varcon-based solution had several issues:

1. **Archaic spellings**: Varcon contains outdated British spellings like "waggon" (modern British uses "wagon") and "instal" (incorrect - both variants use "install")

2. **Semantic ambiguity errors**: Varcon suggests converting words like "check" → "cheque", "tire" → "tyre", "program" → "programme" without considering context. In British English:
   - "check" (verify) and "cheque" (payment) are different words
   - "tire" (weary) and "tyre" (wheel) are different words
   - "program" (software) and "programme" (TV show) are different words

3. **Manual maintenance burden**: Required constant manual curation of an exclusion list to remove problematic entries

4. **Stale upstream**: Last updated in 2020, no recent maintenance

### Decision

Replace varcon with two swappable dictionary sources:

1. **DWYL Dictionary** (default) - Curated list from [dwyl/english-words](https://github.com/dwyl/english-words)
   - ~180 common spelling pairs
   - Human-curated, less noise
   - Well-maintained (12k+ stars)
   - Excludes archaic spellings by design

2. **CSpell Dictionary** - Extracted from [CSpell dictionaries](https://github.com/streetsidesoftware/cspell-dicts)
   - More comprehensive coverage
   - Actively maintained (1.5M+ weekly downloads)
   - SCOWL-based filtering excludes archaic words
   - Community-maintained upstream

### Exclusions Applied to Both Dictionaries

Both dictionaries explicitly exclude:

| US Word | UK Word   | Reason                                   |
| ------- | --------- | ---------------------------------------- |
| check   | cheque    | Semantic ambiguity (verify vs payment)   |
| tire    | tyre      | Semantic ambiguity (weary vs wheel)      |
| program | programme | Semantic ambiguity (software vs TV show) |
| catalog | catalogue | Computing context prefers "catalog"      |
| dialog  | dialogue  | Computing context prefers "dialog"       |
| wagon   | waggon    | Archaic - modern British uses "wagon"    |
| install | instal    | Incorrect - both variants use "install"  |

### Implementation

- Created `src/audio_toolkit/spelling_dictionaries.rs` module
- Defined `SpellingDictionary` enum with `Dwyl` and `Cspell` variants
- Both dictionaries use static `HashMap` for O(1) lookup
- Added `spelling_dictionary` setting to `AppSettings`
- Removed varcon dependency from Cargo.toml

### Consequences

**Positive:**

- No more manual exclusion list maintenance
- Archaic spellings excluded by upstream filtering
- User can switch between dictionaries via settings
- Smaller codebase (removed ~100 lines of exclusion logic)

**Negative:**

- Less comprehensive than varcon's 10,000+ clusters
- Need to update dictionaries periodically from upstream

**Neutral:**

- Performance unchanged (both use HashMap lookup)
- Same conversion logic for plurals/past tense/-ing forms

### Alternatives Considered

1. **nspell + wooorm/dictionaries**: Would require runtime dictionary comparison, slower startup
2. **Custom curated list**: Maintenance burden moved to us
3. **Keep varcon with more exclusions**: Ongoing maintenance, doesn't solve staleness

### References

- Research document: `/research/us-british-spelling-conversion-solutions.md`
- DWYL source: https://github.com/dwyl/english-words/blob/master/uk-us-dict.txt
- CSpell dicts: https://github.com/streetsidesoftware/cspell-dicts
