# History Feature Logic Bug Report

**Review Date:** 2026-06-20  
**Files Reviewed:**
- `/Users/caffae/Local-Projects-2026/Handy-Fork/Handy-Mac/src/components/settings/history/HistorySettings.tsx`
- `/Users/caffae/Local-Projects-2026/Handy-Fork/Handy-Mac/src/components/ui/AudioPlayer.tsx`
- `/Users/caffae/Local-Projects-2026/Handy-Fork/Handy-Mac/src/stores/settingsStore.ts`
- `/Users/caffae/Local-Projects-2026/Handy-Fork/Handy-Mac/src-tauri/src/managers/history.rs`
- `/Users/caffae/Local-Projects-2026/Handy-Fork/Handy-Mac/src-tauri/src/commands/history.rs`

---

## Critical Bugs

### 1. Race Condition in Pagination Loading (CRITICAL)

**Location:** `HistorySettings.tsx`, lines 96-134

**Bug Description:**
The `loadPage` function uses `loadingRef` to prevent concurrent loads, but this ref is not synchronized with the React `loading` state. This creates a race condition where:
1. Multiple rapid scrolls can trigger simultaneous `loadPage` calls before the first completes
2. The `loading` state is set to `false` in the `finally` block even when another load is in progress via `loadingRef`
3. Cursor-based pagination can load duplicate entries if concurrent requests resolve out of order

**Current Code:**
```typescript
const loadPage = useCallback(async (cursor?: number) => {
  const isFirstPage = cursor === undefined;
  if (!isFirstPage && loadingRef.current) return; // Only checks ref
  loadingRef.current = true;
  
  if (isFirstPage) {
    setLoading(true); // State not checked
    setError(null);
  }
  // ...
}, []);
```

**Impact:** 
- Duplicate entries in the list
- Out-of-order entries
- Inconsistent UI state
- Potential infinite scroll issues

**Suggested Fix:**
```typescript
const loadPage = useCallback(async (cursor?: number) => {
  const isFirstPage = cursor === undefined;
  
  // Check both ref and state
  if (loadingRef.current || loading) return;
  
  loadingRef.current = true;
  
  if (isFirstPage) {
    setLoading(true);
    setError(null);
  }
  
  try {
    const result = await commands.getHistoryEntries(
      cursor ?? null,
      PAGE_SIZE,
    );
    if (result.status === "ok") {
      const { entries: newEntries, has_more } = result.data;
      setEntries((prev) => {
        // Deduplicate by ID to prevent race condition duplicates
        const existingIds = new Set(prev.map(e => e.id));
        const uniqueNewEntries = newEntries.filter(e => !existingIds.has(e.id));
        return isFirstPage ? newEntries : [...prev, ...uniqueNewEntries];
      });
      setHasMore(has_more);
    }
    // ...
  } finally {
    // Only clear loading if we're still on the same "request"
    if (isFirstPage) {
      setLoading(false);
    }
    loadingRef.current = false;
  }
}, [loading]); // Add loading dependency
```

---

### 2. Missing Event Handler for "deleted" Action (HIGH)

**Location:** `HistorySettings.tsx`, lines 175-192

**Bug Description:**
The event listener comment states: `"deleted" and "toggled" are handled by optimistic updates only, so we intentionally ignore them here to avoid double-mutation.`

However, the backend DOES emit `HistoryUpdatePayload::Deleted` events (line 915-917 in `history.rs`), and the `Toggled` event is also emitted but ignored. If the backend deletes an entry (e.g., via cleanup or another client in a multi-window scenario), the frontend won't reflect this change.

**Current Code:**
```typescript
useEffect(() => {
  const unlisten = events.historyUpdatePayload.listen((event) => {
    const payload: HistoryUpdatePayload = event.payload;
    if (payload.action === "added") {
      setEntries((prev) => [payload.entry, ...prev]);
    } else if (payload.action === "updated") {
      setEntries((prev) =>
        prev.map((e) => (e.id === payload.entry.id ? payload.entry : e)),
      );
    }
    // "deleted" and "toggled" are ignored!
  });
  // ...
}, []);
```

