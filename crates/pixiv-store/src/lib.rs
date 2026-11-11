//! SQLite persistence.
//!
//! # Scope
//!
//! Local state only: search history, view history, a key/value settings store,
//! and download records. No network, no UI types.
//!
//! # Requirements
//!
//! * A schema version table with forward-only migrations, so an updated build
//!   never loses a user's history.
//! * Every call must be usable from the async runtime without blocking the UI
//!   thread; the store is cheap to clone and internally synchronised.
//! * Nothing sensitive is ever written: the session cookie lives in a separate
//!   file with restrictive handling, not in SQLite.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::{Mutex, MutexGuard};
use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;

/// Persistence failures.
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("数据库错误：{0}")]
    Sqlite(String),
    #[error("I/O 错误：{0}")]
    Io(String),
    #[error("{0}")]
    Other(String),
}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sqlite(e.to_string())
    }
}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

/// One viewed illustration.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HistoryEntry {
    pub illust_id: String,
    pub title: String,
    pub author: String,
    pub thumb_url: String,
    /// Unix seconds.
    pub viewed_at: i64,
}

/// One download, for the download manager page.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DownloadRecord {
    pub id: i64,
    pub illust_id: String,
    pub title: String,
    pub author: String,
    pub page_index: u32,
    pub file_path: String,
    pub created_at: i64,
}

/// Aggregate counters for the settings page.
#[derive(Debug, Clone, Copy, Default)]
pub struct StoreStats {
    pub history_entries: u64,
    pub download_entries: u64,
}

// ---------------------------------------------------------------------------
// connections, pragmas and migrations
// ---------------------------------------------------------------------------

/// How long a statement waits for the write lock before failing.
const BUSY_TIMEOUT: Duration = Duration::from_millis(5_000);

/// Migration step 1: every table this crate ever reads or writes.
///
/// Kept strictly additive — a later step may only add tables, columns or
/// indexes, never rename or drop one, so an old database keeps opening.
const MIGRATION_STEP_1: &str = r#"
CREATE TABLE IF NOT EXISTS search_history (
    word        TEXT    NOT NULL PRIMARY KEY,
    searched_at INTEGER NOT NULL,
    seq         INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS view_history (
    illust_id TEXT    NOT NULL PRIMARY KEY,
    title     TEXT    NOT NULL,
    author    TEXT    NOT NULL,
    thumb_url TEXT    NOT NULL,
    viewed_at INTEGER NOT NULL,
    seq       INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS settings (
    "key"   TEXT NOT NULL PRIMARY KEY,
    "value" TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS downloads (
    id         INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    illust_id  TEXT    NOT NULL,
    title      TEXT    NOT NULL,
    author     TEXT    NOT NULL,
    page_index INTEGER NOT NULL,
    file_path  TEXT    NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE (illust_id, page_index)
);

CREATE INDEX IF NOT EXISTS idx_view_history_viewed_at ON view_history (viewed_at DESC);
CREATE INDEX IF NOT EXISTS idx_downloads_created_at ON downloads (created_at DESC);
"#;

/// Ordered migration steps; entry `i` upgrades the schema to version `i + 1`.
const MIGRATIONS: &[&str] = &[MIGRATION_STEP_1];

/// Version the current build produces.
const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

/// Bring an open connection up to [`SCHEMA_VERSION`], one guarded step at a time.
fn apply_migrations(conn: &mut Connection) -> Result<(), StoreError> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS schema_version (
            singleton INTEGER NOT NULL PRIMARY KEY CHECK (singleton = 1),
            version   INTEGER NOT NULL
        );
        INSERT INTO schema_version (singleton, version)
            SELECT 1, 0
            WHERE NOT EXISTS (SELECT 1 FROM schema_version WHERE singleton = 1);
        "#,
    )?;

    let mut current: i64 = conn.query_row(
        "SELECT version FROM schema_version WHERE singleton = 1",
        [],
        |row| row.get(0),
    )?;

    for (index, step) in MIGRATIONS.iter().enumerate() {
        // `index + 1` is tiny; the saturating conversion only satisfies the type.
        let target = i64::try_from(index + 1).unwrap_or(i64::MAX);
        if current >= target {
            continue;
        }
        let tx = conn.transaction()?;
        tx.execute_batch(step)?;
        tx.execute(
            "UPDATE schema_version SET version = ?1 WHERE singleton = 1",
            params![target],
        )?;
        tx.commit()?;
        current = target;
        tracing::debug!(version = target, "sqlite 架构迁移已应用");
    }

    if current > SCHEMA_VERSION {
        tracing::warn!(
            found = current,
            expected = SCHEMA_VERSION,
            "数据库架构版本高于当前构建，保持向前兼容读取"
        );
    }
    Ok(())
}

/// Apply the pragmas every connection needs, then migrate.
fn prepare(mut conn: Connection) -> Result<Store, StoreError> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", true)?;
    apply_migrations(&mut conn)?;
    Ok(Store {
        inner: Arc::new(Mutex::new(conn)),
    })
}

