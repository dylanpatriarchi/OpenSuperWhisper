//! SQLite-backed recordings store, ported from the Swift GRDB `RecordingStore`
//! (`OpenSuperWhisper/Models/Recording.swift`).
//!
//! Schema notes (this is a NEW database for the Tauri app — no binary
//! compatibility with the Swift app's DB is required):
//! - `id` is stored as a TEXT UUID string (the Swift schema also used a `.text`
//!   primary key for the UUID).
//! - `timestamp` is stored as INTEGER unix-epoch milliseconds (GRDB stored a
//!   datetime string; integer millis sort correctly, are unambiguous, and cross
//!   the IPC boundary as a plain number).
//! - `status` is stored as the same lowercase TEXT raw values the Swift enum
//!   used (`pending`, `converting`, `transcribing`, `completed`, `failed`).
//! - Column names keep the Swift camelCase spelling (`fileName`,
//!   `sourceFileURL`, `rawTranscription`) so the migration steps are a
//!   line-for-line port.
//!
//! Migrations mirror the Swift `DatabaseMigrator` registrations: `v1`,
//! `v2_add_status`, `v3_add_raw_transcription`, tracked in a `migrations`
//! table, with each `ALTER TABLE` guarded by a column-existence check so
//! re-running is idempotent (the Swift `db.columns(in:)` idiom).
//!
//! Like the Swift store, audio files live in a `recordings/` directory and are
//! deleted in lockstep with their rows.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const MILLIS_PER_DAY: i64 = 24 * 60 * 60 * 1000;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid uuid in database: {0}")]
    InvalidUuid(String),
    #[error("invalid status in database: {0}")]
    InvalidStatus(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// Mirrors the Swift `RecordingStatus` raw values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecordingStatus {
    Pending,
    Converting,
    Transcribing,
    Completed,
    Failed,
}

impl RecordingStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RecordingStatus::Pending => "pending",
            RecordingStatus::Converting => "converting",
            RecordingStatus::Transcribing => "transcribing",
            RecordingStatus::Completed => "completed",
            RecordingStatus::Failed => "failed",
        }
    }

    /// The Swift `isPending` predicate: statuses eligible for (re)processing.
    pub fn is_pending(self) -> bool {
        matches!(
            self,
            RecordingStatus::Pending | RecordingStatus::Converting | RecordingStatus::Transcribing
        )
    }
}

impl std::str::FromStr for RecordingStatus {
    type Err = StoreError;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "pending" => Ok(RecordingStatus::Pending),
            "converting" => Ok(RecordingStatus::Converting),
            "transcribing" => Ok(RecordingStatus::Transcribing),
            "completed" => Ok(RecordingStatus::Completed),
            "failed" => Ok(RecordingStatus::Failed),
            other => Err(StoreError::InvalidStatus(other.to_string())),
        }
    }
}

/// SQL fragment matching the Swift `pendingStatuses` filter.
const PENDING_STATUSES_SQL: &str = "('pending', 'converting', 'transcribing')";

/// Mirrors the Swift `Recording` model. Serializes with camelCase keys for the
/// JS side of the Tauri IPC boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Recording {
    pub id: Uuid,
    /// Unix epoch milliseconds.
    pub timestamp: i64,
    pub file_name: String,
    pub transcription: String,
    /// Seconds, like the Swift `TimeInterval`.
    pub duration: f64,
    pub status: RecordingStatus,
    pub progress: f32,
    pub source_file_url: Option<String>,
    /// The transcription exactly as the engine produced it, before any
    /// reformulation. A rewrite must never be the only surviving copy of what
    /// the user actually dictated.
    pub raw_transcription: Option<String>,
    /// Not persisted (the Swift model excludes it from `CodingKeys`); always
    /// `false` when loaded from the database.
    #[serde(default)]
    pub is_regeneration: bool,
}

/// Current unix time in milliseconds.
pub fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Single-writer store over a `Mutex<Connection>`, mirroring GRDB
/// `DatabaseQueue` semantics. Usable as `Arc<RecordingsStore>` from Tauri
/// state (`Mutex` makes it `Sync`).
pub struct RecordingsStore {
    conn: Mutex<Connection>,
    recordings_dir: PathBuf,
}