**Impact:**
- Entries deleted by backend cleanup remain visible until page refresh
- Inconsistent state between frontend and backend
- User may try to interact with already-deleted entries

**Suggested Fix:**
```typescript
useEffect(() => {
  const unlisten = events.historyUpdatePayload.listen((event) => {
    const payload: HistoryUpdatePayload = event.payload;
    if (payload.action === "added") {
      setEntries((prev) => [payload.entry, ...prev]);
    } else if (payload.action === "updated") {
      setEntries((prev) =>
        prev.map((e) => (e.id === payload.entry.id ? payload.entry : e)),
      );
    } else if (payload.action === "deleted") {
      // Handle backend-initiated deletions (cleanup, other windows)
      setEntries((prev) => prev.filter((e) => e.id !== payload.id));
    } else if (payload.action === "toggled") {
      // Refresh the specific entry when toggled by another source
      commands.getHistoryEntries(payload.id, 1).then(result => {
        if (result.status === "ok" && result.data.entries.length > 0) {
          setEntries((prev) =>
            prev.map((e) => (e.id === payload.id ? result.data.entries[0] : e)),
          );
        }
      });
    }
  });

  return () => {
    unlisten.then((fn) => fn());
  };
}, []);
```

---

### 3. Unprotected JSON.parse Without Try-Catch (HIGH)

**Location:** `HistorySettings.tsx`, lines 479-486

**Bug Description:**
The `routingResult` is parsed from JSON without any error handling. If the `routing_result` field contains malformed JSON (corrupted data, manual database edits, etc.), the entire component will crash.

**Current Code:**
```typescript
const routingResult = entry.routing_result
  ? (JSON.parse(entry.routing_result) as Array<{
      status: string;
      handler: string;
      classification: string;
      file_path: string | null;
    }>)
  : null;
```

**Impact:**
- White screen of death if malformed JSON exists
- Entire history list becomes unusable

**Suggested Fix:**
```typescript
const routingResult = useMemo(() => {
  if (!entry.routing_result) return null;
  try {
    return JSON.parse(entry.routing_result) as Array<{
      status: string;
      handler: string;
      classification: string;
      file_path: string | null;
    }>;
  } catch (e) {
    console.error("Failed to parse routing result:", e);
    return null;
  }
}, [entry.routing_result]);
```

---

## High Severity Bugs

### 4. Infinite Scroll Sentinel Active During Search (HIGH)

**Location:** `HistorySettings.tsx`, lines 151-172, 373

**Bug Description:**
When a user searches, the infinite scroll sentinel (`sentinelRef`) is hidden from view (line 373), but the IntersectionObserver is still active. If the search results are few and the sentinel becomes visible again, it triggers `loadPage` with the last entry's ID, loading entries that don't match the search query. This causes confusion as new entries appear that don't match the search.

**Current Code:**
```typescript
// Line 373 - Sentinel hidden when searching
{searchQuery.trim() === "" && <div ref={sentinelRef} className="h-1" />}

// Lines 151-172 - Observer always active when hasMore is true
useEffect(() => {
  if (loading) return;
  const sentinel = sentinelRef.current;
  if (!sentinel || !hasMore) return; // Doesn't check searchQuery
  
  const observer = new IntersectionObserver(
    (observerEntries) => {
      const first = observerEntries[0];
      if (first.isIntersecting) {
        const lastEntry = entriesRef.current[entriesRef.current.length - 1];
        if (lastEntry) {
          loadPage(lastEntry.id); // Loads unfiltered entries!
        }
      }
    },
    { threshold: 0 },
  );
  // ...
}, [loading, hasMore, loadPage]);
```

**Impact:**
- Search results polluted with non-matching entries
- Confusing UX where user sees entries not matching their search

**Suggested Fix:**
```typescript
useEffect(() => {
  if (loading) return;
  if (searchQuery.trim() !== "") return; // Skip if searching
  
  const sentinel = sentinelRef.current;
  if (!sentinel || !hasMore) return;
  // ... rest of observer setup
}, [loading, hasMore, loadPage, searchQuery]); // Add searchQuery dependency
```

---

### 5. Race Condition in Delete Operation (HIGH)

