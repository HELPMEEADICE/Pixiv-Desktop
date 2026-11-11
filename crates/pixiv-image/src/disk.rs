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
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use tracing::warn;

/// Encoded-bytes tier with a byte budget and an explicit prune path.
pub(crate) struct DiskTier {
    dir: PathBuf,
    budget: u64,
    bytes: AtomicU64,
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
    pub(crate) fn put(&self, key: &str, bytes: &[u8]) -> io::Result<()> {
        let path = self.path(key);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let previous = fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
        let temp = path.with_extension("part");
        fs::write(&temp, bytes)?;
        fs::rename(&temp, &path)?;
        self.bytes.fetch_add(bytes.len() as u64, Ordering::Relaxed);
        self.bytes.fetch_sub(previous, Ordering::Relaxed);
        if self.bytes() > self.budget {
            self.enforce_budget();
        }
        Ok(())
    }

    pub(crate) fn remove(&self, key: &str) -> io::Result<()> {
        let path = self.path(key);
        let freed = fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
        match fs::remove_file(&path) {
            Ok(()) => {
                self.bytes.fetch_sub(freed, Ordering::Relaxed);
                Ok(())
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err),
        }
    }

    /// Delete everything the tier holds, returning the bytes reclaimed.
    pub(crate) fn clear(&self) -> io::Result<u64> {
        let entries = walk(&self.dir)?;
        let mut reclaimed = 0;
        for (_, size, path) in entries {
            match fs::remove_file(&path) {
                Ok(()) => reclaimed += size,
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(err),
            }
        }
        self.bytes.store(0, Ordering::Relaxed);
        Ok(reclaimed)
    }

    /// Drop oldest entries until the budget holds. Called only when over.
    fn enforce_budget(&self) {
        let Ok(mut entries) = walk(&self.dir) else {
            return;
        };
        entries.sort_by_key(|entry| entry.0);
        let mut total = self.bytes();
        for (_, size, path) in entries {
            if total <= self.budget {
                break;
            }
            if fs::remove_file(&path).is_ok() {
                total = total.saturating_sub(size);
            }
        }
        self.bytes.store(total, Ordering::Relaxed);
    }
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
