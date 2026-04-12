//! Layer A: global content-addressable store of `FileUnit`s.
//!
//! On disk: one file per (ContentHash, ParserVersion) at
//! `<content_root>/<aa>/<bb>/<hex>.p<v>.fu`, holding a bincode-serialized
//! `FileUnit`. Writes are atomic (write tmp + rename). Reads are lock-free.
//! Duplicate writers of the same hash are idempotent (identical bytes).

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;

use crate::cache::layout;
use crate::cache::metrics::ScanMetrics;
use crate::cache::types::{ContentHash, FileUnit, ParserVersion};

/// Handle to the on-disk content store. Cheap to clone.
#[derive(Clone)]
pub struct ContentStore {
    root: PathBuf,
    metrics: Arc<ScanMetrics>,
}

impl ContentStore {
    /// Open (and create if missing) the content store at `root`.
    pub fn open(root: PathBuf, metrics: Arc<ScanMetrics>) -> io::Result<Self> {
        std::fs::create_dir_all(&root)?;
        Ok(Self { root, metrics })
    }

    /// Check whether an entry exists, without reading it.
    pub fn contains(&self, hash: &ContentHash, parser_v: ParserVersion) -> bool {
        layout::file_unit_path(&self.root, hash, parser_v).exists()
    }

    /// Fetch a FileUnit by content hash. Returns `None` if absent or unreadable.
    pub fn get(&self, hash: &ContentHash, parser_v: ParserVersion) -> Option<FileUnit> {
        let path = layout::file_unit_path(&self.root, hash, parser_v);
        let bytes = std::fs::read(&path).ok()?;
        let unit: FileUnit = bincode::deserialize(&bytes).ok()?;
        self.metrics.layer_a_hits.fetch_add(1, Relaxed);
        Some(unit)
    }

    /// Write a FileUnit. Idempotent: if the entry already exists, does nothing
    /// (no bytes written, no metric bump).
    pub fn put(&self, hash: &ContentHash, unit: &FileUnit) -> io::Result<()> {
        let path = layout::file_unit_path(&self.root, hash, unit.parser_version);
        if path.exists() {
            return Ok(());
        }
        let parent = path.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "no parent dir")
        })?;
        std::fs::create_dir_all(parent)?;
        let payload = bincode::serialize(unit)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        let tmp = parent.join(format!(
            ".tmp-{}-{}",
            hash.to_hex(),
            std::process::id()
        ));
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&payload)?;
            f.sync_data()?;
        }
        // Atomic on POSIX when tmp and final are on the same filesystem.
        match std::fs::rename(&tmp, &path) {
            Ok(()) => {
                self.metrics.layer_a_writes.fetch_add(1, Relaxed);
                Ok(())
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                // If another writer raced us and the entry now exists, that's fine.
                if path.exists() {
                    Ok(())
                } else {
                    Err(e)
                }
            }
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::PARSER_VERSION;

    fn dummy_unit() -> FileUnit {
        FileUnit {
            parser_version: PARSER_VERSION,
            lang: None,
            symbols: vec![],
            word_hashes: vec![1, 2, 3],
        }
    }

    #[test]
    fn put_then_get() {
        let tmp = tempfile::tempdir().unwrap();
        let store =
            ContentStore::open(tmp.path().to_path_buf(), Arc::new(ScanMetrics::new())).unwrap();
        let h = ContentHash::of_bytes(b"abc");
        assert!(!store.contains(&h, PARSER_VERSION));
        store.put(&h, &dummy_unit()).unwrap();
        assert!(store.contains(&h, PARSER_VERSION));
        let got = store.get(&h, PARSER_VERSION).unwrap();
        assert_eq!(got.word_hashes, vec![1, 2, 3]);
    }

    #[test]
    fn put_is_idempotent_no_second_write() {
        let tmp = tempfile::tempdir().unwrap();
        let metrics = Arc::new(ScanMetrics::new());
        let store = ContentStore::open(tmp.path().to_path_buf(), metrics.clone()).unwrap();
        let h = ContentHash::of_bytes(b"dedup");
        store.put(&h, &dummy_unit()).unwrap();
        let writes1 = metrics.layer_a_writes.load(Relaxed);
        store.put(&h, &dummy_unit()).unwrap();
        let writes2 = metrics.layer_a_writes.load(Relaxed);
        assert_eq!(writes1, writes2, "idempotent put must not re-write");
    }

    #[test]
    fn get_missing_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let store =
            ContentStore::open(tmp.path().to_path_buf(), Arc::new(ScanMetrics::new())).unwrap();
        let h = ContentHash::of_bytes(b"absent");
        assert!(store.get(&h, PARSER_VERSION).is_none());
    }
}
