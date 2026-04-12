//! Integration tests that validate the end-to-end objectives of the cache v3
//! design (see `docs/cache-v3-design.md` §9).
//!
//! Each test pairs a **performance property** (metric counter deltas) with a
//! **correctness property** (query results match a baseline). Tests use an
//! isolated `$QUICKLSP_CACHE_DIR` per test to avoid cross-talk with other
//! tests or the user's real cache.

use std::path::Path;

use quicklsp::Workspace;

/// Isolates each test to its own cache root via the `QUICKLSP_CACHE_DIR`
/// environment variable. Because env vars are process-global, tests that
/// use this helper MUST NOT run concurrently — we mark them with
/// `#[serial]` via a tiny homemade lock (no extra crate).
struct IsolatedCache {
    tmp: tempfile::TempDir,
}

impl IsolatedCache {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("QUICKLSP_CACHE_DIR", tmp.path());
        Self { tmp }
    }
    fn path(&self) -> &Path {
        self.tmp.path()
    }
}

impl Drop for IsolatedCache {
    fn drop(&mut self) {
        std::env::remove_var("QUICKLSP_CACHE_DIR");
    }
}

/// Since env vars are process-global and cargo runs tests concurrently by
/// default, we serialize all tests in this file through a single mutex.
static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn guard() -> std::sync::MutexGuard<'static, ()> {
    // On poison, recover — previous panic inside a test is expected to
    // terminate that test, not mark the mutex unusable for later tests.
    match TEST_LOCK.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}

// ── Helpers ────────────────────────────────────────────────────────────

/// Write a minimal "repo" of N synthetic Rust files into `dir`.
fn make_synthetic_repo(dir: &Path, n: usize) {
    for i in 0..n {
        std::fs::write(
            dir.join(format!("f_{i}.rs")),
            format!("fn unique_{i}() {{ shared(); }}\nfn shared() {{}}", i = i),
        )
        .unwrap();
    }
}

