# Transcription Accuracy Experiments Guide

This guide explains how to run transcription accuracy experiments using the tagged recordings data.

## Overview

The Handy app serves as a **data tagging tool**. Users tag recordings with:

- **Ground Truth** - What they actually said (corrected transcription)
- **Quality** - Recording quality rating (good/okay/bad)
- **Speech Speed** - How fast they spoke (fast/normal/slow)

As an agentic AI, you can query this tagged data and run experiments to measure transcription accuracy across different models and parameters.

## Database Schema

### Main Table: `transcription_history`

```sql
CREATE TABLE transcription_history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    file_name TEXT NOT NULL,
    timestamp INTEGER NOT NULL,
    saved BOOLEAN NOT NULL DEFAULT 0,
    title TEXT NOT NULL,
    transcription_text TEXT NOT NULL,
    post_processed_text TEXT,
    post_process_prompt TEXT,
    post_process_requested BOOLEAN NOT NULL DEFAULT 0,
    model_id TEXT,
    routed BOOLEAN NOT NULL DEFAULT 0,
    routing_result TEXT,
    tags TEXT,
    ground_truth TEXT,           -- What user actually said
    quality TEXT,                 -- "good", "okay", or "bad"
    speech_speed TEXT             -- "fast", "normal", or "slow"
);
```

### Experiment Tables (For Programmatic Use)

These tables exist for storing experiment results:

```sql
CREATE TABLE experiment_groups (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    recording_id INTEGER NOT NULL,
    original_transcript TEXT NOT NULL,
    ground_truth TEXT,
    speech_speed TEXT DEFAULT 'normal',
    recording_quality TEXT DEFAULT 'good',
    created_at INTEGER NOT NULL,
    is_complete BOOLEAN NOT NULL DEFAULT 0,
    notes TEXT,
    FOREIGN KEY (recording_id) REFERENCES transcription_history(id)
);

CREATE TABLE transcription_variants (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    experiment_group_id INTEGER NOT NULL,
    model_id TEXT NOT NULL,
    parameters TEXT NOT NULL,
    transcription_text TEXT NOT NULL,
    match_score REAL,
    ranking INTEGER,
    is_acceptable BOOLEAN NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    notes TEXT,
    FOREIGN KEY (experiment_group_id) REFERENCES experiment_groups(id)
);
```

## Querying Tagged Recordings

### Get All Tagged Recordings

```sql
SELECT
    id,
    file_name,
    transcription_text,
    ground_truth,
    quality,
    speech_speed,
    model_id
FROM transcription_history
WHERE saved = 1
  AND ground_truth IS NOT NULL
ORDER BY timestamp DESC;
```

### Filter by Quality

```sql
-- Only good quality recordings
SELECT * FROM transcription_history
WHERE saved = 1
  AND ground_truth IS NOT NULL
  AND quality = 'good';

-- Exclude bad quality
SELECT * FROM transcription_history
WHERE saved = 1
  AND ground_truth IS NOT NULL
  AND quality IN ('good', 'okay');
```

### Filter by Speech Speed

```sql
-- Fast speech only
SELECT * FROM transcription_history
WHERE saved = 1
  AND ground_truth IS NOT NULL
  AND speech_speed = 'fast';

-- Compare across speeds
SELECT speech_speed, COUNT(*)
FROM transcription_history
WHERE saved = 1 AND ground_truth IS NOT NULL
GROUP BY speech_speed;
```

### Get Dataset Summary

```sql
SELECT
    quality,
    speech_speed,
    COUNT(*) as count
FROM transcription_history
WHERE saved = 1 AND ground_truth IS NOT NULL
GROUP BY quality, speech_speed
ORDER BY quality, speech_speed;
```

## Types of Experiments

### 1. Model Comparison

Compare transcription accuracy across different models:

- **Whisper Small vs Medium vs Large vs Turbo**
- **Parakeet TDT models**
- **Moonshine models**
- **Any combination of available models**

**Goal**: Find which model performs best on your recordings.

### 2. Speed vs Accuracy

Measure transcription time and accuracy:

```rust
let start = Instant::now();
let result = transcription_manager.transcribe_for_benchmark(audio)?;
let duration = start.elapsed();
```

**Metrics to track**:

- Transcription time (ms)
- Match score (%)
- Words per second

### 3. Quality Stratification

Test if certain models perform better on specific quality levels:

