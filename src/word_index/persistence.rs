//! Index persistence: meta.json for per-file freshness checks, warm startup.
//!
//! Indexes are stored in the XDG cache directory under a per-project subdirectory:
//! `~/.cache/quicklsp/<project-hash>/` with three data files + meta.json.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Metadata about a persisted word index (v2: per-file mtime tracking).
#[derive(Debug, Serialize, Deserialize)]
pub struct IndexMeta {
    pub version: u32,
    pub file_count: u64,
    pub entry_count: u64,
    pub word_count: u64,
    pub built_at: u64,
    /// Per-file modification times (path → epoch seconds).
    pub file_mtimes: HashMap<String, u64>,
}

impl IndexMeta {
    pub fn meta_path(index_dir: &Path) -> PathBuf {
        index_dir.join("meta.json")
    }

    pub fn save(&self, index_dir: &Path) -> std::io::Result<()> {
        let path = Self::meta_path(index_dir);
        let json = serde_json::to_string(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        std::fs::write(path, json)
    }

    pub fn load(index_dir: &Path) -> std::io::Result<Self> {
        let path = Self::meta_path(index_dir);
        let json = std::fs::read_to_string(path)?;
        serde_json::from_str(&json)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    /// Check if this index is fully fresh (no files changed).
    pub fn is_fresh(&self, current_mtimes: &[(PathBuf, SystemTime)]) -> bool {
        if self.version != CURRENT_VERSION {
            return false;
        }
        if current_mtimes.len() != self.file_mtimes.len() {
            return false;
        }
        for (path, mtime) in current_mtimes {
            let path_str = path.to_string_lossy();
            let secs = mtime
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            match self.file_mtimes.get(path_str.as_ref()) {
                Some(&stored) if stored == secs => {}
                _ => return false,
            }
        }
        true
    }

    /// Build the mtime map from collected file mtimes.
    pub fn build_mtime_map(files: &[(PathBuf, SystemTime)]) -> HashMap<String, u64> {
        files
            .iter()
            .map(|(path, mtime)| {
                let secs = mtime
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                (path.to_string_lossy().into_owned(), secs)
            })
            .collect()
    }
}

pub const CURRENT_VERSION: u32 = 2;

/// Compute the XDG cache directory for a project's index.
pub fn index_dir_for_project(project_root: &Path) -> Option<PathBuf> {
    let cache_base = std::env::var("XDG_CACHE_HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| PathBuf::from(h).join(".cache"))
        })?;

    let canonical = std::fs::canonicalize(project_root)
        .unwrap_or_else(|_| project_root.to_path_buf());
    let hash = path_hash(&canonical);

    Some(cache_base.join("quicklsp").join(format!("{hash:016x}")))
}

fn path_hash(path: &Path) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in path.to_string_lossy().as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Collect file paths and mtimes for freshness checking.
pub fn collect_file_mtimes(root: &Path, skip_dirs: &dyn Fn(&str) -> bool) -> Vec<(PathBuf, SystemTime)> {
    let mut result = Vec::new();
    collect_mtimes_recursive(root, skip_dirs, &mut result, 0);
    result.sort_by(|a, b| a.0.cmp(&b.0));
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
        let mut file_mtimes = HashMap::new();
        file_mtimes.insert("/a.rs".to_string(), 1700000000u64);
        file_mtimes.insert("/b.rs".to_string(), 1700000001u64);
        let meta = IndexMeta {
            version: CURRENT_VERSION,
            file_count: 2,
            entry_count: 5000,
            word_count: 1500,
            built_at: 1700000000,
            file_mtimes,
        };
        meta.save(dir.path()).unwrap();

        let loaded = IndexMeta::load(dir.path()).unwrap();
        assert_eq!(loaded.version, CURRENT_VERSION);
        assert_eq!(loaded.file_count, 2);
        assert_eq!(loaded.file_mtimes.len(), 2);
    }

    #[test]
    fn index_dir_uses_xdg_cache() {
        std::env::set_var("XDG_CACHE_HOME", "/tmp/test-xdg-cache");
        let dir = index_dir_for_project(Path::new("/home/user/myproject")).unwrap();
        assert!(dir.starts_with("/tmp/test-xdg-cache/quicklsp/"));
        std::env::remove_var("XDG_CACHE_HOME");
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
