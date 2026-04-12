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
    _tmp: tempfile::TempDir,
}

impl IsolatedCache {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("QUICKLSP_CACHE_DIR", tmp.path());
        Self { _tmp: tmp }
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

