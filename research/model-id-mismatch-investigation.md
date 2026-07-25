# Model ID Pipeline Investigation

**Date:** 2026-07-23
**Branch:** new-models
**Question:** Is there an ID mismatch between what the frontend sends when selecting a model and what the Rust backend expects?

## Conclusion: NO MISMATCH

The IDs are **consistent at every step** of the pipeline. There is no place where the frontend strips, transforms, or re-encodes the model ID before sending it back to the backend. The full compound ID flows cleanly through the entire chain.

---

## Detailed ID Trace (using Parakeet Unified EN as example)

### Step 1: Catalog Entry in `catalog.json`

```json
{
  "id": "handy-computer/parakeet-unified-en-0.6b-gguf",
  "default_quant": "Q8_0",
  "files": [
    { "filename": "parakeet-unified-en-0.6b-Q4_K_M.gguf", ... },
    { "filename": "parakeet-unified-en-0.6b-Q8_0.gguf", ... }
  ]
}
```

- `CatalogModel.id` = `"handy-computer/parakeet-unified-en-0.6b-gguf"` (this is the HF repo ID, NOT the final model ID)

### Step 2: Descriptor Construction (`catalog/mod.rs`, line 72)

```rust
let default_filename = default_quant_file(&m.files, m.default_quant.as_deref())
    .map(|f| f.filename.clone())
    .unwrap_or_default();

ModelDescriptor {
    id: format!("{}/{}", m.id, default_filename),
    // ...
}
```

- `ModelDescriptor.id` = `"handy-computer/parakeet-unified-en-0.6b-gguf/parakeet-unified-en-0.6b-Q8_0.gguf"`

### Step 3: Insertion into HashMap (`managers/model.rs`, line 1184-1193)

```rust
fn seed_catalog_models(available_models: &mut HashMap<String, ModelInfo>) {
    for desc in crate::catalog::CATALOG.iter() {
        if let Entry::Vacant(slot) = available_models.entry(desc.id.clone()) {
            slot.insert(desc.to_model_info(&DiskStatus::default()));
        }
    }
}
```

- HashMap key = `"handy-computer/parakeet-unified-en-0.6b-gguf/parakeet-unified-en-0.6b-Q8_0.gguf"`
- `ModelInfo.id` = same (set in `to_model_info`, line 236: `id: self.id.clone()`)

### Step 4: Frontend receives models (`managers/model.rs`, line 1156-1174)

```rust
pub fn get_available_models(&self) -> Vec<ModelInfo> {
    let models = self.available_models.lock().unwrap();
    models.values().cloned().collect()
    // sorted by rank, recommended, accuracy, speed, name
}
```

- Returns all `ModelInfo` structs. Each has `.id` = compound ID.

### Step 5: Frontend receives and stores (`stores/modelStore.ts`, line 87-92)

```ts
const result = await commands.getAvailableModels();
if (result.status === "ok") {
    set({ models: result.data, error: null });
}
```

- `models` array contains `ModelInfo[]` with `.id` = compound ID.

### Step 6: User clicks model in dropdown (`components/model-selector/ModelDropdown.tsx`, line 34)

```tsx
onClick={() => handleModelClick(model.id)}
```

- Passes `model.id` (the compound ID from the ModelInfo list).

### Step 7: ModelSelector.tsx forwards (`components/model-selector/ModelSelector.tsx`, line 152-156)

```ts
const handleModelSelect = async (modelId: string) => {
    setPendingModelId(modelId);
    const success = await selectModel(modelId);
};
```

### Step 8: Store calls Tauri command (`stores/modelStore.ts`, line 153-156)

```ts
selectModel: async (modelId: string) => {
    const result = await commands.setActiveModel(modelId);
    if (result.status === "ok") {
        set({ currentModel: modelId, ... });
    }
};
```

- Sends `modelId` (compound ID) to `setActiveModel` Tauri command.

### Step 9: Tauri command (`commands/models.rs`, line 185-192)

```rust
pub async fn set_active_model(
    app_handle: AppHandle,
    model_id: String,
) -> Result<(), String> {
    switch_active_model(&app_handle, &model_id)
}
```

### Step 10: Switch logic validates (`commands/models.rs`, line 115-117)

```rust
let model_info = model_manager
    .get_model_info(model_id)
    .ok_or_else(|| format!("Model not found: {}", model_id))?;
```

### Step 11: HashMap lookup (`managers/model.rs`, line 1260-1263)

```rust
pub fn get_model_info(&self, model_id: &str) -> Option<ModelInfo> {
    let models = self.available_models.lock().unwrap();
    models.get(model_id).cloned()
}
```

- HashMap key = same compound ID. **Match confirmed.**

---

## What happens with non-existent IDs?

**It does NOT silently fail.** The error propagates back to the frontend:

