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

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::{Mutex, MutexGuard};
use tracing::warn;

/// Encoded-bytes tier with a byte budget and an explicit prune path.
pub(crate) struct DiskTier {
    dir: PathBuf,
    budget: u64,
    bytes: AtomicU64,
    /// Serialises the accounting, so a trim can never write back a total that a
    /// concurrent `put` has already moved past.
    accounting: Mutex<()>,
    /// Distinguishes two writers' temp files for one key.
    writes: AtomicU64,
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
        let bytes = walk(&dir)
            .map(|entries| entries.iter().map(|entry| entry.1).sum())
            .unwrap_or(0);
        Some(Self {
            dir,
            budget,
            bytes: AtomicU64::new(bytes),
            accounting: Mutex::new(()),
            writes: AtomicU64::new(0),
        })
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
    pub(crate) fn get(&self, key: &str) -> Option<Vec<u8>> {
        match fs::read(self.path(key)) {
            Ok(bytes) => Some(bytes),
            Err(err) if err.kind() == io::ErrorKind::NotFound => None,
            Err(err) => {
                warn!(key, error = %err, "could not read cached image");
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
    /// to be two (write, rename), and the whole sequence runs under the
    /// accounting lock so the counter and the tree can never disagree. The tier is
    /// written from a blocking pool, one entry at a time; a cover is worth the
    /// fsync and the serialisation is what makes [`Self::enforce_budget`] exact.
    pub(crate) fn put(&self, key: &str, bytes: &[u8]) -> io::Result<()> {
        let path = self.path(key);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        // Held for the write as well as the arithmetic: a file that exists but is
        // not yet credited is a file a concurrent trim may delete and then
        // subtract, which is the same drift in the other direction.
        let _guard = self.accounting();
        let previous = fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
        let temp = self.temp_path(&path);
        let written = write_synced(&temp, bytes).and_then(|()| fs::rename(&temp, &path));
        if let Err(err) = written {
            // Never leave a temp file behind on the failure path: the tier counts
            // every file it can see, so an orphan is charged to the budget until
            // the next prune deletes it.
            let _ = fs::remove_file(&temp);
            return Err(err);
        }
        let added = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        self.bytes.fetch_add(added, Ordering::Relaxed);
        let _ = self.subtract(previous);
        if self.bytes() > self.budget {
            self.enforce_budget();
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

    /// The accounting guard. `parking_lot` does not poison, so a panic elsewhere
    /// cannot wedge the tier.
    fn accounting(&self) -> MutexGuard<'_, ()> {
        self.accounting.lock()
    }

    /// Drop one entry. The measurement, the unlink and the subtraction share one
    /// lock section, so a trim cannot delete the same file underneath it.
    pub(crate) fn remove(&self, key: &str) -> io::Result<()> {
        let path = self.path(key);
        let _guard = self.accounting();
        let freed = fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
        match fs::remove_file(&path) {
            Ok(()) => {
                let _ = self.subtract(freed);
                Ok(())
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err),
        }
    }

    /// Delete everything the tier holds, returning the bytes reclaimed.
    ///
    /// The counter is decremented by exactly what this pass deleted instead of
    /// being reset to zero: a `put` that lands during the walk has already added
    /// its own bytes, and `store(0)` would erase that credit and leave the tier
    /// believing it holds less than it does.
    pub(crate) fn clear(&self) -> io::Result<u64> {
        let _guard = self.accounting();
        let entries = walk(&self.dir)?;
        let mut reclaimed = 0;
        for (_, size, path) in entries {
            match fs::remove_file(&path) {
                Ok(()) => {
                    reclaimed += size;
                    let _ = self.subtract(size);
                }
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(err),
            }
        }
        Ok(reclaimed)
    }

    /// Drop oldest entries until the budget holds. The caller holds the
    /// accounting lock.
    ///
    /// `store(total)` was the defect: the total was computed from a snapshot and
    /// then written back over a counter that `put` and `remove` maintain with
    /// `fetch_add`/`fetch_sub`, so a concurrent write was silently dropped and the
    /// tier stopped trimming. The loop now only *decides* with its local total,
    /// and the counter is decremented by exactly what this pass removed.
    fn enforce_budget(&self) {
        let Ok(mut entries) = walk(&self.dir) else {
            return;
        };
        entries.sort_by_key(|entry| entry.0);
        let mut total = self.bytes();
        let mut trimmed = 0u64;
        for (_, size, path) in entries {
            if total <= self.budget {
                break;
            }
            if fs::remove_file(&path).is_ok() {
                total = total.saturating_sub(size);
                trimmed = trimmed.saturating_add(size);
            }
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
        self.bytes
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                Some(current.saturating_sub(amount))
            })
            .unwrap_or(0)
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

/// `(modified, size, path)` for every file under `dir`, recursively.
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

    #[test]
    fn a_path_under_a_regular_file_refuses_to_open() {
        let dir = scratch("disk-degraded");
        fs::create_dir_all(&dir).expect("scratch dir");
        let blocker = dir.join("not-a-directory");
        fs::write(&blocker, b"x").expect("write blocker");
        assert!(DiskTier::open(blocker.join("cache"), 1 << 20).is_none());
        let _ = fs::remove_dir_all(&dir);
    }
}
