# Quicklsp Cache v3 — Git-Worktree & Clone-Aware Index

**Status:** Design proposal
**Scope:** Replace the path-keyed, mtime-validated cache with a content-addressable,
repo-identity-aware cache that reuses file-level index data across worktrees, clones,
and branches.

---

## 1. Objective

Make the index cache **reusable across git worktrees, clones, and branches**
belonging to the same user — so that identical source is never re-parsed and never
stored twice, regardless of where it lives on disk.

Today the cache is keyed by canonical project path and validated by mtime
(`src/word_index/persistence.rs`), so:

- A clone of the same repo to a new path is indexed from scratch.
- Two worktrees of the same repo index independently.
- A branch switch invalidates files that are byte-identical to a prior state.

## 2. The Shift

Pivot the cache's axis of identity from **"where the file lives"** to **"what the
file contains"**, layered under a **"what repository is this"** namespace.

## 3. Three Architectural Decisions

### 3.1 Two-layer storage: shared content store + per-worktree manifest

- **Layer A — Content Store (global, per user).** Content-addressable store keyed
  by `BLAKE3(file_bytes)`. Each entry holds the parse result for that file:
  symbols, word hashes, language family, parser version. Entries are immutable and
  write-once. Identical content anywhere on the user's machine maps to one entry.
- **Layer B — Manifest (per worktree).** Small local database mapping
  `relative_path → content_hash` plus stat/git metadata. This is the only thing
  that changes when files change, branches switch, or worktrees diverge.

Expensive data (ASTs, symbols, word hashes) is shared. Cheap data (path
bookkeeping) is local. Branch switches and fresh clones become **manifest
rewrites over already-known content**, not full rescans.

### 3.2 Repo identity from git, with graceful fallback

Resolve "this is the same repository" across disk locations via tiered detection:

1. Parse `.git` (file or directory) and resolve the common git-dir — naturally
   handles linked worktrees and submodules.
2. Use the repo's **root commit** as the primary identity; fall back to
   **normalized remote URLs**; fall back to canonical path for non-git trees.

Two worktrees of the same repo resolve to the **same repo identity** (sharing
Layer A perfectly) but **different worktree keys** (separate manifests). A
`.quicklsp.toml` override is the escape hatch for unusual topologies.

### 3.3 MVCC concurrency: lock-free reads, partitioned writes

Many LSP instances read concurrently; writes are local and idempotent:

- **Layer A** → append-only segment files + **LMDB** index. Readers use lock-free
  memory-mapped access; writers append and commit through LMDB's single-writer
  MVCC. Duplicate writes are safe because content hashing makes them idempotent.
- **Layer B** → **SQLite in WAL mode**, one database per worktree. Each LSP owns
  its own manifest; no cross-process writer contention.
- **Cross-process coordination** → handled by the storage engines plus a
  lightweight advisory lock preventing two LSPs from duplicating an initial scan
  of the same worktree.

No bespoke WAL, no central lock server, no in-process global mutex on the hot path.

## 4. On-Disk Layout

```
$XDG_CACHE_HOME/quicklsp/
├── registry.lmdb/                 # RepoIdentity ──► repo_id
├── content/
│   ├── index.lmdb/                # content_hash ──► (segment_id, off, len, fmt)
│   └── segments/
│       └── seg-NNNNN.log
└── repos/
    └── <repo_id>/
        ├── repo.json              # root_commit, remotes, last_seen
        └── worktrees/
            └── <worktree_key>/
                ├── manifest.sqlite  (WAL mode)
                ├── manifest.sqlite-wal
                └── manifest.sqlite-shm
```

- **`content/`** is truly global per user (mode 0700, never shared between users).
- **`repos/<repo_id>/`** is a scoping convenience for GC and quotas.

### 4.1 Data shapes

**Layer A segment record:**

```
magic[4] | hash[32] | fmt[2] | parser_v[2] | len[4] | bincode(FileUnit) | crc32[4]
```

```rust
struct FileUnit {
    lang: LangFamily,
    symbols: Vec<Symbol>,        // reuses existing Symbol struct
    word_hashes: Vec<u32>,       // unique per-file FNV-1a hashes
}
```

**Layer B (SQLite WAL):**

```sql
CREATE TABLE manifest (
    rel_path     TEXT PRIMARY KEY,
    content_hash BLOB NOT NULL,
    lang         INTEGER NOT NULL,
    size         INTEGER NOT NULL,
    mtime_ns     INTEGER NOT NULL,
    git_oid      BLOB,
    generation   INTEGER NOT NULL
);
CREATE INDEX manifest_hash ON manifest(content_hash);

CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT);
-- keys: current_branch, head_oid, generation, parser_version, built_at
```