**Location:** `HistorySettings.tsx`, lines 245-258

**Bug Description:**
The `deleteAudioEntry` function performs an optimistic update (removing the entry from state) before the API call. However, if the delete fails:
1. It calls `loadPage()` to reload
2. But `loadPage()` clears all entries and reloads from the beginning
3. This loses the user's scroll position and any entries that were loaded via infinite scroll
4. If called during pagination loading, it can cause inconsistent state

**Current Code:**
```typescript
const deleteAudioEntry = async (id: number) => {
  // Optimistically remove
  setEntries((prev) => prev.filter((e) => e.id !== id));
  try {
    const result = await commands.deleteHistoryEntry(id);
    if (result.status !== "ok") {
      // Reload on failure - this is destructive!
      loadPage();
    }
  } catch (error) {
    console.error("Failed to delete entry:", error);
    loadPage(); // Destructive reload
  }
};
```

**Impact:**
- Loss of user's scroll position
- Unnecessary full reload of all entries
- Poor UX on slow connections

**Suggested Fix:**
```typescript
const deleteAudioEntry = async (id: number) => {
  // Store entry for potential restoration
  const entryToDelete = entries.find((e) => e.id === id);
  
  // Optimistically remove
  setEntries((prev) => prev.filter((e) => e.id !== id));
  
  try {
    const result = await commands.deleteHistoryEntry(id);
    if (result.status !== "ok") {
      // Restore entry on failure instead of full reload
      if (entryToDelete) {
        setEntries((prev) => [...prev, entryToDelete].sort((a, b) => b.id - a.id));
      }
      toast.error(t("settings.history.deleteFailed"));
    }
  } catch (error) {
    console.error("Failed to delete entry:", error);
    // Restore entry on error
    if (entryToDelete) {
      setEntries((prev) => [...prev, entryToDelete].sort((a, b) => b.id - a.id));
    }
    toast.error(t("settings.history.deleteError"));
  }
};
```

---

### 6. Missing Error Handling in retryHistoryEntry (HIGH)

**Location:** `HistorySettings.tsx`, lines 260-265

**Bug Description:**
The `retryHistoryEntry` function throws an error on failure but doesn't handle the case where the history entry might be deleted between the initial fetch and the retry. Additionally, there's no loading state coordination between the component and the child `HistoryEntryComponent`.

**Current Code:**
```typescript
const retryHistoryEntry = async (id: number) => {
  const result = await commands.retryHistoryEntryTranscription(id);
  if (result.status !== "ok") {
    throw new Error(String(result.error));
  }
};
```

**Impact:**
- Uncaught errors can crash the app
- No user feedback on retry failure in parent component
- Potential race condition with concurrent retries

**Suggested Fix:**
```typescript
const retryHistoryEntry = async (id: number) => {
  try {
    const result = await commands.retryHistoryEntryTranscription(id);
    if (result.status !== "ok") {
      const errorMsg = String(result.error || "Unknown error");
      console.error("Retry failed:", errorMsg);
      toast.error(t("settings.history.retryFailed", { error: errorMsg }));
      throw new Error(errorMsg);
    }
    toast.success(t("settings.history.retrySuccess"));
  } catch (error) {
    if (error instanceof Error) {
      throw error;
    }
    throw new Error(String(error));
  }
};
```

---

### 7. Blob URL Memory Leak in AudioPlayer (HIGH)

**Location:** `AudioPlayer.tsx`, lines 156-162

**Bug Description:**
The blob URL cleanup only runs when the component unmounts or when `loadedSrc` changes. However, if `getAudioUrl` returns a blob URL for Linux (line 231 in HistorySettings.tsx), and the AudioPlayer is unmounted before the audio loads, the blob URL may not be cleaned up properly.

**Current Code:**
```typescript
useEffect(() => {
  return () => {
    if (loadedSrc?.startsWith("blob:")) {
      URL.revokeObjectURL(loadedSrc);
    }
  };
}, [loadedSrc]);
```

**Impact:**
- Memory leak on Linux when users navigate away before audio loads
- Accumulation of blob URLs in memory

