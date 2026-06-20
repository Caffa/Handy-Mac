use anyhow::{anyhow, Result};
use chrono::{DateTime, Local, Utc};
use log::{debug, error, info, warn};
use rusqlite::{params, Connection, OptionalExtension};
use rusqlite_migration::{Migrations, M};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::fs;
use std::path::PathBuf;
use tauri::AppHandle;
use tauri_specta::Event;

/// Database migrations for transcription history.
/// Each migration is applied in order. The library tracks which migrations
/// have been applied using SQLite's user_version pragma.
///
/// Note: For users upgrading from tauri-plugin-sql, migrate_from_tauri_plugin_sql()
/// converts the old _sqlx_migrations table tracking to the user_version pragma,
/// ensuring migrations don't re-run on existing databases.
static MIGRATIONS: &[M] = &[
    M::up(
        "CREATE TABLE IF NOT EXISTS transcription_history (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            file_name TEXT NOT NULL,
            timestamp INTEGER NOT NULL,
            saved BOOLEAN NOT NULL DEFAULT 0,
            title TEXT NOT NULL,
            transcription_text TEXT NOT NULL
        );",
    ),
    M::up("ALTER TABLE transcription_history ADD COLUMN post_processed_text TEXT;"),
    M::up("ALTER TABLE transcription_history ADD COLUMN post_process_prompt TEXT;"),
    M::up("ALTER TABLE transcription_history ADD COLUMN post_process_requested BOOLEAN NOT NULL DEFAULT 0;"),
    M::up("ALTER TABLE transcription_history ADD COLUMN model_id TEXT;"),
    M::up("ALTER TABLE transcription_history ADD COLUMN routed BOOLEAN NOT NULL DEFAULT 0;"),
    M::up("ALTER TABLE transcription_history ADD COLUMN routing_result TEXT;"),
    M::up("ALTER TABLE transcription_history ADD COLUMN tags TEXT;"),
    // Quality and metadata for data tagging
    M::up("ALTER TABLE transcription_history ADD COLUMN ground_truth TEXT;"),
    M::up("ALTER TABLE transcription_history ADD COLUMN quality TEXT;"),
    M::up("ALTER TABLE transcription_history ADD COLUMN speech_speed TEXT;"),
    // Experiment system: track transcription accuracy tests (for programmatic use)
    M::up(
        "CREATE TABLE IF NOT EXISTS experiment_groups (
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
        );",
    ),
    M::up(
        "CREATE TABLE IF NOT EXISTS transcription_variants (
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
        );",
    ),
];

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct ExperimentGroup {
    pub id: i64,
    pub recording_id: i64,
    pub original_transcript: String,
    pub ground_truth: Option<String>,
    pub speech_speed: String,
    pub recording_quality: String,
    pub created_at: i64,
    pub is_complete: bool,
    pub notes: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct TranscriptionVariant {
    pub id: i64,
    pub experiment_group_id: i64,
    pub model_id: String,
    pub parameters: String,
    pub transcription_text: String,
    pub match_score: Option<f32>,
    pub ranking: Option<i32>,
    pub is_acceptable: bool,
    pub created_at: i64,
    pub notes: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct PaginatedHistory {
    pub entries: Vec<HistoryEntry>,
    pub has_more: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type, tauri_specta::Event)]
#[serde(tag = "action")]
pub enum HistoryUpdatePayload {
    #[serde(rename = "added")]
    Added { entry: HistoryEntry },
    #[serde(rename = "updated")]
    Updated { entry: HistoryEntry },
    #[serde(rename = "deleted")]
    Deleted { id: i64 },
    #[serde(rename = "toggled")]
    Toggled { id: i64 },
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct HistoryEntry {
    pub id: i64,
    pub file_name: String,
    pub timestamp: i64,
    pub saved: bool,
    pub title: String,
    pub transcription_text: String,
    pub post_processed_text: Option<String>,
    pub post_process_prompt: Option<String>,
    pub post_process_requested: bool,
    pub model_id: Option<String>,
    pub routed: bool,
    /// JSON string of routing handler results, e.g.
    /// `[{"status":"✅","handler":"Daily","classification":"diary_entry","file_path":null}]`.
    /// Set after the boss_router subprocess completes.
    pub routing_result: Option<String>,
    /// JSON array of tags for categorizing recordings, e.g. `["fast", "slow", "test"]`.
    /// Used for research and experimentation purposes.
    pub tags: Option<String>,
    /// Ground truth text - what the user actually said (corrected transcription).
    /// Used for transcription accuracy experiments.
    pub ground_truth: Option<String>,
    /// Quality rating: "good", "okay", "bad".
    /// User's assessment of recording quality.
    pub quality: Option<String>,
    /// Speech speed: "fast", "normal", "slow".
    /// User's assessment of speech speed.
    pub speech_speed: Option<String>,
}

pub struct HistoryManager {
    app_handle: AppHandle,
    recordings_dir: PathBuf,
    db_path: PathBuf,
}

impl HistoryManager {
    pub fn new(app_handle: &AppHandle) -> Result<Self> {
        // Create recordings directory in app data dir
        let app_data_dir = crate::portable::app_data_dir(app_handle)?;
        let recordings_dir = app_data_dir.join("recordings");
        let db_path = app_data_dir.join("history.db");

        // Ensure recordings directory exists
        if !recordings_dir.exists() {
            fs::create_dir_all(&recordings_dir)?;
            debug!("Created recordings directory: {:?}", recordings_dir);
        }

        let manager = Self {
            app_handle: app_handle.clone(),
            recordings_dir,
            db_path,
        };

        // Initialize database and run migrations synchronously
        manager.init_database()?;

        Ok(manager)
    }

    fn init_database(&self) -> Result<()> {
        info!("Initializing database at {:?}", self.db_path);

        let mut conn = Connection::open(&self.db_path)?;

        // Handle migration from tauri-plugin-sql to rusqlite_migration
        // tauri-plugin-sql used _sqlx_migrations table, rusqlite_migration uses user_version pragma
        self.migrate_from_tauri_plugin_sql(&conn)?;

        // Create migrations object and run to latest version
        let migrations = Migrations::new(MIGRATIONS.to_vec());

        // Validate migrations in debug builds
        #[cfg(debug_assertions)]
        migrations.validate().expect("Invalid migrations");

        // Get current version before migration
        let version_before: i32 =
            conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        debug!("Database version before migration: {}", version_before);

        // Apply any pending migrations
        migrations.to_latest(&mut conn)?;

        // Get version after migration
        let version_after: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;

        if version_after > version_before {
            info!(
                "Database migrated from version {} to {}",
                version_before, version_after
            );
        } else {
            debug!("Database already at latest version {}", version_after);
        }

        // Defensive schema verification: ensure all expected columns exist
        // This handles edge cases where user_version is correct but columns are missing
        // (e.g., from failed migrations that were manually patched)
        self.verify_and_fix_schema(&conn)?;

        Ok(())
    }

    /// Migrate from tauri-plugin-sql's migration tracking to rusqlite_migration's.
    /// tauri-plugin-sql used a _sqlx_migrations table, while rusqlite_migration uses
    /// SQLite's user_version pragma. This function checks if the old system was in use
    /// and sets the user_version accordingly so migrations don't re-run.
    fn migrate_from_tauri_plugin_sql(&self, conn: &Connection) -> Result<()> {
        // Check if the old _sqlx_migrations table exists
        let has_sqlx_migrations: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='_sqlx_migrations'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(false);

        if !has_sqlx_migrations {
            return Ok(());
        }

        // Check current user_version
        let current_version: i32 =
            conn.pragma_query_value(None, "user_version", |row| row.get(0))?;

        if current_version > 0 {
            // Already migrated to rusqlite_migration system
            return Ok(());
        }

        // Get the highest version from the old migrations table
        let old_version: i32 = conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM _sqlx_migrations WHERE success = 1",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        if old_version > 0 {
            info!(
                "Migrating from tauri-plugin-sql (version {}) to rusqlite_migration",
                old_version
            );

            // Set user_version to match the old migration state
            conn.pragma_update(None, "user_version", old_version)?;

            // Optionally drop the old migrations table (keeping it doesn't hurt)
            // conn.execute("DROP TABLE IF EXISTS _sqlx_migrations", [])?;

            info!(
                "Migration tracking converted: user_version set to {}",
                old_version
            );
        }

        Ok(())
    }

    /// Verify database schema integrity and fix missing columns.
    /// This handles edge cases where user_version is correct but columns are missing
    /// (e.g., from failed migrations or manual database edits).
    fn verify_and_fix_schema(&self, conn: &Connection) -> Result<()> {
        // Expected columns for transcription_history (migrations 1-11)
        let expected_columns = [
            "id",
            "file_name",
            "timestamp",
            "saved",
            "title",
            "transcription_text",
            "post_processed_text",
            "post_process_prompt",
            "post_process_requested",
            "model_id",
            "routed",
            "routing_result",
            "tags",
            "ground_truth",
            "quality",
            "speech_speed",
        ];

        // Get actual columns from the database
        let mut stmt = conn.prepare(
            "SELECT name FROM pragma_table_info('transcription_history') ORDER BY cid",
        )?;
        let actual_columns: Vec<String> = stmt
            .query_map([], |row| row.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        // Find missing columns
        let missing_columns: Vec<&str> = expected_columns
            .iter()
            .filter(|col| !actual_columns.contains(&col.to_string()))
            .copied()
            .collect();

        if missing_columns.is_empty() {
            debug!("Schema verification passed - all columns present");
            return Ok(());
        }

        // Log warning about schema inconsistency
        warn!(
            "Schema inconsistency detected: missing columns {:?}. User may have encountered \
             a partial migration. Attempting to fix...",
            missing_columns
        );

        // Add missing columns
        for column in missing_columns {
            let column_def = match column {
                "ground_truth" => "ground_truth TEXT",
                "quality" => "quality TEXT",
                "speech_speed" => "speech_speed TEXT",
                // These columns should have been created by migrations, but if somehow missing,
                // add them with safe defaults
                "post_processed_text" => "post_processed_text TEXT",
                "post_process_prompt" => "post_process_prompt TEXT",
                "post_process_requested" => "post_process_requested BOOLEAN NOT NULL DEFAULT 0",
                "model_id" => "model_id TEXT",
                "routed" => "routed BOOLEAN NOT NULL DEFAULT 0",
                "routing_result" => "routing_result TEXT",
                "tags" => "tags TEXT",
                // Base columns should never be missing (table creation), but handle gracefully
                _ => {
                    warn!("Unexpected missing column '{}', skipping", column);
                    continue;
                }
            };

            info!("Adding missing column '{}' to transcription_history", column);
            conn.execute(
                &format!("ALTER TABLE transcription_history ADD COLUMN {}", column_def),
                [],
            )?;
        }

        info!("Schema verification completed - all columns now present");
        Ok(())
    }

    fn get_connection(&self) -> Result<Connection> {
        Ok(Connection::open(&self.db_path)?)
    }

    fn map_history_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryEntry> {
        Ok(HistoryEntry {
            id: row.get("id")?,
            file_name: row.get("file_name")?,
            timestamp: row.get("timestamp")?,
            saved: row.get("saved")?,
            title: row.get("title")?,
            transcription_text: row.get("transcription_text")?,
            post_processed_text: row.get("post_processed_text")?,
            post_process_prompt: row.get("post_process_prompt")?,
            post_process_requested: row.get("post_process_requested")?,
            model_id: row.get("model_id")?,
            routed: row.get("routed")?,
            routing_result: row.get("routing_result")?,
            tags: row.get("tags")?,
            ground_truth: row.get("ground_truth")?,
            quality: row.get("quality")?,
            speech_speed: row.get("speech_speed")?,
        })
    }

    pub fn recordings_dir(&self) -> &std::path::Path {
        &self.recordings_dir
    }

    /// Save a new history entry to the database.
    /// The WAV file should already have been written to the recordings directory.
    pub fn save_entry(
        &self,
        file_name: String,
        transcription_text: String,
        post_process_requested: bool,
        post_processed_text: Option<String>,
        post_process_prompt: Option<String>,
        model_id: Option<String>,
        routed: bool,
    ) -> Result<HistoryEntry> {
        let timestamp = Utc::now().timestamp();
        let title = self.format_timestamp_title(timestamp);

        let conn = self.get_connection()?;
        conn.execute(
            "INSERT INTO transcription_history (
                file_name,
                timestamp,
                saved,
                title,
                transcription_text,
                post_processed_text,
                post_process_prompt,
                post_process_requested,
                model_id,
                routed
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                &file_name,
                timestamp,
                false,
                &title,
                &transcription_text,
                &post_processed_text,
                &post_process_prompt,
                post_process_requested,
                &model_id,
                routed,
            ],
        )?;

        let entry = HistoryEntry {
            id: conn.last_insert_rowid(),
            file_name,
            timestamp,
            saved: false,
            title,
            transcription_text,
            post_processed_text,
            post_process_prompt,
            post_process_requested,
            model_id,
            routed,
            routing_result: None,
            tags: None,
            ground_truth: None,
            quality: None,
            speech_speed: None,
        };

        debug!("Saved history entry with id {}", entry.id);

        self.cleanup_old_entries()?;

        // Emit typed event for real-time frontend updates
        if let Err(e) = (HistoryUpdatePayload::Added {
            entry: entry.clone(),
        })
        .emit(&self.app_handle)
        {
            error!("Failed to emit history-updated event: {}", e);
        }

        Ok(entry)
    }

    /// Update an existing history entry with new transcription results (used by retry).
    pub fn update_transcription(
        &self,
        id: i64,
        transcription_text: String,
        post_processed_text: Option<String>,
        post_process_prompt: Option<String>,
        model_id: Option<String>,
    ) -> Result<HistoryEntry> {
        let conn = self.get_connection()?;
        let updated = conn.execute(
            "UPDATE transcription_history
             SET transcription_text = ?1,
                 post_processed_text = ?2,
                 post_process_prompt = ?3,
                 model_id = ?4
             WHERE id = ?5",
            params![
                transcription_text,
                post_processed_text,
                post_process_prompt,
                model_id,
                id
            ],
        )?;

        if updated == 0 {
            return Err(anyhow!("History entry {} not found", id));
        }

        let entry = conn
            .query_row(
                "SELECT id, file_name, timestamp, saved, title, transcription_text, post_processed_text, post_process_prompt, post_process_requested, model_id, routed, routing_result
                 FROM transcription_history WHERE id = ?1",
                params![id],
                Self::map_history_entry,
            )?;

        debug!("Updated transcription for history entry {}", id);

        if let Err(e) = (HistoryUpdatePayload::Updated {
            entry: entry.clone(),
        })
        .emit(&self.app_handle)
        {
            error!("Failed to emit history-updated event: {}", e);
        }

        Ok(entry)
    }

    /// Save routing results to an existing history entry (called after boss_router subprocess).
    pub fn update_routing_result(
        &self,
        id: i64,
        routing_result: Option<String>,
    ) -> Result<HistoryEntry> {
        let conn = self.get_connection()?;
        let updated = conn.execute(
            "UPDATE transcription_history
             SET routing_result = ?1
             WHERE id = ?2",
            params![routing_result, id],
        )?;

        if updated == 0 {
            return Err(anyhow!("History entry {id} not found for routing update."));
        }

        let entry = conn
            .query_row(
                "SELECT id, file_name, timestamp, saved, title, transcription_text, post_processed_text, post_process_prompt, post_process_requested, model_id, routed, routing_result, tags, ground_truth, quality, speech_speed
                 FROM transcription_history WHERE id = ?1",
                params![id],
                Self::map_history_entry,
            )?;

        debug!("Updated routing result for history entry {}", id);

        if let Err(e) = (HistoryUpdatePayload::Updated {
            entry: entry.clone(),
        })
        .emit(&self.app_handle)
        {
            error!("Failed to emit history-updated event: {}", e);
        }

        Ok(entry)
    }

    pub fn cleanup_old_entries(&self) -> Result<()> {
        let retention_period = crate::settings::get_recording_retention_period(&self.app_handle);

        match retention_period {
            crate::settings::RecordingRetentionPeriod::Never => {
                // Don't delete anything
                return Ok(());
            }
            crate::settings::RecordingRetentionPeriod::PreserveLimit => {
                // Use the old count-based logic with history_limit
                let limit = crate::settings::get_history_limit(&self.app_handle);
                return self.cleanup_by_count(limit);
            }
            _ => {
                // Use time-based logic
                return self.cleanup_by_time(retention_period);
            }
        }
    }

    fn delete_entries_and_files(&self, entries: &[(i64, String)]) -> Result<usize> {
        if entries.is_empty() {
            return Ok(0);
        }

        let conn = self.get_connection()?;
        let mut deleted_count = 0;

        for (id, file_name) in entries {
            // Delete database entry
            conn.execute(
                "DELETE FROM transcription_history WHERE id = ?1",
                params![id],
            )?;

            // Delete WAV file
            let file_path = self.recordings_dir.join(file_name);
            if file_path.exists() {
                if let Err(e) = fs::remove_file(&file_path) {
                    error!("Failed to delete WAV file {}: {}", file_name, e);
                } else {
                    debug!("Deleted old WAV file: {}", file_name);
                    deleted_count += 1;
                }
            }
        }

        Ok(deleted_count)
    }

    fn cleanup_by_count(&self, limit: usize) -> Result<()> {
        let conn = self.get_connection()?;

        // Get all entries that are not saved, ordered by timestamp desc
        let mut stmt = conn.prepare(
            "SELECT id, file_name FROM transcription_history WHERE saved = 0 ORDER BY timestamp DESC "
        )?;

        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, i64>("id")?, row.get::<_, String>("file_name")?))
        })?;

        let mut entries: Vec<(i64, String)> = Vec::new();
        for row in rows {
            entries.push(row?);
        }

        if entries.len() > limit {
            let entries_to_delete = &entries[limit..];
            let deleted_count = self.delete_entries_and_files(entries_to_delete)?;

            if deleted_count > 0 {
                debug!("Cleaned up {deleted_count} old history entries by count.");
            }
        }

        Ok(())
    }

    fn cleanup_by_time(
        &self,
        retention_period: crate::settings::RecordingRetentionPeriod,
    ) -> Result<()> {
        let conn = self.get_connection()?;

        // Calculate cutoff timestamp (current time minus retention period)
        let now = Utc::now().timestamp();
        let cutoff_timestamp = match retention_period {
            crate::settings::RecordingRetentionPeriod::Days3 => now - (3 * 24 * 60 * 60), // 3 days in seconds
            crate::settings::RecordingRetentionPeriod::Weeks2 => now - (2 * 7 * 24 * 60 * 60), // 2 weeks in seconds
            crate::settings::RecordingRetentionPeriod::Months3 => now - (3 * 30 * 24 * 60 * 60), // 3 months in seconds (approximate)
            _ => unreachable!("should not be reached."),
        };

        // Get all unsaved entries older than the cutoff timestamp
        let mut stmt = conn.prepare(
            "SELECT id, file_name FROM transcription_history WHERE saved = 0 AND timestamp < ?1",
        )?;

        let rows = stmt.query_map(params![cutoff_timestamp], |row| {
            Ok((row.get::<_, i64>("id")?, row.get::<_, String>("file_name")?))
        })?;

        let mut entries_to_delete: Vec<(i64, String)> = Vec::new();
        for row in rows {
            entries_to_delete.push(row?);
        }

        let deleted_count = self.delete_entries_and_files(&entries_to_delete)?;

        if deleted_count > 0 {
            debug!(
                "Cleaned up {} old history entries based on retention period.",
                deleted_count
            );
        }

        Ok(())
    }

    pub async fn get_history_entries(
        &self,
        cursor: Option<i64>,
        limit: Option<usize>,
    ) -> Result<PaginatedHistory> {
        let conn = self.get_connection()?;
        let limit = limit.map(|l| l.min(100));

        let mut entries: Vec<HistoryEntry> = match (cursor, limit) {
            (Some(cursor_id), Some(lim)) => {
                let fetch_count = (lim + 1) as i64;
                let mut stmt = conn.prepare(
                    "SELECT id, file_name, timestamp, saved, title, transcription_text, post_processed_text, post_process_prompt, post_process_requested, model_id, routed, routing_result, tags, ground_truth, quality, speech_speed
                     FROM transcription_history
                     WHERE id < ?1
                     ORDER BY id DESC
                     LIMIT ?2",
                )?;
                let result = stmt
                    .query_map(params![cursor_id, fetch_count], Self::map_history_entry)?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                result
            }
            (None, Some(lim)) => {
                let fetch_count = (lim + 1) as i64;
                let mut stmt = conn.prepare(
                    "SELECT id, file_name, timestamp, saved, title, transcription_text, post_processed_text, post_process_prompt, post_process_requested, model_id, routed, routing_result, tags, ground_truth, quality, speech_speed
                     FROM transcription_history
                     ORDER BY id DESC
                     LIMIT ?1",
                )?;
                let result = stmt
                    .query_map(params![fetch_count], Self::map_history_entry)?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                result
            }
            (_, None) => {
                let mut stmt = conn.prepare(
                    "SELECT id, file_name, timestamp, saved, title, transcription_text, post_processed_text, post_process_prompt, post_process_requested, model_id, routed, routing_result, tags, ground_truth, quality, speech_speed
                     FROM transcription_history
                     ORDER BY id DESC ",
                )?;
                let result = stmt
                    .query_map([], Self::map_history_entry)?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                result
            }
        };

        let has_more = limit.is_some_and(|lim| entries.len() > lim);
        if has_more {
            entries.pop();
        }

        Ok(PaginatedHistory { entries, has_more })
    }

    #[cfg(test)]
    fn get_latest_entry_with_conn(conn: &Connection) -> Result<Option<HistoryEntry>> {
        let mut stmt = conn.prepare(
            "SELECT
                id,
                file_name,
                timestamp,
                saved,
                title,
                transcription_text,
                post_processed_text,
                post_process_prompt,
                post_process_requested,
                model_id,
                routed,
                routing_result,
                tags,
                ground_truth,
                quality,
                speech_speed
             FROM transcription_history
             ORDER BY timestamp DESC
             LIMIT 1",
        )?;

        let entry = stmt.query_row([], Self::map_history_entry).optional()?;
        Ok(entry)
    }

    /// Get the latest entry with non-empty transcription text.
    pub fn get_latest_completed_entry(&self) -> Result<Option<HistoryEntry>> {
        let conn = self.get_connection()?;
        Self::get_latest_completed_entry_with_conn(&conn)
    }

    fn get_latest_completed_entry_with_conn(conn: &Connection) -> Result<Option<HistoryEntry>> {
        let mut stmt = conn.prepare(
            "SELECT
                id,
                file_name,
                timestamp,
                saved,
                title,
                transcription_text,
                post_processed_text,
                post_process_prompt,
                post_process_requested,
                model_id,
                routed,
                routing_result,
                tags,
                ground_truth,
                quality,
                speech_speed
             FROM transcription_history
             WHERE LENGTH(transcription_text) > 0
             ORDER BY timestamp DESC
             LIMIT 1",
        )?;

        let entry = stmt.query_row([], Self::map_history_entry).optional()?;
        Ok(entry)
    }

    pub async fn toggle_saved_status(&self, id: i64) -> Result<()> {
        let conn = self.get_connection()?;

        // Get current saved status
        let current_saved: bool = conn.query_row(
            "SELECT saved FROM transcription_history WHERE id = ?1",
            params![id],
            |row| row.get("saved"),
        )?;

        let new_saved = !current_saved;

        conn.execute(
            "UPDATE transcription_history SET saved = ?1 WHERE id = ?2",
            params![new_saved, id],
        )?;

        debug!("Toggled saved status for entry {}: {}", id, new_saved);

        // Emit history updated event
        if let Err(e) = (HistoryUpdatePayload::Toggled { id }).emit(&self.app_handle) {
            error!("Failed to emit history-updated event: {}", e);
        }

        Ok(())
    }

    /// Update tags for a history entry.
    /// Tags should be a JSON array string like `["fast", "test"]` or None to clear.
    pub async fn update_tags(&self, id: i64, tags: Option<String>) -> Result<()> {
        let conn = self.get_connection()?;

        conn.execute(
            "UPDATE transcription_history SET tags = ?1 WHERE id = ?2",
            params![tags, id],
        )?;

        debug!("Updated tags for entry {}: {:?}", id, tags);

        // Get the updated entry to emit
        let entry = self.get_entry_by_id(id).await?;
        if let Some(entry) = entry {
            if let Err(e) = (HistoryUpdatePayload::Updated { entry }).emit(&self.app_handle) {
                error!("Failed to emit history-updated event: {}", e);
            }
        }

        Ok(())
    }

    /// Update metadata (ground_truth, quality, speech_speed) for a history entry.
    /// Used for data tagging in the experiment system.
    pub async fn update_metadata(
        &self,
        id: i64,
        ground_truth: Option<String>,
        quality: Option<String>,
        speech_speed: Option<String>,
    ) -> Result<()> {
        let conn = self.get_connection()?;

        if let Some(gt) = &ground_truth {
            conn.execute(
                "UPDATE transcription_history SET ground_truth = ?1 WHERE id = ?2",
                params![gt, id],
            )?;
        }

        if let Some(q) = &quality {
            conn.execute(
                "UPDATE transcription_history SET quality = ?1 WHERE id = ?2",
                params![q, id],
            )?;
        }

        if let Some(ss) = &speech_speed {
            conn.execute(
                "UPDATE transcription_history SET speech_speed = ?1 WHERE id = ?2",
                params![ss, id],
            )?;
        }

        debug!(
            "Updated metadata for entry {}: ground_truth={:?}, quality={:?}, speech_speed={:?}",
            id, ground_truth, quality, speech_speed
        );

        // Get the updated entry to emit
        let entry = self.get_entry_by_id(id).await?;
        if let Some(entry) = entry {
            if let Err(e) = (HistoryUpdatePayload::Updated { entry }).emit(&self.app_handle) {
                error!("Failed to emit history-updated event: {}", e);
            }
        }

        Ok(())
    }

    pub fn get_audio_file_path(&self, file_name: &str) -> PathBuf {
        self.recordings_dir.join(file_name)
    }

    /// Get the total number of history entries (for benchmark eligibility check).
    pub fn get_history_count(&self) -> Result<usize> {
        let conn = self.get_connection()?;
        let count: usize =
            conn.query_row("SELECT COUNT(*) FROM transcription_history ", [], |row| {
                row.get(0)
            })?;
        Ok(count)
    }

    /// Get the N most recent history entries (for benchmarking audio clips).
    pub async fn get_recent_entries(&self, limit: usize) -> Result<Vec<HistoryEntry>> {
        let conn = self.get_connection()?;
        let mut stmt = conn.prepare(
            "SELECT
                id,
                file_name,
                timestamp,
                saved,
                title,
                transcription_text,
                post_processed_text,
                post_process_prompt,
                post_process_requested,
                model_id,
                routed,
                routing_result,
                tags,
                ground_truth,
                quality,
                speech_speed
             FROM transcription_history
             ORDER BY timestamp DESC
             LIMIT ?1",
        )?;

        let entries = stmt
            .query_map([limit as i64], Self::map_history_entry)?
            .filter_map(|e| e.ok())
            .collect::<Vec<_>>();

        Ok(entries)
    }

    pub async fn get_entry_by_id(&self, id: i64) -> Result<Option<HistoryEntry>> {
        let conn = self.get_connection()?;
        let mut stmt = conn.prepare(
            "SELECT
                id,
                file_name,
                timestamp,
                saved,
                title,
                transcription_text,
                post_processed_text,
                post_process_prompt,
                post_process_requested,
                model_id,
                routed,
                routing_result,
                tags,
                ground_truth,
                quality,
                speech_speed
             FROM transcription_history
             WHERE id = ?1",
        )?;

        let entry = stmt.query_row([id], Self::map_history_entry).optional()?;

        Ok(entry)
    }

    pub async fn delete_entry(&self, id: i64) -> Result<()> {
        let conn = self.get_connection()?;

        // Get the entry to find the file name
        if let Some(entry) = self.get_entry_by_id(id).await? {
            // Delete the audio file first
            let file_path = self.get_audio_file_path(&entry.file_name);
            if file_path.exists() {
                if let Err(e) = fs::remove_file(&file_path) {
                    error!("Failed to delete audio file {}: {}", entry.file_name, e);
                    // Continue with database deletion even if file deletion fails
                }
            }
        }

        // Delete from database
        conn.execute(
            "DELETE FROM transcription_history WHERE id = ?1",
            params![id],
        )?;

        debug!("Deleted history entry with id: {}", id);

        // Emit history updated event
        if let Err(e) = (HistoryUpdatePayload::Deleted { id }).emit(&self.app_handle) {
            error!("Failed to emit history-updated event: {}", e);
        }

        Ok(())
    }

    fn format_timestamp_title(&self, timestamp: i64) -> String {
        if let Some(utc_datetime) = DateTime::from_timestamp(timestamp, 0) {
            // Convert UTC to local timezone
            let local_datetime = utc_datetime.with_timezone(&Local);
            local_datetime.format("%B %e, %Y - %l:%M%P ").to_string()
        } else {
            format!("Recording {}", timestamp)
        }
    }

    // ========== Experiment Management ==========

    /// Create a new experiment group for a saved recording
    pub async fn create_experiment_group(
        &self,
        recording_id: i64,
        original_transcript: String,
    ) -> Result<ExperimentGroup> {
        let conn = self.get_connection()?;
        let created_at = Utc::now().timestamp();

        conn.execute(
            "INSERT INTO experiment_groups (
                recording_id,
                original_transcript,
                ground_truth,
                speech_speed,
                recording_quality,
                created_at,
                is_complete,
                notes
            ) VALUES (?1, ?2, NULL, 'normal', 'good', ?3, 0, NULL)",
            params![recording_id, original_transcript, created_at],
        )?;

        let id = conn.last_insert_rowid();

        let group = ExperimentGroup {
            id,
            recording_id,
            original_transcript,
            ground_truth: None,
            speech_speed: "normal".to_string(),
            recording_quality: "good".to_string(),
            created_at,
            is_complete: false,
            notes: None,
        };

        debug!("Created experiment group {} for recording {}", id, recording_id);
        Ok(group)
    }

    /// Get experiment group by recording ID
    pub async fn get_experiment_group_by_recording(&self, recording_id: i64) -> Result<Option<ExperimentGroup>> {
        let conn = self.get_connection()?;
        let mut stmt = conn.prepare(
            "SELECT id, recording_id, original_transcript, ground_truth, speech_speed, recording_quality, created_at, is_complete, notes
             FROM experiment_groups
             WHERE recording_id = ?1",
        )?;

        let group = stmt
            .query_row([recording_id], |row| {
                Ok(ExperimentGroup {
                    id: row.get(0)?,
                    recording_id: row.get(1)?,
                    original_transcript: row.get(2)?,
                    ground_truth: row.get(3)?,
                    speech_speed: row.get(4)?,
                    recording_quality: row.get(5)?,
                    created_at: row.get(6)?,
                    is_complete: row.get(7)?,
                    notes: row.get(8)?,
                })
            })
            .optional()?;

        Ok(group)
    }

    /// Update experiment group (ground truth, speech speed, quality, etc.)
    pub async fn update_experiment_group(
        &self,
        id: i64,
        ground_truth: Option<String>,
        speech_speed: Option<String>,
        recording_quality: Option<String>,
        notes: Option<String>,
        is_complete: Option<bool>,
    ) -> Result<ExperimentGroup> {
        let conn = self.get_connection()?;

        // Build UPDATE query with concrete parameters
        if ground_truth.is_some() {
            conn.execute(
                "UPDATE experiment_groups SET ground_truth = ?1 WHERE id = ?2",
                params![ground_truth, id],
            )?;
        }
        if let Some(speed) = speech_speed {
            conn.execute(
                "UPDATE experiment_groups SET speech_speed = ?1 WHERE id = ?2",
                params![speed, id],
            )?;
        }
        if let Some(quality) = recording_quality {
            conn.execute(
                "UPDATE experiment_groups SET recording_quality = ?1 WHERE id = ?2",
                params![quality, id],
            )?;
        }
        if notes.is_some() {
            conn.execute(
                "UPDATE experiment_groups SET notes = ?1 WHERE id = ?2",
                params![notes, id],
            )?;
        }
        if let Some(complete) = is_complete {
            conn.execute(
                "UPDATE experiment_groups SET is_complete = ?1 WHERE id = ?2",
                params![complete as i32, id],
            )?;
        }

        drop(conn);
        self.get_experiment_group_by_id(id).await?
            .ok_or_else(|| anyhow!("Experiment group {} not found after update", id))
    }

    /// Get experiment group by ID
    pub async fn get_experiment_group_by_id(&self, id: i64) -> Result<Option<ExperimentGroup>> {
        let conn = self.get_connection()?;
        let mut stmt = conn.prepare(
            "SELECT id, recording_id, original_transcript, ground_truth, speech_speed, recording_quality, created_at, is_complete, notes
             FROM experiment_groups
             WHERE id = ?1",
        )?;

        let group = stmt
            .query_row([id], |row| {
                Ok(ExperimentGroup {
                    id: row.get(0)?,
                    recording_id: row.get(1)?,
                    original_transcript: row.get(2)?,
                    ground_truth: row.get(3)?,
                    speech_speed: row.get(4)?,
                    recording_quality: row.get(5)?,
                    created_at: row.get(6)?,
                    is_complete: row.get(7)?,
                    notes: row.get(8)?,
                })
            })
            .optional()?;

        Ok(group)
    }

    /// Add a transcription variant to an experiment group
    pub async fn add_variant(
        &self,
        experiment_group_id: i64,
        model_id: String,
        parameters: String,
        transcription_text: String,
    ) -> Result<TranscriptionVariant> {
        let conn = self.get_connection()?;
        let created_at = Utc::now().timestamp();

        conn.execute(
            "INSERT INTO transcription_variants (
                experiment_group_id,
                model_id,
                parameters,
                transcription_text,
                match_score,
                ranking,
                is_acceptable,
                created_at,
                notes
            ) VALUES (?1, ?2, ?3, ?4, NULL, NULL, 0, ?5, NULL)",
            params![experiment_group_id, model_id, parameters, transcription_text, created_at],
        )?;

        let id = conn.last_insert_rowid();

        Ok(TranscriptionVariant {
            id,
            experiment_group_id,
            model_id,
            parameters,
            transcription_text,
            match_score: None,
            ranking: None,
            is_acceptable: false,
            created_at,
            notes: None,
        })
    }

    /// Get all variants for an experiment group
    pub async fn get_variants_for_experiment(&self, experiment_group_id: i64) -> Result<Vec<TranscriptionVariant>> {
        let conn = self.get_connection()?;
        let mut stmt = conn.prepare(
            "SELECT id, experiment_group_id, model_id, parameters, transcription_text, match_score, ranking, is_acceptable, created_at, notes
             FROM transcription_variants
             WHERE experiment_group_id = ?1
             ORDER BY ranking ASC NULLS LAST, created_at ASC",
        )?;

        let variants = stmt
            .query_map([experiment_group_id], |row| {
                Ok(TranscriptionVariant {
                    id: row.get(0)?,
                    experiment_group_id: row.get(1)?,
                    model_id: row.get(2)?,
                    parameters: row.get(3)?,
                    transcription_text: row.get(4)?,
                    match_score: row.get(5)?,
                    ranking: row.get(6)?,
                    is_acceptable: row.get(7)?,
                    created_at: row.get(8)?,
                    notes: row.get(9)?,
                })
            })?
            .filter_map(|v| v.ok())
            .collect();

        Ok(variants)
    }

    /// Update a variant (ranking, acceptability, notes, match score)
    pub async fn update_variant(
        &self,
        id: i64,
        ranking: Option<i32>,
        is_acceptable: Option<bool>,
        notes: Option<String>,
        match_score: Option<f32>,
    ) -> Result<TranscriptionVariant> {
        let conn = self.get_connection()?;

        // Update each field separately to avoid dynamic trait objects
        if let Some(r) = ranking {
            conn.execute(
                "UPDATE transcription_variants SET ranking = ?1 WHERE id = ?2",
                params![r, id],
            )?;
        }
        if let Some(acc) = is_acceptable {
            conn.execute(
                "UPDATE transcription_variants SET is_acceptable = ?1 WHERE id = ?2",
                params![acc as i32, id],
            )?;
        }
        if notes.is_some() {
            conn.execute(
                "UPDATE transcription_variants SET notes = ?1 WHERE id = ?2",
                params![notes, id],
            )?;
        }
        if let Some(score) = match_score {
            conn.execute(
                "UPDATE transcription_variants SET match_score = ?1 WHERE id = ?2",
                params![score, id],
            )?;
        }

        drop(conn);
        self.get_variant_by_id(id).await?
            .ok_or_else(|| anyhow!("Variant {} not found after update", id))
    }

    /// Get variant by ID
    pub async fn get_variant_by_id(&self, id: i64) -> Result<Option<TranscriptionVariant>> {
        let conn = self.get_connection()?;
        let mut stmt = conn.prepare(
            "SELECT id, experiment_group_id, model_id, parameters, transcription_text, match_score, ranking, is_acceptable, created_at, notes
             FROM transcription_variants
             WHERE id = ?1",
        )?;

        let variant = stmt
            .query_row([id], |row| {
                Ok(TranscriptionVariant {
                    id: row.get(0)?,
                    experiment_group_id: row.get(1)?,
                    model_id: row.get(2)?,
                    parameters: row.get(3)?,
                    transcription_text: row.get(4)?,
                    match_score: row.get(5)?,
                    ranking: row.get(6)?,
                    is_acceptable: row.get(7)?,
                    created_at: row.get(8)?,
                    notes: row.get(9)?,
                })
            })
            .optional()?;

        Ok(variant)
    }

    /// Get all complete experiment groups (with ground truth and at least one variant)
    pub async fn get_complete_experiment_groups(&self) -> Result<Vec<ExperimentGroup>> {
        let conn = self.get_connection()?;
        let mut stmt = conn.prepare(
            "SELECT DISTINCT eg.id, eg.recording_id, eg.original_transcript, eg.ground_truth, eg.speech_speed, eg.recording_quality, eg.created_at, eg.is_complete, eg.notes
             FROM experiment_groups eg
             INNER JOIN transcription_variants tv ON eg.id = tv.experiment_group_id
             WHERE eg.ground_truth IS NOT NULL
             ORDER BY eg.created_at DESC",
        )?;

        let groups = stmt
            .query_map([], |row| {
                Ok(ExperimentGroup {
                    id: row.get(0)?,
                    recording_id: row.get(1)?,
                    original_transcript: row.get(2)?,
                    ground_truth: row.get(3)?,
                    speech_speed: row.get(4)?,
                    recording_quality: row.get(5)?,
                    created_at: row.get(6)?,
                    is_complete: row.get(7)?,
                    notes: row.get(8)?,
                })
            })?
            .filter_map(|g| g.ok())
            .collect();

        Ok(groups)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{params, Connection};

    fn setup_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory database ");
        conn.execute_batch(
            "CREATE TABLE transcription_history (
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
                ground_truth TEXT,
                quality TEXT,
                speech_speed TEXT
            );",
        )
        .expect("create transcription_history table ");
        conn
    }

    fn insert_entry(conn: &Connection, timestamp: i64, text: &str, post_processed: Option<&str>) {
        conn.execute(
            "INSERT INTO transcription_history (
                file_name,
                timestamp,
                saved,
                title,
                transcription_text,
                post_processed_text,
                post_process_prompt,
                post_process_requested,
                model_id,
                routed,
                routing_result,
                tags,
                ground_truth,
                quality,
                speech_speed
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                format!("handy-{timestamp}.wav "),
                timestamp,
                false,
                format!("Recording {}", timestamp),
                text,
                post_processed,
                Option::<String>::None,
                false,
                Option::<String>::None,
                false,
                Option::<String>::None,
                Option::<String>::None,
                Option::<String>::None,
                Option::<String>::None,
                Option::<String>::None,
            ],
        )
        .expect("insert history entry ");
    }

    #[test]
    fn get_latest_entry_returns_none_when_empty() {
        let conn = setup_conn();
        let entry = HistoryManager::get_latest_entry_with_conn(&conn).expect("fetch latest entry ");
        assert!(entry.is_none());
    }

    #[test]
    fn get_latest_entry_returns_newest_entry() {
        let conn = setup_conn();
        insert_entry(&conn, 100, "first", None);
        insert_entry(&conn, 200, "second", Some("processed"));

        let entry = HistoryManager::get_latest_entry_with_conn(&conn)
            .expect("fetch latest entry ")
            .expect("entry exists ");

        assert_eq!(entry.timestamp, 200);
        assert_eq!(entry.transcription_text, "second");
        assert_eq!(entry.post_processed_text.as_deref(), Some("processed"));
    }

    #[test]
    fn get_latest_completed_entry_returns_only_nonempty() {
        let conn = setup_conn();
        insert_entry(&conn, 100, "completed", None);
        insert_entry(&conn, 200, "", None);

        let entry = HistoryManager::get_latest_completed_entry_with_conn(&conn)
            .expect("should find completed entry ")
            .expect("completed entry found ");

        assert_eq!(entry.timestamp, 100);
        assert_eq!(entry.transcription_text, "completed ");
    }
}