/// Copy a directory tree (files only, same structure) into `dst`.
fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let ft = entry.file_type().unwrap();
        let to = dst.join(entry.file_name());
        if ft.is_dir() {
            copy_tree(&entry.path(), &to);
        } else if ft.is_file() {
            std::fs::copy(entry.path(), to).unwrap();
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct QueryFixture {
    def_counts: Vec<(String, usize)>,
    ref_counts: Vec<(String, usize)>,
}

/// A deterministic summary of workspace behavior used as the correctness
/// baseline across locations/branches.
fn query_fixture(ws: &Workspace, names: &[&str]) -> QueryFixture {
    let mut def_counts: Vec<(String, usize)> = names
        .iter()
        .map(|n| (n.to_string(), ws.find_definitions(n).len()))
        .collect();
    def_counts.sort();
    let mut ref_counts: Vec<(String, usize)> = names
        .iter()
        .map(|n| (n.to_string(), ws.find_references(n).len()))
        .collect();
    ref_counts.sort();
    QueryFixture {
        def_counts,
        ref_counts,
    }
}

// ── T1: Clone reuse ────────────────────────────────────────────────────

#[test]
fn t1_second_clone_zero_parses() {
    let _g = guard();
    let _cache = IsolatedCache::new();

    let clone1 = tempfile::tempdir().unwrap();
    let clone2 = tempfile::tempdir().unwrap();
    make_synthetic_repo(clone1.path(), 40);
    copy_tree(clone1.path(), clone2.path());

    // First clone: warm up Layer A.
    let ws1 = Workspace::new();
    ws1.scan_directory(clone1.path(), None);
    let baseline = query_fixture(&ws1, &["unique_0", "unique_20", "shared"]);
    assert_eq!(baseline.def_counts.iter().find(|(n, _)| n == "shared").unwrap().1, 40);

    // Second clone: reset metrics, scan again.
    let ws2 = Workspace::new();
    ws2.metrics().reset();
    ws2.scan_directory(clone2.path(), None);
    let snap = ws2.metrics().snapshot();

    // Performance: no parsing. Every file's content hash is already in Layer A.
    assert_eq!(
        snap.files_parsed, 0,
        "second clone must not parse anything (hits: {}, writes: {})",
        snap.layer_a_hits, snap.layer_a_writes
    );
    assert_eq!(
        snap.layer_a_writes, 0,
        "second clone must not write new FileUnits"
    );
    assert!(snap.layer_a_hits >= 40, "every file should hit Layer A");

    // Correctness: identical query results.
    let got = query_fixture(&ws2, &["unique_0", "unique_20", "shared"]);
    assert_eq!(got, baseline);
}

// ── T4: Parent subsumption ─────────────────────────────────────────────

#[test]
fn t4_parent_of_indexed_subtree_reuses_manifest() {
    let _g = guard();
    let _cache = IsolatedCache::new();

    let repo = tempfile::tempdir().unwrap();
    let src = repo.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    make_synthetic_repo(&src, 12);

    // Sibling directory that only exists under the parent, not under src.
    let tests = repo.path().join("tests");
    std::fs::create_dir_all(&tests).unwrap();
    std::fs::write(
        tests.join("t.rs"),
        "fn only_in_tests() {}\nfn more_tests() {}",
    )
    .unwrap();

    // Phase 1: index only src/.
    let ws_sub = Workspace::new();
    ws_sub.scan_directory(&src, None);
    let sub_baseline = query_fixture(&ws_sub, &["unique_0", "shared"]);
    assert_eq!(sub_baseline.def_counts.iter().find(|(n, _)| n == "shared").unwrap().1, 12);
    drop(ws_sub); // flush SQLite connections for the child manifest.

    // Phase 2: open the parent. Must subsume the src/ manifest.
    let ws_parent = Workspace::new();
    ws_parent.metrics().reset();
    ws_parent.scan_directory(repo.path(), None);
    let snap = ws_parent.metrics().snapshot();

    // Performance property: at least the src subtree rows were copied from
    // the child manifest.
    assert!(
        snap.manifest_rows_copied >= 12,
        "expected subsumption of >=12 rows, got {}",
        snap.manifest_rows_copied
    );
    // The 12 src/ files should produce zero parses (stat fast-path + Layer A hit).
    // Only the 1 new tests/ file may need to be parsed.
    assert!(
        snap.files_parsed <= 1,
        "parent should only parse new files; got files_parsed={}",
        snap.files_parsed
    );

    // Correctness:
    // (a) Queries against src/ symbols still match.
    assert_eq!(ws_parent.find_definitions("shared").len(), 12);
    assert_eq!(ws_parent.find_definitions("unique_0").len(), 1);
    // (b) New siblings are visible.
    assert_eq!(ws_parent.find_definitions("only_in_tests").len(), 1);
}

// ── T12: Content change only ───────────────────────────────────────────

#[test]
fn t12_edit_writes_exactly_one_new_file_unit() {
    let _g = guard();
    let _cache = IsolatedCache::new();

    let repo = tempfile::tempdir().unwrap();
    make_synthetic_repo(repo.path(), 10);

    let ws = Workspace::new();
    ws.scan_directory(repo.path(), None);

    // Touch one file: same lang, different content.
    let edited = repo.path().join("f_3.rs");
    std::fs::write(
        &edited,
        "fn unique_3() { shared(); }\nfn shared() {}\nfn newly_added() {}",
    )
    .unwrap();
    // Force a different mtime (filesystem resolution may match).
    let t = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
    filetime::set_file_mtime(&edited, t).ok();

    ws.metrics().reset();
    ws.scan_directory(repo.path(), None);
    let snap = ws.metrics().snapshot();

    // Exactly one file's bytes should have been read + hashed + parsed.
    assert!(
        snap.files_blake3_hashed <= 1,
        "only the edited file needs BLAKE3 re-hashing, got {}",
        snap.files_blake3_hashed
    );
    assert_eq!(
        snap.files_parsed, 1,
        "exactly the edited file gets re-parsed, got {}",
        snap.files_parsed
    );
    assert_eq!(
        snap.layer_a_writes, 1,
        "new content → one Layer A write, got {}",
        snap.layer_a_writes
    );

    // Correctness: new symbol is visible.
    assert_eq!(ws.find_definitions("newly_added").len(), 1);
    assert_eq!(ws.find_definitions("shared").len(), 10);
}

// ── T2: New worktree-like location reuses Layer A ──────────────────────

#[test]
fn t2_second_worktree_shares_layer_a() {
    // Not a true git worktree (we don't exercise gix here), but
    // functionally identical for Layer A purposes: same content at a
    // different canonical path. Since no .git is present, the two
    // directories get distinct RepoIds (CanonicalPath source), but Layer
    // A is globally keyed by content hash and SHOULD still dedup.
    let _g = guard();
    let _cache = IsolatedCache::new();

    let wt1 = tempfile::tempdir().unwrap();
    let wt2 = tempfile::tempdir().unwrap();
    make_synthetic_repo(wt1.path(), 30);
    copy_tree(wt1.path(), wt2.path());

    let ws1 = Workspace::new();
    ws1.scan_directory(wt1.path(), None);

    let ws2 = Workspace::new();
    ws2.metrics().reset();
    ws2.scan_directory(wt2.path(), None);
    let snap = ws2.metrics().snapshot();

    // Layer A dedup: the second workspace writes no new FileUnits.
    assert_eq!(
        snap.layer_a_writes, 0,
        "second workspace with identical content: zero Layer A writes, got {}",
        snap.layer_a_writes
    );
    assert_eq!(
        snap.files_parsed, 0,
        "second workspace with identical content: zero parses"
    );

    // Correctness: queries identical.
    let b1 = query_fixture(&ws1, &["unique_5", "shared"]);
    let b2 = query_fixture(&ws2, &["unique_5", "shared"]);
    assert_eq!(b1, b2);
}

// ── T5: Child of indexed parent ────────────────────────────────────────

#[test]
fn t5_child_of_indexed_parent_zero_parses() {
    let _g = guard();
    let _cache = IsolatedCache::new();

    let repo = tempfile::tempdir().unwrap();
    let src = repo.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    make_synthetic_repo(&src, 15);

    let ws_parent = Workspace::new();
    ws_parent.scan_directory(repo.path(), None);
    drop(ws_parent);

    // Now open the child subdirectory.
    let ws_child = Workspace::new();
    ws_child.metrics().reset();
    ws_child.scan_directory(&src, None);
    let snap = ws_child.metrics().snapshot();

    // Child subsumes the parent's matching rows.
    assert!(
        snap.manifest_rows_copied >= 15,
        "expected subsumption of >=15 rows from parent, got {}",
        snap.manifest_rows_copied
    );
    assert_eq!(snap.files_parsed, 0, "no parsing needed on child open");

    // Correctness.
    assert_eq!(ws_child.find_definitions("shared").len(), 15);
}

// ── T13: Path rename, content unchanged ────────────────────────────────

#[test]
fn t13_rename_without_content_change_skips_parse() {
    let _g = guard();
    let _cache = IsolatedCache::new();

    let repo = tempfile::tempdir().unwrap();
    make_synthetic_repo(repo.path(), 8);

    let ws = Workspace::new();
    ws.scan_directory(repo.path(), None);

    let old = repo.path().join("f_3.rs");
    let new = repo.path().join("renamed.rs");
    std::fs::rename(&old, &new).unwrap();

    ws.metrics().reset();
    ws.scan_directory(repo.path(), None);
    let snap = ws.metrics().snapshot();

    // The renamed file still hits Layer A; no parses needed.
    assert_eq!(
        snap.files_parsed, 0,
        "rename alone must not parse, got {}",
        snap.files_parsed
    );
    assert_eq!(
        snap.layer_a_writes, 0,
        "rename alone must not write Layer A"
    );

    // Correctness: the symbol is still defined somewhere.
    assert_eq!(ws.find_definitions("unique_3").len(), 1);
}

// ── We intentionally don't pull in filetime as a dep ───────────────────
// Use a minimal inline fallback for mtime bumping that relies on the
// filesystem clock advancing naturally on most systems. If the OS clock
// is coarse, we explicitly set a future mtime via utimensat through libc.

#[cfg(unix)]
mod filetime {
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    pub fn set_file_mtime(
        path: &Path,
        t: std::time::SystemTime,
    ) -> std::io::Result<()> {
        use std::ffi::CString;
        let c = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has NUL")
        })?;
        let d = t.duration_since(std::time::UNIX_EPOCH).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "time before epoch")
        })?;
        let tv = libc::timespec {
            tv_sec: d.as_secs() as libc::time_t,
            tv_nsec: d.subsec_nanos() as _,
        };
        let times = [tv, tv];
        let ret = unsafe {
            libc::utimensat(libc::AT_FDCWD, c.as_ptr(), times.as_ptr(), 0)
        };
        if ret == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
}

