//! Runtime cache state owned by a `Workspace`.
//!
//! Wraps Layer A (content store) + Layer B (manifest) + the in-memory
//! posting list rebuilt from them. Also holds the `RepoIdentity` for the
//! workspace root.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;

use ahash::AHashMap;

use crate::cache::content_store::ContentStore;
use crate::cache::identity::{self, RepoIdentity};
use crate::cache::layout;
use crate::cache::manifest::{Manifest, ManifestRow};
use crate::cache::metrics::ScanMetrics;
use crate::cache::registry::Registry;
use crate::cache::types::{ContentHash, FileUnit, ParserVersion};

/// In-memory cache state. One per `Workspace`.
pub struct CacheState {
    pub identity: RepoIdentity,
    pub content_store: ContentStore,
    pub manifest: Mutex<Manifest>,
    /// word_hash (FNV-1a) → rel_paths that contain it.
    /// Rebuilt on scan; queried by `find_references`.
    pub postings: AHashMap<u32, Vec<String>>,
    pub metrics: Arc<ScanMetrics>,
}

impl CacheState {
    /// Open (or create) the cache for a workspace root.
    ///
    /// Registers the worktree in the global registry. Does not populate any
    /// in-memory data — use `load_in_memory` / scan to do that.
    pub fn open(root: &Path, metrics: Arc<ScanMetrics>) -> std::io::Result<Self> {
        let identity = identity::detect_identity(root);

        let content_root = layout::content_root().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "no cache root")
        })?;
        let content_store = ContentStore::open(content_root, metrics.clone())?;

        let wt_dir = layout::worktree_dir(&identity.repo_id, &identity.worktree_key)
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "no worktree dir")
            })?;
        std::fs::create_dir_all(&wt_dir)?;
        let manifest_path = wt_dir.join("manifest.sqlite");
        let manifest = Manifest::open(&manifest_path)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

        // Register in global registry (best-effort — failures are non-fatal).
        if let Some(reg_path) = layout::registry_path() {
            if let Ok(reg) = Registry::open(&reg_path) {
                let _ = reg.upsert(
                    &identity.repo_id,
                    &identity.worktree_key,
                    &identity.working_dir,
                );
            }
        }

        Ok(Self {
            identity,
            content_store,
            manifest: Mutex::new(manifest),
            postings: AHashMap::new(),
            metrics,
        })
    }

    /// Convert an absolute path to a manifest-relative string, if possible.
    pub fn rel_path(&self, path: &Path) -> Option<String> {
        let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        canon
            .strip_prefix(&self.identity.working_dir)
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
    }

    /// Resolve a manifest-relative path back to an absolute path.
    pub fn abs_path(&self, rel: &str) -> PathBuf {
        self.identity.working_dir.join(rel)
    }

    /// Add a (rel_path, word_hashes) pair to the in-memory posting list.
    pub fn add_to_postings(&mut self, rel_path: &str, word_hashes: &[u32]) {
        for &wh in word_hashes {
            self.postings
                .entry(wh)
                .or_default()
                .push(rel_path.to_string());
        }
    }

    /// Remove a rel_path from all posting lists it appears in.
    pub fn remove_from_postings(&mut self, rel_path: &str) {
        for files in self.postings.values_mut() {
            files.retain(|p| p != rel_path);
        }
    }

    /// Find candidate files that contain `name` via the posting list.
    pub fn candidate_files(&self, name: &str) -> Vec<PathBuf> {
        let hash = crate::cache::word_hash_fnv1a(name);
        match self.postings.get(&hash) {
            Some(files) => files.iter().map(|r| self.abs_path(r)).collect(),
            None => Vec::new(),
        }
    }

    /// Ensure the manifest's `parser_version` matches. If it doesn't, wipe
    /// the manifest (but not Layer A — other parser versions may share it).
    pub fn check_parser_version(&self, current: ParserVersion) -> std::io::Result<bool> {
        let m = self
            .manifest
            .lock()
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::Other, "manifest poisoned"))?;
        let stored = m
            .parser_version()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        Ok(stored == Some(current))
    }

    /// Locate prior manifests for the same `repo_id` whose `working_dir` is
    /// an ancestor or descendant of ours (see design §5.2). Returns rows
    /// re-prefixed to our current `working_dir`. Already-verified Layer A
    /// hits are trusted; stat validation happens later in the main scan.
    pub fn collect_subsumable_rows(&self) -> Vec<ManifestRow> {
        let Some(reg_path) = layout::registry_path() else {
            return Vec::new();
        };
        let Ok(reg) = Registry::open(&reg_path) else {
            return Vec::new();
        };
        let others = reg
            .worktrees_for_repo(&self.identity.repo_id)
            .unwrap_or_default();

        let here = &self.identity.working_dir;
        let mut out: Vec<ManifestRow> = Vec::new();
        // For deterministic conflict resolution between overlapping subtrees,
        // higher generation wins; a later pass de-dupes by rel_path.
        for wt in others {
            if wt.worktree_key == self.identity.worktree_key {
                continue;
            }
            let other_dir = &wt.working_dir;
            let relation = path_relation(here, other_dir);
            let Some(kind) = relation else {
                continue;
            };
            let Some(other_dir_abs) =
                layout::worktree_dir(&wt.repo_id, &wt.worktree_key)
            else {
                continue;
            };
            let other_manifest_path = other_dir_abs.join("manifest.sqlite");
            if !other_manifest_path.exists() {
                continue;
            }
            let Ok(other_manifest) = Manifest::open(&other_manifest_path) else {
                continue;
            };
            match kind {
                PathRelation::OtherIsDescendant(rel) => {
                    // Opening a parent of an already-indexed subtree.
                    // Their rel_paths are relative to other_dir; ours need
                    // to be prefixed with `rel/`.
                    let rows = other_manifest.all_rows().unwrap_or_default();
                    for mut r in rows {
                        r.rel_path = format!(
                            "{}{}{}",
                            rel,
                            std::path::MAIN_SEPARATOR,
                            r.rel_path
                        );
                        out.push(r);
                    }
                }
                PathRelation::OtherIsAncestor(rel) => {
                    // Opening a child of an indexed parent. We only want
                    // their rows under `rel/`, with that prefix stripped.
                    let prefix_with_sep = format!("{}{}", rel, std::path::MAIN_SEPARATOR);
                    let rows = other_manifest
                        .rows_with_prefix(&prefix_with_sep)
                        .unwrap_or_default();
                    for mut r in rows {
                        r.rel_path = r
                            .rel_path
                            .strip_prefix(&prefix_with_sep)
                            .unwrap_or(&r.rel_path)
                            .to_string();
                        out.push(r);
                    }
                }
            }
        }
        // De-dupe by rel_path, highest generation wins.
        out.sort_by(|a, b| a.rel_path.cmp(&b.rel_path).then(b.generation.cmp(&a.generation)));
        out.dedup_by(|a, b| a.rel_path == b.rel_path);
        out
    }
}