impl RecordingsStore {
    /// Opens (creating if needed) the database at `db_path` and runs the
    /// migrations. `recordings_dir` is where the audio files named by
    /// `Recording::file_name` live; it is created if missing.
    pub fn open(db_path: impl AsRef<Path>, recordings_dir: impl AsRef<Path>) -> Result<Self> {
        let db_path = db_path.as_ref();
        let recordings_dir = recordings_dir.as_ref().to_path_buf();
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::create_dir_all(&recordings_dir)?;

        let conn = Connection::open(db_path)?;
        migrate(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            recordings_dir,
        })
    }

    pub fn recordings_dir(&self) -> &Path {
        &self.recordings_dir
    }

    /// Absolute path of a recording's audio file, or `None` if the stored file
    /// name would escape the recordings directory (the Swift
    /// `isDeletableRecordingURL` guard).
    pub fn audio_path(&self, file_name: &str) -> Option<PathBuf> {
        if file_name.is_empty() {
            return None;
        }
        let name = Path::new(file_name);
        // A bare file name has exactly one Normal component: no separators,
        // no `..`, no absolute prefix.
        let mut components = name.components();
        match (components.next(), components.next()) {
            (Some(std::path::Component::Normal(_)), None) => Some(self.recordings_dir.join(name)),
            _ => None,
        }
    }

    // --- writes -----------------------------------------------------------

    /// Inserts a recording. Synchronous and returns `Result`: the caller must
    /// see the failure — this is the "awaited save" invariant of the Swift
    /// `addRecordingSync`.
    pub fn add_recording(&self, recording: &Recording) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO recordings
                (id, timestamp, fileName, transcription, duration, status, progress, sourceFileURL, rawTranscription)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                recording.id.to_string(),
                recording.timestamp,
                recording.file_name,
                recording.transcription,
                recording.duration,
                recording.status.as_str(),
                recording.progress as f64,
                recording.source_file_url,
                recording.raw_transcription,
            ],
        )?;
        Ok(())
    }

    /// Full-row update, mirroring the Swift `updateRecordingSync`.
    pub fn update_recording(&self, recording: &Recording) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE recordings SET
                timestamp = ?2, fileName = ?3, transcription = ?4, duration = ?5,
                status = ?6, progress = ?7, sourceFileURL = ?8, rawTranscription = ?9
             WHERE id = ?1",
            params![
                recording.id.to_string(),
                recording.timestamp,
                recording.file_name,
                recording.transcription,
                recording.duration,
                recording.status.as_str(),
                recording.progress as f64,
                recording.source_file_url,
                recording.raw_transcription,
            ],
        )?;
        Ok(())
    }

    /// Mirrors `updateRecordingProgressOnlySync`: transcription + progress +
    /// status in one statement. (Transient per-tick progress that never touches
    /// the DB is the app layer's concern, not this crate's.)
    pub fn update_transcription(
        &self,
        id: Uuid,
        transcription: &str,
        progress: f32,
        status: RecordingStatus,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE recordings SET transcription = ?2, progress = ?3, status = ?4 WHERE id = ?1",
            params![
                id.to_string(),
                transcription,
                progress as f64,
                status.as_str()
            ],
        )?;
        Ok(())
    }

    /// Mirrors `updateRecordingStatusOnly`: progress + status.
    pub fn update_status(&self, id: Uuid, progress: f32, status: RecordingStatus) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE recordings SET progress = ?2, status = ?3 WHERE id = ?1",
            params![id.to_string(), progress as f64, status.as_str()],
        )?;
        Ok(())
    }

    /// Mirrors `updateRecordingStatusOnly` when only raw transcription needs
    /// preserving — sets `rawTranscription` for a recording.
    pub fn update_raw_transcription(&self, id: Uuid, raw_transcription: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE recordings SET rawTranscription = ?2 WHERE id = ?1",
            params![id.to_string(), raw_transcription],
        )?;
        Ok(())
    }

    /// Mirrors `updateSourceFileURL`.
    pub fn update_source_file_url(&self, id: Uuid, source_url: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE recordings SET sourceFileURL = ?2 WHERE id = ?1",
            params![id.to_string(), source_url],
        )?;
        Ok(())
    }

    /// Deletes the row and its audio file (missing files are tolerated),
    /// mirroring `deleteRecordingSync`. Returns `true` if a row was deleted.
    pub fn delete_recording(&self, id: Uuid) -> Result<bool> {
        let file_name: Option<String> = {
            let conn = self.conn.lock().unwrap();
            let file_name = conn
                .query_row(
                    "SELECT fileName FROM recordings WHERE id = ?1",
                    params![id.to_string()],
                    |row| row.get(0),
                )
                .map(Some)
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    e => Err(e),
                })?;
            if file_name.is_some() {
                conn.execute(
                    "DELETE FROM recordings WHERE id = ?1",
                    params![id.to_string()],
                )?;
            }
            file_name
        };
        match file_name {
            Some(name) => {
                self.remove_audio_file(&name);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Deletes several recordings (rows and audio files).
    pub fn delete_recordings(&self, ids: &[Uuid]) -> Result<usize> {
        let mut deleted = 0;
        for &id in ids {
            if self.delete_recording(id)? {
                deleted += 1;
            }
        }
        Ok(deleted)
    }

    /// Mirrors `deleteAllRecordings`: removes every audio file, then every row.
    pub fn delete_all_recordings(&self) -> Result<usize> {
        let file_names: Vec<String> = {
            let conn = self.conn.lock().unwrap();
            let mut stmt = conn.prepare("SELECT fileName FROM recordings")?;
            let names = stmt
                .query_map([], |row| row.get(0))?
                .collect::<std::result::Result<Vec<String>, _>>()?;
            conn.execute("DELETE FROM recordings", [])?;
            names
        };
        for name in &file_names {
            self.remove_audio_file(name);
        }
        Ok(file_names.len())
    }

    /// Mirrors `deleteRecordings(olderThanDays:)`: deletes non-pending
    /// recordings whose timestamp is older than the cutoff, removing audio
    /// files in lockstep. `days <= 0` deletes nothing (the Swift
    /// `retentionCutoffDate` guard). Returns the number of rows deleted.
    pub fn delete_recordings_older_than(&self, days: i64) -> Result<usize> {
        if days <= 0 {
            return Ok(0);
        }
        let cutoff = now_millis() - days * MILLIS_PER_DAY;
        let outdated: Vec<(String, String)> = {
            let conn = self.conn.lock().unwrap();
            let mut stmt = conn.prepare(&format!(
                "SELECT id, fileName FROM recordings
                 WHERE timestamp < ?1 AND status NOT IN {PENDING_STATUSES_SQL}"
            ))?;
            let rows = stmt
                .query_map(params![cutoff], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            for (id, _) in &rows {
                conn.execute("DELETE FROM recordings WHERE id = ?1", params![id])?;
            }
            rows
        };
        for (_, file_name) in &outdated {
            self.remove_audio_file(file_name);
        }
        Ok(outdated.len())
    }

    // --- reads ------------------------------------------------------------

    /// All recordings, newest first (the Swift default ordering).
    pub fn get_all(&self) -> Result<Vec<Recording>> {
        self.query_recordings("ORDER BY timestamp DESC", &[])
    }

    /// Mirrors `fetchRecordings(limit:offset:)`.
    pub fn get_recordings(&self, limit: i64, offset: i64) -> Result<Vec<Recording>> {
        self.query_recordings(
            "ORDER BY timestamp DESC LIMIT ?1 OFFSET ?2",
            &[&limit, &offset],
        )
    }

    pub fn get_recording(&self, id: Uuid) -> Result<Option<Recording>> {
        Ok(self
            .query_recordings("WHERE id = ?1 LIMIT 1", &[&id.to_string()])?
            .into_iter()
            .next())
    }

    /// Mirrors `getPendingRecordings`: recordings whose status is pending,
    /// converting, or transcribing — the exact filter the Swift store uses to
    /// resume interrupted transcriptions — oldest first.
    pub fn get_pending_recordings(&self) -> Result<Vec<Recording>> {
        self.query_recordings(
            &format!("WHERE status IN {PENDING_STATUSES_SQL} ORDER BY timestamp ASC"),
            &[],
        )
    }

    /// Mirrors `getNextPendingRecording`.
    pub fn get_next_pending_recording(&self) -> Result<Option<Recording>> {
        Ok(self
            .query_recordings(
                &format!("WHERE status IN {PENDING_STATUSES_SQL} ORDER BY timestamp ASC LIMIT 1"),
                &[],
            )?
            .into_iter()
            .next())
    }

    /// Mirrors `searchRecordings(query:)`: case-insensitive substring match on
    /// the transcription, newest first.
    pub fn search_recordings(&self, query: &str, limit: i64, offset: i64) -> Result<Vec<Recording>> {
        let pattern = format!(
            "%{}%",
            query.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
        );
        self.query_recordings(
            "WHERE transcription LIKE ?1 ESCAPE '\\' ORDER BY timestamp DESC LIMIT ?2 OFFSET ?3",
            &[&pattern, &limit, &offset],
        )
    }

    // --- internals --------------------------------------------------------

    fn query_recordings(
        &self,
        tail: &str,
        params: &[&dyn rusqlite::ToSql],
    ) -> Result<Vec<Recording>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT id, timestamp, fileName, transcription, duration, status, progress,
                    sourceFileURL, rawTranscription
             FROM recordings {tail}"
        ))?;
        let rows = stmt
            .query_map(params, row_to_raw)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter().map(raw_to_recording).collect()
    }

    /// Removes an audio file, tolerating files that are already gone and
    /// refusing paths that would escape the recordings directory.
    fn remove_audio_file(&self, file_name: &str) {
        if let Some(path) = self.audio_path(file_name) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Intermediate row shape: everything as SQL-native types, so the rusqlite
/// closure stays infallible and UUID/status parsing errors surface as
/// `StoreError`.
type RawRow = (
    String,         // id
    i64,            // timestamp
    String,         // fileName
    String,         // transcription
    f64,            // duration
    String,         // status
    f64,            // progress
    Option<String>, // sourceFileURL
    Option<String>, // rawTranscription
);

fn row_to_raw(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
    ))
}

