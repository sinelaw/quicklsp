//! Index persistence: meta.json for freshness checks, warm startup.
//!
//! Indexes are stored in the XDG cache directory under a per-project subdirectory:
//! `~/.cache/quicklsp/<project-hash>/index.v<N>.qlsp`

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Metadata about a persisted word index.
#[derive(Debug, Serialize, Deserialize)]
pub struct IndexMeta {
    /// Index format version (bump on breaking changes).
    pub version: u32,
    /// Number of files that were indexed.
    pub file_count: u64,
    /// Total number of index entries (occurrences).
    pub entry_count: u64,
    /// Number of unique words in the directory.
    pub word_count: u64,
    /// Timestamp when the index was built (seconds since UNIX epoch).
    pub built_at: u64,
    /// Hashes of indexed file paths + mtimes for freshness checking.
    /// We store a single hash rather than per-file data to keep meta.json small.
    pub content_hash: u64,
}

impl IndexMeta {
    /// Path to meta.json in the index directory.
    pub fn meta_path(index_dir: &Path) -> PathBuf {
        index_dir.join("meta.json")
    }

    /// Write meta.json to the index directory.
    pub fn save(&self, index_dir: &Path) -> std::io::Result<()> {
        let path = Self::meta_path(index_dir);
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        std::fs::write(path, json)
    }

    /// Load meta.json from the index directory.
    pub fn load(index_dir: &Path) -> std::io::Result<Self> {
        let path = Self::meta_path(index_dir);
        let json = std::fs::read_to_string(path)?;
        serde_json::from_str(&json)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    /// Check if this index is still fresh for the given project.
    pub fn is_fresh(&self, current_hash: u64) -> bool {
        self.version == CURRENT_VERSION && self.content_hash == current_hash
    }
}

/// Current index format version.
pub const CURRENT_VERSION: u32 = 1;

/// Compute the XDG cache directory for a project's index.
///
/// Returns `$XDG_CACHE_HOME/quicklsp/<project-hash>/` where project-hash
/// is a hex-encoded FNV-1a hash of the canonicalized project root path.
/// Falls back to `~/.cache/quicklsp/<project-hash>/` if XDG_CACHE_HOME is unset.
pub fn index_dir_for_project(project_root: &Path) -> Option<PathBuf> {
    let cache_base = std::env::var("XDG_CACHE_HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| PathBuf::from(h).join(".cache"))
        })?;

    // Hash the canonical project root for a stable, unique directory name
    let canonical = std::fs::canonicalize(project_root)
        .unwrap_or_else(|_| project_root.to_path_buf());
    let hash = path_hash(&canonical);

    Some(cache_base.join("quicklsp").join(format!("{hash:016x}")))
}

/// Versioned index filename: `index.v<N>.qlsp`
pub fn index_filename() -> String {
    format!("index.v{CURRENT_VERSION}.qlsp")
}

/// FNV-1a hash of a path, used for per-project cache directory naming.
fn path_hash(path: &Path) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in path.to_string_lossy().as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Compute a content hash from file paths and their modification times.
/// This is a fast, non-cryptographic hash for freshness checking.
pub fn compute_content_hash(files: &[(PathBuf, SystemTime)]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325; // FNV-1a offset basis
    for (path, mtime) in files {
        // Hash the path
        for byte in path.to_string_lossy().as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x100000001b3); // FNV prime
        }
        // Hash the mtime
        if let Ok(duration) = mtime.duration_since(SystemTime::UNIX_EPOCH) {
            let secs = duration.as_secs();
            for i in 0..8 {
                hash ^= (secs >> (i * 8)) & 0xff;
                hash = hash.wrapping_mul(0x100000001b3);
            }
        }
    }
    hash
}

/// Collect file paths and mtimes for content hash computation.
pub fn collect_file_mtimes(root: &Path, skip_dirs: &dyn Fn(&str) -> bool) -> Vec<(PathBuf, SystemTime)> {
    let mut result = Vec::new();
    collect_mtimes_recursive(root, skip_dirs, &mut result, 0);
    result.sort_by(|a, b| a.0.cmp(&b.0)); // sort for deterministic hash
    result
}

fn collect_mtimes_recursive(
    dir: &Path,
    skip_dirs: &dyn Fn(&str) -> bool,
    result: &mut Vec<(PathBuf, SystemTime)>,
    depth: usize,
) {
    if depth > 20 {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if skip_dirs(name) {
                    continue;
                }
            }
            collect_mtimes_recursive(&path, skip_dirs, result, depth + 1);
        } else if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if crate::parsing::tokenizer::LangFamily::from_extension(ext).is_some() {
                    if let Ok(meta) = std::fs::metadata(&path) {
                        if let Ok(mtime) = meta.modified() {
                            result.push((path, mtime));
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let meta = IndexMeta {
            version: CURRENT_VERSION,
            file_count: 100,
            entry_count: 5000,
            word_count: 1500,
            built_at: 1700000000,
            content_hash: 0xdeadbeef,
        };
        meta.save(dir.path()).unwrap();

        let loaded = IndexMeta::load(dir.path()).unwrap();
        assert_eq!(loaded.version, CURRENT_VERSION);
        assert_eq!(loaded.file_count, 100);
        assert_eq!(loaded.content_hash, 0xdeadbeef);
        assert!(loaded.is_fresh(0xdeadbeef));
        assert!(!loaded.is_fresh(0xbaadf00d));
    }

    #[test]
    fn content_hash_deterministic() {
        let files = vec![
            (PathBuf::from("/a.rs"), SystemTime::UNIX_EPOCH),
            (PathBuf::from("/b.rs"), SystemTime::UNIX_EPOCH),
        ];
        let h1 = compute_content_hash(&files);
        let h2 = compute_content_hash(&files);
        assert_eq!(h1, h2);
    }

    #[test]
    fn content_hash_changes_with_files() {
        let files1 = vec![(PathBuf::from("/a.rs"), SystemTime::UNIX_EPOCH)];
        let files2 = vec![(PathBuf::from("/b.rs"), SystemTime::UNIX_EPOCH)];
        assert_ne!(compute_content_hash(&files1), compute_content_hash(&files2));
    }

    #[test]
    fn index_dir_uses_xdg_cache() {
        // With XDG_CACHE_HOME set
        std::env::set_var("XDG_CACHE_HOME", "/tmp/test-xdg-cache");
        let dir = index_dir_for_project(Path::new("/home/user/myproject")).unwrap();
        assert!(dir.starts_with("/tmp/test-xdg-cache/quicklsp/"));
        assert!(!dir.to_string_lossy().contains("myproject")); // hashed, not literal
        std::env::remove_var("XDG_CACHE_HOME");
    }

    #[test]
    fn index_filename_includes_version() {
        let name = index_filename();
        assert!(name.starts_with("index.v"), "got: {name}");
        assert!(name.ends_with(".qlsp"), "got: {name}");
        assert!(name.contains(&CURRENT_VERSION.to_string()), "got: {name}");
    }

    #[test]
    fn different_projects_get_different_dirs() {
        std::env::set_var("XDG_CACHE_HOME", "/tmp/test-xdg-cache2");
        let d1 = index_dir_for_project(Path::new("/project/a")).unwrap();
        let d2 = index_dir_for_project(Path::new("/project/b")).unwrap();
        assert_ne!(d1, d2);
        std::env::remove_var("XDG_CACHE_HOME");
    }
}
