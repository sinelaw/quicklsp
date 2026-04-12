//! Repo / worktree identity detection.
//!
//! `RepoId` identifies "the same repository" across disk locations.
//! `WorktreeKey` identifies "this specific checkout on disk".
//! Two worktrees of the same repo share a `RepoId` but have different `WorktreeKey`s.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::cache::types::ContentHash;

/// Stable identifier for a repository across clones/worktrees.
///
/// Derived from git root commit + normalized remote URLs when available;
/// falls back to a canonical-path fingerprint for non-git trees.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RepoId(pub [u8; 32]);

impl RepoId {
    pub fn to_hex(&self) -> String {
        ContentHash(self.0).to_hex()
    }
    pub fn from_hex(s: &str) -> Option<Self> {
        ContentHash::from_hex(s).map(|c| RepoId(c.0))
    }
}

impl std::fmt::Debug for RepoId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RepoId({})", self.to_hex())
    }
}

/// Stable identifier for a specific checkout on disk.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorktreeKey(pub [u8; 32]);

impl WorktreeKey {
    pub fn to_hex(&self) -> String {
        ContentHash(self.0).to_hex()
    }
    pub fn from_hex(s: &str) -> Option<Self> {
        ContentHash::from_hex(s).map(|c| WorktreeKey(c.0))
    }
}

impl std::fmt::Debug for WorktreeKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "WorktreeKey({})", self.to_hex())
    }
}

/// Source of a repo identity — primarily for observability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum IdentitySource {
    /// Git root commit (strongest; clones of the same repo match).
    GitRootCommit { oid: String },
    /// Git remotes but no readable root commit (e.g. shallow clone).
    GitRemotes { urls: Vec<String> },
    /// Non-git: canonical path fingerprint.
    CanonicalPath,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoIdentity {
    pub repo_id: RepoId,
    pub worktree_key: WorktreeKey,
    pub source: IdentitySource,
    /// Canonical working directory of this worktree.
    pub working_dir: PathBuf,
    /// Canonical git common-dir (shared across worktrees of the same repo), if any.
    pub common_dir: Option<PathBuf>,
}

/// Detect repo + worktree identity for a workspace root.
///
/// Algorithm (see design §2):
/// 1. Resolve `.git` (directory or linked-worktree file → gitdir → commondir).
/// 2. Try `git rev-list --max-parents=0 HEAD` for the root commit.
/// 3. Collect normalized remote URLs from `config`.
/// 4. Compose `RepoId = BLAKE3(source-bytes)`.
/// 5. `WorktreeKey = BLAKE3(repo_id || commondir || working_dir)`.
pub fn detect_identity(root: &Path) -> RepoIdentity {
    let working_dir = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());

    // Step 1: resolve .git → gitdir → commondir.
    let (gitdir, common_dir) = resolve_git_dirs(&working_dir);

    // Step 2: root commit.
    let root_commit = common_dir.as_ref().and_then(|cd| git_root_commit(cd));
    // Step 3: normalized remotes (used in fallback).
    let remotes = common_dir
        .as_ref()
        .map(|cd| git_remotes(cd))
        .unwrap_or_default();

    // Step 4: compose RepoId.
    let (repo_id, source) = if let Some(oid) = root_commit.clone() {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"v3:git:root_commit:");
        hasher.update(oid.as_bytes());
        for url in &remotes {
            hasher.update(b"\n");
            hasher.update(url.as_bytes());
        }
        let repo_id = RepoId(*hasher.finalize().as_bytes());
        (repo_id, IdentitySource::GitRootCommit { oid })
    } else if !remotes.is_empty() {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"v3:git:remotes:");
        for url in &remotes {
            hasher.update(b"\n");
            hasher.update(url.as_bytes());
        }
        let repo_id = RepoId(*hasher.finalize().as_bytes());
        (repo_id, IdentitySource::GitRemotes { urls: remotes })
    } else {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"v3:path:");
        hasher.update(working_dir.to_string_lossy().as_bytes());
        let repo_id = RepoId(*hasher.finalize().as_bytes());
        (repo_id, IdentitySource::CanonicalPath)
    };

    // Step 5: WorktreeKey.
    let mut h = blake3::Hasher::new();
    h.update(b"v3:worktree:");
    h.update(&repo_id.0);
    h.update(b"|commondir=");
    if let Some(ref cd) = common_dir {
        h.update(cd.to_string_lossy().as_bytes());
    }
    h.update(b"|working_dir=");
    h.update(working_dir.to_string_lossy().as_bytes());
    let worktree_key = WorktreeKey(*h.finalize().as_bytes());

    let _ = gitdir; // currently unused beyond the common-dir resolution below

    RepoIdentity {
        repo_id,
        worktree_key,
        source,
        working_dir,
        common_dir,
    }
}

