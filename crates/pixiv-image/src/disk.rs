//! On-disk tier: the encoded response bytes, keyed by [`crate::cache_key`].
//!
//! Storing the encoded bytes rather than decoded pixels keeps the tier ~10x
//! smaller than the memory tier for the same URL count; the price is a decode on
//! a cache hit, which already happens on a blocking thread.
//!
//! Every method here is synchronous filesystem work and is expected to be called
//! inside `tokio::task::spawn_blocking`, never directly on the async path. A
//! directory that cannot be created or written degrades the whole tier to
//! memory-only: [`DiskTier::open`] returns `None` and the caller warns.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::{Mutex, RwLock};
use tracing::warn;

/// One entry the tier believes it holds.
#[derive(Debug, Clone, Copy)]
struct IndexEntry {
    /// When `put` published it; a trim deletes oldest first.
    modified: SystemTime,
    size: u64,
}

/// Encoded-bytes tier with a byte budget and an explicit prune path.
pub(crate) struct DiskTier {
    dir: PathBuf,
    budget: u64,
    bytes: AtomicU64,
    /// Every entry the tier holds: key -> size, with the stamp a trim sorts by.
    ///
    /// This lock is the accounting lock. It used to guard only the arithmetic,
    /// while `enforce_budget` learned what to delete by walking the tree - a
    /// recursive `read_dir` plus a `metadata` per entry, on every write once the
    /// budget was full (plan §2.3 O2). The map is both the ledger and the candidate
    /// list now, so what the counter credits and what a trim may delete are one
    /// fact under one lock.
    index: Mutex<BTreeMap<String, IndexEntry>>,
    /// Distinguishes two writers' temp files for one key.
    writes: AtomicU64,
    /// Writers run together; a sweep runs alone.
    ///
    /// The index mutex decides the arithmetic, and it is held only for the
    /// arithmetic now (O11). What it cannot decide is whether a file may be
    /// unlinked while another writer's bytes are on their way to it: a sweep that
    /// deletes a file whose entry is not yet credited is exactly the drift O2
    /// removed from the counter. So every unlink path - remove, clear,
    /// enforce_budget - takes this guard for writing, and put takes it for reading
    /// across create, write, fsync and rename. Six writers then wait only for the
    /// disk, and a trim waits for the writers to finish - never the other way
    /// round, because no path holds the index while asking for this one.
    sweep: RwLock<()>,
    /// How long a test write pretends to take. Test-only.
    #[cfg(test)]
    slow_write: Mutex<Option<std::time::Duration>>,
    /// Directory scans this tier has run, the opening one included.
    ///
    /// O2's acceptance is "zero on the hot path", and a counter is the only way to
    /// state that as a measurement instead of a reading. Test-only accessor.
    scans: AtomicU64,
}

impl DiskTier {
    /// Open (creating if needed) the cache directory.
    ///
    /// Returns `None` when the directory is missing, not a directory, or not
    /// writable — the caller then runs memory-only.
    pub(crate) fn open(dir: PathBuf, budget: u64) -> Option<Self> {
        if let Err(err) = fs::create_dir_all(&dir) {
            warn!(
                dir = %dir.display(),
                error = %err,
                "image cache directory is unusable; running memory-only"
            );
            return None;
        }
        let probe = dir.join(".probe");
        if let Err(err) = fs::write(&probe, b"") {
            warn!(
                dir = %dir.display(),
                error = %err,
                "image cache directory is not writable; running memory-only"
            );
            return None;
        }
        if let Err(err) = fs::remove_file(&probe) {
            warn!(dir = %dir.display(), error = %err, "could not remove cache probe file");
        }
        // The one scan the tier needs: everything after this is maintained by `put`
        // and `remove`, and a tier opened over an existing directory must start with
        // a ledger that describes it.
        let index = scan_index(&dir).unwrap_or_default();
        let bytes: u64 = index.values().map(|entry| entry.size).sum();
        Some(Self {
            dir,
            budget,
            bytes: AtomicU64::new(bytes),
            index: Mutex::new(index),
            writes: AtomicU64::new(0),
            sweep: RwLock::new(()),
            #[cfg(test)]
            slow_write: Mutex::new(None),
            // The scan above is this tier's first, and counting it keeps
            // `scans()` honest about what a hot path is allowed to cost.
            scans: AtomicU64::new(1),
        })
    }