/// Bump the mtime of a file far enough in the future to guarantee a
/// stat mismatch on every filesystem resolution.
fn bump_mtime(path: &Path) {
    let t = std::time::SystemTime::now() + std::time::Duration::from_secs(10);
    filetime::set_file_mtime(path, t).unwrap();
}

// ── T3: Branch switch (K of N files changed) ───────────────────────────

#[test]
fn t3_branch_switch_parses_only_changed_files() {
    let _g = guard();
    let _cache = IsolatedCache::new();

    let repo = tempfile::tempdir().unwrap();
    make_synthetic_repo(repo.path(), 50);

    let ws = Workspace::new();
    ws.scan_directory(repo.path(), None);

    // Simulate a branch switch: 7 of 50 files get new content.
    let changed = [3usize, 7, 11, 20, 33, 40, 49];
    for &i in &changed {
        let p = repo.path().join(format!("f_{i}.rs"));
        std::fs::write(
            &p,
            format!("fn unique_{i}() {{ shared(); }}\nfn shared() {{}}\nfn changed_marker_{i}() {{}}",
                    i=i),
        ).unwrap();
        bump_mtime(&p);
    }

    ws.metrics().reset();
    ws.scan_directory(repo.path(), None);
    let snap = ws.metrics().snapshot();

    // Property: exactly |changed| files re-parsed and written to Layer A.
    assert_eq!(
        snap.files_parsed, changed.len() as u64,
        "exactly {} files re-parsed, got {}",
        changed.len(),
        snap.files_parsed
    );
    assert_eq!(
        snap.layer_a_writes, changed.len() as u64,
        "exactly {} Layer A writes",
        changed.len()
    );
    assert_eq!(
        snap.files_blake3_hashed, changed.len() as u64,
        "only changed files need BLAKE3"
    );
    assert_eq!(snap.files_stat_called, 50);

    // Correctness: new symbols visible, missing-from-changed-files symbols still
    // present from their unchanged siblings.
    for &i in &changed {
        assert_eq!(
            ws.find_definitions(&format!("changed_marker_{i}")).len(),
            1,
            "marker for file {i} present after rescan"
        );
    }
    assert_eq!(ws.find_definitions("shared").len(), 50);
}