**Suggested Fix:**
```typescript
// Track all created blob URLs for cleanup
const blobUrlsRef = useRef<Set<string>>(new Set());

useEffect(() => {
  return () => {
    // Cleanup all tracked blob URLs
    blobUrlsRef.current.forEach(url => {
      URL.revokeObjectURL(url);
    });
    blobUrlsRef.current.clear();
  };
}, []);

// When setting loadedSrc
const setLoadedSrcWithTracking = useCallback((url: string | null) => {
  if (url?.startsWith("blob:")) {
    blobUrlsRef.current.add(url);
  }
  setLoadedSrc(url);
}, []);
```

---

## Medium Severity Bugs

### 8. Ground Truth Edit State Not Reset on Entry Change (MEDIUM)

**Location:** `HistorySettings.tsx`, lines 435-436

**Bug Description:**
The `editingGroundTruth` and `groundTruth` state are initialized from the entry prop but never reset when a different entry is rendered (React reuses component instances). This means if a user:
1. Opens entry A and edits ground truth
2. Deletes entry A
3. A different entry B appears at the same position

Entry B will show the edit mode from entry A with entry A's data.

**Current Code:**
```typescript
const [editingGroundTruth, setEditingGroundTruth] = useState(false);
const [groundTruth, setGroundTruth] = useState(entry.ground_truth || entry.transcription_text);
```

**Impact:**
- Edit mode persists across different entries
- Potential data corruption if user saves wrong value

**Suggested Fix:**
```typescript
// Reset state when entry ID changes
useEffect(() => {
  setEditingGroundTruth(false);
  setGroundTruth(entry.ground_truth || entry.transcription_text);
}, [entry.id, entry.ground_truth, entry.transcription_text]);
```

---

### 9. Metadata Update Doesn't Handle Tags (MEDIUM)

**Location:** `HistorySettings.tsx`, lines 279-315

**Bug Description:**
The `updateMetadata` function updates `ground_truth`, `quality`, and `speech_speed`, but there's a separate `updateHistoryEntryTags` command that isn't integrated. If a user wants to add tags while updating metadata, they need two separate API calls, which can cause race conditions.

Additionally, the backend `update_metadata` doesn't update all fields atomically - it makes separate UPDATE calls for each field that is present.

**Current Code:**
```typescript
const updateMetadata = async (
  id: number,
  ground_truth?: string,
  quality?: string,
  speech_speed?: string,
) => {
  // Optimistic update
  setEntries((prev) =>
    prev.map((e) => {
      if (e.id !== id) return e;
      return {
        ...e,
        ...(ground_truth !== undefined && { ground_truth }),
        ...(quality !== undefined && { quality }),
        ...(speech_speed !== undefined && { speech_speed }),
      };
    }),
  );
  // ...
};
```

**Impact:**
- Inconsistent state if metadata and tags updated simultaneously
- Multiple network requests instead of one

**Suggested Fix:**
Add tags support to the metadata update or provide a combined function:
```typescript
const updateMetadata = async (
  id: number,
  ground_truth?: string,
  quality?: string,
  speech_speed?: string,
  tags?: string[] | null,
) => {
  // Optimistic update including tags
  setEntries((prev) =>
    prev.map((e) => {
      if (e.id !== id) return e;
      return {
        ...e,
        ...(ground_truth !== undefined && { ground_truth }),
        ...(quality !== undefined && { quality }),
        ...(speech_speed !== undefined && { speech_speed }),
        ...(tags !== undefined && { tags: tags ? JSON.stringify(tags) : null }),
      };
    }),
  );
  
  try {
    // Use Promise.all for concurrent updates
    const promises: Promise<unknown>[] = [
      commands.updateHistoryEntryMetadata(
        id,
        ground_truth ?? null,
        quality ?? null,
        speech_speed ?? null,
      )
    ];
    
    if (tags !== undefined) {
      promises.push(
        commands.updateHistoryEntryTags(id, tags ? JSON.stringify(tags) : null)
      );
    }
    
    const results = await Promise.all(promises);
    const hasError = results.some(r => r.status !== "ok");
    
    if (hasError) {
      loadPage(); // Reload on partial failure
    }
  } catch (e) {
    console.error("Failed to update metadata:", e);
    loadPage();
  }
};
```