    /// Run one recursive scan of the tree and count it.
    ///
    /// Only `clear` uses this: its contract is that the directory holds nothing, a
    /// crashed writer's `.part` leftover included, and no index entry describes that
    /// file.
    fn scan(&self) -> io::Result<Vec<(SystemTime, u64, PathBuf)>> {
        self.scans.fetch_add(1, Ordering::Relaxed);
        walk(&self.dir)
    }

    /// Make every write take this long. Test-only: O11's measurement.
    #[cfg(test)]
    pub(crate) fn slow_write(&self, pause: std::time::Duration) {
        *self.slow_write.lock() = Some(pause);
    }

    /// Write one temp file, with the test's artificial cost in front of it.
    fn write_temp(&self, temp: &Path, bytes: &[u8]) -> io::Result<()> {
        #[cfg(test)]
        {
            // Copied out of the mutex before sleeping: the guard from a
            // scrutinee lives to the end of its `if let`, and holding it across the
            // sleep would serialise the very writers this seam exists to release.
            let pause = *self.slow_write.lock();
            if let Some(pause) = pause {
                std::thread::sleep(pause);
            }
        }
        write_synced(temp, bytes)
    }

    /// Directory scans run so far, the opening one included. Test-only.
    #[cfg(test)]
    pub(crate) fn scans(&self) -> u64 {
        self.scans.load(Ordering::Relaxed)
    }

    /// `(key, size)` for every entry the tier believes it holds. Test-only.
    #[cfg(test)]
    pub(crate) fn indexed(&self) -> Vec<(String, u64)> {
        self.index
            .lock()
            .iter()
            .map(|(key, entry)| (key.clone(), entry.size))
            .collect()
    }