// ── T6: Cross-repo vendored duplicate ──────────────────────────────────

#[test]
fn t6_cross_repo_vendored_duplicate_dedups() {
    let _g = guard();
    let _cache = IsolatedCache::new();

    // Project A with a shared-subtree file. (We avoid the literal name
    // "vendor" because the scanner skips it by default; any unskipped
    // directory with shared content illustrates the same property.)
    let a = tempfile::tempdir().unwrap();
    std::fs::write(a.path().join("app.rs"), "fn app_main() {}").unwrap();
    std::fs::create_dir_all(a.path().join("third_party")).unwrap();
    std::fs::write(
        a.path().join("third_party/lib.rs"),
        "fn vendor_fn() { helper(); }\nfn helper() {}",
    )
    .unwrap();

    let ws_a = Workspace::new();
    ws_a.scan_directory(a.path(), None);

    // Project B with the same third-party file (bit-identical) but different app.
    let b = tempfile::tempdir().unwrap();
    std::fs::write(b.path().join("tool.rs"), "fn tool_main() {}").unwrap();
    std::fs::create_dir_all(b.path().join("third_party")).unwrap();
    std::fs::write(
        b.path().join("third_party/lib.rs"),
        "fn vendor_fn() { helper(); }\nfn helper() {}",
    )
    .unwrap();

    let ws_b = Workspace::new();
    ws_b.metrics().reset();
    ws_b.scan_directory(b.path(), None);
    let snap = ws_b.metrics().snapshot();

    // vendor/lib.rs was already in Layer A (written by project A). It should
    // hit Layer A on project B's scan → no parse. Only tool.rs is new.
    assert_eq!(
        snap.files_parsed, 1,
        "only the non-vendored file gets parsed; got {}",
        snap.files_parsed
    );
    assert_eq!(
        snap.layer_a_writes, 1,
        "only the novel file writes Layer A"
    );
    assert!(
        snap.layer_a_hits >= 1,
        "vendored file should hit Layer A"
    );

    // Correctness: project B sees its own app + the vendored symbols.
    assert_eq!(ws_b.find_definitions("tool_main").len(), 1);
    assert_eq!(ws_b.find_definitions("helper").len(), 1);
    assert_eq!(ws_b.find_definitions("app_main").len(), 0, "leakage check");
}