```
Good Quality Recordings:
  - Model A: 95% accuracy
  - Model B: 92% accuracy

Bad Quality Recordings:
  - Model A: 75% accuracy
  - Model B: 82% accuracy  <- Better on bad recordings!
```

### 4. Speech Speed Analysis

Test if models perform differently at different speeds:

```
Fast Speech: Model A (94%), Model B (88%)
Normal Speech: Model A (96%), Model B (95%)
Slow Speech: Model A (97%), Model B (96%)
```

### 5. Parameter Tuning

Test different model parameters:

- **Whisper**: `single_segment`, `use_greedy`, language settings
- **Parakeet**: confidence thresholds
- **Temperature** settings for language models

### 6. Hybrid Mode Evaluation

Test the hybrid mode (switches models based on audio duration):

```
Short audio (<5s): Fast model
Long audio (>5s): Accurate model

Compare to: Always use accurate model
```

## Running Experiments Programmatically

### Step 1: Load Tagged Recordings

```rust
// Query database
let conn = history_manager.get_connection()?;
let mut stmt = conn.prepare(
    "SELECT id, file_name, ground_truth, quality, speech_speed
     FROM transcription_history
     WHERE saved = 1 AND ground_truth IS NOT NULL"
)?;

let recordings: Vec<Recording> = stmt.query_map([], |row| {
    Ok(Recording {
        id: row.get(0)?,
        file_name: row.get(1)?,
        ground_truth: row.get(2)?,
        quality: row.get(3)?,
        speech_speed: row.get(4)?,
    })
})?.collect::<Result<Vec<_>, _>>()?;
```

### Step 2: Load Audio

```rust
use crate::audio_toolkit::read_wav_samples;

for recording in &recordings {
    let audio_path = history_manager.get_audio_file_path(&recording.file_name);
    let samples = read_wav_samples(&audio_path)?;

    // Run experiments on samples
}
```

### Step 3: Transcribe with Different Models

```rust
let models = vec!["turbo", "medium", "small", "parakeet-tdt-0.6b-v3"];

for model_id in &models {
    transcription_manager.load_model(model_id)?;

    for recording in &recordings {
        let audio = read_wav_samples(&recording.audio_path)?;
        let result = transcription_manager.transcribe_for_benchmark(audio)?;

        // Calculate match score
        let score = calculate_match_score(&result.text, &recording.ground_truth);

        // Store in experiment_variants table
        store_variant(experiment_id, model_id, result.text, score)?;
    }
}
```

### Step 4: Calculate Match Score

```rust
fn calculate_match_score(transcription: &str, ground_truth: &str) -> f32 {
    let a = transcription.to_lowercase();
    let b = ground_truth.to_lowercase();

    if a == b {
        return 100.0;
    }

    let words_a: Vec<&str> = a.split_whitespace().collect();
    let words_b: Vec<&str> = b.split_whitespace().collect();

    let common = words_a.iter()
        .filter(|w| words_b.contains(w))
        .count();

    let total = words_a.len().max(words_b.len());

    (common as f32 / total as f32 * 100.0).round()
}
```

### Step 5: Store Results

```rust
// Insert into experiment_groups
conn.execute(
    "INSERT INTO experiment_groups (recording_id, ground_truth, speech_speed, recording_quality, created_at)
     VALUES (?1, ?2, ?3, ?4, ?5)",
    params![recording.id, recording.ground_truth, recording.speech_speed, recording.quality, now],
)?;

let group_id = conn.last_insert_rowid();

// Insert into transcription_variants
conn.execute(
    "INSERT INTO transcription_variants (experiment_group_id, model_id, parameters, transcription_text, match_score, created_at)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    params![group_id, model_id, "{}", result.text, score, now],
)?;
```

## Exporting Results

### Export to JSON

```rust
let results: Vec<ExperimentResult> = conn.query_map(
    "SELECT
        eg.id as experiment_id,
        th.file_name,
        eg.ground_truth,
        eg.recording_quality,
        eg.speech_speed,
        tv.model_id,
        tv.transcription_text,
        tv.match_score
     FROM experiment_groups eg
     JOIN transcription_history th ON eg.recording_id = th.id
     JOIN transcription_variants tv ON tv.experiment_group_id = eg.id
     ORDER BY eg.id, tv.match_score DESC",
    [],
    |row| Ok(ExperimentResult {
        experiment_id: row.get(0)?,
        file_name: row.get(1)?,
        ground_truth: row.get(2)?,
        quality: row.get(3)?,
        speed: row.get(4)?,
        model_id: row.get(5)?,
        transcription: row.get(6)?,
        match_score: row.get(7)?,
    })
)?.collect::<Result<Vec<_>, _>>()?;

let json = serde_json::to_string_pretty(&results)?;
std::fs::write("experiment_results.json", json)?;
```

