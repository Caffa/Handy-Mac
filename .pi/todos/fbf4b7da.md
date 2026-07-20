{
  "id": "fbf4b7da",
  "title": "Fix regressions from removing MAX_FRAGMENT_EXTENSION",
  "tags": [],
  "status": "completed",
  "created_at": "2026-04-26T07:55:54.883Z",
  "assigned_to_session": "019dc8a9-367f-761e-8eb5-8fc06ba7eee3"
}

Fixed regressions by restoring MAX_FRAGMENT_EXTENSION=5 (defense-in-depth) and adding 36 missing 2-letter words + 6 abbreviations to COMMON_WORDS. All 198 tests pass with 0 regressions.\n\nKey learning: Removing MAX_FRAGMENT_EXTENSION entirely was too aggressive. The dual-layer approach (COMMON_WORDS + MAX_FRAGMENT_EXTENSION) is more robust because it catches both known and unknown words:\n- COMMON_WORDS protects against false positives for known words (re, ex, un, mac, etc.)\n- MAX_FRAGMENT_EXTENSION limits the extension length for unknown words\n- Together they provide comprehensive protection
