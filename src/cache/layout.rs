//! On-disk layout of the cache.
//!
//! ```text
//! $XDG_CACHE_HOME/quicklsp/
//! ├── registry.sqlite               (global, all repos)
//! ├── content/
//! │   └── <aa>/<bb>/<hex>.p<v>.fu    (per FileUnit, bincode-serialized)
//! └── repos/
//!     └── <repo_id>/
//!         └── worktrees/
//!             └── <worktree_key>/
//!                 └── manifest.sqlite
//! ```

use std::path::PathBuf;

use crate::cache::identity::{RepoId, WorktreeKey};
use crate::cache::types::{ContentHash, ParserVersion};

/// Returns `$XDG_CACHE_HOME/quicklsp` or `$HOME/.cache/quicklsp`.
pub fn cache_root() -> Option<PathBuf> {
    if let Ok(override_path) = std::env::var("QUICKLSP_CACHE_DIR") {
        return Some(PathBuf::from(override_path));
    }
    let base = std::env::var("XDG_CACHE_HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| PathBuf::from(h).join(".cache"))
        })?;
    Some(base.join("quicklsp"))
}

pub fn content_root() -> Option<PathBuf> {
    cache_root().map(|r| r.join("content"))
}

pub fn registry_path() -> Option<PathBuf> {
    cache_root().map(|r| r.join("registry.sqlite"))
}

pub fn worktree_dir(repo_id: &RepoId, worktree_key: &WorktreeKey) -> Option<PathBuf> {
    cache_root().map(|r| {
        r.join("repos")
            .join(repo_id.to_hex())
            .join("worktrees")
            .join(worktree_key.to_hex())
    })
}

/// Path to a single FileUnit payload on disk.
pub fn file_unit_path(
    content_root: &std::path::Path,
    hash: &ContentHash,
    parser_v: ParserVersion,
) -> PathBuf {
    let hex = hash.to_hex();
    content_root
        .join(&hex[0..2])
        .join(&hex[2..4])
        .join(format!("{hex}.p{parser_v}.fu"))
}
