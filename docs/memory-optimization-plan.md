# Memory Optimization Plan: Reducing Peak RSS

## Problem

When indexing the full Linux kernel (64,826 C/H files), quicklsp's peak RSS
reaches 2.77 GB (observed via `/proc/self/statm` at the "Word index builder
ready" log line). After the word index is built and `malloc_trim` runs, RSS
drops to 1.67 GB. The goal is to reduce the peak so quicklsp can index large
codebases on machines with less RAM.

## How we measure

### Massif (Valgrind heap profiler)

We use Valgrind's Massif tool to get an empirical, allocation-level breakdown
of heap usage at the exact moment of peak. Massif snapshots heap state
periodically and records a detailed call-tree for the peak snapshot.

Command used:

```
rm -rf ~/.cache/quicklsp/*
valgrind --tool=massif --depth=20 --detailed-freq=5 --max-snapshots=100 \
    target/release/quicklsp-bench --phase index /tmp/linux/drivers/net
```

We profiled on `drivers/net` (6,136 files, 7.2M occurrences) because valgrind
adds ~25x overhead and the full kernel would take too long. The allocation
patterns are representative since all C files go through the same pipeline.

### /proc/self/statm (RSS monitoring)

The benchmark binary and `scan_directory` already log RSS at key points:
- "memory [start]"
- "Word index builder ready: N entries, rss=..."
- "Word index written, rss=..."
- "memory [after index]"

This gives us the full-run RSS profile. On the full kernel:
- Start: 3 MB
- Builder ready (peak): 2,718 MB
- After word index written: 2,771 MB
- After malloc_trim (final): 1,660 MB

### memory_breakdown()

The benchmark prints a measured breakdown of live data structures after
indexing completes. For the full kernel this showed 705 MB of measured
heap across symbols (294 MB), word index directory (205 MB), fuzzy index
(115 MB), definition index (78 MB), and file path keys (4 MB).

## Massif results: drivers/net peak snapshot (before optimization)

**Peak heap: 550 MB** (snapshot 58 of 60, captured during `WordIndexBuilder::build()`).

Top allocation sites at the moment of peak:

| Allocation site | Bytes | % of peak |
|---|---|---|
| `WordIndexBuilder::build` — pushing entries into `file_buckets` (format.rs:250) | 209,179,840 | 38.0% |
| `WordIndexBuilder::drain_file_occurrences` — `self.entries.reserve` (format.rs:217) | 171,458,560 | 31.2% |
| `Symbol` vec push in `extract_definition` (c.rs:54) | 26,535,552 | 4.8% |
| `WordIndexBuilder::intern_word` — word_table + word_lookup (format.rs:169) | 25,165,824 | 4.6% |
| `DeletionIndex::insert` — fuzzy trigram index (deletion_neighborhood.rs:34,42) | 22,749,376 | 4.1% |
| DashMap `definitions` — rehash (lib.rs:1192) | 12,845,312 | 2.3% |
| Everything else (below 2%) | ~81 MB | ~15% |
| **Total** | **550,164,096** | **100%** |

## Step 1: In-place sort (DONE)

### What changed

Replaced the `entries → file_buckets` copy in `WordIndexBuilder::build()`
with an in-place sort by `(path_id, word_id, line)`. Contiguous slices of the
sorted array serve as the old per-file buckets. The `file_buckets`
`Vec<Vec<CompactEntry>>` allocation is completely eliminated.

`write_files_bin` and posting list construction were refactored to iterate
slices of the sorted array (via a `build_file_slices` helper) instead of
separate Vecs.

### Empirical results

Measured on a synthetic 6,030-file C corpus (1,736,904 entries), 3 runs each,
via `/proc/self/statm` RSS probes inside `build()`.

| Measurement point | Before (avg) | After (avg) | Change |
|---|---|---|---|
| RSS before sort/group | 75 MB | 75 MB | 0 |
| RSS peak during build() | 118 MB | 75 MB | **-43 MB (-36%)** |
| RSS at "word index written" | 95 MB | 59 MB | **-36 MB (-38%)** |
| RSS final (after index) | 41.7 MB | 40.5 MB | -1.2 MB |
| build() total time | 121.7 ms | 174.7 ms | +53 ms (+44%) |
| End-to-end scan time | 2.43 s | 2.45 s | +20 ms (~0%) |

The in-place sort adds zero extra memory — RSS is flat before and after sort.
End-to-end time is unchanged because build() is ~5% of total scan time.

## Step 2: Stream entries to disk during accumulation

### Goal

Eliminate the `self.entries` Vec from peak entirely. This is the single
largest allocation at peak — Massif measured it at 171 MB on drivers/net
(31% of peak), and on the full kernel it dominates at well over 1 GB.

Currently, all CompactEntry values are accumulated in a single in-memory Vec
during the parallel tokenization phase, then sorted in-place during build().
The entire Vec must fit in RAM.

### Approach

During `drain_file_occurrences`, instead of pushing to an in-memory Vec,
append each CompactEntry (20 bytes) to a temporary file on disk. Each
parallel chunk gets its own temp file (one per rayon chunk) to avoid lock
contention.

At build time, perform an external merge sort:

1. Sort each chunk file individually (read into memory one chunk at a time,
   sort, write back). Each chunk is bounded by `CHUNK_SIZE` (currently 100
   files), so its memory is bounded.
2. K-way merge the sorted chunk files into the final sorted order
   `(path_id, word_id, line)`, streaming directly into `write_files_bin`
   and posting list construction.

The peak memory for the entries becomes: **one chunk worth of entries**
(bounded by CHUNK_SIZE files), instead of the entire corpus.

### What stays in memory

- `word_table` + `word_lookup`: interned words (Massif: 25 MB on drivers/net)
- `path_table` + `path_lookup`: interned paths (small)
- Symbols in `DashMap<PathBuf, FileEntry>` (Massif: 27 MB on drivers/net)
- Fuzzy index (Massif: 23 MB on drivers/net)
- Definitions DashMap (Massif: 13 MB on drivers/net)
- One chunk of entries during sort phase
- K-way merge heap + I/O buffers

### What moves to disk

- The entire `self.entries` Vec (Massif: 171 MB on drivers/net, >>1 GB on
  full kernel). This is the dominant allocation.

### Tradeoffs

- **I/O overhead**: Writing and re-reading entries adds disk I/O. On SSD this
  should be fast — the data is sequential and fits in OS page cache for
  moderate codebases. On HDD or very large corpora it will be slower.
- **Complexity**: External merge sort is well-understood but adds temp file
  management, error handling for partial writes, and cleanup on failure.
- **Temp disk space**: Requires ~20 bytes × total_entries of temp space.
  For the full kernel (~47M entries) this is ~940 MB on disk.

### Expected impact

This would reduce peak RSS by roughly the size of the entries Vec. The
remaining peak would be dominated by symbols + fuzzy + definitions + word
intern tables. Exact impact must be measured empirically after implementation.

## Step 3: Build fuzzy index lazily

The fuzzy trigram index (Massif: 23 MB on drivers/net, `memory_breakdown`:
115 MB on full kernel) is built during scan but only queried when the user
types. Deferring construction to first use removes it from peak entirely.

- Simple to implement: store the symbol list, build trigrams on first
  `search_symbols` call behind a `OnceLock` or similar.
- First fuzzy query pays a one-time build cost.
- Independent of Steps 1–2.

## Step 4: Shrink or externalize symbols

`memory_breakdown` on the full kernel measured symbols at 294 MB — the
single largest resident data structure. Each `Symbol` contains multiple
owned Strings (name, def_keyword, doc_comment, signature, container).

Options:
- **Intern symbol strings**: Use a shared string pool instead of per-symbol
  owned Strings. Many symbols share the same `def_keyword` ("fn", "struct",
  "impl", etc).
- **Store symbols on disk**: Write symbols to a memory-mapped file during
  scan, load on demand per-file when needed for hover/definition queries.
  Most symbol data is only needed for the file currently being viewed.
- **Drop non-essential fields after scan**: Fields like `doc_comment` and
  `signature` are only needed for hover. They could be re-extracted from
  source on demand instead of cached.

Exact savings must be measured after implementation.

## Ultimate goal: bounded-memory indexing

The end state is an indexing pipeline whose peak RSS is bounded by a
configurable constant (e.g. 256 MB), regardless of codebase size. This
requires that **no data structure grows proportionally to the total number of
files or occurrences in the corpus**.

### Architecture

1. **Streaming tokenization → disk**: Entries written to temp chunk files
   during scan (Step 2). Memory per chunk is bounded.

2. **External merge sort**: Chunk files sorted and merged on disk.
   Memory = O(k × buffer_size) where k = number of chunks.

3. **Streaming index write**: `files.v2.bin`, `index.v2.bin`, `words.v2.bin`
   written in a single streaming pass over the sorted data. Posting lists
   built incrementally.

4. **Symbols on disk**: Symbol data stored in a memory-mapped file or
   on-disk format, loaded per-file on demand (Step 4).

5. **Lazy fuzzy index**: Built on first query, not during scan (Step 3).
   Or also disk-backed.

6. **Intern tables bounded or spilled**: The word intern table grows with
   unique words (typically 100K–500K). If this becomes a problem, use a
   disk-backed hash map. In practice, intern tables are much smaller than
   the entries Vec, so this is likely not needed.

Under this architecture, peak RSS during indexing is bounded by:
- One chunk of file contents (for tokenization)
- Merge buffers
- Intern tables (bounded by unique word/path count, not total occurrences)
- Fixed-size I/O buffers

After indexing, resident memory is bounded by:
- Word directory (small, proportional to unique words)
- Whatever symbols/definitions are needed for the active file
- Fuzzy index (if built, or also disk-backed)

Everything else lives on disk and is accessed via the three-file format
(already designed for this) or memory-mapped files.

### What this enables

A machine with 512 MB of free RAM could index a codebase of arbitrary size,
limited only by disk space and time. The current architecture requires RAM
proportional to total occurrences; the bounded architecture requires RAM
proportional to chunk size (configurable).

## Step 5: Truly incremental word index updates

### Problem

When any file changes, the entire word index is rebuilt from scratch:
every file is re-read, re-tokenized, all entries are re-sorted, and all
three output files are rewritten. The cost is O(total entries) —
proportional to the entire codebase, not the change.

At 100K files with ~30 entries per file = ~3M entries, the merge+write
alone is significant. At Linux kernel scale (65K files, 47M entries),
it dominates. The goal: updating 1 file should cost O(entries in that
file), not O(entries in all files).

### Why the naive "read unchanged from disk" approach isn't enough

A naive fix (read unchanged files' occurrences from old files.v2.bin
instead of re-tokenizing) still requires:

- Reading all ~3M unchanged entries from disk (42 MB at 14 bytes each)
- Feeding them all through the builder
- K-way merge sorting all entries
- Rewriting all three output files from scratch

This is faster than re-tokenizing (no CPU for parsing) but still
O(total entries) in I/O and merge time. For a 100K-file codebase
that's still seconds of wall time to rewrite hundreds of MB.

### Design: overlay-based incremental updates

Instead of rewriting the base index on every change, maintain a small
**overlay** that records deltas. Queries merge base + overlay at read
time. Periodically compact the overlay into the base.

This is the same principle as LSM-trees (LevelDB, RocksDB, Lucene).

#### On-disk layout

```
~/.cache/quicklsp/<project>/
  words.v2.bin      ← base string tables (word_id → word, path_id → path)
  files.v2.bin      ← base per-file occurrences
  index.v2.bin      ← base posting lists
  meta.json          ← base metadata + per-file mtimes
  symbols.bin        ← base symbol cache

  overlay.log        ← NEW: append-only change log
```

#### overlay.log format (append-only)

The overlay is a sequential log of tagged entries. Each file change
appends one or more entries to the end. No seeking, no rewriting of
earlier data. The file is never modified in-place — only appended to
or replaced wholesale during compaction.

```
[header: 12 bytes]
  magic(8) = "QLSO\x01\x00\x00\x00"
  base_word_count(u32)         — word_table.len() when base was built

[repeated entries, append-only]:

  TAG 0x01 — new word
    tag(u8) = 0x01
    len(u16)
    word_bytes(len)
    — assigns word_id = base_word_count + (sequential counter)

  TAG 0x02 — new path
    tag(u8) = 0x02
    len(u32)
    path_bytes(len)
    — assigns path_id = base_path_count + (sequential counter)

  TAG 0x03 — file patch (replaces all occurrences for a file)
    tag(u8) = 0x03
    path_id(u32)
    mtime(u64)                 — unix seconds, for freshness checks
    occ_count(u32)
    occ_count × 14 bytes       — (word_id(4) + line(4) + col(4) + len(2))
    — occurrences sorted by (word_id, line), same layout as files.v2.bin
    — logically replaces any earlier patch for this path_id

  TAG 0x04 — file removed
    tag(u8) = 0x04
    path_id(u32)
    — logically removes this file from the index

  TAG 0x05 — symbol update (for a single file)
    tag(u8) = 0x05
    path_id(u32)
    symbol_count(u32)
    symbol_count × symbol_data  — same binary layout as symbols.bin per-file
    — replaces cached symbols for this file
```

**Append on file change**: tokenize the changed file, then append
TAG 0x01 entries for any new words, TAG 0x02 if it's a new file,
TAG 0x03 with the new occurrences, TAG 0x05 with updated symbols.
Total I/O: one sequential write of a few KB. No fsync needed — the
log is reconstructable from source files.

**Multiple edits to the same file**: just append another TAG 0x03.
When loading, last entry for each path_id wins. Dead entries before
it are ignored (reclaimed at compaction).

**Crash safety**: a truncated final entry is detected by validating
entry size against remaining bytes. The reader discards any
incomplete trailing entry. No data corruption — at worst the last
edit is lost, and the next save re-appends it.

**No separate overlay_meta.json needed**: the log itself contains
mtimes (in TAG 0x03) and removed files (TAG 0x04). On load, scan
the log to reconstruct the overlay state. The log is small, so this
scan is fast.

#### Loading the overlay (startup)

Read overlay.log sequentially from start to end:

1. For each TAG 0x01: append word to `new_words` vec.
2. For each TAG 0x02: append path to `new_paths` vec.
3. For each TAG 0x03: insert/replace in `patched_files[path_id]`.
   Record mtime.
4. For each TAG 0x04: insert into `removed_files` set.
5. For each TAG 0x05: update cached symbols for the file.

Build the in-memory `word_to_files` reverse index from patched
files' occurrences.

Cost: O(log size). The log is bounded by compaction threshold, so
this is fast — typically under 1 MB even after hundreds of edits.

#### Query path: merge base + overlay

**find_references(word):**

1. Look up `word` in base `word_dir` → `(posting_offset, posting_count)`.
   Also check if word is in the overlay's new words (small hash map
   lookup — overlay is tiny).