// ── T11: Parser version bump ───────────────────────────────────────────

#[test]
fn t11_parser_version_mismatch_invalidates_manifest_not_layer_a() {
    let _g = guard();
    let _cache = IsolatedCache::new();

    let repo = tempfile::tempdir().unwrap();
    make_synthetic_repo(repo.path(), 12);

    // Cold scan: parses all 12 and writes them to Layer A at the current
    // PARSER_VERSION.
    let ws = Workspace::new();
    ws.scan_directory(repo.path(), None);

    // Simulate a parser-version bump at the MANIFEST layer only: the
    // manifest's stored version no longer matches the binary's current
    // PARSER_VERSION. Layer A entries at the current version are still
    // present — this is the designed separation: Layer A is keyed by
    // (hash, parser_version) and never invalidated by manifest changes.
    ws.test_force_manifest_parser_version(999_999);

    ws.metrics().reset();
    ws.scan_directory(repo.path(), None);
    let snap = ws.metrics().snapshot();

    // Property A: manifest rows were invalidated → no stat fast-path hits
    //             → every file is re-hashed.
    assert_eq!(
        snap.files_blake3_hashed, 12,
        "manifest miss forces re-hash of every file, got {}",
        snap.files_blake3_hashed
    );
    // Property B: Layer A entries are still valid → no re-parsing, no writes.
    assert_eq!(
        snap.files_parsed, 0,
        "Layer A survives a manifest-only version bump; got {} parses",
        snap.files_parsed
    );
    assert_eq!(snap.layer_a_writes, 0);
    assert_eq!(snap.layer_a_hits, 12);

    // Correctness: queries still work after the bump.
    assert_eq!(ws.find_definitions("shared").len(), 12);
}

/// A full (Layer A + manifest) invalidation — as would happen after a
/// binary recompile that changes PARSER_VERSION and leaves the on-disk
/// Layer A files at the old `.pN.fu` suffix unreachable. We simulate this
/// by wiping the content directory and forcing the manifest to mismatch.
#[test]
fn parser_full_invalidation_triggers_reparse_and_rewrite() {
    let _g = guard();
    let cache = IsolatedCache::new();
    let cache_dir = cache.path().to_path_buf();

    let repo = tempfile::tempdir().unwrap();
    make_synthetic_repo(repo.path(), 8);

    let ws = Workspace::new();
    ws.scan_directory(repo.path(), None);

    // Wipe Layer A (simulates a parser_version bump that changes the
    // on-disk filename suffix) and bump the manifest version so the
    // stat fast-path can't short-circuit via a row lookup.
    let content_dir = cache_dir.join("content");
    std::fs::remove_dir_all(&content_dir).ok();
    ws.test_force_manifest_parser_version(999_999);

    ws.metrics().reset();
    ws.scan_directory(repo.path(), None);
    let snap = ws.metrics().snapshot();

    assert_eq!(snap.files_parsed, 8, "all files re-parsed");
    assert_eq!(snap.layer_a_writes, 8, "all files re-written");
    assert_eq!(snap.layer_a_hits, 0);
    // Correctness preserved.
    assert_eq!(ws.find_definitions("shared").len(), 8);
}

// ── T14: Non-git tree, repeated scan = idempotent ──────────────────────

#[test]
fn t14_rescan_same_tree_is_free() {
    let _g = guard();
    let _cache = IsolatedCache::new();

    let repo = tempfile::tempdir().unwrap();
    make_synthetic_repo(repo.path(), 20);

    let ws = Workspace::new();
    ws.scan_directory(repo.path(), None);

    // Second scan with no filesystem changes.
    ws.metrics().reset();
    ws.scan_directory(repo.path(), None);
    let snap = ws.metrics().snapshot();

    // Property: stat-only pass. No bytes read, no hashing, no parsing, no writes.
    assert_eq!(snap.files_stat_called, 20);
    assert_eq!(snap.files_bytes_read, 0, "stat-fresh: no reads");
    assert_eq!(snap.files_blake3_hashed, 0, "stat-fresh: no hashing");
    assert_eq!(snap.files_parsed, 0, "stat-fresh: no parsing");
    assert_eq!(snap.layer_a_writes, 0, "stat-fresh: no Layer A writes");
    assert_eq!(snap.layer_a_hits, 20, "every file: one Layer A get");

    // Correctness: results identical to first scan.
    assert_eq!(ws.find_definitions("shared").len(), 20);
}