## 5. Key Algorithms

### 5.1 Cold startup

```
repo_id, worktree_key := detect_identity(root)
open manifest (SQLite WAL), open content index (LMDB)
if manifest matches current parser_version and stat-fresh: warm start

# Manifest subsumption (see §5.2)
else:
    candidates := registry.query(repo_id = this_repo)
                          .filter(|wt| is_ancestor_or_descendant(root, wt.working_dir))
    for each candidate, generation-descending:
        copy+re-prefix rows into new manifest (INSERT ... SELECT)
        mark those rel_paths as "provisionally fresh"

    par_for each file (rayon), skipping provisionally-fresh paths whose stat matches:
        stat → (size, mtime)
        if clean+tracked in git: hash := git_oid_to_blake3(oid)
        else:                    hash := blake3(read_file)
        if hash not in content_index:
            parse → FileUnit → append to segment → insert into index
        emit (rel_path, hash, lang, size, mtime, git_oid)
    bulk-INSERT into manifest in one txn
build in-memory posting list from manifest + Layer A
```

### 5.2 Manifest subsumption (parent/child directory reuse)

When a user opens a root whose canonical path is an ancestor or descendant of a
previously-indexed worktree under the **same `repo_id`**, we reuse that
manifest instead of scanning from scratch.

**Registry query.** Before scanning, ask the registry for all existing
`worktree_key`s with matching `repo_id` and filter by canonical-path prefix
relationship.

**Row copy with prefix rewrite.** For each matching child/parent manifest:

- **Opening a parent** (`/repo/` after `/repo/src/` was indexed): copy all rows
  from the child manifest, prepending the relative subpath (`src/`) to each
  `rel_path`. Then scan only the delta — siblings of the child subtree.
- **Opening a child** (`/repo/src/` after `/repo/` was indexed): copy rows
  whose `rel_path` starts with `src/`, stripping the prefix. No filesystem
  scan of content needed beyond the stat-freshness pass.

**Conflict resolution.** Overlapping subtrees (e.g. `/repo/src/` and
`/repo/src/parser/`): prefer the manifest with the highest `generation`; break
ties by `built_at`.

**Staleness handling.** Copied rows are "provisionally fresh"; the normal stat
fast-path (§5.4) validates each. Mismatches fall back to re-hash, which still
hits Layer A in the common case — no parsing required.

**Properties.**
- Zero AST work: all `FileUnit`s already live in Layer A (global per user).
- Zero novel hashing for subsumed paths; at most one stat per file.
- Works for non-git trees too, as long as `repo_id` agrees (e.g. same
  canonical-path fingerprint root).

### 5.3 Branch switch

Driven by `didChangeWatchedFiles` or HEAD-change detection, scoped to
`git diff --name-only old_head new_head`. For each changed path: look up new
`content_hash`; O(1) resolution via Layer A in the common case; only novel
content triggers parsing.

### 5.4 Freshness fast-path

1. `(size, mtime)` match against manifest row → HIT, no hashing.
2. Else, `git_oid` match against manifest row → HIT, touch mtime.
3. Else, look up `git_oid_to_blake3` side table → HIT, update manifest row.
4. Else, read bytes + BLAKE3 + possibly parse.

### 5.5 Unsaved editor buffers

- In-memory overlays in `Workspace`, scoped to the owning LSP process.
- Overlay wins for that path over any on-disk `FileUnit`.
- Never persisted. On `didSave`: hash bytes, promote into Layer A if new,
  update manifest. On `didClose`: drop overlay.

## 6. Key Decisions & Trade-offs

| Decision | Choice | Main alternative | Why |
| --- | --- | --- | --- |
| Cache atom | Per-file CAS (`ContentHash → FileUnit`) | Path+mtime (today) | Cross-branch/worktree/clone reuse. |
| Hash function | BLAKE3 + opportunistic git-OID fast path | SHA-1 / xxh3 | Fast, collision-resistant, non-git friendly. |
| Repo identity | `root_commit` ▶ normalized remotes ▶ canonical path | Canonical path only | Handles clones across paths; graceful degradation. |
| Worktree identity | `BLAKE3(repo_id ‖ gitdir ‖ working_dir)` | Path alone | Distinguishes worktrees of same repo without re-indexing content. |
| Layer A engine | Append-only segments + LMDB index | LMDB-only / SQLite / RocksDB | Lock-free mmap reads, idempotent appends, minimal migration. |
| Layer B engine | SQLite WAL, one DB per worktree | Shared SQLite / custom WAL | Many-reader/one-writer story; indexed path lookups; isolation. |
| Cross-LSP concurrency | MVCC reads + per-writer locks | Global mutex / bespoke multi-WAL | Scales to N windows without a coordinator. |
| Cache scope | Global per user | Per-repo | Maximal dedup (e.g. vendored deps); one GC policy. |
| Parent/child directory reuse | Manifest subsumption via registry + prefix rewrite | Rescan every new root | Opening a parent/child of an indexed subtree costs only a stat pass. |
| Branch-switch acceleration | Git-diff driven incremental rescan | Snapshots / Merkle manifests | Simplest; uses git's own diff machinery. |
| GC | Mark-and-sweep over union of manifests | Refcount on write | Race-free under idempotent appends. |