/// Unix seconds, saturating rather than panicking on a pre-1970 clock.
fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// `usize` limit as a SQLite `LIMIT` value.
fn limit_to_i64(limit: usize) -> i64 {
    i64::try_from(limit).unwrap_or(i64::MAX)
}

/// The local database.
#[derive(Clone, Debug)]
pub struct Store {
    inner: Arc<Mutex<Connection>>,
}

impl Store {
    /// Open (creating if needed) a database at `path`, running migrations.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        // Let-chains need Rust 1.88; the workspace targets 1.85, so stay nested.
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        prepare(Connection::open(path)?)
    }

    /// In-memory database, used by tests and by Demo Mode.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        prepare(Connection::open_in_memory()?)
    }

    /// Schema version currently applied.
    pub fn schema_version(&self) -> Result<i64, StoreError> {
        let conn = self.lock();
        let version = conn.query_row(
            "SELECT version FROM schema_version WHERE singleton = 1",
            [],
            |row| row.get(0),
        )?;
        Ok(version)
    }

    /// Borrow the connection. `parking_lot` has no poisoning, so a panic in
    /// another thread yields the lock rather than a poisoned guard.
    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.inner.lock()
    }

    // ---- search history -------------------------------------------------

    /// Most recent search words, newest first, de-duplicated.
    pub fn search_history(&self, limit: usize) -> Result<Vec<String>, StoreError> {
        let conn = self.lock();
        let mut stmt =
            conn.prepare("SELECT word FROM search_history ORDER BY seq DESC LIMIT ?1")?;
        let rows = stmt.query_map(params![limit_to_i64(limit)], |row| row.get::<_, String>(0))?;
        let mut words = Vec::new();
        for word in rows {
            words.push(word?);
        }
        Ok(words)
    }

    /// Record a search; re-searching an existing word moves it to the front.
    pub fn push_search(&self, word: &str) -> Result<(), StoreError> {
        let word = word.trim();
        if word.is_empty() {
            return Ok(());
        }
        let conn = self.lock();
        // A monotonic sequence keeps "front" exact even within one second.
        let next_seq: i64 = conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM search_history",
            [],
            |row| row.get(0),
        )?;
        conn.execute(
            "INSERT INTO search_history (word, searched_at, seq) VALUES (?1, ?2, ?3)
             ON CONFLICT(word) DO UPDATE SET searched_at = excluded.searched_at, seq = excluded.seq",
            params![word, now_secs(), next_seq],
        )?;
        Ok(())
    }

    pub fn clear_search_history(&self) -> Result<(), StoreError> {
        let conn = self.lock();
        conn.execute("DELETE FROM search_history", [])?;
        Ok(())
    }

    // ---- view history ---------------------------------------------------

    /// Record a view; repeat views update the timestamp rather than duplicating.
    pub fn record_view(&self, entry: &HistoryEntry) -> Result<(), StoreError> {
        let conn = self.lock();
        let next_seq: i64 = conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM view_history",
            [],
            |row| row.get(0),
        )?;
        conn.execute(
            "INSERT INTO view_history (illust_id, title, author, thumb_url, viewed_at, seq)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(illust_id) DO UPDATE SET
                 title     = excluded.title,
                 author    = excluded.author,
                 thumb_url = excluded.thumb_url,
                 viewed_at = excluded.viewed_at,
                 seq       = excluded.seq",
            params![
                entry.illust_id,
                entry.title,
                entry.author,
                entry.thumb_url,
                entry.viewed_at,
                next_seq,
            ],
        )?;
        Ok(())
    }

    pub fn view_history(&self, limit: usize) -> Result<Vec<HistoryEntry>, StoreError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT illust_id, title, author, thumb_url, viewed_at
             FROM view_history
             ORDER BY viewed_at DESC, seq DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit_to_i64(limit)], |row| {
            Ok(HistoryEntry {
                illust_id: row.get(0)?,
                title: row.get(1)?,
                author: row.get(2)?,
                thumb_url: row.get(3)?,
                viewed_at: row.get(4)?,
            })
        })?;
        let mut entries = Vec::new();
        for entry in rows {
            entries.push(entry?);
        }
        Ok(entries)
    }

    pub fn clear_view_history(&self) -> Result<(), StoreError> {
        let conn = self.lock();
        conn.execute("DELETE FROM view_history", [])?;
        Ok(())
    }

    // ---- settings -------------------------------------------------------

    pub fn get_setting(&self, key: &str) -> Result<Option<String>, StoreError> {
        let conn = self.lock();
        let value = conn
            .query_row(
                r#"SELECT "value" FROM settings WHERE "key" = ?1"#,
                params![key],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(value)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), StoreError> {
        let conn = self.lock();
        conn.execute(
            r#"INSERT OR REPLACE INTO settings ("key", "value") VALUES (?1, ?2)"#,
            params![key, value],
        )?;
        Ok(())
    }

    pub fn remove_setting(&self, key: &str) -> Result<(), StoreError> {
        let conn = self.lock();
        conn.execute(r#"DELETE FROM settings WHERE "key" = ?1"#, params![key])?;
        Ok(())
    }

    // ---- downloads ------------------------------------------------------

    /// Insert a record, returning its row id. Duplicate (illust, page) pairs are
    /// rejected so re-downloading the same page does not create noise.
    ///
    /// # Errors
    ///
    /// A pair already present in `downloads` surfaces as
    /// [`StoreError::Sqlite`] carrying SQLite's UNIQUE-constraint message
    /// (`UNIQUE constraint failed: downloads.illust_id, downloads.page_index`),
    /// so callers can match on the text or simply treat it as "already
    /// recorded" and keep the existing row id.
    pub fn insert_download(&self, record: &DownloadRecord) -> Result<i64, StoreError> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO downloads (illust_id, title, author, page_index, file_path, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                record.illust_id,
                record.title,
                record.author,
                record.page_index,
                record.file_path,
                record.created_at,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn downloads(&self, limit: usize) -> Result<Vec<DownloadRecord>, StoreError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, illust_id, title, author, page_index, file_path, created_at
             FROM downloads
             ORDER BY created_at DESC, id DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit_to_i64(limit)], |row| {
            Ok(DownloadRecord {
                id: row.get(0)?,
                illust_id: row.get(1)?,
                title: row.get(2)?,
                author: row.get(3)?,
                page_index: row.get(4)?,
                file_path: row.get(5)?,
                created_at: row.get(6)?,
            })
        })?;
        let mut records = Vec::new();
        for record in rows {
            records.push(record?);
        }
        Ok(records)
    }

    pub fn delete_download(&self, id: i64) -> Result<(), StoreError> {
        let conn = self.lock();
        conn.execute("DELETE FROM downloads WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn stats(&self) -> Result<StoreStats, StoreError> {
        let conn = self.lock();
        // Both counters in one statement: no table name is ever formatted in.
        let (history_entries, download_entries): (i64, i64) = conn.query_row(
            "SELECT (SELECT COUNT(*) FROM view_history), (SELECT COUNT(*) FROM downloads)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok(StoreStats {
            history_entries: u64::try_from(history_entries).unwrap_or(0),
            download_entries: u64::try_from(download_entries).unwrap_or(0),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn store() -> Store {
        Store::open_in_memory().expect("in-memory store")
    }

    fn words(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    fn download(illust_id: &str, page_index: u32, created_at: i64) -> DownloadRecord {
        DownloadRecord {
            id: 0,
            illust_id: illust_id.to_string(),
            title: format!("title-{illust_id}"),
            author: "artist".to_string(),
            page_index,
            file_path: format!("C:/downloads/{illust_id}_p{page_index}.png"),
            created_at,
        }
    }

    fn view(illust_id: &str, viewed_at: i64) -> HistoryEntry {
        HistoryEntry {
            illust_id: illust_id.to_string(),
            title: format!("viewed-{illust_id}"),
            author: "artist".to_string(),
            thumb_url: format!("https://example.invalid/{illust_id}_p0.jpg"),
            viewed_at,
        }
    }

    #[test]
    fn schema_version_is_one_after_open() {
        let s = store();
        assert_eq!(s.schema_version().unwrap(), 1);

        let fresh = crate::Store::open_in_memory().unwrap();
        assert_eq!(fresh.schema_version().unwrap(), 1);
    }

    #[test]
    fn search_history_is_newest_first_and_deduplicated() {
        let s = store();
        for word in ["alpha", "beta", "gamma"] {
            s.push_search(word).unwrap();
        }
        assert_eq!(
            s.search_history(10).unwrap(),
            words(&["gamma", "beta", "alpha"])
        );

        // Re-searching an existing word moves it to the front without duplicating.
        s.push_search("alpha").unwrap();
        assert_eq!(
            s.search_history(10).unwrap(),
            words(&["alpha", "gamma", "beta"])
        );
        assert_eq!(s.search_history(100).unwrap().len(), 3);

        // Limits are honoured, including the degenerate ones.
        assert_eq!(s.search_history(2).unwrap(), words(&["alpha", "gamma"]));
        assert!(s.search_history(0).unwrap().is_empty());

        s.clear_search_history().unwrap();
        assert!(s.search_history(10).unwrap().is_empty());
    }

    #[test]
    fn push_search_ignores_blank_words_and_trims() {
        let s = store();
        for blank in ["", "   ", "\t\n ", "\r\n"] {
            s.push_search(blank).unwrap();
        }
        assert!(s.search_history(10).unwrap().is_empty());

        s.push_search("  初音ミク  ").unwrap();
        assert_eq!(s.search_history(10).unwrap(), words(&["初音ミク"]));
    }

    #[test]
    fn record_view_upserts_on_illust_id() {
        let s = store();
        let first = view("100", 1_000);
        s.record_view(&first).unwrap();

        let later = HistoryEntry {
            title: "updated title".to_string(),
            thumb_url: "https://example.invalid/updated.jpg".to_string(),
            viewed_at: 2_000,
            ..first.clone()
        };
        s.record_view(&later).unwrap();

        let rows = s.view_history(10).unwrap();
        assert_eq!(rows.len(), 1, "repeat views must not duplicate rows");
        assert_eq!(rows[0].illust_id, "100");
        assert_eq!(rows[0].viewed_at, 2_000);
        assert_eq!(rows[0].title, "updated title");
        assert_eq!(rows[0].thumb_url, "https://example.invalid/updated.jpg");

        // A different illust is a new row, ordered newest first.
        let other = view("200", 3_000);
        s.record_view(&other).unwrap();
        assert_eq!(s.view_history(10).unwrap(), vec![other, later]);
        assert_eq!(s.view_history(1).unwrap().len(), 1);

        s.clear_view_history().unwrap();
        assert!(s.view_history(10).unwrap().is_empty());
    }

    #[test]
    fn settings_roundtrip_with_quotes_and_newlines() {
        let s = store();
        let messy = "line1\nline2 \"double\" 'single'\n\ttabbed \\ backslash ; -- #";
        s.set_setting("profile.bio", messy).unwrap();
        assert_eq!(
            s.get_setting("profile.bio").unwrap().as_deref(),
            Some(messy)
        );

        // Setting the same key twice replaces, never duplicates.
        s.set_setting("profile.bio", "replaced").unwrap();
        assert_eq!(
            s.get_setting("profile.bio").unwrap().as_deref(),
            Some("replaced")
        );

        assert_eq!(s.get_setting("missing.key").unwrap(), None);

        s.remove_setting("profile.bio").unwrap();
        assert_eq!(s.get_setting("profile.bio").unwrap(), None);
        // Removing an absent key is a no-op, not an error.
        s.remove_setting("profile.bio").unwrap();
    }

    #[test]
    fn untrusted_text_is_bound_not_interpolated() {
        let s = store();
        let nasty_word = "'); DROP TABLE search_history; --";
        s.push_search(nasty_word).unwrap();
        assert_eq!(s.search_history(10).unwrap(), words(&[nasty_word]));

        let nasty_key = "k'); DROP TABLE settings; --";
        s.set_setting(nasty_key, nasty_word).unwrap();
        assert_eq!(
            s.get_setting(nasty_key).unwrap().as_deref(),
            Some(nasty_word)
        );

        s.record_view(&HistoryEntry {
            illust_id: "1' OR '1'='1".to_string(),
            title: "x'); DELETE FROM view_history; --".to_string(),
            ..HistoryEntry::default()
        })
        .unwrap();
        assert_eq!(s.view_history(10).unwrap().len(), 1);
        assert_eq!(s.stats().unwrap().history_entries, 1);
    }

    #[test]
    fn duplicate_download_pair_is_rejected() {
        let s = store();
        let first_id = s.insert_download(&download("111", 0, 100)).unwrap();
        assert!(first_id > 0);

        let err = s.insert_download(&download("111", 0, 999)).unwrap_err();
        match err {
            StoreError::Sqlite(message) => {
                assert!(message.contains("UNIQUE"), "unexpected message: {message}");
                assert!(
                    message.contains("illust_id"),
                    "unexpected message: {message}"
                );
            }
            other => panic!("expected a UNIQUE violation, got {other:?}"),
        }
        assert_eq!(
            s.downloads(10).unwrap().len(),
            1,
            "rejected insert must not add a row"
        );

        // Same illust, different page is allowed.
        let second_id = s.insert_download(&download("111", 1, 101)).unwrap();
        assert_ne!(first_id, second_id);
        assert_eq!(s.downloads(10).unwrap().len(), 2);
    }

    #[test]
    fn downloads_are_newest_first_and_deletable() {
        let s = store();
        s.insert_download(&download("a", 0, 100)).unwrap();
        let id_b = s.insert_download(&download("b", 0, 300)).unwrap();
        let id_c = s.insert_download(&download("c", 0, 200)).unwrap();

        let rows = s.downloads(10).unwrap();
        let order: Vec<&str> = rows.iter().map(|r| r.illust_id.as_str()).collect();
        assert_eq!(order, ["b", "c", "a"]);
        assert_eq!(rows[0].id, id_b);
        assert_eq!(rows[0].page_index, 0);
        assert_eq!(rows[2].file_path, "C:/downloads/a_p0.png");
        assert_eq!(s.downloads(1).unwrap().len(), 1);
        assert!(s.downloads(0).unwrap().is_empty());

        s.delete_download(id_c).unwrap();
        let order: Vec<String> = s
            .downloads(10)
            .unwrap()
            .into_iter()
            .map(|r| r.illust_id)
            .collect();
        assert_eq!(order, words(&["b", "a"]));
        // Deleting an unknown id is a no-op, not an error.
        s.delete_download(9_999).unwrap();
        assert_eq!(s.downloads(10).unwrap().len(), 2);
    }

    #[test]
    fn stats_counts_rows() {
        let s = store();
        let empty = s.stats().unwrap();
        assert_eq!(empty.history_entries, 0);
        assert_eq!(empty.download_entries, 0);

        s.record_view(&view("1", 10)).unwrap();
        s.record_view(&view("1", 20)).unwrap();
        s.record_view(&view("2", 30)).unwrap();
        s.push_search("ignored by stats").unwrap();
        s.insert_download(&download("1", 0, 10)).unwrap();
        s.insert_download(&download("1", 1, 11)).unwrap();

        let stats = s.stats().unwrap();
        assert_eq!(stats.history_entries, 2);
        assert_eq!(stats.download_entries, 2);
    }

    #[test]
    fn data_survives_close_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("deeper").join("pixiv.db");

        {
            let s = Store::open(&path).unwrap();
            assert_eq!(s.schema_version().unwrap(), 1);
            s.push_search("初音ミク").unwrap();
            s.push_search("blue archive").unwrap();
            s.record_view(&view("42", 1_700_000_000)).unwrap();
            s.set_setting("theme", "dark\n\"solid\"").unwrap();
            s.insert_download(&download("42", 0, 1_700_000_001))
                .unwrap();
        }

        assert!(path.exists(), "database file must be created");

        let reopened = Store::open(&path).unwrap();
        assert_eq!(
            reopened.schema_version().unwrap(),
            1,
            "migrations stay idempotent"
        );
        assert_eq!(
            reopened.search_history(10).unwrap(),
            words(&["blue archive", "初音ミク"])
        );
        assert_eq!(
            reopened.view_history(10).unwrap(),
            vec![view("42", 1_700_000_000)]
        );
        assert_eq!(
            reopened.get_setting("theme").unwrap().as_deref(),
            Some("dark\n\"solid\"")
        );
        assert_eq!(reopened.downloads(10).unwrap().len(), 1);
        assert_eq!(reopened.stats().unwrap().download_entries, 1);

        // Re-opening a populated database twice must not duplicate anything.
        drop(Store::open(&path).unwrap());
        assert_eq!(Store::open(&path).unwrap().downloads(10).unwrap().len(), 1);
    }

    #[test]
    fn file_database_uses_wal_foreign_keys_and_busy_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("pragmas.db")).unwrap();
        let conn = store.lock();

        let journal_mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode.to_lowercase(), "wal");

        let foreign_keys: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        assert_eq!(foreign_keys, 1);

        let busy_timeout: i64 = conn
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .unwrap();
        assert_eq!(busy_timeout, 5_000);
    }

    #[test]
    fn store_is_cloneable_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Store>();

        let s = store();
        let mut handles = Vec::new();
        for worker in 0..4 {
            let clone = s.clone();
            handles.push(thread::spawn(move || {
                for item in 0..10 {
                    clone.push_search(&format!("word-{worker}-{item}")).unwrap();
                    clone
                        .set_setting(&format!("key-{worker}-{item}"), "value")
                        .unwrap();
                    clone
                        .record_view(&super::tests::view(&format!("{worker}-{item}"), item))
                        .unwrap();
                }
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(s.search_history(1_000).unwrap().len(), 40);
        assert_eq!(s.view_history(1_000).unwrap().len(), 40);
        assert_eq!(s.stats().unwrap().history_entries, 40);
        assert_eq!(s.get_setting("key-3-9").unwrap().as_deref(), Some("value"));
        // The only shared value: the last writer wins, but nothing is corrupt.
        assert_eq!(s.schema_version().unwrap(), 1);
    }
}