// ── T15: Unsaved overlay visibility ────────────────────────────────────

#[test]
fn t15_editor_overlay_wins_within_workspace() {
    let _g = guard();
    let _cache = IsolatedCache::new();

    let repo = tempfile::tempdir().unwrap();
    let file = repo.path().join("edit.rs");
    std::fs::write(&file, "fn on_disk_symbol() {}").unwrap();

    let ws = Workspace::new();
    ws.scan_directory(repo.path(), None);
    assert_eq!(ws.find_definitions("on_disk_symbol").len(), 1);

    // Editor opens the file with modified content (didOpen-style overlay).
    ws.index_file(file.clone(), "fn overlay_symbol() {}".to_string());

    // Overlay symbol is visible.
    assert_eq!(ws.find_definitions("overlay_symbol").len(), 1);
    // Pre-edit symbol is gone from this Workspace's view.
    assert_eq!(ws.find_definitions("on_disk_symbol").len(), 0);

    // Close the overlay — on-disk state becomes authoritative again.
    ws.close_file(&file);
    ws.index_file(
        file.clone(),
        std::fs::read_to_string(&file).unwrap(),
    );
    // Overlay now matches disk again.
    assert_eq!(ws.find_definitions("on_disk_symbol").len(), 1);

    // A second Workspace pointing at the same tree sees only the persisted
    // (on-disk) version — overlays are per-process.
    let ws2 = Workspace::new();
    ws2.scan_directory(repo.path(), None);
    assert_eq!(ws2.find_definitions("on_disk_symbol").len(), 1);
    assert_eq!(
        ws2.find_definitions("overlay_symbol").len(),
        0,
        "overlays must not leak across Workspaces"
    );
}

// ── Content dedup within a single tree ─────────────────────────────────

#[test]
fn content_dedup_within_tree_one_write_per_unique_hash() {
    let _g = guard();
    let _cache = IsolatedCache::new();

    let repo = tempfile::tempdir().unwrap();
    // Five files, all with byte-identical content.
    let body = "fn twin_fn() {}\nfn other_twin() {}";
    for i in 0..5 {
        std::fs::write(repo.path().join(format!("dup_{i}.rs")), body).unwrap();
    }
    // Two files with a different but shared body.
    let body2 = "fn third_fn() {}";
    for i in 0..2 {
        std::fs::write(repo.path().join(format!("alt_{i}.rs")), body2).unwrap();
    }

    let ws = Workspace::new();
    ws.scan_directory(repo.path(), None);
    let snap = ws.metrics().snapshot();

    // There are 7 distinct paths but only 2 distinct content hashes.
    // Layer A write is keyed by hash; after the first write, subsequent
    // identical-hash puts are idempotent no-ops.
    assert_eq!(
        snap.layer_a_writes, 2,
        "one Layer A write per unique content (2 distinct bodies); got {}",
        snap.layer_a_writes
    );
    assert_eq!(
        snap.files_parsed, 2,
        "parse once per unique hash"
    );
    // Stat was called once per path.
    assert_eq!(snap.files_stat_called, 7);

    // Correctness: each dup/alt file contributes a definition entry.
    assert_eq!(ws.find_definitions("twin_fn").len(), 5);
    assert_eq!(ws.find_definitions("third_fn").len(), 2);
}

// ── File removal propagates to manifest + in-memory state ──────────────

#[test]
fn file_removal_detected_on_rescan() {
    let _g = guard();
    let _cache = IsolatedCache::new();

    let repo = tempfile::tempdir().unwrap();
    make_synthetic_repo(repo.path(), 6);

    let ws = Workspace::new();
    ws.scan_directory(repo.path(), None);
    assert_eq!(ws.find_definitions("unique_3").len(), 1);

    std::fs::remove_file(repo.path().join("f_3.rs")).unwrap();

    ws.metrics().reset();
    ws.scan_directory(repo.path(), None);
    let snap = ws.metrics().snapshot();

    // 5 remaining files → stat calls.
    assert_eq!(snap.files_stat_called, 5);
    assert_eq!(snap.files_parsed, 0, "no new parsing needed");

    // In-memory state reflects the removal.
    assert_eq!(
        ws.find_definitions("unique_3").len(),
        0,
        "removed file's symbol cleared from definitions"
    );
    assert_eq!(
        ws.find_definitions("shared").len(),
        5,
        "shared count drops from 6 to 5"
    );
}