2. Read base posting list: file_ids that contain this word.

3. Filter out file_ids that are patched or removed in the overlay.

4. For surviving base file_ids: read occurrences from base
   `files.v2.bin` (existing code path, unchanged).

5. For patched file_ids: read occurrences from in-memory overlay
   (loaded at startup from overlay.log).

6. For new file_ids (added files in overlay): scan their overlay
   occurrences for the word.

7. Merge results.

Cost: O(base postings for word + overlay size). The overlay is small
(only changed files), so this adds negligible overhead to queries.

**Per-file occurrences (for files.v2.bin reads):**

- If file_id is NOT in overlay → read from base files.v2.bin (unchanged)
- If file_id IS in overlay → read from in-memory overlay
- This is a simple branch before the existing seek+read logic.

#### Update path: O(changed file's entries)

When a file changes:

1. **Tokenize** the changed file → occurrences.

2. **Intern new words**: Check each word against the base word table
   + existing overlay words (both in memory). Words not found get new
   IDs. Append TAG 0x01 entries to the log for each new word.

3. **Append TAG 0x03** to overlay.log: path_id + mtime + occurrences.

4. **Append TAG 0x05** to overlay.log: updated symbols for this file.

5. **Update in-memory overlay**: replace entry in `patched_files`,
   rebuild `word_to_files` for this file.