1. `switch_active_model` returns `Err("Model not found: {id}")` (commands/models.rs, line 117)
2. The Tauri command returns this as `Err(String)` 
3. `setActiveModel` returns `{ status: "error", error: "..." }` to frontend
4. `selectModel` in modelStore.ts catches it (line 164-166):
   ```ts
   set({ error: `Failed to switch to model: ${result.error}` });
   return false;
   ```
5. `handleModelSelect` in ModelSelector.tsx sees `success === false` (line 157-161):
   ```ts
   setPendingModelId(null);
   setModelStatus("error");
   setModelError("Failed to switch model");
   ```

The user sees an error state. No silent failure.

---

## `model-state-changed` Events

### Rust emits these events (`managers/transcription.rs`):

```rust
pub struct ModelStateEvent {
    pub event_type: String,    // "loading_started", "loading_completed", "loading_failed", "unloaded", "selection_changed"
    pub model_id: Option<String>,
    pub model_name: Option<String>,
    pub error: Option<String>,
}
```

**Event types emitted:**

| Event Type | When | `model_id` | `model_name` |
|---|---|---|---|
| `loading_started` | Model begins loading | `Some(model_id)` | `None` |
| `loading_completed` | Model loaded successfully | `Some(model_id)` | `Some(name)` |
| `loading_failed` | Model failed to load | `Some(model_id)` | `Some(name)` |
| `unloaded` | Model manually unloaded | `None` | `None` |
| `selection_changed` | Model selected (Immediate unload mode) | `Some(model_id)` | `Some(name)` |

### Frontend listeners:

**`ModelSelector.tsx` (line 73-98)** — Updates local `modelStatus` state:
```tsx
listen<ModelStateEvent>("model-state-changed", (event) => {
    const { event_type, error } = event.payload;
    switch (event_type) {
        case "loading_started": setModelStatus("loading"); break;
        case "loading_completed": setModelStatus("ready"); break;
        case "loading_failed": setModelStatus("error"); break;
        case "unloaded": setModelStatus("unloaded"); break;
        // NOTE: "selection_changed" is NOT handled here
    }
});
```

**`modelStore.ts` (line 460-465)** — Refreshes entire model list:
```ts
listen("model-state-changed", () => {
    get().loadModels();      // re-fetches all ModelInfo from backend
    get().loadCurrentModel(); // re-fetches selected_model from settings
});
```

### Notable: `selection_changed` event

When the unload timeout is set to "Immediately" (`commands/models.rs`, line 153-169), `switch_active_model` skips loading and emits `selection_changed` instead. The `modelStore.ts` listener picks this up (refreshes models + current model), but `ModelSelector.tsx` does NOT handle `selection_changed` in its switch statement. This is fine because `modelStore.ts` re-fetches `currentModel` which updates the displayed model name.

---

## Two ID Schemas Coexist Safely

The system has two ID formats:

1. **Legacy hardcoded models** (model.rs `new()`): Simple slug IDs
   - `"small"`, `"medium"`, `"turbo"`, `"large"`, `"parakeet-tdt-0.6b-v2"`, etc.
   - Used as both HashMap key AND `ModelInfo.id`

2. **Catalog models** (catalog/mod.rs): Compound IDs
   - `"handy-computer/parakeet-unified-en-0.6b-gguf/parakeet-unified-en-0.6b-Q8_0.gguf"`
   - Used as both HashMap key AND `ModelInfo.id`

Both are inserted into the same `available_models: HashMap<String, ModelInfo>`. The `seed_catalog_models` function uses `Entry::Vacant` so catalog entries don't overwrite legacy ones. The frontend doesn't care about the format — it just uses whatever `.id` the `ModelInfo` carries.

---

## Key Files Referenced

| File | Lines | Role |
|---|---|---|
| `src-tauri/src/catalog/mod.rs` | 62-98 | Constructs descriptor ID as `{repo_id}/{filename}` |
| `src-tauri/src/catalog/catalog.json` | — | Source of catalog model data |
| `src-tauri/src/managers/model.rs` | 222-261 | `ModelDescriptor::to_model_info()` copies `id` to `ModelInfo` |
| `src-tauri/src/managers/model.rs` | 1156-1174 | `get_available_models()` returns all `ModelInfo` from HashMap |
| `src-tauri/src/managers/model.rs` | 1184-1193 | `seed_catalog_models()` inserts catalog entries into HashMap |
| `src-tauri/src/managers/model.rs` | 1260-1263 | `get_model_info()` does HashMap lookup by model_id |
| `src-tauri/src/commands/models.rs` | 96-181 | `switch_active_model()` validates + loads model |
| `src-tauri/src/commands/models.rs` | 185-192 | `set_active_model` Tauri command entry point |
| `src-tauri/src/managers/transcription.rs` | 60-66 | `ModelStateEvent` struct definition |
| `src/stores/modelStore.ts` | 87-92, 153-172 | Frontend model loading and selection |
| `src/components/model-selector/ModelDropdown.tsx` | 34 | Passes `model.id` to `onModelSelect` |
| `src/components/model-selector/ModelSelector.tsx` | 50-98, 152-163 | Model status tracking and selection |