// ── Subsumption conflict resolution: higher generation wins ────────────

#[test]
fn subsumption_newer_generation_supersedes_older() {
    let _g = guard();
    let _cache = IsolatedCache::new();

    let repo = tempfile::tempdir().unwrap();
    let src = repo.path().join("src");
    let inner = src.join("inner");
    std::fs::create_dir_all(&inner).unwrap();

    // src/inner/x.rs — indexed at two different nesting levels, each with
    // its own manifest (so they produce overlapping subsumable rows).
    std::fs::write(inner.join("x.rs"), "fn fx() {}").unwrap();

    let ws_inner = Workspace::new();
    ws_inner.scan_directory(&inner, None);
    drop(ws_inner);

    let ws_src = Workspace::new();
    ws_src.scan_directory(&src, None);
    drop(ws_src);

    // Now open the outermost root. Both prior manifests are candidates,
    // and their rel_paths for the one shared file collide after re-prefixing:
    // - from ws_inner:  inner rel "x.rs" → "src/inner/x.rs"
    // - from ws_src:    rel "inner/x.rs" → "src/inner/x.rs"
    let ws_outer = Workspace::new();
    ws_outer.metrics().reset();
    ws_outer.scan_directory(repo.path(), None);
    let snap = ws_outer.metrics().snapshot();

    // Regardless of which generation wins, the subsumption must pick exactly
    // one row for x.rs → no duplicate parses, no duplicate Layer A writes.
    assert_eq!(
        snap.files_parsed, 0,
        "conflict resolution should not reparse; got {}",
        snap.files_parsed
    );
    assert_eq!(ws_outer.find_definitions("fx").len(), 1);
}

// ── Cross-language dedup by content ────────────────────────────────────

#[test]
fn cross_language_tree_works_with_cache() {
    let _g = guard();
    let _cache = IsolatedCache::new();

    let repo = tempfile::tempdir().unwrap();
    std::fs::write(repo.path().join("lib.rs"), "fn rs_fn() {}").unwrap();
    std::fs::write(repo.path().join("app.py"), "def py_fn():\n    pass\n").unwrap();
    std::fs::write(repo.path().join("main.go"), "func go_fn() {}").unwrap();

    let ws = Workspace::new();
    ws.scan_directory(repo.path(), None);
    let snap = ws.metrics().snapshot();

    assert_eq!(snap.files_parsed, 3);
    assert_eq!(snap.layer_a_writes, 3);

    assert_eq!(ws.find_definitions("rs_fn").len(), 1);
    assert_eq!(ws.find_definitions("py_fn").len(), 1);
    assert_eq!(ws.find_definitions("go_fn").len(), 1);

    // Rescan: stat-fresh everywhere.
    ws.metrics().reset();
    ws.scan_directory(repo.path(), None);
    let snap2 = ws.metrics().snapshot();
    assert_eq!(snap2.files_parsed, 0);
    assert_eq!(snap2.layer_a_writes, 0);
}

// ── Metric-ratio performance assertions ───────────────────────────────

/// These aren't correctness tests — they're *performance guards* expressed
/// via counter ratios. They fail loudly if an optimization regresses.
mod perf_ratios {
    use super::*;

    /// On a fresh clone of a known repo, Layer A hit rate must be 100%.
    #[test]
    fn clone_reuse_hit_rate_is_100() {
        let _g = guard();
        let _cache = IsolatedCache::new();

        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        make_synthetic_repo(a.path(), 50);
        copy_tree(a.path(), b.path());

        let ws_a = Workspace::new();
        ws_a.scan_directory(a.path(), None);

        let ws_b = Workspace::new();
        ws_b.metrics().reset();
        ws_b.scan_directory(b.path(), None);
        let s = ws_b.metrics().snapshot();

        let total = s.layer_a_hits + s.layer_a_writes;
        assert!(total >= 50);
        let hit_rate = (s.layer_a_hits as f64) / (total as f64);
        assert!(
            hit_rate >= 1.0 - 1e-9,
            "clone reuse hit rate must be 100% (got {hit_rate:.4}, hits={} writes={})",
            s.layer_a_hits,
            s.layer_a_writes
        );
    }