Cost: O(entries in changed file). One append write. No reading or
rewriting of base index or other overlay entries.

#### Compaction: amortized O(total)

When the overlay grows beyond a threshold (e.g., >5% of base size
or >1000 patched files), compact by rebuilding the full base index:

1. The current cold-index path already produces fresh base files.
2. Delete overlay.log.
3. The new base incorporates all overlay changes.

This is the current full rebuild, but it happens infrequently.
Amortized cost per update: O(1) average, O(total) worst case.

Compaction can be triggered:
- At startup if overlay is large.
- In background during idle periods.
- Never during active editing (to avoid blocking).

#### In-memory structures

After loading base + overlay:

```
WordIndex {
    // Base index (existing)
    index_dir: PathBuf,
    word_dir: WordDirectory,        // base word directory
    file_table: Vec<(u64, u32)>,    // base file table
    path_table: Vec<String>,
    word_table: Vec<String>,

    // Overlay (new)
    overlay: Option<Overlay>,
}

Overlay {
    patched_files: HashMap<u32, OverlayFile>,  // path_id → new occs
    removed_files: HashSet<u32>,                // invalidated path_ids
    new_words: Vec<String>,                     // IDs = base_word_count + i
    new_paths: Vec<String>,                     // IDs = base_path_count + i
    // Reverse posting index for overlay entries (small)
    word_to_files: HashMap<u32, Vec<u32>>,      // word_id → [path_id]
}

OverlayFile {
    occurrences: Vec<(u32, u32, u32, u16)>,  // (word_id, line, col, len)
}
```