---

### 10. Stale Closure in Animation Loop (MEDIUM)

**Location:** `AudioPlayer.tsx`, lines 44-54

**Bug Description:**
The `tick` callback has an empty dependency array, meaning it closes over the initial values of `isDraggingRef` and `isPlayingRef`. While refs are used to avoid stale closures, the `tick` function itself is recreated on every render but only the first version is used in `requestAnimationFrame`.

**Current Code:**
```typescript
const tick = useCallback(() => {
  if (audioRef.current && !isDraggingRef.current) {
    const time = audioRef.current.currentTime;
    setCurrentTime(time);
  }

  if (isPlayingRef.current) {
    animationRef.current = requestAnimationFrame(tick);
  }
}, []); // Empty dependency array
```

**Impact:**
- Potential memory leak if component unmounts during animation
- Callback identity issues with React DevTools

**Suggested Fix:**
```typescript
const tick = useCallback(() => {
  if (audioRef.current && !isDraggingRef.current) {
    const time = audioRef.current.currentTime;
    setCurrentTime(time);
  }

  if (isPlayingRef.current) {
    animationRef.current = requestAnimationFrame(tick);
  }
}, []); // Keep empty, but add proper cleanup

// In the useEffect that manages the loop, ensure cleanup:
useEffect(() => {
  if (isPlaying && !isDragging) {
    if (!animationRef.current) {
      animationRef.current = requestAnimationFrame(tick);
    }
  } else {
    if (animationRef.current) {
      cancelAnimationFrame(animationRef.current);
      animationRef.current = undefined;
    }
  }
  
  return () => {
    if (animationRef.current) {
      cancelAnimationFrame(animationRef.current);
      animationRef.current = undefined;
    }
  };
}, [isPlaying, isDragging, tick]);
```

---

## Low Severity Bugs

### 11. Missing Validation for history_limit (LOW)

**Location:** `settingsStore.ts`, line 149

**Bug Description:**
The `history_limit` setting is passed directly to the backend without any validation. Negative values or extremely large values could cause issues.

**Current Code:**
```typescript
history_limit: (value) => commands.updateHistoryLimit(value as number),
```

**Suggested Fix:**
```typescript
history_limit: (value) => {
  const limit = Math.max(0, Math.min(10000, Number(value) || 100));
  return commands.updateHistoryLimit(limit);
},
```

---

### 12. Unused "toggled" Event Payload (LOW)

**Location:** `history.rs`, lines 116-118, 736-738

**Bug Description:**
The backend emits `Toggled` events with just the ID, but the frontend ignores it. This means the frontend can't easily update its state when another window/instance toggles the saved status. The current comment says it's intentional to avoid double-mutation, but this creates inconsistency in multi-window scenarios.

**Current Backend Code:**
```rust
if let Err(e) = (HistoryUpdatePayload::Toggled { id }).emit(&self.app_handle) {
    error!("Failed to emit history-updated event: {}", e);
}
```

**Impact:**
- Minor inconsistency in multi-window scenarios
- Wasted event emissions

**Suggested Fix:**
Either remove the event emission from backend or handle it in frontend:
```rust
// Backend: Remove unused event emission
// Frontend already handles optimistic updates for the same window
// For multi-window sync, use a full "updated" event instead
```

---

### 13. Transaction Boundary Issues in Backend (LOW)

**Location:** `history.rs`, multiple locations

**Bug Description:**
Several database operations in `history.rs` don't use transactions when updating multiple fields:
- `update_metadata` (lines 768-812) - separate UPDATE statements
- `update_experiment_group` (lines 1004-1050) - separate UPDATE statements
- `update_variant` (lines 1154-1193) - separate UPDATE statements

If one UPDATE fails, the database can be left in an inconsistent state.