## 7. Migration

- `CURRENT_VERSION` bumps `2 → 3` in `src/word_index/persistence.rs`.
- First start with v3 binary + existing v2 cache: migrator walks the old
  `LogIndex`, hashes file contents, writes `FileUnit`s into Layer A (skipping
  duplicates), builds a v3 manifest. Old cache renamed to `.v2-legacy`.

## 8. Net Effect

- Opening a **new clone** of a known repo: no parsing, manifest build only.
- Adding a **new git worktree**: no parsing, manifest build only.
- Opening a **parent or child** of an already-indexed directory: no parsing,
  manifest rows copied with a prefix rewrite.
- **Switching branches:** parse only files whose content actually changed.
- **Disk usage:** one copy of each unique file's index data per user.
- **Multiple editor windows** on multiple worktrees: concurrent, no lock contention.

The design preserves the existing engine shape (`Workspace`, `Symbol`, posting
lists, rayon-parallel scan) and migrates the cache layer underneath — no rewrite
of the LSP surface, just a change in what the cache is keyed by and where it lives.

## 9. Integration Tests — Objective Validation

These tests validate that the **end-to-end objectives** of the design are met.
They are behavior-focused (properties over implementation details) and live in
`tests/cache_v3/`. Each test asserts both a **performance property** (no
spurious work) and a **correctness property** (results match a baseline).

### 9.1 Instrumentation hooks (test-only)

To measure "no massive scanning", the engine exposes counters under a
`#[cfg(any(test, feature = "metrics"))]` gate:

```rust
struct ScanMetrics {
    files_stat_called:     AtomicU64,
    files_bytes_read:      AtomicU64,
    files_blake3_hashed:   AtomicU64,
    files_parsed:          AtomicU64,   // full tokenize/symbol-extract pass
    layer_a_writes:        AtomicU64,
    layer_a_hits:          AtomicU64,
    manifest_rows_copied:  AtomicU64,   // subsumption
}
```

Tests read these before/after operations and assert deltas.

### 9.2 Correctness baseline

Every behavioral test pairs its property assertion with a **result-equivalence
check** against a freshly-built workspace on the same source, using a
`query_fixture()` helper that exercises:

- `workspace/symbol` (global fuzzy search) for N representative names.
- `textDocument/definition` for N (file, line, col) pairs.
- `textDocument/references` for N symbols.
- `textDocument/documentSymbol` per file.

The fixture's outputs are normalized (paths → rel_paths, stable sort) and
compared by equality. **Any reuse optimization that changes observable results
fails the test.** This is the correctness backstop for every performance claim
below.

### 9.3 Test matrix

| # | Scenario | Performance property | Correctness property |
| - | -------- | -------------------- | -------------------- |
| T1 | Second clone of a known repo at a new path | `files_parsed == 0`; `layer_a_writes == 0`; `files_blake3_hashed ≤ files_stat_called` | Query results identical to first clone |
| T2 | `git worktree add` of known repo | `files_parsed == 0`; `layer_a_writes == 0` | Identical queries on both worktrees |
| T3 | Branch switch within one worktree, N files changed | `files_parsed ≤ N`; `layer_a_writes ≤ N` | Results reflect the new branch |
| T4 | Open parent of indexed subtree | `manifest_rows_copied == rows(child)`; `files_parsed == 0` for subtree | Queries against subtree return same results as before; new siblings indexed |
| T5 | Open child of indexed parent | `manifest_rows_copied == rows_matching_prefix`; `files_parsed == 0`; `files_blake3_hashed == 0` | Queries restricted to child match parent's results on those paths |
| T6 | Cross-repo vendored duplicate | Second project's scan: `files_parsed == 0` for duplicated files | `definition`/`references` resolve locally within each project (no cross-project leakage) |
| T7 | Symbolic link / bind mount / fresh path to same canonical tree | `files_parsed == 0`; Layer B reuses an existing manifest via canonical-path collapse | Identical queries |
| T8 | Concurrent LSP instances on two worktrees of same repo | No corruption; both processes complete; `layer_a_writes` is the set-union, not the multiset | Identical queries per worktree |
| T9 | Two LSP instances on the **same** worktree | Only one full scan observed (advisory scan-lock honored); second waits and short-circuits | Identical query results from both |
| T10 | Force kill during scan | Next start completes; no duplicate Layer A entries; manifest either matches last generation or rebuilds cleanly | Results match fresh baseline |
| T11 | Parser version bump | All paths re-parse once; Layer A grows with new `parser_v` keys; old entries untouched | Results reflect new parser |
| T12 | Content change only (whitespace edit → re-save) | `layer_a_writes == 1` (new hash); manifest updates 1 row | Results reflect edit |
| T13 | Path rename, content unchanged | `files_parsed == 0`; `layer_a_writes == 0`; manifest updates 1 row | Results resolve under new path |
| T14 | Non-git project opened in two locations | Each location scans independently (no identity match); Layer A still dedups identical files | Identical queries both locations |
| T15 | Unsaved buffer visibility | Overlay wins within the owning process; other LSP process sees on-disk state | Queries reflect overlay for owner; unaffected elsewhere |