Memory for the overlay: proportional to the number of changed files ×
their avg occurrences. For 10 changed files × 500 entries: ~100 KB.
Negligible.

### What stays the same

- Cold index path: unchanged. Builds base index from scratch.
- FullyFresh warm startup: unchanged. Loads base index, no overlay.
- All existing query methods: unchanged internally, just add an
  overlay merge step.
- Three-file format: unchanged. Overlay is an additional file.

### Implementation order

**5a. Overlay data structures + append-only writer**

Add `Overlay` struct and `OverlayWriter`. The writer opens overlay.log
in append mode and provides methods: `append_new_word(word)`,
`append_file_patch(path_id, mtime, occurrences)`,
`append_file_removed(path_id)`, `append_symbol_update(path_id, symbols)`.
Each method does a single buffered write. No seeking.

**5b. Overlay reader + in-memory index**

Add `Overlay::load(path)` that reads overlay.log sequentially and
builds the in-memory `patched_files` / `removed_files` / `new_words`
maps. Handle truncated entries gracefully (discard incomplete tail).

**5c. Query path: merge base + overlay**

Modify `find_references()` to filter invalidated file_ids and include
overlay entries. Modify per-file occurrence reads to check overlay
first. This is a small code change in the existing `find_references`.

**5d. PartiallyStale write path**

