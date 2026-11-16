//! Novel reading progress, the recently-read list, and reader settings.
//!
//! # The rule this module enforces
//!
//! Position is the one piece of state a reader notices losing. A save is an
//! upsert keyed by `novel_id`, so the reader can write after every scroll pause
//! without the table growing, and every write stamps `updated_at` from the
//! database side rather than trusting the caller's clock.
//!
//! # Why progress and history are separate tables
//!
//! They answer different questions and have different lifetimes. Progress is a
//! position: exactly one row per novel, overwritten, read when the novel opens.
//! History is a log of *when* the reader last touched a novel, ordered by a
//! monotonic `seq` so "newest first" stays exact inside one second — the same
//! rule `view_history` uses for illustrations. Clearing the "recently read" list
//! must not throw away where someone stopped, and resetting a position must not
//! erase the fact that they read it, so the two never share a row.
//!
//! # Settings reuse
//!
//! Reader appearance lives in the existing `settings` table under a `novel.`
//! prefix instead of a table of its own: a settings page that clears everything
//! then still catches the reader preferences, and a key/value row is exactly
//! what a JSON blob needs.

use rusqlite::{OptionalExtension, Row, params};

use crate::{HISTORY_RETENTION, Store, StoreError, limit_to_i64, now_secs};

/// Every key this module writes into `settings` starts with this.
pub const SETTING_PREFIX: &str = "novel.";

/// Keep everything but the oldest rows beyond [`HISTORY_RETENTION`].
///
/// The recently-read list is a shortlist like the two illustration histories, and
/// it grows on the same `seq` rule, so it is capped by the same clause (C7).
const SQL_PRUNE_NOVEL: &str =
    "DELETE FROM novel_history WHERE seq <= (SELECT MAX(seq) FROM novel_history) - ?1";

/// Where the reader stopped in one novel.
///
/// `block_index` indexes the flat block list the parser produces and
/// `char_offset` indexes the characters inside that block; both are stored so a
/// later parser change to one block cannot silently move the position in the
/// rest of the document. `percent` is kept as given — it is what the series
/// view shows and rounding it here would lose the caller's precision.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NovelProgress {
    pub novel_id: String,
    /// Empty when the novel does not belong to a series.
    pub series_id: String,
    pub block_index: u32,
    pub char_offset: u32,
    pub percent: f32,
    pub finished: bool,
    /// Unix seconds, stamped by the store on every save.
    pub updated_at: i64,
}

/// One recently-read row, with where the reader stopped in it (F4).
///
/// The two tables answer different questions (see the module comment), so the
/// join is done once, here, instead of by every caller that wants both: the
/// novels page draws the list *and* the progress rule under each row, and a
/// second query per row would be one SQLite call per painted line.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RecentNovel {
    pub entry: NovelHistoryEntry,
    /// `None` when this novel has never been opened far enough to have a row.
    /// A row with no progress draws no rule rather than a 0 % one.
    pub progress: Option<NovelProgress>,
}

/// One row of the recently-read list.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NovelHistoryEntry {
    pub novel_id: String,
    pub title: String,
    pub author: String,
    pub cover: String,
    /// Empty when the novel does not belong to a series.
    pub series_id: String,
    /// Unix seconds, stamped by the store on every record.
    pub read_at: i64,
}

impl Store {
    /// Where the reader stopped, if this novel has ever been opened.
    pub fn novel_progress(&self, id: &str) -> Result<Option<NovelProgress>, StoreError> {
        let conn = self.lock();
        let progress = conn
            .query_row(
                "SELECT novel_id, series_id, block_index, char_offset, percent, finished, updated_at
                 FROM novel_progress
                 WHERE novel_id = ?1",
                params![id],
                progress_from_row,
            )
            .optional()?;
        Ok(progress)
    }