    /// On a full stat-fresh rescan, zero bytes are read from disk for content.
    #[test]
    fn stat_fresh_rescan_reads_zero_bytes() {
        let _g = guard();
        let _cache = IsolatedCache::new();

        let repo = tempfile::tempdir().unwrap();
        make_synthetic_repo(repo.path(), 30);

        let ws = Workspace::new();
        ws.scan_directory(repo.path(), None);
        let warm_bytes = ws.metrics().snapshot().files_bytes_read;
        assert!(warm_bytes > 0, "cold scan must read something");

        ws.metrics().reset();
        ws.scan_directory(repo.path(), None);
        let s = ws.metrics().snapshot();

        assert_eq!(s.files_bytes_read, 0);
        assert_eq!(s.files_blake3_hashed, 0);
        assert_eq!(s.files_parsed, 0);
    }

    /// When K of N files change, reparse ratio is exactly K/N.
    #[test]
    fn reparse_ratio_equals_change_ratio() {
        let _g = guard();
        let _cache = IsolatedCache::new();

        let repo = tempfile::tempdir().unwrap();
        let n = 40usize;
        make_synthetic_repo(repo.path(), n);

        let ws = Workspace::new();
        ws.scan_directory(repo.path(), None);

        let changed = [1usize, 2, 5, 11, 29, 37];
        for &i in &changed {
            let p = repo.path().join(format!("f_{i}.rs"));
            std::fs::write(
                &p,
                format!("fn unique_{i}() {{}}\nfn shared() {{}}\nfn diff_{i}() {{}}", i = i),
            )
            .unwrap();
            bump_mtime(&p);
        }

        ws.metrics().reset();
        ws.scan_directory(repo.path(), None);
        let s = ws.metrics().snapshot();

        let reparse_ratio = (s.files_parsed as f64) / (s.files_stat_called as f64);
        let expected = (changed.len() as f64) / (n as f64);
        assert!(
            (reparse_ratio - expected).abs() < 1e-9,
            "reparse ratio should be exactly {expected}, got {reparse_ratio}"
        );
    }

    /// Subsumption coverage: when opening a parent of an indexed subtree,
    /// manifest_rows_copied / files_stat_called ≥ (rows in subtree / total).
    #[test]
    fn subsumption_coverage_matches_subtree_size() {
        let _g = guard();
        let _cache = IsolatedCache::new();

        let repo = tempfile::tempdir().unwrap();
        let src = repo.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        make_synthetic_repo(&src, 30);
        // 5 extra files outside the indexed subtree.
        std::fs::create_dir_all(repo.path().join("bin")).unwrap();
        for i in 0..5 {
            std::fs::write(
                repo.path().join("bin").join(format!("b_{i}.rs")),
                format!("fn bin_{i}() {{}}", i = i),
            )
            .unwrap();
        }

        let ws_sub = Workspace::new();
        ws_sub.scan_directory(&src, None);
        drop(ws_sub);

        let ws_parent = Workspace::new();
        ws_parent.metrics().reset();
        ws_parent.scan_directory(repo.path(), None);
        let s = ws_parent.metrics().snapshot();

        // At least 30 rows are subsumed from the src/ manifest.
        assert!(
            s.manifest_rows_copied >= 30,
            "expected >=30 subsumed rows, got {}",
            s.manifest_rows_copied
        );
        // Only the 5 sibling files should ever be parsed.
        assert!(
            s.files_parsed <= 5,
            "expected parses for 5 new files, got {}",
            s.files_parsed
        );
    }

    /// Dedup savings: N files with identical content → only 1 parse + 1 write.
    #[test]
    fn dedup_of_identical_files_saves_parses() {
        let _g = guard();
        let _cache = IsolatedCache::new();

        let repo = tempfile::tempdir().unwrap();
        let body = "fn ident() {}";
        for i in 0..25 {
            std::fs::write(repo.path().join(format!("f_{i}.rs")), body).unwrap();
        }

        let ws = Workspace::new();
        ws.scan_directory(repo.path(), None);
        let s = ws.metrics().snapshot();

        assert_eq!(s.layer_a_writes, 1, "one write, got {}", s.layer_a_writes);
        assert_eq!(s.files_parsed, 1, "one parse");
        // All 25 files stat'd.
        assert_eq!(s.files_stat_called, 25);
        // Layer A was hit 24 times (the 24 duplicates).
        assert!(s.layer_a_hits >= 24);
    }
}

