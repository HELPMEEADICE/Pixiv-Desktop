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

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::{Mutex, MutexGuard};
use rusqlite::{CachedStatement, Connection, OptionalExtension, params};
use thiserror::Error;

mod novel;

pub use novel::{NovelHistoryEntry, NovelProgress, RecentNovel};

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

// ---------------------------------------------------------------------------
// The statements on the hot paths
// ---------------------------------------------------------------------------
//
// One copy each, named, because the prepared-statement cache is keyed by the SQL
// text and the query plan is a fact about that text: a query written out twice
// would be compiled twice, and G34's EXPLAIN QUERY PLAN assertion would then be
// asserting about a different string from the one the store runs.

const SQL_SEARCH_HISTORY: &str = "SELECT word FROM search_history ORDER BY seq DESC LIMIT ?1";
const SQL_SEARCH_NEXT_SEQ: &str = "SELECT COALESCE(MAX(seq), 0) + 1 FROM search_history";
const SQL_PUSH_SEARCH: &str =
    "INSERT INTO search_history (word, searched_at, seq) VALUES (?1, ?2, ?3)
     ON CONFLICT(word) DO UPDATE SET searched_at = excluded.searched_at, seq = excluded.seq";
const SQL_VIEW_NEXT_SEQ: &str = "SELECT COALESCE(MAX(seq), 0) + 1 FROM view_history";
const SQL_RECORD_VIEW: &str =
    "INSERT INTO view_history (illust_id, title, author, thumb_url, viewed_at, seq)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6)
     ON CONFLICT(illust_id) DO UPDATE SET
         title     = excluded.title,
         author    = excluded.author,
         thumb_url = excluded.thumb_url,
         viewed_at = excluded.viewed_at,
         seq       = excluded.seq";
const SQL_VIEW_HISTORY: &str = "SELECT illust_id, title, author, thumb_url, viewed_at
     FROM view_history
     ORDER BY viewed_at DESC, seq DESC
     LIMIT ?1";
const SQL_DOWNLOADS: &str = "SELECT id, illust_id, title, author, page_index, file_path, created_at
     FROM downloads
     ORDER BY created_at DESC, id DESC
     LIMIT ?1";

/// How many rows each history list keeps.
///
/// A history is a shortlist, not an archive (C7): every list here is read with a
/// `LIMIT` well under this, a table that only ever grows makes [`Store::stats`]
/// grow with it, and every `MAX(seq)` in the schema reads a wider index. The cap
/// is applied when a row is written, so the table never exceeds it and no sweep
/// has to run on a timer.
pub const HISTORY_RETENTION: usize = 500;

/// Keep everything but the oldest rows beyond [`HISTORY_RETENTION`].
///
/// `seq` is monotonic per table, so "older than the newest N" is one comparison
/// against `MAX(seq)`. A table shorter than the cap deletes nothing: the
/// right-hand side is not positive yet, and `seq` starts at 1.
const SQL_PRUNE_SEARCH: &str =
    "DELETE FROM search_history WHERE seq <= (SELECT MAX(seq) FROM search_history) - ?1";
const SQL_PRUNE_VIEW: &str =
    "DELETE FROM view_history WHERE seq <= (SELECT MAX(seq) FROM view_history) - ?1";

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

/// Migration step 2: the sequence indexes.
///
/// `push_search` and `record_view` each read `MAX(seq)` to compute the next
/// sequence, and `search_history()` orders by `seq DESC`; without an index all
/// three read the whole table. `IF NOT EXISTS` keeps the step re-runnable: a
/// database whose version row was lost (or a copy restored from a backup) can be
/// upgraded again without the batch failing on an index that is already there.
const MIGRATION_STEP_2: &str = r#"
CREATE INDEX IF NOT EXISTS idx_search_history_seq ON search_history (seq DESC);
CREATE INDEX IF NOT EXISTS idx_view_history_seq ON view_history (seq DESC);
"#;