    /// Save a position, replacing the row for that novel.
    ///
    /// `updated_at` is always stamped with the current time: a caller that
    /// re-saves an old value (a UI undo, a stale widget) must not move the row
    /// back in time.
    pub fn save_novel_progress(&self, progress: &NovelProgress) -> Result<(), StoreError> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO novel_progress
                 (novel_id, series_id, block_index, char_offset, percent, finished, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(novel_id) DO UPDATE SET
                 series_id   = excluded.series_id,
                 block_index = excluded.block_index,
                 char_offset = excluded.char_offset,
                 percent     = excluded.percent,
                 finished    = excluded.finished,
                 updated_at  = excluded.updated_at",
            params![
                progress.novel_id,
                progress.series_id,
                i64::from(progress.block_index),
                i64::from(progress.char_offset),
                f64::from(progress.percent),
                progress.finished,
                now_secs(),
            ],
        )?;
        Ok(())
    }

    /// Every position recorded inside one series, most recently touched first.
    ///
    /// An empty `series_id` is a valid key, not a wildcard: it returns the
    /// standalone novels, which is what a "you were reading" shelf wants when it
    /// is not looking at a series.
    pub fn series_progress(&self, series_id: &str) -> Result<Vec<NovelProgress>, StoreError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT novel_id, series_id, block_index, char_offset, percent, finished, updated_at
             FROM novel_progress
             WHERE series_id = ?1
             ORDER BY updated_at DESC, novel_id ASC",
        )?;
        let rows = stmt.query_map(params![series_id], progress_from_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// The recently-read list, newest first.
    pub fn novel_history(&self, limit: usize) -> Result<Vec<NovelHistoryEntry>, StoreError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT novel_id, title, author, cover, series_id, read_at
             FROM novel_history
             ORDER BY seq DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit_to_i64(limit)], |row| {
            Ok(NovelHistoryEntry {
                novel_id: row.get(0)?,
                title: row.get(1)?,
                author: row.get(2)?,
                cover: row.get(3)?,
                series_id: row.get(4)?,
                read_at: row.get(5)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// The recently-read list, each row joined to its stored position (F4).
    ///
    /// One statement, not one per row: the novels page draws the list and the
    /// progress rule beneath each entry, and `novel_progress` is keyed by the same
    /// `novel_id`, so a LEFT JOIN answers both halves of the row at once. A novel
    /// that was opened but never scrolled has no progress row and comes back with
    /// `None`, which is what "draw no rule" means.
    ///
    /// The order is `novel_history`'s own `seq DESC` - identical to
    /// [`Store::novel_history`] - so the two lists cannot disagree about which
    /// novel is first.
    pub fn recent_rows_with_progress(&self, limit: usize) -> Result<Vec<RecentNovel>, StoreError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT h.novel_id, h.title, h.author, h.cover, h.series_id, h.read_at,
                    p.novel_id, p.series_id, p.block_index, p.char_offset, p.percent,
                    p.finished, p.updated_at
             FROM novel_history AS h
             LEFT JOIN novel_progress AS p ON p.novel_id = h.novel_id
             ORDER BY h.seq DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit_to_i64(limit)], |row| {
            let progress = if row.get::<_, Option<String>>(6)?.is_some() {
                Some(progress_at(row, 6)?)
            } else {
                None
            };
            Ok(RecentNovel {
                entry: NovelHistoryEntry {
                    novel_id: row.get(0)?,
                    title: row.get(1)?,
                    author: row.get(2)?,
                    cover: row.get(3)?,
                    series_id: row.get(4)?,
                    read_at: row.get(5)?,
                },
                progress,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Record that a novel was opened, moving it to the front of the list.
    ///
    /// `seq` is `MAX(seq) + 1` rather than the timestamp for the same reason
    /// `view_history` uses it: two novels opened in the same second must still
    /// come back in the order they were opened.
    pub fn record_novel_read(&self, entry: &NovelHistoryEntry) -> Result<(), StoreError> {
        let conn = self.lock();
        let next_seq: i64 = conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM novel_history",
            [],
            |row| row.get(0),
        )?;
        conn.execute(
            "INSERT INTO novel_history (novel_id, title, author, cover, series_id, seq, read_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(novel_id) DO UPDATE SET
                 title     = excluded.title,
                 author    = excluded.author,
                 cover     = excluded.cover,
                 series_id = excluded.series_id,
                 seq       = excluded.seq,
                 read_at   = excluded.read_at",
            params![
                entry.novel_id,
                entry.title,
                entry.author,
                entry.cover,
                entry.series_id,
                next_seq,
                now_secs(),
            ],
        )?;
        // Retention (C7): the row just written is the newest one.
        conn.execute(SQL_PRUNE_NOVEL, params![limit_to_i64(HISTORY_RETENTION)])?;
        Ok(())
    }

    /// Empty the recently-read list. Positions are deliberately untouched: they
    /// live in `novel_progress` and are reset separately.
    pub fn clear_novel_history(&self) -> Result<(), StoreError> {
        let conn = self.lock();
        conn.execute("DELETE FROM novel_history", [])?;
        Ok(())
    }

    /// Read a reader preference stored in the shared `settings` table.
    pub fn novel_setting(&self, key: &str) -> Result<Option<String>, StoreError> {
        self.get_setting(&format!("{SETTING_PREFIX}{key}"))
    }

    /// Write a reader preference, replacing any stored value.
    pub fn set_novel_setting(&self, key: &str, value: &str) -> Result<(), StoreError> {
        self.set_setting(&format!("{SETTING_PREFIX}{key}"), value)
    }

    /// `(progress rows, history rows)` for the settings page.
    ///
    /// Both counters in one statement, like [`Store::stats`]: no table name is
    /// ever formatted into SQL.
    pub fn novel_stats(&self) -> Result<(u64, u64), StoreError> {
        let conn = self.lock();
        let (progress, history): (i64, i64) = conn.query_row(
            "SELECT (SELECT COUNT(*) FROM novel_progress), (SELECT COUNT(*) FROM novel_history)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok((
            u64::try_from(progress).unwrap_or(0),
            u64::try_from(history).unwrap_or(0),
        ))
    }
}

fn progress_from_row(row: &Row<'_>) -> rusqlite::Result<NovelProgress> {
    progress_at(row, 0)
}

/// [`progress_from_row`] for a row whose progress columns start at `offset`.
///
/// The join in [`Store::recent_rows_with_progress`] puts the six history columns
/// in front of the seven progress ones, and one reader with an offset is what
/// keeps the two statements' decoders from drifting apart.
fn progress_at(row: &Row<'_>, offset: usize) -> rusqlite::Result<NovelProgress> {
    Ok(NovelProgress {
        novel_id: row.get(offset)?,
        series_id: row.get(offset + 1)?,
        // A hand-edited or truncated database can hold anything in an INTEGER
        // column; a position outside the u32 range is not recoverable, so it
        // reads as the start of the document instead of failing the open.
        block_index: u32_from(row.get::<_, i64>(offset + 2)?),
        char_offset: u32_from(row.get::<_, i64>(offset + 3)?),
        percent: row.get::<_, f64>(offset + 4)? as f32,
        finished: row.get::<_, i64>(offset + 5)? != 0,
        updated_at: row.get(offset + 6)?,
    })
}

fn u32_from(value: i64) -> u32 {
    u32::try_from(value).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open_in_memory().expect("in-memory store")
    }

    fn progress(novel_id: &str, series_id: &str, percent: f32) -> NovelProgress {
        NovelProgress {
            novel_id: novel_id.to_string(),
            series_id: series_id.to_string(),
            block_index: 7,
            char_offset: 120,
            percent,
            finished: false,
            updated_at: 0,
        }
    }

    fn history(novel_id: &str, read_at: i64) -> NovelHistoryEntry {
        NovelHistoryEntry {
            novel_id: novel_id.to_string(),
            title: format!("title-{novel_id}"),
            author: "author".to_string(),
            cover: format!("https://i.pximg.net/{novel_id}.jpg"),
            series_id: String::new(),
            read_at,
        }
    }

    fn ids(entries: &[NovelHistoryEntry]) -> Vec<&str> {
        entries
            .iter()
            .map(|entry| entry.novel_id.as_str())
            .collect()
    }

    /// The plan SQLite actually uses, as one printable line per row.
    fn plan_of(store: &Store, sql: &str) -> String {
        let conn = store.lock();
        let mut stmt = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .expect("query plan");
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(3))
            .expect("plan rows");
        rows.map(|row| row.expect("plan detail"))
            .collect::<Vec<_>>()
            .join(" | ")
    }

    /// C7: the recently-read list is capped by the same clause as the two
    /// illustration histories, so a reader who opens a novel a day cannot grow
    /// the table for a decade.
    ///
    /// The negative experiment is deleting `SQL_PRUNE_NOVEL` from
    /// `record_novel_read`: the two asserts below read 540 where they want 500.
    #[test]
    fn the_recently_read_list_keeps_the_last_five_hundred_rows() {
        let s = store();
        let writes = HISTORY_RETENTION + 40;
        for index in 0..writes {
            s.record_novel_read(&history(&index.to_string(), i64::try_from(index).unwrap()))
                .unwrap();
        }

        let rows = s.novel_history(writes).unwrap();
        assert_eq!(rows.len(), HISTORY_RETENTION);
        assert_eq!(rows[0].novel_id, (writes - 1).to_string());
        assert_eq!(rows[HISTORY_RETENTION - 1].novel_id, "40");
        // `novel_stats` counts the table, so the cap bounds the settings page too.
        assert_eq!(s.novel_stats().unwrap().1, HISTORY_RETENTION as u64);
    }

    #[test]
    fn the_novel_tables_exist_after_a_fresh_migration() {
        let s = store();
        assert_eq!(s.schema_version().unwrap(), 4);
        assert!(s.novel_progress("1").unwrap().is_none());
        assert!(s.novel_history(10).unwrap().is_empty());
        assert!(s.series_progress("555").unwrap().is_empty());
        assert_eq!(s.novel_stats().unwrap(), (0, 0));
        assert_eq!(s.novel_setting("reader").unwrap(), None);
        s.clear_novel_history()
            .expect("clearing an empty table is a no-op");
    }

    #[test]
    fn progress_roundtrips_with_every_field() {
        let s = store();
        let mut saved = progress("100", "555", 0.1);
        saved.finished = true;
        s.save_novel_progress(&saved).unwrap();

        let stored = s.novel_progress("100").unwrap().expect("row");
        assert_eq!(stored.novel_id, "100");
        assert_eq!(stored.series_id, "555");
        assert_eq!(stored.block_index, 7);
        assert_eq!(stored.char_offset, 120);
        // Percent is stored as given: the series bar shows it, so rounding here
        // would lose the caller's precision.
        assert_eq!(stored.percent.to_bits(), 0.1f32.to_bits());
        assert!(stored.finished);
        assert!(stored.updated_at > 0, "the store stamps updated_at");

        // An unknown id is None, not an error.
        assert!(s.novel_progress("does-not-exist").unwrap().is_none());
    }

    #[test]
    fn percent_is_stored_as_given() {
        let s = store();
        for percent in [0.0f32, 0.5, 1.0, 33.333_332, 99.5] {
            let mut row = progress("1", "", percent);
            row.percent = percent;
            s.save_novel_progress(&row).unwrap();
            let stored = s.novel_progress("1").unwrap().expect("row");
            assert_eq!(stored.percent, percent, "percent {percent}");
        }
    }

    #[test]
    fn saving_twice_updates_in_place() {
        let s = store();
        s.save_novel_progress(&progress("1", "", 0.1)).unwrap();
        let first = s.novel_progress("1").unwrap().expect("row");

        let mut later = progress("1", "999", 0.9);
        later.block_index = 99;
        later.char_offset = 4_000;
        later.finished = true;
        s.save_novel_progress(&later).unwrap();

        let second = s.novel_progress("1").unwrap().expect("row");
        assert_eq!(second.block_index, 99);
        assert_eq!(second.char_offset, 4_000);
        assert_eq!(second.series_id, "999");
        assert_eq!(second.percent, 0.9);
        assert!(second.finished);
        assert!(second.updated_at >= first.updated_at);
        assert_eq!(
            s.novel_stats().unwrap().0,
            1,
            "an upsert must not duplicate"
        );
    }

    #[test]
    fn series_progress_returns_only_that_series() {
        let s = store();
        for (novel, series) in [("1", "555"), ("2", "555"), ("3", "777"), ("4", "")] {
            s.save_novel_progress(&progress(novel, series, 0.5))
                .unwrap();
        }

        let mut series: Vec<String> = s
            .series_progress("555")
            .unwrap()
            .into_iter()
            .map(|row| row.novel_id)
            .collect();
        series.sort();
        assert_eq!(series, ["1", "2"]);

        assert_eq!(s.series_progress("777").unwrap().len(), 1);
        assert!(s.series_progress("unknown").unwrap().is_empty());

        // An empty series id is a key, not a wildcard: it returns the standalone
        // novels and nothing else.
        let standalone = s.series_progress("").unwrap();
        assert_eq!(standalone.len(), 1);
        assert_eq!(standalone[0].novel_id, "4");
        assert_eq!(standalone[0].series_id, "");
    }

    #[test]
    fn an_empty_series_id_survives_on_both_tables() {
        let s = store();
        let mut row = progress("standalone", "", 0.4);
        row.series_id = String::new();
        s.save_novel_progress(&row).unwrap();
        assert_eq!(
            s.novel_progress("standalone")
                .unwrap()
                .expect("row")
                .series_id,
            ""
        );

        let mut entry = history("standalone", 0);
        entry.series_id.clear();
        s.record_novel_read(&entry).unwrap();
        assert_eq!(s.novel_history(1).unwrap()[0].series_id, "");
    }

    #[test]
    fn history_is_newest_first_and_deduplicated() {
        let s = store();
        for id in ["a", "b", "c"] {
            s.record_novel_read(&history(id, 1_000)).unwrap();
        }
        assert_eq!(ids(&s.novel_history(10).unwrap()), ["c", "b", "a"]);

        // Re-reading an older novel moves it to the front without duplicating.
        s.record_novel_read(&history("a", 2_000)).unwrap();
        assert_eq!(ids(&s.novel_history(10).unwrap()), ["a", "c", "b"]);
        assert_eq!(s.novel_history(100).unwrap().len(), 3);

        // read_at is stamped by the store, not taken from the caller's value,
        // and the row keeps the metadata it was given.
        let newest = &s.novel_history(1).unwrap()[0];
        assert_ne!(newest.read_at, 2_000);
        assert!(newest.read_at > 0);
        assert_eq!(newest.title, "title-a");
        assert_eq!(newest.cover, "https://i.pximg.net/a.jpg");

        // Limits are honoured, including the degenerate ones.
        assert_eq!(ids(&s.novel_history(2).unwrap()), ["a", "c"]);
        assert!(s.novel_history(0).unwrap().is_empty());
    }

    #[test]
    fn clearing_the_history_leaves_progress_alone() {
        let s = store();
        s.save_novel_progress(&progress("1", "", 0.5)).unwrap();
        s.record_novel_read(&history("1", 1)).unwrap();
        s.record_novel_read(&history("2", 2)).unwrap();
        assert_eq!(s.novel_stats().unwrap(), (1, 2));

        s.clear_novel_history().unwrap();
        assert!(s.novel_history(10).unwrap().is_empty());
        assert!(
            s.novel_progress("1").unwrap().is_some(),
            "clearing the list must not lose the position"
        );
        assert_eq!(s.novel_stats().unwrap(), (1, 0));

        // And clearing it twice is a no-op, not an error.
        s.clear_novel_history().unwrap();
    }

    #[test]
    fn novel_settings_live_under_the_prefix() {
        let s = store();
        assert_eq!(s.novel_setting("reader").unwrap(), None);

        s.set_novel_setting("reader", r#"{"font_size":19.0}"#)
            .unwrap();
        assert_eq!(
            s.novel_setting("reader").unwrap().as_deref(),
            Some(r#"{"font_size":19.0}"#)
        );

        // It is the shared settings table, addressed through the prefix only.
        assert_eq!(s.get_setting("reader").unwrap(), None);
        assert_eq!(
            s.get_setting("novel.reader").unwrap().as_deref(),
            Some(r#"{"font_size":19.0}"#)
        );

        // Replacing works, and an ordinary settings key is not disturbed.
        s.set_setting("theme", "dark").unwrap();
        s.set_novel_setting("reader", "{}").unwrap();
        assert_eq!(s.novel_setting("reader").unwrap().as_deref(), Some("{}"));
        assert_eq!(s.get_setting("theme").unwrap().as_deref(), Some("dark"));
    }

    #[test]
    fn untrusted_text_is_bound_in_the_novel_tables() {
        let s = store();
        let nasty = "1'); DROP TABLE novel_progress; --";

        let mut row = progress("1", "", 0.5);
        row.novel_id = nasty.to_string();
        row.series_id = nasty.to_string();
        s.save_novel_progress(&row).unwrap();
        assert_eq!(
            s.novel_progress(nasty).unwrap().expect("row").series_id,
            nasty
        );

        let mut entry = history("1", 0);
        entry.title = nasty.to_string();
        entry.novel_id = nasty.to_string();
        s.record_novel_read(&entry).unwrap();
        assert_eq!(s.novel_history(10).unwrap()[0].title, nasty);

        s.set_novel_setting(nasty, nasty).unwrap();
        assert_eq!(s.novel_setting(nasty).unwrap().as_deref(), Some(nasty));

        // Both tables are still there and still count one row each.
        assert_eq!(s.novel_stats().unwrap(), (1, 1));
    }

    #[test]
    fn the_history_list_is_served_in_index_order() {
        let s = store();
        for id in ["a", "b", "c", "d", "e"] {
            s.record_novel_read(&history(id, 1)).unwrap();
        }
        let plan = plan_of(
            &s,
            "SELECT novel_id FROM novel_history ORDER BY seq DESC LIMIT 5",
        );
        assert!(plan.contains("idx_novel_history_seq"), "{plan}");
        assert!(!plan.contains("USE TEMP B-TREE FOR ORDER BY"), "{plan}");

        // MAX(seq) is a single seek of the same index, not a table scan.
        let next = plan_of(&s, "SELECT COALESCE(MAX(seq), 0) + 1 FROM novel_history");
        assert!(!next.contains("SCAN novel_history"), "{next}");
    }

    /// C9: the series index carries its sort key, so "newest first" is a read of
    /// the index instead of a temp B-tree over the series.
    ///
    /// The negative experiment is restoring the single-column index
    /// (`ON novel_progress (series_id)`, which is what migration step 3 wrote):
    /// SQLite then seeks `series_id` and sorts the answer, and the plan grows
    /// `USE TEMP B-TREE FOR ORDER BY`.
    #[test]
    fn the_series_index_carries_the_updated_at_sort_key() {
        let s = store();
        for index in 0..50 {
            let novel = format!("{index}");
            s.save_novel_progress(&progress(&novel, "555", 0.5))
                .unwrap();
        }
        let plan = plan_of(
            &s,
            "SELECT novel_id FROM novel_progress
             WHERE series_id = '555'
             ORDER BY updated_at DESC
             LIMIT 20",
        );
        assert!(plan.contains("idx_novel_progress_series"), "{plan}");
        assert!(
            !plan.contains("TEMP B-TREE"),
            "the sort must be served by the index: {plan}"
        );
        assert!(!plan.contains("SCAN novel_progress"), "{plan}");

        // And the index really is the two-column shape, not merely the name: a
        // query that orders by the second column of the pair is served, which a
        // single-column index cannot do.
        let columns = index_columns(&s, "idx_novel_progress_series");
        assert_eq!(columns, ["series_id", "updated_at"], "{columns:?}");
    }

    fn index_columns(s: &Store, name: &str) -> Vec<String> {
        let conn = s.lock();
        let mut stmt = conn
            .prepare("SELECT name FROM pragma_index_info(?1) ORDER BY seqno")
            .expect("pragma_index_info");
        let rows = stmt
            .query_map(params![name], |row| row.get::<_, String>(0))
            .unwrap();
        rows.map(|row| row.unwrap()).collect()
    }

    /// F4: the recent list carries the stored position, and a novel that was never
    /// scrolled carries none instead of a 0 %.
    #[test]
    fn the_recent_list_joins_the_stored_position_and_leaves_the_rest_empty() {
        let s = store();
        s.record_novel_read(&history("a", 1)).unwrap();
        s.record_novel_read(&history("b", 2)).unwrap();
        let mut read = progress("a", "", 0.42);
        read.block_index = 7;
        read.char_offset = 120;
        s.save_novel_progress(&read).unwrap();

        let rows = s.recent_rows_with_progress(10).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].entry.novel_id, "b", "history order is preserved");
        assert!(
            rows[0].progress.is_none(),
            "an unopened novel has no progress to draw"
        );
        assert_eq!(rows[1].entry.novel_id, "a");
        let stored = rows[1].progress.as_ref().expect("the join found the row");
        assert_eq!(stored.novel_id, "a");
        assert_eq!(stored.block_index, 7);
        assert_eq!(stored.char_offset, 120);
        assert_eq!(stored.percent, 0.42);

        // The list is the same list, in the same order, as the plain one.
        let plain: Vec<String> = s
            .novel_history(10)
            .unwrap()
            .into_iter()
            .map(|entry| entry.novel_id)
            .collect();
        let joined: Vec<String> = rows.into_iter().map(|row| row.entry.novel_id).collect();
        assert_eq!(joined, plain);

        // Limits apply here too, and the degenerate ones are honoured.
        assert_eq!(s.recent_rows_with_progress(1).unwrap().len(), 1);
        assert!(s.recent_rows_with_progress(0).unwrap().is_empty());
    }

    #[test]
    fn the_series_lookup_uses_its_index() {
        let s = store();
        for index in 0..200 {
            let novel = format!("{index}");
            s.save_novel_progress(&progress(&novel, "555", 0.5))
                .unwrap();
        }
        let plan = plan_of(
            &s,
            "SELECT novel_id FROM novel_progress WHERE series_id = '555'",
        );
        assert!(plan.contains("idx_novel_progress_series"), "{plan}");
    }
}
