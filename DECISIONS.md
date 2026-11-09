# Architecture Decisions

## 2026-11-09: USB watchdog smart-ignores Bluetooth devices by name heuristic

### Status

**Accepted**

### Context

Bluetooth headsets (AirPods etc.) that expose an input device flap on connect/disconnect. The USB watchdog treated those flaps and any subsequent dead-looking recording exactly like USB dock flaps: it armed the one-shot wake bypass and attempted uhubctl power cycles that could never succeed ("device not found in USB tree") because Bluetooth devices do not appear in the USB tree.

### Decision

Classify devices by name only, using the existing `is_bluetooth_device_name` heuristic (substring/startswith match on AirPods/Beats/WH-/WF-/JBL/Bose/Jabra/Marshall/Galaxy Buds/FreeBuds/Bluetooth — the same logic already used for the BT output keep-alive). A POSITIVE match on (a) the configured watchdog device name or (b) the attempted/selected input mic name skips all watchdog accounting, bypass arming, wake probing, and power cycling. Empty, "Default", or unrecognized names keep the previous path exactly.

Guard-before-probe invariant: every skip happens as the first statement of the entry point, before any counter write, event emission, or uhubctl invocation — so a BT device costs zero subprocesses and zero events.

### Reconsider when

- CoreAudio transport-type detection becomes reliably available from Rust (kAudioDevicePropertyTransport) — replace the name heuristic with the authoritative transport classification and delete the substring list.
- The heuristic misfires in practice (e.g. a USB device whose marketing name contains "Bose"/"JBL", or a BT mic named something not in the list) — either extend the list or wire in the transport probe for the ambiguous case only.
- A user legitimately wants wake/flap cycling for a device whose name the heuristic considers Bluetooth (they would need a setting; deliberately not added now to keep scope minimal).

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