fn raw_to_recording(raw: RawRow) -> Result<Recording> {
    let (id, timestamp, file_name, transcription, duration, status, progress, source, raw_tx) = raw;
    Ok(Recording {
        id: Uuid::parse_str(&id).map_err(|_| StoreError::InvalidUuid(id))?,
        timestamp,
        file_name,
        transcription,
        duration,
        status: status.parse()?,
        progress: progress as f32,
        source_file_url: source,
        raw_transcription: raw_tx,
        is_regeneration: false,
    })
}

// --- migrations -----------------------------------------------------------

fn column_exists(conn: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for name in names {
        if name? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn has_migration(conn: &Connection, identifier: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT COUNT(*) FROM migrations WHERE identifier = ?1",
        params![identifier],
        |row| row.get::<_, i64>(0),
    )
    .map(|count| count > 0)
}

/// Additive migrations, a line-for-line port of the Swift `DatabaseMigrator`
/// setup. Applied migrations are recorded in a `migrations` table (the GRDB
/// `grdb_migrations` idiom); each ALTER is additionally guarded by a
/// column-existence check so re-running against any prior schema state is
/// idempotent.
fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS migrations (identifier TEXT PRIMARY KEY NOT NULL)",
    )?;

    // v1: base table (ifNotExists, like the Swift v1).
    if !has_migration(conn, "v1")? {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS recordings (
                id TEXT PRIMARY KEY NOT NULL,
                timestamp INTEGER NOT NULL,
                fileName TEXT NOT NULL,
                transcription TEXT NOT NULL COLLATE NOCASE,
                duration REAL NOT NULL
            );
            CREATE INDEX IF NOT EXISTS recordings_on_timestamp ON recordings(timestamp);
            CREATE INDEX IF NOT EXISTS recordings_on_transcription ON recordings(transcription);",
        )?;
        conn.execute("INSERT INTO migrations (identifier) VALUES ('v1')", [])?;
    }

    // v2_add_status: status/progress/sourceFileURL, each guarded by a
    // column-existence check before ALTER (the Swift `db.columns` idiom).
    if !has_migration(conn, "v2_add_status")? {
        if !column_exists(conn, "recordings", "status")? {
            conn.execute(
                "ALTER TABLE recordings ADD COLUMN status TEXT NOT NULL DEFAULT 'completed'",
                [],
            )?;
        }
        if !column_exists(conn, "recordings", "progress")? {
            conn.execute(
                "ALTER TABLE recordings ADD COLUMN progress REAL NOT NULL DEFAULT 1.0",
                [],
            )?;
        }
        if !column_exists(conn, "recordings", "sourceFileURL")? {
            conn.execute("ALTER TABLE recordings ADD COLUMN sourceFileURL TEXT", [])?;
        }
        conn.execute(
            "INSERT INTO migrations (identifier) VALUES ('v2_add_status')",
            [],
        )?;
    }

    // v3_add_raw_transcription.
    if !has_migration(conn, "v3_add_raw_transcription")? {
        if !column_exists(conn, "recordings", "rawTranscription")? {
            conn.execute("ALTER TABLE recordings ADD COLUMN rawTranscription TEXT", [])?;
        }
        conn.execute(
            "INSERT INTO migrations (identifier) VALUES ('v3_add_raw_transcription')",
            [],
        )?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    struct Fixture {
        _dir: TempDir,
        db_path: PathBuf,
        recordings_dir: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let db_path = dir.path().join("recordings.sqlite");
            let recordings_dir = dir.path().join("recordings");
            Self {
                _dir: dir,
                db_path,
                recordings_dir,
            }
        }

        fn open(&self) -> RecordingsStore {
            RecordingsStore::open(&self.db_path, &self.recordings_dir).expect("open store")
        }
    }

    fn sample(timestamp: i64, status: RecordingStatus) -> Recording {
        let id = Uuid::new_v4();
        Recording {
            id,
            timestamp,
            file_name: format!("{id}.wav"),
            transcription: "ciao mondo".to_string(),
            duration: 2.5,
            status,
            progress: if status == RecordingStatus::Completed {
                1.0
            } else {
                0.0
            },
            source_file_url: None,
            raw_transcription: None,
            is_regeneration: false,
        }
    }

    #[test]
    fn crud_round_trip() {
        let fx = Fixture::new();
        let store = fx.open();

        let mut rec = sample(now_millis(), RecordingStatus::Pending);
        rec.source_file_url = Some("/tmp/original.m4a".to_string());
        store.add_recording(&rec).unwrap();

        let all = store.get_all().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0], rec);

        // Duplicate id must fail (primary key), proving errors surface.
        assert!(store.add_recording(&rec).is_err());

        store
            .update_transcription(rec.id, "testo finale", 1.0, RecordingStatus::Completed)
            .unwrap();
        store
            .update_raw_transcription(rec.id, "testo grezzo")
            .unwrap();
        store
            .update_source_file_url(rec.id, "/tmp/other.m4a")
            .unwrap();

        let got = store.get_recording(rec.id).unwrap().unwrap();
        assert_eq!(got.transcription, "testo finale");
        assert_eq!(got.raw_transcription.as_deref(), Some("testo grezzo"));
        assert_eq!(got.source_file_url.as_deref(), Some("/tmp/other.m4a"));
        assert_eq!(got.status, RecordingStatus::Completed);
        assert_eq!(got.progress, 1.0);

        store
            .update_status(rec.id, 0.5, RecordingStatus::Transcribing)
            .unwrap();
        let got = store.get_recording(rec.id).unwrap().unwrap();
        assert_eq!(got.status, RecordingStatus::Transcribing);
        assert_eq!(got.progress, 0.5);

        // Full-row update.
        let mut updated = got.clone();
        updated.duration = 9.75;
        updated.status = RecordingStatus::Failed;
        store.update_recording(&updated).unwrap();
        let got = store.get_recording(rec.id).unwrap().unwrap();
        assert_eq!(got.duration, 9.75);
        assert_eq!(got.status, RecordingStatus::Failed);

        assert!(store.delete_recording(rec.id).unwrap());
        assert!(!store.delete_recording(rec.id).unwrap());
        assert!(store.get_all().unwrap().is_empty());
    }

    #[test]
    fn delete_removes_audio_file_and_tolerates_missing() {
        let fx = Fixture::new();
        let store = fx.open();

        let with_file = sample(now_millis(), RecordingStatus::Completed);
        let without_file = sample(now_millis(), RecordingStatus::Completed);
        store.add_recording(&with_file).unwrap();
        store.add_recording(&without_file).unwrap();

        let audio = fx.recordings_dir.join(&with_file.file_name);
        fs::write(&audio, b"riff").unwrap();

        assert!(store.delete_recording(with_file.id).unwrap());
        assert!(!audio.exists(), "audio file must be deleted with the row");
        // No file on disk for this one: still succeeds.
        assert!(store.delete_recording(without_file.id).unwrap());
    }

    #[test]
    fn migration_is_idempotent_across_reopens() {
        let fx = Fixture::new();
        let rec = sample(now_millis(), RecordingStatus::Completed);
        {
            let store = fx.open();
            store.add_recording(&rec).unwrap();
        }
        // Re-open the same DB: migrations must re-run without error or loss.
        let store = fx.open();
        let all = store.get_all().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0], rec);
    }

    #[test]
    fn v1_schema_migrates_to_v3_without_data_loss() {
        let fx = Fixture::new();
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();

        // Hand-build a raw v1 database, as an old app version would have left it.
        {
            let conn = Connection::open(&fx.db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE recordings (
                    id TEXT PRIMARY KEY NOT NULL,
                    timestamp INTEGER NOT NULL,
                    fileName TEXT NOT NULL,
                    transcription TEXT NOT NULL COLLATE NOCASE,
                    duration REAL NOT NULL
                );",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO recordings (id, timestamp, fileName, transcription, duration)
                 VALUES (?1, 1000, 'a.wav', 'vecchia trascrizione', 1.5),
                        (?2, 2000, 'b.wav', 'altra trascrizione', 3.0)",
                params![id_a.to_string(), id_b.to_string()],
            )
            .unwrap();
        }

        let store = fx.open();
        let all = store.get_all().unwrap();
        assert_eq!(all.len(), 2, "v1 rows must survive migration");

        // Newest first: b (2000) then a (1000).
        assert_eq!(all[0].id, id_b);
        assert_eq!(all[1].id, id_a);
        assert_eq!(all[1].transcription, "vecchia trascrizione");
        assert_eq!(all[1].duration, 1.5);
        // v2 defaults: completed / 1.0 / NULL, v3 default: NULL.
        for rec in &all {
            assert_eq!(rec.status, RecordingStatus::Completed);
            assert_eq!(rec.progress, 1.0);
            assert_eq!(rec.source_file_url, None);
            assert_eq!(rec.raw_transcription, None);
        }

        // And the migrated DB is fully writable with the new columns.
        store
            .update_transcription(id_a, "nuova", 1.0, RecordingStatus::Completed)
            .unwrap();
        // Re-open once more: idempotent even after the guarded ALTERs ran.
        drop(store);
        let store = fx.open();
        assert_eq!(store.get_all().unwrap().len(), 2);
    }

    #[test]
    fn pending_filter_and_next_pending() {
        let fx = Fixture::new();
        let store = fx.open();

        let completed = sample(1000, RecordingStatus::Completed);
        let failed = sample(2000, RecordingStatus::Failed);
        let transcribing = sample(3000, RecordingStatus::Transcribing);
        let pending_old = sample(500, RecordingStatus::Pending);
        let converting = sample(4000, RecordingStatus::Converting);
        for r in [&completed, &failed, &transcribing, &pending_old, &converting] {
            store.add_recording(r).unwrap();
        }

        let pending = store.get_pending_recordings().unwrap();
        // pending/converting/transcribing only, oldest first.
        assert_eq!(
            pending.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![pending_old.id, transcribing.id, converting.id]
        );

        let next = store.get_next_pending_recording().unwrap().unwrap();
        assert_eq!(next.id, pending_old.id);
    }

    #[test]
    fn delete_older_than_removes_rows_and_files_but_keeps_pending() {
        let fx = Fixture::new();
        let store = fx.open();

        let now = now_millis();
        let old = now - 40 * MILLIS_PER_DAY;

        let old_completed = sample(old, RecordingStatus::Completed);
        let old_failed = sample(old, RecordingStatus::Failed); // no audio file on disk
        let old_pending = sample(old, RecordingStatus::Pending);
        let recent_completed = sample(now, RecordingStatus::Completed);
        for r in [&old_completed, &old_failed, &old_pending, &recent_completed] {
            store.add_recording(r).unwrap();
        }
        let old_audio = fx.recordings_dir.join(&old_completed.file_name);
        let recent_audio = fx.recordings_dir.join(&recent_completed.file_name);
        fs::write(&old_audio, b"old").unwrap();
        fs::write(&recent_audio, b"new").unwrap();

        let deleted = store.delete_recordings_older_than(30).unwrap();
        assert_eq!(deleted, 2, "old completed + old failed");

        let remaining: Vec<Uuid> = store.get_all().unwrap().iter().map(|r| r.id).collect();
        assert!(remaining.contains(&old_pending.id), "pending rows are kept");
        assert!(remaining.contains(&recent_completed.id));
        assert_eq!(remaining.len(), 2);
        assert!(!old_audio.exists(), "old audio file removed");
        assert!(recent_audio.exists(), "recent audio file kept");

        // days <= 0 is a no-op (the Swift retentionCutoffDate guard).
        assert_eq!(store.delete_recordings_older_than(0).unwrap(), 0);
        assert_eq!(store.get_all().unwrap().len(), 2);
    }

    #[test]
    fn get_all_is_ordered_newest_first() {
        let fx = Fixture::new();
        let store = fx.open();

        let a = sample(100, RecordingStatus::Completed);
        let b = sample(300, RecordingStatus::Completed);
        let c = sample(200, RecordingStatus::Completed);
        for r in [&a, &b, &c] {
            store.add_recording(r).unwrap();
        }

        let ids: Vec<Uuid> = store.get_all().unwrap().iter().map(|r| r.id).collect();
        assert_eq!(ids, vec![b.id, c.id, a.id]);

        // Pagination follows the same ordering.
        let page: Vec<Uuid> = store
            .get_recordings(2, 1)
            .unwrap()
            .iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(page, vec![c.id, a.id]);
    }

    #[test]
    fn search_matches_case_insensitively() {
        let fx = Fixture::new();
        let store = fx.open();

        let mut a = sample(100, RecordingStatus::Completed);
        a.transcription = "Buongiorno Mondo".to_string();
        let mut b = sample(200, RecordingStatus::Completed);
        b.transcription = "altro testo".to_string();
        store.add_recording(&a).unwrap();
        store.add_recording(&b).unwrap();

        let hits = store.search_recordings("mondo", 100, 0).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, a.id);

        // LIKE wildcards in the query are treated literally.
        assert!(store.search_recordings("%", 100, 0).unwrap().is_empty());
    }

    #[test]
    fn audio_path_rejects_escaping_names() {
        let fx = Fixture::new();
        let store = fx.open();
        assert!(store.audio_path("ok.wav").is_some());
        assert!(store.audio_path("").is_none());
        assert!(store.audio_path("../evil.wav").is_none());
        assert!(store.audio_path("a/b.wav").is_none());
        assert!(store.audio_path("/etc/passwd").is_none());
    }

    #[test]
    fn delete_all_removes_rows_and_files() {
        let fx = Fixture::new();
        let store = fx.open();

        let a = sample(100, RecordingStatus::Completed);
        let b = sample(200, RecordingStatus::Failed);
        store.add_recording(&a).unwrap();
        store.add_recording(&b).unwrap();
        let audio = fx.recordings_dir.join(&a.file_name);
        fs::write(&audio, b"riff").unwrap();

        assert_eq!(store.delete_all_recordings().unwrap(), 2);
        assert!(store.get_all().unwrap().is_empty());
        assert!(!audio.exists());
    }

    #[test]
    fn serde_uses_camel_case_for_js() {
        let rec = sample(1234, RecordingStatus::Transcribing);
        let json = serde_json::to_value(&rec).unwrap();
        assert!(json.get("fileName").is_some());
        assert!(json.get("sourceFileURL").is_none()); // camelCase: sourceFileUrl
        assert!(json.get("sourceFileUrl").is_some());
        assert!(json.get("rawTranscription").is_some());
        assert!(json.get("isRegeneration").is_some());
        assert_eq!(json.get("status").unwrap(), "transcribing");
    }

    #[test]
    fn store_is_usable_across_threads_via_arc() {
        let fx = Fixture::new();
        let store = std::sync::Arc::new(fx.open());

        let handles: Vec<_> = (0..4)
            .map(|i| {
                let store = store.clone();
                std::thread::spawn(move || {
                    let rec = sample(i * 100, RecordingStatus::Completed);
                    store.add_recording(&rec).unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(store.get_all().unwrap().len(), 4);
    }
}