When `scan_directory` detects changed files and a base index exists:
instead of rebuilding the full index, tokenize only the changed files
and append their patches to overlay.log. Skip the par_chunks loop
for all other files. This is the key performance win.

**5e. Compaction**

When overlay exceeds threshold on startup (e.g., file size > 10% of
base files.v2.bin, or >1000 patched files), trigger a full rebuild.
The cold path already does this — just delete overlay.log afterward.

**5f. did_change / did_save integration**

When the LSP server receives `textDocument/didChange` or `didSave`,
tokenize the single changed file and append to overlay.log. This
makes single-file edits O(1 file) instead of triggering a full
re-index. The existing `index_file` / `update_file` methods become
the entry point for this.

### Performance at 100K file scale

| Scenario | Current | With overlay |
|---|---|---|
| Cold index (100K files) | ~40s | ~40s (same) |
| Warm startup (0 changes) | ~100ms | ~100ms (same) |
| 1 file changed | ~40s full rebuild | **~50ms** (tokenize + write overlay) |
| 100 files changed | ~40s full rebuild | **~500ms** (tokenize 100 + write overlay) |
| 10K files changed | ~40s full rebuild | ~5s tokenize, then trigger compaction |
| Query (word in 200 files) | ~2ms | ~2ms + overlay scan (~0.1ms) |

The key win: **single-file update drops from O(total) to O(changed)**.
For a 100K-file codebase, that's 3-4 orders of magnitude faster.

### Tradeoffs

- **Query overhead**: Each query must check the overlay. The overlay
  is in memory (hash lookup), so overhead is <0.1ms per query.
  Negligible compared to disk I/O for base posting lists.
- **Complexity**: Two-level index with merge-on-read. More code paths
  to test. But the overlay is simple (append-only log, no random
  access writes, no rewriting).
- **Crash safety**: Append-only design means partial writes only affect
  the last entry. Reader validates entry sizes and discards truncated
  trailing data. No corruption of earlier entries. At worst, the last
  edit is lost and re-appended on next save.
- **Dead entries**: Editing the same file 100 times appends 100 patches
  but only the last one matters. The dead space is reclaimed at
  compaction. Between compactions, log size = sum of all patches ever
  written (not just live ones). This is bounded by the compaction
  threshold.
- **Disk space**: Overlay duplicates data for patched files. At most
  ~5% of base size (compaction threshold). Negligible.
- **Startup cost**: Loading the overlay requires a sequential scan of
  the log. At 1 MB/ms for cached I/O, a 5 MB overlay loads in ~5ms.

### Verification

- Full test suite passes.
- Incremental: modify 1 file, verify `find_references` returns updated
  results.
- Add new file with new words, verify they're queryable.
- Delete a file, verify its references disappear.
- Compact, verify results identical to a fresh cold index.
- Benchmark: 100K-file corpus, modify 1 file, measure update time.

## Completed steps

1. ~~Implement in-place sort in `WordIndexBuilder::build()`.~~ **DONE**
2. ~~Refactor `write_files_bin` and posting-list builder to accept sorted
   slices instead of `Vec<Vec<CompactEntry>>`.~~ **DONE**
3. ~~Implement Step 2 (stream entries to disk + external merge sort).~~ **DONE**
4. ~~Implement Step 3 (lazy fuzzy index).~~ **DONE**
5. ~~Implement Step 4 (strip doc_comment/signature from symbols).~~ **DONE**
6. ~~Persist symbols to disk for instant warm startup.~~ **DONE**
7. ~~Detect modified files and re-parse only changed ones.~~ **DONE**

## Next steps

8. Implement Step 5 (truly incremental word index from disk).
9. Measure incremental update time empirically.
10. Run the full test suite after to verify correctness.