#[derive(Debug)]
enum PathRelation {
    /// Our working_dir contains the other one; `String` is the sub-path from us to them.
    OtherIsDescendant(String),
    /// The other working_dir contains ours; `String` is the sub-path from them to us.
    OtherIsAncestor(String),
}

fn path_relation(here: &Path, other: &Path) -> Option<PathRelation> {
    if let Ok(rel) = other.strip_prefix(here) {
        let s = rel.to_string_lossy().into_owned();
        if s.is_empty() {
            return None;
        }
        return Some(PathRelation::OtherIsDescendant(s));
    }
    if let Ok(rel) = here.strip_prefix(other) {
        let s = rel.to_string_lossy().into_owned();
        if s.is_empty() {
            return None;
        }
        return Some(PathRelation::OtherIsAncestor(s));
    }
    None
}

/// Opaque handle used during a scan: parses a file to a `FileUnit` if
/// Layer A doesn't already have one, then upserts to both Layer A and the
/// in-memory posting list. Pure static methods — no `self`.
pub struct CacheOps;

impl CacheOps {
    /// Look up a hash in Layer A; if absent, run `parse` and write.
    /// Returns the FileUnit (either loaded or newly constructed).
    pub fn ensure_file_unit<F>(
        store: &ContentStore,
        metrics: &ScanMetrics,
        hash: &ContentHash,
        parser_version: ParserVersion,
        parse: F,
    ) -> std::io::Result<FileUnit>
    where
        F: FnOnce() -> FileUnit,
    {
        if let Some(unit) = store.get(hash, parser_version) {
            return Ok(unit);
        }
        metrics.files_parsed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let unit = parse();
        store.put(hash, &unit)?;
        Ok(unit)
    }
}

/// Build a `ManifestRow` from collected fields.
#[allow(clippy::too_many_arguments)]
pub fn build_row(
    rel_path: String,
    content_hash: ContentHash,
    lang: Option<u32>,
    size: u64,
    mtime_ns: i128,
    generation: u64,
) -> ManifestRow {
    ManifestRow {
        rel_path,
        content_hash,
        lang,
        size,
        mtime_ns,
        git_oid: None,
        generation,
    }
}
