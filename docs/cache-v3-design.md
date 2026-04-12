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

## 9. Open Questions

- Should `git_oid → blake3` be precomputed at worktree open, or populated lazily?
  Recommend lazy first, precompute later based on telemetry.
- Quota/eviction policy and default `QUICKLSP_CACHE_MAX_GB` value.
- Whether to expose `.quicklsp.toml` overrides in v3 or defer.