/// Walk up from `start` looking for a `.git` entry. Returns `(gitdir, commondir)`
/// where `gitdir` may equal `commondir` (standard clone) or point into
/// `commondir/worktrees/<name>/` (linked worktree).
fn resolve_git_dirs(start: &Path) -> (Option<PathBuf>, Option<PathBuf>) {
    let mut cur = Some(start.to_path_buf());
    while let Some(dir) = cur {
        let candidate = dir.join(".git");
        if candidate.is_dir() {
            let gitdir = std::fs::canonicalize(&candidate).unwrap_or(candidate);
            let common_dir = read_commondir(&gitdir).unwrap_or_else(|| gitdir.clone());
            return (Some(gitdir), Some(common_dir));
        }
        if candidate.is_file() {
            if let Ok(contents) = std::fs::read_to_string(&candidate) {
                if let Some(rest) = contents.trim().strip_prefix("gitdir:") {
                    let raw = rest.trim();
                    let linked = if Path::new(raw).is_absolute() {
                        PathBuf::from(raw)
                    } else {
                        dir.join(raw)
                    };
                    let gitdir = std::fs::canonicalize(&linked).unwrap_or(linked);
                    let common_dir =
                        read_commondir(&gitdir).unwrap_or_else(|| gitdir.clone());
                    return (Some(gitdir), Some(common_dir));
                }
            }
        }
        cur = dir.parent().map(Path::to_path_buf);
    }
    (None, None)
}

/// If `gitdir/commondir` exists, read it and resolve to an absolute path
/// (rooted at `gitdir` if relative). Otherwise return `None`.
fn read_commondir(gitdir: &Path) -> Option<PathBuf> {
    let f = gitdir.join("commondir");
    let contents = std::fs::read_to_string(&f).ok()?;
    let rel = contents.trim();
    let abs = if Path::new(rel).is_absolute() {
        PathBuf::from(rel)
    } else {
        gitdir.join(rel)
    };
    std::fs::canonicalize(&abs).ok().or(Some(abs))
}

/// Run `git -C <common_dir> rev-list --max-parents=0 HEAD` and return first root commit.
fn git_root_commit(common_dir: &Path) -> Option<String> {
    // common_dir is the .git directory — pass it via --git-dir so we don't
    // need a working tree.
    let out = std::process::Command::new("git")
        .arg("--git-dir")
        .arg(common_dir)
        .arg("rev-list")
        .arg("--max-parents=0")
        .arg("HEAD")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    let mut oids: Vec<String> = s
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    if oids.is_empty() {
        return None;
    }
    oids.sort();
    // For merged histories with multiple root commits, join them deterministically.
    Some(oids.join(","))
}

fn git_remotes(common_dir: &Path) -> Vec<String> {
    let out = std::process::Command::new("git")
        .arg("--git-dir")
        .arg(common_dir)
        .arg("config")
        .arg("--get-regexp")
        .arg("^remote\\..*\\.url$")
        .output();
    let Ok(out) = out else { return Vec::new() };
    if !out.status.success() {
        return Vec::new();
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let mut urls: Vec<String> = s
        .lines()
        .filter_map(|l| l.split_once(' ').map(|(_, u)| normalize_remote(u.trim())))
        .collect();
    urls.sort();
    urls.dedup();
    urls
}

fn normalize_remote(url: &str) -> String {
    // Turn git@github.com:foo/bar(.git) into https://github.com/foo/bar and
    // strip trailing .git / trailing slash. Best-effort.
    let url = url.trim().trim_end_matches('/');
    let url = url.strip_suffix(".git").unwrap_or(url);
    if let Some(rest) = url.strip_prefix("git@") {
        if let Some((host, path)) = rest.split_once(':') {
            return format!("https://{host}/{path}").to_lowercase();
        }
    }
    url.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_git_dir_yields_path_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let id = detect_identity(tmp.path());
        assert!(matches!(id.source, IdentitySource::CanonicalPath));
        assert_eq!(
            id.working_dir,
            std::fs::canonicalize(tmp.path()).unwrap()
        );
    }

    #[test]
    fn same_non_git_dir_is_stable() {
        let tmp = tempfile::tempdir().unwrap();
        let a = detect_identity(tmp.path());
        let b = detect_identity(tmp.path());
        assert_eq!(a.repo_id, b.repo_id);
        assert_eq!(a.worktree_key, b.worktree_key);
    }

    #[test]
    fn different_non_git_dirs_differ() {
        let t1 = tempfile::tempdir().unwrap();
        let t2 = tempfile::tempdir().unwrap();
        let a = detect_identity(t1.path());
        let b = detect_identity(t2.path());
        assert_ne!(a.repo_id, b.repo_id);
    }

    #[test]
    fn normalize_ssh_remote() {
        assert_eq!(
            normalize_remote("git@github.com:foo/bar.git"),
            "https://github.com/foo/bar"
        );
        assert_eq!(
            normalize_remote("https://github.com/Foo/Bar/"),
            "https://github.com/foo/bar"
        );
    }
}