### Export to CSV

```rust
let mut csv = String::from("experiment_id,file_name,ground_truth,quality,speed,model,transcription,match_score\n");

for result in results {
    csv.push_str(&format!(
        "{},{},{},{},{},{},{},{}\n",
        result.experiment_id,
        result.file_name,
        result.ground_truth,
        result.quality,
        result.speed,
        result.model_id,
        result.transcription.replace("\"", "\"\""),
        result.match_score
    ));
}

std::fs::write("experiment_results.csv", csv)?;
```

## Analysis Ideas

### Calculate Model Accuracy

```sql
SELECT
    tv.model_id,
    COUNT(*) as recordings_tested,
    AVG(tv.match_score) as avg_accuracy,
    MIN(tv.match_score) as min_accuracy,
    MAX(tv.match_score) as max_accuracy
FROM transcription_variants tv
JOIN experiment_groups eg ON tv.experiment_group_id = eg.id
GROUP BY tv.model_id
ORDER BY avg_accuracy DESC;
```

### Accuracy by Quality Level

```sql
SELECT
    eg.recording_quality,
    tv.model_id,
    AVG(tv.match_score) as avg_accuracy
FROM transcription_variants tv
JOIN experiment_groups eg ON tv.experiment_group_id = eg.id
GROUP BY eg.recording_quality, tv.model_id
ORDER BY eg.recording_quality, avg_accuracy DESC;
```

### Find Worst Transcriptions

```sql
SELECT
    th.file_name,
    eg.ground_truth,
    tv.model_id,
    tv.transcription_text,
    tv.match_score
FROM transcription_variants tv
JOIN experiment_groups eg ON tv.experiment_group_id = eg.id
JOIN transcription_history th ON eg.recording_id = th.id
WHERE tv.match_score < 70
ORDER BY tv.match_score ASC;
```

## Best Practices

### 1. Tag Consistently

- Always set ground truth for saved recordings
- Use consistent quality ratings
- Tag speech speed accurately

### 2. Run Multiple Models

- Test at least 3-5 models per recording
- Include both fast and accurate models
- Test different model sizes

### 3. Stratify by Conditions

- Test on all quality levels (good/okay/bad)
- Test on all speeds (fast/normal/slow)
- Get enough samples per condition

### 4. Measure Everything

- Match score (accuracy)
- Transcription time (speed)
- Model size (memory)
- Device characteristics

### 5. Validate Ground Truth

- Ground truth should be exact
- Check for typos
- Ensure same language/dialect

## Example Experiment Session

When the user asks you to run experiments:

1. **Count available data**

   ```sql
   SELECT COUNT(*) FROM transcription_history
   WHERE saved = 1 AND ground_truth IS NOT NULL;
   ```

2. **Select models to test**
   - Use `commands.getAvailableModels()` to see what's downloaded
   - Include a mix: small/medium/large, different engines

3. **Run experiments**
   - For each model:
     - Load model
     - For each recording:
       - Load audio
       - Transcribe
       - Calculate match score
       - Store result

4. **Generate report**
   - Average accuracy per model
   - Accuracy by quality level
   - Accuracy by speech speed
   - Speed comparison (time)

5. **Export data**
   - JSON for programmatic use
   - CSV for analysis
   - Summary report

## Commands Available

From the Rust backend:

- `getAvailableModels()` - List downloaded models
- `load_model(model_id)` - Load a specific model
- `transcribe_for_benchmark(audio)` - Transcribe with default params
- `getHistoryEntries(cursor, limit)` - Get paginated history
- `updateHistoryEntryMetadata(id, ground_truth, quality, speech_speed)` - Tag recording

From the database:

- Query `transcription_history` for tagged data
- Insert into `experiment_groups` and `transcription_variants` for results
- Query experiment results for analysis

## Notes

- **Experiment tables** are for programmatic use only
- **Users never see them** in the UI
- **You (the AI)** create/populate them when running experiments
- The app is a **data tagging tool**, you handle the experiments