### 9.4 Representative test sketches

**T1 — Clone reuse (the headline test)**

```
// Setup
index(repo_a_at_path_1)              // cold, populates Layer A
baseline := query_fixture(repo_a_at_path_1)
reset_metrics()

// Act
index(repo_a_at_path_2)              // second clone, different path

// Assert properties
assert files_parsed == 0
assert layer_a_writes == 0
assert manifest_rows_copied == 0      // no subsumption, just Layer A hits
assert query_fixture(repo_a_at_path_2) == baseline
```

**T4 — Parent subsumption**

```
index(/repo/src)                      // cold
sub_baseline := query_fixture(/repo/src)
reset_metrics()

index(/repo)

assert manifest_rows_copied >= file_count(/repo/src)
assert files_parsed <= file_count(/repo) - file_count(/repo/src)
// Queries over the subtree must still match:
assert query_fixture(/repo/src, restrict=True) == sub_baseline
// And must see the new siblings:
assert query_fixture(/repo).contains_symbols_from(/repo/tests)
```

**T8 — Concurrent worktrees**

```
spawn LSP_1 on worktree_1
spawn LSP_2 on worktree_2
in parallel: each does full index + full query_fixture

assert both complete within timeout
assert no "database is locked" / "segment corrupt" errors in logs
assert union of parsed files across both == unique_content_count
assert query results on each worktree match single-process baselines
```

**T10 — Crash safety**

```
spawn scanner, SIGKILL halfway through
restart, run to completion
assert layer_a has no duplicate hashes
assert query_fixture() == fresh_baseline_query_fixture()
```

### 9.5 Fixtures

- **Synthetic repo generator** — deterministic tree of N files across supported
  languages, with configurable size/duplication to cover Layer A hit rates.
- **Real-repo smoke test** — pin a small public repo (e.g. a tagged
  `ripgrep`/`serde` snapshot fetched into `tests/fixtures/` once) and run T1,
  T2, T3, T4 against it. Guarded behind a feature flag to keep CI fast.
- **Harness** — a `TestHarness` that:
  - redirects `XDG_CACHE_HOME` to a temp dir (isolated per test),
  - spawns real `Workspace` instances (not mocks),
  - can `git init` / `git clone` / `git worktree add` in tempdirs,
  - drives the LSP via its real tower-lsp surface so tests exercise the same
    path production code does.

### 9.6 What we explicitly do **not** test here

- Internal storage engine specifics (segment layout, LMDB txn sizes). Those are
  covered by unit tests in the relevant modules.
- Tokenizer correctness on individual files. Already covered by existing
  `tests/` for the parser.
- Absolute wall-clock numbers. Tests assert **ratios and counts**
  (`files_parsed == 0`), not timings, so they're stable across CI hardware.

### 9.7 CI integration

- T1–T7, T11–T15 run on every PR (fast, < ~30s total on synthetic fixtures).
- T8–T10 (concurrency, crash) run on a nightly workflow with higher timeouts.
- Metrics regressions (e.g. `files_parsed` creeping above expected bounds in
  a property test) fail loudly rather than silently degrading.

## 10. Open Questions

- Should `git_oid → blake3` be precomputed at worktree open, or populated lazily?
  Recommend lazy first, precompute later based on telemetry.
- Quota/eviction policy and default `QUICKLSP_CACHE_MAX_GB` value.
- Whether to expose `.quicklsp.toml` overrides in v3 or defer.