/// Migration step 3: novel reading progress and the recently-read list.
///
/// Forward-only like every other step: the two tables are new, nothing existing
/// is touched, so a build that predates them keeps opening the same file.
///
/// `series_id` defaults to the empty string rather than NULL: a novel outside a
/// series has no id, and one representation for "no series" keeps the index on
/// the column usable and the comparisons simple.
const MIGRATION_STEP_3: &str = r#"
CREATE TABLE IF NOT EXISTS novel_progress (
    novel_id    TEXT    NOT NULL PRIMARY KEY,
    series_id   TEXT    NOT NULL DEFAULT '',
    block_index INTEGER NOT NULL,
    char_offset INTEGER NOT NULL,
    percent     REAL    NOT NULL,
    finished    INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS novel_history (
    novel_id  TEXT    NOT NULL PRIMARY KEY,
    title     TEXT    NOT NULL,
    author    TEXT    NOT NULL,
    cover     TEXT    NOT NULL,
    series_id TEXT    NOT NULL DEFAULT '',
    seq       INTEGER NOT NULL,
    read_at   INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_novel_history_seq ON novel_history (seq DESC);
CREATE INDEX IF NOT EXISTS idx_novel_progress_series ON novel_progress (series_id);
"#;

/// Migration step 4: the series progress index gains its sort key (C9).
///
/// `series_progress` asks "where did the reader stop in this series, newest
/// first", and an index on `series_id` alone answers only the first half: SQLite
/// seeks the two rows and then sorts them, which is a temp B-tree over the whole
/// series. Widening the same index to `(series_id, updated_at DESC)` makes the
/// sort a read of the index it already opened. The pair is re-stated under one
/// name rather than added beside it because two indexes over the same column list
/// would both be maintained on every save and only one of them read; the batch is
/// still re-runnable (`DROP ... IF EXISTS` then `CREATE ... IF NOT EXISTS`), which
/// is the property step 2 and step 3 also keep. No row is touched.
const MIGRATION_STEP_4: &str = r#"
DROP INDEX IF EXISTS idx_novel_progress_series;
CREATE INDEX IF NOT EXISTS idx_novel_progress_series ON novel_progress (series_id, updated_at DESC);
"#;

/// Ordered migration steps; entry `i` upgrades the schema to version `i + 1`.
const MIGRATIONS: &[&str] = &[
    MIGRATION_STEP_1,
    MIGRATION_STEP_2,
    MIGRATION_STEP_3,
    MIGRATION_STEP_4,
];

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
        cache: Arc::new(StatementCache::default()),
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

/// The prepared-statement cache, plus the count O9 is measured by.
///
/// `rusqlite` already keeps compiled statements on the connection
/// (`Connection::prepare_cached`), keyed by SQL text. It does not report whether a
/// call was a hit, and O9's acceptance is a number, so the store keeps the mirror
/// image of that cache: the set of texts this connection has already compiled, and
/// a counter of the misses. The two cannot drift apart here because every text is a
/// constant, the cache holds more statements than this crate has, and the only
/// thing that empties a rusqlite statement cache is a schema change - which happens
/// in `apply_migrations`, before any `Store` exists to ask a question through.
#[derive(Debug, Default)]
struct StatementCache {
    compiled: Mutex<BTreeSet<&'static str>>,
    compiles: AtomicU64,
}

impl StatementCache {
    /// Record one request for `sql`; `true` when it has to be compiled.
    fn miss(&self, sql: &'static str) -> bool {
        if !self.compiled.lock().insert(sql) {
            return false;
        }
        self.compiles.fetch_add(1, Ordering::Relaxed);
        true
    }
}

/// The local database.
#[derive(Clone, Debug)]
pub struct Store {
    inner: Arc<Mutex<Connection>>,
    /// Compiled statements, and how many compiles there were.
    cache: Arc<StatementCache>,
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

    /// One statement, from the cache when it is already compiled.
    ///
    /// Every query on a hot path goes through here, which is why the count is the
    /// measurement O9 asks for: a second call with the same SQL text costs no
    /// compile at all.
    fn cached<'a>(
        &self,
        conn: &'a Connection,
        sql: &'static str,
    ) -> Result<CachedStatement<'a>, StoreError> {
        if self.cache.miss(sql) {
            tracing::trace!(sql, "已编译并缓存一条语句");
        }
        Ok(conn.prepare_cached(sql)?)
    }

    /// Statements this store has compiled. Test-only: O9's unit.
    #[cfg(test)]
    pub(crate) fn compiles(&self) -> u64 {
        self.cache.compiles.load(Ordering::Relaxed)
    }

    // ---- search history -------------------------------------------------

    /// Most recent search words, newest first, de-duplicated.
    pub fn search_history(&self, limit: usize) -> Result<Vec<String>, StoreError> {
        let conn = self.lock();
        let mut stmt = self.cached(&conn, SQL_SEARCH_HISTORY)?;
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
        let next_seq: i64 = self
            .cached(&conn, SQL_SEARCH_NEXT_SEQ)?
            .query_row([], |row| row.get(0))?;
        self.cached(&conn, SQL_PUSH_SEARCH)?
            .execute(params![word, now_secs(), next_seq])?;
        // Retention (C7): the row just written is the newest one, so the cap is
        // applied on the write path instead of on a timer.
        self.cached(&conn, SQL_PRUNE_SEARCH)?
            .execute(params![limit_to_i64(HISTORY_RETENTION)])?;
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
        let next_seq: i64 = self
            .cached(&conn, SQL_VIEW_NEXT_SEQ)?
            .query_row([], |row| row.get(0))?;
        self.cached(&conn, SQL_RECORD_VIEW)?.execute(params![
            entry.illust_id,
            entry.title,
            entry.author,
            entry.thumb_url,
            entry.viewed_at,
            next_seq,
        ])?;
        // Retention (C7), the same clause `push_search` applies.
        self.cached(&conn, SQL_PRUNE_VIEW)?
            .execute(params![limit_to_i64(HISTORY_RETENTION)])?;
        Ok(())
    }

    pub fn view_history(&self, limit: usize) -> Result<Vec<HistoryEntry>, StoreError> {
        let conn = self.lock();
        let mut stmt = self.cached(&conn, SQL_VIEW_HISTORY)?;
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
        let mut stmt = self.cached(&conn, SQL_DOWNLOADS)?;
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

    /// The plan SQLite actually uses, as one printable line per row.
    ///
    /// `args` are bound, because a plan for a statement with a parameter cannot be
    /// obtained without one: SQLite refuses to explain an unbound `LIMIT`.
    fn plan_of(conn: &Connection, sql: &str, args: &[i64]) -> String {
        let mut stmt = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .expect("query plan");
        let rows = stmt
            .query_map(rusqlite::params_from_iter(args.iter()), |row| {
                row.get::<_, String>(3)
            })
            .expect("plan rows");
        rows.map(|row| row.expect("plan detail"))
            .collect::<Vec<_>>()
            .join(" | ")
    }

    /// O9: the second write-then-read of the same shape compiles nothing.
    ///
    /// Every query on this path goes through the prepared-statement cache, so the
    /// three texts are compiled once per store and then reused. The first round
    /// trip is the control: without it a counter stuck at zero would pass.
    #[test]
    fn a_second_history_write_and_read_compiles_no_statement() {
        let s = store();
        s.record_view(&view("1", 10)).unwrap();
        assert_eq!(s.view_history(5).unwrap().len(), 1);
        let warm = s.compiles();
        assert_eq!(
            warm,
            4,
            "MAX(seq), the insert, the retention sweep and the read, once each"
        );

        s.record_view(&view("2", 20)).unwrap();
        assert_eq!(s.view_history(5).unwrap().len(), 2);
        assert_eq!(
            s.compiles(),
            warm,
            "the second round trip asked for the same three texts"
        );

        // The counter is not stuck: a text the store has not seen before costs one.
        s.push_search("alpha").unwrap();
        assert_eq!(
            s.compiles(),
            warm + 3,
            "MAX(seq), the insert and its sweep, once each"
        );
        s.push_search("beta").unwrap();
        assert_eq!(
            s.compiles(),
            warm + 3,
            "the same three texts, already compiled"
        );
    }

    /// C7: a history keeps its last [`HISTORY_RETENTION`] rows and no more.
    ///
    /// The negative experiment is deleting the `SQL_PRUNE_*` call from the write
    /// path: both lists then come back with all 620 rows, `stats()` with them, and
    /// the three assertions below read 620 where they want 500.
    #[test]
    fn a_history_keeps_the_last_five_hundred_rows() {
        let s = store();
        let writes = HISTORY_RETENTION + 120;
        for index in 0..writes {
            s.push_search(&format!("word-{index}")).unwrap();
            s.record_view(&view(&index.to_string(), i64::try_from(index).unwrap()))
                .unwrap();
        }
        assert_eq!(HISTORY_RETENTION, 500);

        // Newest first, and the oldest 120 rows are gone.
        let views = s.view_history(writes).unwrap();
        assert_eq!(views.len(), HISTORY_RETENTION);
        assert_eq!(views[0].illust_id, (writes - 1).to_string());
        assert_eq!(views[HISTORY_RETENTION - 1].illust_id, "120");

        let words = s.search_history(writes).unwrap();
        assert_eq!(words.len(), HISTORY_RETENTION);
        assert_eq!(words[0], format!("word-{}", writes - 1));

        // `stats()` counts the same table, so the cap bounds it too.
        assert_eq!(s.stats().unwrap().history_entries, 500);
    }

    /// The retention clause has the shape G34 asks of the hot queries: a bounded
    /// walk of the sequence index, not a read of the table it just wrote.
    ///
    /// The negative experiment is dropping `idx_view_history_seq` (or its
    /// `search_history` twin): the plan names `SCAN <table>` and this fails.
    #[test]
    fn the_retention_sweep_reads_the_sequence_index() {
        let s = store();
        for index in 0..8 {
            s.push_search(&format!("word-{index}")).unwrap();
            s.record_view(&view(&index.to_string(), index)).unwrap();
        }
        let conn = s.lock();
        for (table, sql, index_name) in [
            ("search_history", SQL_PRUNE_SEARCH, "idx_search_history_seq"),
            ("view_history", SQL_PRUNE_VIEW, "idx_view_history_seq"),
        ] {
            let plan = plan_of(&conn, sql, &[limit_to_i64(HISTORY_RETENTION)]);
            assert!(
                plan.contains(&format!(
                    "SEARCH {table} USING COVERING INDEX {index_name} (seq<?)"
                )),
                "{table}: {plan}"
            );
            let full_scan = plan
                .split(" | ")
                .any(|part| part.trim_start().starts_with("SCAN ") && !part.contains("USING"));
            assert!(!full_scan, "{table} reads the whole table: {plan}");
        }
    }

    /// G34: the three hot queries read an index, not a table.
    ///
    /// Each is a `LIMIT` list ordered by a column the schema indexes, so a bare
    /// `SCAN <table>` here means the index is gone and the list is a full read plus
    /// a sort. Measured at this commit: `search_history` walks
    /// `idx_search_history_seq`, and both `view_history` and `downloads` walk their
    /// `created_at`/`viewed_at` index and then sort the *last* ORDER BY term with a
    /// temp b-tree (`seq DESC`, `id DESC`) - the tie-break is not covered by a
    /// composite index. That is a sort of one page of rows, not a scan of the table,
    /// and G34 asks for the scan; a step-4 migration adding
    /// `(viewed_at DESC, seq DESC)` and `(created_at DESC, id DESC)` would remove
    /// the sort as well, and it is deliberately not in this commit because it moves
    /// the schema version, which another track's test pins.
    ///
    /// The negative experiment is dropping `idx_downloads_created_at` (or its
    /// `view_history` twin): the plan names the scan and this fails.
    #[test]
    fn the_three_hot_queries_never_scan_a_table() {
        let s = store();
        for index in 0..64 {
            s.push_search(&format!("word-{index}")).unwrap();
            s.record_view(&view(&index.to_string(), index)).unwrap();
            s.insert_download(&download(&index.to_string(), 0, index))
                .unwrap();
        }
        let conn = s.lock();
        for (name, sql) in [
            ("search_history", SQL_SEARCH_HISTORY),
            ("view_history", SQL_VIEW_HISTORY),
            ("downloads", SQL_DOWNLOADS),
        ] {
            let plan = plan_of(&conn, sql, &[20]);
            // SQLite calls an ordered walk of an index a SCAN too, and for a
            // `LIMIT` list that is exactly the plan wanted: it stops after the
            // limit. What must not appear is the same line without a `USING`,
            // which is a full read of the table.
            let full_scan = plan
                .split(" | ")
                .any(|part| part.trim_start().starts_with("SCAN ") && !part.contains("USING"));
            assert!(!full_scan, "{name} reads the whole table: {plan}");
            assert!(
                !plan.contains("USE TEMP B-TREE FOR ORDER BY"),
                "{name} sorts the table: {plan}"
            );
        }
    }
    /// migration step 2 both said `SCAN search_history`: the next sequence was
    /// found by walking every row on each search and each view.
    #[test]
    fn the_sequence_queries_use_an_index() {
        let s = store();
        s.push_search("alpha").unwrap();
        s.record_view(&view("1", 10)).unwrap();
        let conn = s.lock();

        // SQLite turns `MAX(seq)` into a single seek of the index, which is why
        // the plan reads SEARCH and not SCAN.
        let search_seq = plan_of(
            &conn,
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM search_history",
            &[],
        );
        assert!(
            search_seq
                .contains("SEARCH search_history USING COVERING INDEX idx_search_history_seq"),
            "{search_seq}"
        );
        assert!(!search_seq.contains("SCAN"), "{search_seq}");

        let view_seq = plan_of(
            &conn,
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM view_history",
            &[],
        );
        assert!(
            view_seq.contains("SEARCH view_history USING COVERING INDEX idx_view_history_seq"),
            "{view_seq}"
        );
        assert!(!view_seq.contains("SCAN"), "{view_seq}");

        // The recent-search list is served in index order instead of sorted.
        let recent = plan_of(
            &conn,
            "SELECT word FROM search_history ORDER BY seq DESC LIMIT 5",
            &[],
        );
        assert!(
            recent.contains("USING INDEX idx_search_history_seq"),
            "{recent}"
        );
        assert!(!recent.contains("USE TEMP B-TREE FOR ORDER BY"), "{recent}");
    }

    /// A database restored from a backup can carry the index without the version
    /// row, so the step has to be re-runnable rather than merely run once.
    #[test]
    fn the_sequence_migration_can_run_twice() {
        let s = store();
        s.push_search("alpha").unwrap();
        {
            let conn = s.lock();
            conn.execute_batch(MIGRATION_STEP_2)
                .expect("re-applying the step is a no-op");
            conn.execute_batch(MIGRATION_STEP_2)
                .expect("and so is a third application");
        }
        assert_eq!(s.search_history(10).unwrap(), words(&["alpha"]));
        assert_eq!(s.schema_version().unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn schema_version_tracks_the_migration_list() {
        // Four steps today: the tables, the sequence indexes, the novel reading
        // tables, then the series index's sort key (C9).
        assert_eq!(SCHEMA_VERSION, 4);
        let s = store();
        assert_eq!(s.schema_version().unwrap(), SCHEMA_VERSION);

        let fresh = crate::Store::open_in_memory().unwrap();
        assert_eq!(fresh.schema_version().unwrap(), SCHEMA_VERSION);
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

    /// F1: the local download record in one round trip - insert, list, count,
    /// delete - including the pair the table refuses twice and the limit the list
    /// honours.
    ///
    /// The downloads page grew its "已完成（本地记录）" band this round and every row
    /// on it is a row of this table, so the four calls are asserted as one path
    /// rather than one per function: a caller that only ever saw three of them
    /// green is how `stats` stayed uncalled for four rounds.
    #[test]
    fn f1_a_local_download_record_roundtrips_with_its_unique_pair_and_limit() {
        let s = store();

        // Empty table: no band, and the counters agree.
        assert!(s.downloads(50).unwrap().is_empty());
        assert_eq!(s.stats().unwrap().download_entries, 0);

        let first = s
            .insert_download(&download("700000001", 0, 1_700_000_000))
            .unwrap();
        s.insert_download(&download("700000001", 1, 1_700_000_001))
            .unwrap();
        s.insert_download(&download("700000002", 0, 1_700_000_002))
            .unwrap();

        // The UNIQUE pair: the same page of the same work is one record, and the
        // rejection is a named error rather than a silent second row.
        let duplicate = s
            .insert_download(&download("700000001", 0, 1_700_000_003))
            .unwrap_err();
        assert!(
            matches!(&duplicate, StoreError::Sqlite(message) if message.contains("UNIQUE")),
            "{duplicate:?}"
        );
        assert_eq!(s.downloads(50).unwrap().len(), 3);
        assert_eq!(s.stats().unwrap().download_entries, 3);

        // Newest first, and the limit is applied by SQLite rather than by the
        // caller who asked for it.
        let newest = s.downloads(1).unwrap();
        assert_eq!(newest.len(), 1);
        assert_eq!(newest[0].illust_id, "700000002");
        let order: Vec<String> = s
            .downloads(50)
            .unwrap()
            .into_iter()
            .map(|row| format!("{}:{}", row.illust_id, row.page_index))
            .collect();
        assert_eq!(order, words(&["700000002:0", "700000001:1", "700000001:0"]));
        assert!(s.downloads(0).unwrap().is_empty(), "a zero limit is empty");

        // The row that comes back is the row that went in, field for field.
        let stored = &s
            .downloads(50)
            .unwrap()
            .into_iter()
            .find(|row| row.id == first)
            .expect("the first insert is still listed");
        assert_eq!(stored.title, "title-700000001");
        assert_eq!(stored.author, "artist");
        assert_eq!(stored.file_path, "C:/downloads/700000001_p0.png");
        assert_eq!(stored.created_at, 1_700_000_000);

        // Deleting a record is what "忘记这条记录" does; the file is not touched.
        s.delete_download(first).unwrap();
        assert_eq!(s.downloads(50).unwrap().len(), 2);
        assert_eq!(s.stats().unwrap().download_entries, 2);
        s.delete_download(first).unwrap();
        assert_eq!(
            s.stats().unwrap().download_entries,
            2,
            "delete twice is fine"
        );
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

    /// A database exactly as the previous build left it: the schema at version 2,
    /// with real rows in it.
    fn database_at_version_two(path: &std::path::Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE schema_version (
                singleton INTEGER NOT NULL PRIMARY KEY CHECK (singleton = 1),
                version   INTEGER NOT NULL
            );
            INSERT INTO schema_version (singleton, version) VALUES (1, 2);
            "#,
        )
        .unwrap();
        conn.execute_batch(MIGRATION_STEP_1).unwrap();
        conn.execute_batch(MIGRATION_STEP_2).unwrap();
        conn.execute(
            "INSERT INTO view_history (illust_id, title, author, thumb_url, viewed_at, seq)
             VALUES ('42', 'old title', 'artist', 'thumb.jpg', 1000, 1)",
            [],
        )
        .unwrap();
        conn.execute(
            r#"INSERT INTO settings ("key", "value") VALUES ('theme', 'sepia')"#,
            [],
        )
        .unwrap();
    }

    /// Step 3 is additive, so a file written by the previous build upgrades in
    /// place: the rows are still there and the new tables appear alongside them.
    #[test]
    fn upgrading_from_version_two_keeps_the_existing_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v2.db");
        database_at_version_two(&path);

        // The fixture really is a version-2 file with no novel tables.
        {
            let conn = Connection::open(&path).unwrap();
            let version: i64 = conn
                .query_row(
                    "SELECT version FROM schema_version WHERE singleton = 1",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(version, 2);
            let novel_tables: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master
                     WHERE type = 'table' AND name LIKE 'novel_%'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(novel_tables, 0, "step 3 must be what creates them");
        }

        let store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), 4);
        assert_eq!(store.view_history(10).unwrap().len(), 1);
        assert_eq!(store.view_history(10).unwrap()[0].title, "old title");
        assert_eq!(
            store.get_setting("theme").unwrap().as_deref(),
            Some("sepia")
        );

        // The new tables are usable straight away.
        store
            .save_novel_progress(&NovelProgress {
                novel_id: "1".to_string(),
                percent: 0.5,
                ..NovelProgress::default()
            })
            .unwrap();
        assert!(store.novel_progress("1").unwrap().is_some());
        store
            .record_novel_read(&NovelHistoryEntry {
                novel_id: "1".to_string(),
                title: "novel".to_string(),
                ..NovelHistoryEntry::default()
            })
            .unwrap();
        assert_eq!(store.novel_history(10).unwrap().len(), 1);
        assert_eq!(store.novel_stats().unwrap(), (1, 1));

        // Opening it again must not re-run the step or disturb anything.
        drop(store);
        let reopened = Store::open(&path).unwrap();
        assert_eq!(reopened.schema_version().unwrap(), 4);
        assert_eq!(reopened.view_history(10).unwrap().len(), 1);
        assert_eq!(reopened.novel_stats().unwrap(), (1, 1));
    }

    #[test]
    fn data_survives_close_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("deeper").join("pixiv.db");

        {
            let s = Store::open(&path).unwrap();
            assert_eq!(s.schema_version().unwrap(), SCHEMA_VERSION);
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
            SCHEMA_VERSION,
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
        assert_eq!(s.schema_version().unwrap(), SCHEMA_VERSION);
    }
}