**Suggested Fix:**
Wrap multi-field updates in transactions:
```rust
pub async fn update_metadata(
    &self,
    id: i64,
    ground_truth: Option<String>,
    quality: Option<String>,
    speech_speed: Option<String>,
) -> Result<()> {
    let mut conn = self.get_connection()?;
    let tx = conn.transaction()?;
    
    if let Some(gt) = &ground_truth {
        tx.execute(
            "UPDATE transcription_history SET ground_truth = ?1 WHERE id = ?2",
            params![gt, id],
        )?;
    }
    
    if let Some(q) = &quality {
        tx.execute(
            "UPDATE transcription_history SET quality = ?1 WHERE id = ?2",
            params![q, id],
        )?;
    }
    
    if let Some(ss) = &speech_speed {
        tx.execute(
            "UPDATE transcription_history SET speech_speed = ?1 WHERE id = ?2",
            params![ss, id],
        )?;
    }
    
    tx.commit()?;
    // ... emit event
}
```

---

### 14. Cleanup Query Performance (LOW)

**Location:** `history.rs`, lines 525-551

**Bug Description:**
The `cleanup_by_count` method loads ALL unsaved entries into memory before deciding which to delete. For large databases, this could be memory-intensive.

**Current Code:**
```rust
let mut entries: Vec<(i64, String)> = Vec::new();
for row in rows {
    entries.push(row?);
}

if entries.len() > limit {
    let entries_to_delete = &entries[limit..];
    let deleted_count = self.delete_entries_and_files(entries_to_delete)?;
}
```

**Suggested Fix:**
Use a single DELETE query with OFFSET:
```rust
fn cleanup_by_count(&self, limit: usize) -> Result<()> {
    let conn = self.get_connection()?;
    
    // Get IDs to delete using a subquery
    let mut stmt = conn.prepare(
        "SELECT id, file_name FROM transcription_history 
         WHERE saved = 0 
         ORDER BY timestamp DESC 
         LIMIT -1 OFFSET ?1"
    )?;
    
    let entries_to_delete: Vec<(i64, String)> = stmt
        .query_map(params![limit as i64], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    
    if !entries_to_delete.is_empty() {
        let deleted_count = self.delete_entries_and_files(&entries_to_delete)?;
        debug!("Cleaned up {} old history entries by count.", deleted_count);
    }
    
    Ok(())
}
```

---

## Edge Cases

### 15. Empty History Handling (Edge Case)

**Location:** `HistorySettings.tsx`, lines 341-346

The current empty state check only looks at `entries.length`, but after filtering with search, the user could see "No results" instead of "Empty history". This is handled correctly (lines 347-352), but there's no distinction between "no history" and "no matching results".

**Current Behavior:**
- Shows "empty" message when no entries exist
- Shows "noResults" message when search returns nothing

This is actually correct behavior, but worth noting that the messages might need different UI treatments.

---

### 16. Concurrent Updates (Edge Case)

**Location:** `HistorySettings.tsx`, lines 194-214, 279-315

Multiple concurrent metadata updates can cause race conditions. If a user rapidly clicks quality buttons:
1. Click "good" → optimistic update → API call starts
2. Click "okay" → optimistic update → API call starts
3. First API call completes, triggers reload
4. Second API call completes but reload already happened

This can lead to inconsistent UI state.

**Suggested Fix:**
Add debouncing or queue management for updates:
```typescript
const pendingUpdatesRef = useRef<Set<number>>(new Set());

const updateMetadata = async (...) => {
  if (pendingUpdatesRef.current.has(id)) return;
  pendingUpdatesRef.current.add(id);
  
  try {
    // ... update logic
  } finally {
    pendingUpdatesRef.current.delete(id);
  }
};
```

---

## Summary

| Severity | Count | Description |
|----------|-------|-------------|
| Critical | 1 | Race condition in pagination loading |
| High | 6 | Missing event handlers, unprotected JSON.parse, scroll issues, delete race conditions |
| Medium | 3 | State management issues, stale closures |
| Low | 4 | Validation, unused code, performance |

**Priority Fixes (in order):**
1. Fix pagination race condition (CRITICAL)
2. Add JSON.parse error handling (HIGH)
3. Handle "deleted" events (HIGH)
4. Fix sentinel behavior during search (HIGH)
5. Fix delete operation race condition (HIGH)
6. Add retry error handling (HIGH)
7. Fix blob URL memory leak (HIGH)

**Estimated Effort:** 2-3 days for all fixes with testing.