    /// Drop the entry for a key whose file could not be read, giving its bytes back.
    fn forget(&self, key: &str) {
        let mut index = self.index.lock();
        if let Some(entry) = index.remove(key) {
            let _ = self.subtract(entry.size);
        }
    }

    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    /// Bytes currently held on disk.
    pub(crate) fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }

    fn path(&self, key: &str) -> PathBuf {
        // Two hex characters of the digest shard the directory so no single
        // folder grows to tens of thousands of entries.
        let shard = key.get(..2).unwrap_or(key);
        self.dir.join(shard).join(key)
    }

    /// Read one cached entry. A missing or unreadable file is a miss.
    ///
    /// A failed read is also the only signal the tier gets that the tree moved
    /// behind its back - a cache cleaner, a manual prune, a second instance - so the
    /// index entry goes with it. Keeping the entry would credit bytes that no longer
    /// exist, and a trim could then delete nothing while the budget still believed
    /// the tier was full.
    pub(crate) fn get(&self, key: &str) -> Option<Vec<u8>> {
        match fs::read(self.path(key)) {
            Ok(bytes) => Some(bytes),
            Err(err) => {
                if err.kind() != io::ErrorKind::NotFound {
                    warn!(key, error = %err, "could not read cached image");
                }
                self.forget(key);
                None
            }
        }
    }

    /// Store one entry, replacing any previous bytes for the same key.
    ///
    /// Create, write, `sync_all`, rename — in that order, in a temp file whose
    /// name belongs to this writer alone. The old shape was
    /// `path.with_extension("part")`: one fixed name per key, and the key is the
    /// URL digest, so two single-flight cells for the same URL at different
    /// widths shared a temp file and one rename could publish the other
    /// writer's half-written bytes. `fs::write` also returned as soon as the
    /// kernel had the bytes, so a crash could leave the name in place with
    /// nothing behind it.
    ///
    /// Cost: four syscalls per entry (create, write, fsync, rename) where it used
    /// to be two (write, rename). The tier is written from a blocking pool, and a
    /// cover is worth the fsync.
    ///
    /// The index lock covers the arithmetic and nothing else (O11). It used to be
    /// held across create, write, sync_all and rename as well, so six concurrent
    /// covers queued behind six fsyncs: one disk queue where six were available.
    /// What still has to be exclusive is the *unlink* side - a trim or a clear that
    /// deletes a file this call has not credited yet is the drift O2 removed from
    /// the counter - and that is what `sweep` is for: writers share it, sweeps take
    /// it alone.
    pub(crate) fn put(&self, key: &str, bytes: &[u8]) -> io::Result<()> {
        let path = self.path(key);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let added = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        // Held across the file's whole life: a file that exists but is not yet
        // credited is a file a concurrent trim may delete and then subtract, which
        // is the same drift in the other direction.
        let writing = self.sweep.read();
        let temp = self.temp_path(&path);
        let written = self
            .write_temp(&temp, bytes)
            .and_then(|()| fs::rename(&temp, &path));
        if let Err(err) = written {
            // Never leave a temp file behind on the failure path: the next scan
            // would find it, and the entry it belongs to was never credited.
            let _ = fs::remove_file(&temp);
            return Err(err);
        }
        // The previous size comes from the index, not from a `metadata` call: the
        // entry is either there or it is not, and one fewer stat per write is what
        // O2 was for. It is read *here*, under the same lock as the insert, because
        // two writers of one key that both read the old size before either inserted
        // would each credit the file and the counter would count it twice.
        let mut index = self.index.lock();
        let previous = index.get(key).map_or(0, |entry| entry.size);
        index.insert(
            key.to_owned(),
            IndexEntry {
                modified: SystemTime::now(),
                size: added,
            },
        );
        self.bytes.fetch_add(added, Ordering::Relaxed);
        let _ = self.subtract(previous);
        drop(index);
        drop(writing);
        if self.bytes() > self.budget {
            // The sweep guard *before* the index lock: every path that unlinks a
            // file takes the two in that order, and no path holds the index while
            // asking for the sweep - which is why this cannot deadlock.
            let _sweep = self.sweep.write();
            let mut index = self.index.lock();
            self.enforce_budget(&mut index);
        }
        Ok(())
    }

    /// A temp path only this writer can use.
    ///
    /// `{name}.{pid}-{seq}.part`: the sequence makes two writers of one key
    /// distinct inside a process, the pid across processes, and the `.part`
    /// suffix keeps the file unmistakably not-an-entry.
    fn temp_path(&self, path: &Path) -> PathBuf {
        let seq = self.writes.fetch_add(1, Ordering::Relaxed);
        let mut name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "image".to_string());
        name.push_str(&format!(".{}-{seq}.part", std::process::id()));
        path.with_file_name(name)
    }

    /// Drop one entry. The unlink, the index update and the subtraction share one
    /// lock section, so a trim cannot delete the same file underneath it.
    ///
    /// `parking_lot` does not poison, so a panic elsewhere cannot wedge the tier.
    pub(crate) fn remove(&self, key: &str) -> io::Result<()> {
        let path = self.path(key);
        // The guard first, because this call unlinks: a writer that is between its
        // rename and its credit must not lose the file underneath itself.
        let _sweep = self.sweep.write();
        let mut index = self.index.lock();
        let freed = index.get(key).map_or(0, |entry| entry.size);
        match fs::remove_file(&path) {
            Ok(()) => {
                index.remove(key);
                let _ = self.subtract(freed);
                Ok(())
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                // Already gone, so the index was the stale half: drop it and give
                // the bytes back rather than leaving a phantom to be trimmed.
                index.remove(key);
                let _ = self.subtract(freed);
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    /// Delete everything the tier holds, returning the bytes reclaimed.
    ///
    /// This is the one call that scans, and it has to: its contract is that the
    /// directory holds nothing, which includes a crashed writer's `.part` leftover -
    /// a file no index entry describes. The counter is then zeroed *inside* the lock
    /// section `put` must also take, which is what makes `store(0)` exact here; it
    /// was the defect when the lock only covered the arithmetic and a concurrent
    /// `put` could be erased by it.
    pub(crate) fn clear(&self) -> io::Result<u64> {
        // Alone in the tier: no writer may be between a rename and its credit, and
        // no concurrent remove may be unlinking the files this pass is counting.
        let _sweep = self.sweep.write();
        let mut index = self.index.lock();
        let entries = self.scan()?;
        let mut reclaimed = 0;
        for (_, size, path) in entries {
            match fs::remove_file(&path) {
                Ok(()) => reclaimed += size,
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(err),
            }
        }
        index.clear();
        self.bytes.store(0, Ordering::Relaxed);
        Ok(reclaimed)
    }

    /// Drop oldest entries until the budget holds.
    ///
    /// The caller holds the index lock *and* the `sweep` guard for writing, in that
    /// acquisition order (sweep, then index): this is the one path that unlinks
    /// files a writer may still be crediting.
    ///
    /// `store(total)` used to be the defect here: the total came from a snapshot and
    /// was written back over a counter that `put`/`remove` maintain with
    /// `fetch_add`/`fetch_sub`, so a concurrent write was silently dropped and the
    /// tier stopped trimming. The loop still only *decides* with its local total, and
    /// the counter is decremented by exactly what this pass removed.
    ///
    /// The walk is gone: the candidates, their sizes and their stamps all come from
    /// the index, so a trim costs a sort and some unlinks instead of a recursive
    /// `read_dir` plus a `metadata` per entry (plan §2.3 O2).
    fn enforce_budget(&self, index: &mut BTreeMap<String, IndexEntry>) {
        let mut total = self.bytes();
        if total <= self.budget {
            return;
        }
        let mut candidates: Vec<(String, SystemTime, u64)> = index
            .iter()
            .map(|(key, entry)| (key.clone(), entry.modified, entry.size))
            .collect();
        // Oldest first, and the key breaks a tie: two entries published inside one
        // clock tick must trim in a fixed order, or the same run would delete
        // different files each time.
        candidates.sort_by(|left, right| left.1.cmp(&right.1).then_with(|| left.0.cmp(&right.0)));
        let mut trimmed = 0u64;
        for (key, _, size) in candidates {
            if total <= self.budget {
                break;
            }
            match fs::remove_file(self.path(&key)) {
                Ok(()) => {}
                // The file was already gone and the index was the stale half; the
                // phantom still has to stop counting against the budget.
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => {
                    warn!(%key, error = %err, "could not trim cached image");
                    continue;
                }
            }
            index.remove(&key);
            total = total.saturating_sub(size);
            trimmed = trimmed.saturating_add(size);
        }
        if trimmed > 0 {
            let _ = self.subtract(trimmed);
        }
    }

    /// Subtract from the counter without underflowing.
    ///
    /// Two trims can race on one file, and the size read before a delete can
    /// exceed what the counter still credits, so a bare `fetch_sub` would wrap to
    /// a value near `u64::MAX` and switch trimming off for good.
    fn subtract(&self, amount: u64) -> u64 {
        // A compare-exchange loop rather than `fetch_update`: that method is
        // deprecated in the toolchain CI pins (renamed `try_update`) and still the
        // only name in the older one this machine builds with, so naming either
        // would fail on the other. The loop needs neither, and it is the same
        // arithmetic: clamp at zero, retry against whatever a racing writer left.
        let mut current = self.bytes.load(Ordering::Relaxed);
        loop {
            let next = current.saturating_sub(amount);
            match self.bytes.compare_exchange_weak(
                current,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return current,
                Err(observed) => current = observed,
            }
        }
    }
}

/// Create `path`, write every byte of `bytes`, and flush it to the platter.
///
/// `fs::write` stops at the kernel: after it returns the name can exist while
/// the data behind it does not, and only a crash reveals that. Readers here do
/// recover from a torn entry (a decode failure drops it and re-fetches, see
/// `crates/pixiv-image/src/lib.rs`), but relying on that means the recovery path
/// is the durability story. One `sync_all` per entry removes the reliance.
fn write_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = fs::File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// `key -> (stamp, size)` for every entry `dir` holds.
///
/// Runs once, in [`DiskTier::open`]. Two things are deliberately not entries:
/// anything outside a shard folder, and a `*.part` temp, which belongs to a writer
/// that is about to rename it - the index is the list a trim may delete from, so a
/// temp in it would be a file deleted under a live writer.
fn scan_index(dir: &Path) -> io::Result<BTreeMap<String, IndexEntry>> {
    let mut index = BTreeMap::new();
    for shard in fs::read_dir(dir)? {
        let shard = shard?;
        if !shard.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            continue;
        }
        let folder = shard.path();
        for file in fs::read_dir(&folder)? {
            let file = file?;
            let Ok(meta) = file.metadata() else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            let name = file.file_name().to_string_lossy().into_owned();
            if name.ends_with(".part") {
                continue;
            }
            index.insert(
                name,
                IndexEntry {
                    modified: meta.modified().unwrap_or(UNIX_EPOCH),
                    size: meta.len(),
                },
            );
        }
    }
    Ok(index)
}

/// `(modified, size, path)` for every file under `dir`, recursively.
///
/// Only `clear` and the tests still use this: every other path consults the index.
fn walk(dir: &Path) -> io::Result<Vec<(SystemTime, u64, PathBuf)>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let meta = entry.metadata()?;
        if meta.is_dir() {
            entries.extend(walk(&path)?);
        } else if meta.is_file() {
            let modified = meta.modified().unwrap_or(UNIX_EPOCH);
            entries.push((modified, meta.len(), path));
        }
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::DiskTier;
    use std::fs;
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "pixiv-image-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0)
        ))
    }

    #[test]
    fn round_trips_and_counts_bytes() {
        let dir = scratch("disk-round-trip");
        let tier = DiskTier::open(dir.clone(), 1 << 20).expect("disk tier");
        assert_eq!(tier.bytes(), 0);
        tier.put("0011223344", b"hello").expect("put");
        assert_eq!(tier.get("0011223344").as_deref(), Some(&b"hello"[..]));
        assert_eq!(tier.bytes(), 5);
        // Overwrite replaces rather than adds.
        tier.put("0011223344", b"hi").expect("put again");
        assert_eq!(tier.bytes(), 2);
        assert!(tier.get("ffff").is_none());
        assert_eq!(tier.clear().expect("clear"), 2);
        assert_eq!(tier.bytes(), 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn over_budget_writes_prune_the_oldest() {
        let dir = scratch("disk-budget");
        let tier = DiskTier::open(dir.clone(), 8).expect("disk tier");
        tier.put("aa-old", b"1234").expect("put");
        std::thread::sleep(std::time::Duration::from_millis(20));
        tier.put("bb-new", b"5678").expect("put");
        std::thread::sleep(std::time::Duration::from_millis(20));
        tier.put("cc-newest", b"9999").expect("put");
        assert!(tier.bytes() <= 8);
        let _ = fs::remove_dir_all(&dir);
    }

    /// Two writers of one key must not share a temp file. The defect was
    /// `path.with_extension("part")`: one name per key, and the key is the URL
    /// digest, so two single-flight cells for the same URL at different widths
    /// wrote and renamed the same temp.
    #[test]
    fn each_writer_gets_its_own_temp_name() {
        let dir = scratch("disk-temp");
        let tier = DiskTier::open(dir.clone(), 1 << 20).expect("disk tier");
        let path = tier.dir().join("ab").join("abcdef");
        let first = tier.temp_path(&path);
        let second = tier.temp_path(&path);
        assert_ne!(
            first, second,
            "two writers of one key must not share a temp file"
        );
        assert_ne!(first, path.with_extension("part"), "the fixed name is gone");
        assert_eq!(first.parent(), path.parent(), "temp sits beside the entry");
        assert!(first.to_string_lossy().ends_with(".part"));
        let _ = fs::remove_dir_all(&dir);
    }

    /// Concurrent writers of one key publish exactly one of their bodies, whole:
    /// never an interleaving of the two, and no temp file left behind.
    #[test]
    fn two_writers_of_one_key_never_publish_a_torn_entry() {
        use std::sync::Arc;
        let dir = scratch("disk-race");
        let tier = Arc::new(DiskTier::open(dir.clone(), 1 << 20).expect("disk tier"));
        let first = vec![b'a'; 64 * 1024];
        let second = vec![b'b'; 64 * 1024];
        let mut writers = Vec::new();
        for body in [first.clone(), second.clone()] {
            let tier = Arc::clone(&tier);
            writers.push(std::thread::spawn(move || {
                for _ in 0..8 {
                    tier.put("00112233", &body).expect("put");
                }
            }));
        }
        for writer in writers {
            writer.join().expect("writer thread");
        }
        let published = tier.get("00112233").expect("an entry survives");
        assert!(
            published == first || published == second,
            "the published entry is one writer's bytes, not a mix"
        );
        let leftovers: Vec<String> = fs::read_dir(&dir)
            .expect("read cache dir")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".part"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// The counter describes the tree. It used to be a total written back over a
    /// counter that `put`/`remove` maintain with `fetch_add`/`fetch_sub`, so a
    /// concurrent write was dropped and the tier stopped trimming.
    #[test]
    fn the_byte_counter_never_drifts_from_the_tree() {
        use std::sync::Arc;
        let dir = scratch("disk-accounting");
        let tier = Arc::new(DiskTier::open(dir.clone(), 16).expect("disk tier"));
        let mut writers = Vec::new();
        for writer in 0..8u32 {
            let tier = Arc::clone(&tier);
            writers.push(std::thread::spawn(move || {
                tier.put(&format!("{writer:02}-entry"), b"1234")
                    .expect("put");
            }));
        }
        for writer in writers {
            writer.join().expect("writer thread");
        }
        let on_disk: u64 = super::walk(&dir)
            .expect("walk")
            .iter()
            .map(|entry| entry.1)
            .sum();
        assert_eq!(
            tier.bytes(),
            on_disk,
            "the counter must describe the tree, not a stale total"
        );
        assert!(on_disk <= 16, "trimming kept the budget: {on_disk} bytes");
        let _ = fs::remove_dir_all(&dir);
    }

    /// O11: six concurrent writers pay for one write, not six.
    ///
    /// `put` used to hold the index lock across create, write, sync_all and
    /// rename, so six covers serialised behind six fsyncs. With a 20 ms write
    /// injected, six writers finish in about one sleep instead of six - and the
    /// accounting still describes the tree, which is the invariant the lock was
    /// widened for in the first place (O2).
    #[test]
    fn six_concurrent_writers_do_not_queue_behind_one_write() {
        use std::sync::Arc;
        const SLOW: std::time::Duration = std::time::Duration::from_millis(20);
        let dir = scratch("disk-parallel-put");
        let tier = Arc::new(DiskTier::open(dir.clone(), 1 << 20).expect("disk tier"));
        tier.slow_write(SLOW);

        let started = std::time::Instant::now();
        let mut writers = Vec::new();
        for index in 0..6u32 {
            let tier = Arc::clone(&tier);
            writers.push(std::thread::spawn(move || {
                tier.put(&format!("{index:02}-entry"), b"1234")
                    .expect("put");
            }));
        }
        for writer in writers {
            writer.join().expect("writer thread");
        }
        let elapsed = started.elapsed();
        assert!(
            elapsed >= SLOW,
            "the injected write cost was paid: {elapsed:?}"
        );
        assert!(
            elapsed < SLOW * 3,
            "six writers queued behind one another: {elapsed:?}, not about {SLOW:?}"
        );

        let on_disk: u64 = super::walk(&dir)
            .expect("walk")
            .iter()
            .map(|entry| entry.1)
            .sum();
        assert_eq!(
            tier.bytes(),
            on_disk,
            "the counter still describes the tree"
        );
        assert_eq!(tier.indexed().len(), 6, "every writer published its entry");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_path_under_a_regular_file_refuses_to_open() {
        let dir = scratch("disk-degraded");
        fs::create_dir_all(&dir).expect("scratch dir");
        let blocker = dir.join("not-a-directory");
        fs::write(&blocker, b"x").expect("write blocker");
        assert!(DiskTier::open(blocker.join("cache"), 1 << 20).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    /// O2: opens scan once, and nothing else does. The trim in particular used to
    /// start with a recursive `read_dir` plus a `metadata` per entry, on every write
    /// once the budget was full.
    #[test]
    fn the_hot_path_never_scans_the_directory() {
        let dir = scratch("disk-no-scan");
        // A small budget, so every write past the fourth also runs a trim.
        let tier = DiskTier::open(dir.clone(), 16).expect("disk tier");
        assert_eq!(tier.scans(), 1, "opening scans once");

        for index in 0..8u32 {
            tier.put(&format!("{index:02}-entry"), b"1234")
                .expect("put");
        }
        assert_eq!(tier.scans(), 1, "a budget trim must not walk the tree");

        assert!(tier.get("07-entry").is_some(), "the newest entry survives");
        assert_eq!(tier.scans(), 1, "a hit must not walk the tree");
        assert!(tier.get("zz-missing").is_none());
        tier.remove("06-entry").expect("remove");
        assert_eq!(
            tier.scans(),
            1,
            "a miss and a remove must not walk the tree"
        );

        // And the counter is not dead: `clear` has to scan, because its contract
        // covers files no index describes.
        let _ = tier.clear().expect("clear");
        assert_eq!(tier.scans(), 2, "clear scans exactly once");
        let _ = fs::remove_dir_all(&dir);
    }

    /// O2: a tier reopened over the same directory must believe exactly what the
    /// live tier believed, and both must describe the files that are really there.
    #[test]
    fn an_index_rebuilt_by_a_restart_equals_the_one_maintained_live() {
        let dir = scratch("disk-restart");
        let live = {
            let tier = DiskTier::open(dir.clone(), 1 << 20).expect("disk tier");
            for (key, body) in [
                ("aa-first", &b"aaaa"[..]),
                ("bb-second", &b"bbbbbb"[..]),
                ("cc-third", &b"cc"[..]),
            ] {
                tier.put(key, body).expect("put");
            }
            // A removal is part of the live history: the rebuilt index must match
            // it, not the puts alone.
            tier.put("dd-doomed", b"dddd").expect("put");
            tier.remove("dd-doomed").expect("remove");
            let mut live = tier.indexed();
            live.sort();
            live
        };

        // The restart: nothing carried over but the directory itself.
        let reopened = DiskTier::open(dir.clone(), 1 << 20).expect("reopen");
        let mut rebuilt = reopened.indexed();
        rebuilt.sort();
        assert_eq!(
            rebuilt, live,
            "the scan must rebuild the index the writes kept"
        );

        for (key, size) in &rebuilt {
            let shard = key.get(..2).unwrap_or(key);
            let meta = fs::metadata(reopened.dir().join(shard).join(key))
                .expect("every indexed entry is a real file");
            assert_eq!(meta.len(), *size, "size recorded for {key}");
        }
        assert_eq!(reopened.bytes(), 4 + 6 + 2);
        let _ = fs::remove_dir_all(&dir);
    }

    /// O2: a read that fails is a miss *and* the entry goes with it. The file was
    /// removed behind the tier's back, and a phantom entry would be credited against
    /// the budget for the rest of the session while a trim deleted nothing.
    #[test]
    fn a_miss_whose_file_vanished_drops_the_entry() {
        let dir = scratch("disk-vanish");
        let tier = DiskTier::open(dir.clone(), 1 << 20).expect("disk tier");
        tier.put("0011223344", b"hello").expect("put");
        assert_eq!(tier.bytes(), 5);
        assert_eq!(tier.indexed().len(), 1);

        fs::remove_file(tier.dir().join("00").join("0011223344")).expect("delete the entry");

        assert!(tier.get("0011223344").is_none(), "a failed read is a miss");
        assert_eq!(tier.bytes(), 0, "the bytes it claimed are given back");
        assert!(tier.indexed().is_empty(), "and the entry is dropped");
        assert!(
            tier.get("0011223344").is_none(),
            "a second look stays a miss"
        );
        assert_eq!(tier.scans(), 1, "and no scan was needed to notice");
        let _ = fs::remove_dir_all(&dir);
    }
}
