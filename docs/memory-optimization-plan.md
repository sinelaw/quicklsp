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

## Plan

1. ~~Implement in-place sort in `WordIndexBuilder::build()`.~~ **DONE**
2. ~~Refactor `write_files_bin` and posting-list builder to accept sorted
   slices instead of `Vec<Vec<CompactEntry>>`.~~ **DONE**
3. Measure on full kernel to get post-Step-1 peak RSS.
4. Implement Step 2 (stream entries to disk + external merge sort).
5. Implement Step 3 (lazy fuzzy index).
6. Implement Step 4 (externalize symbols).
7. After each step, measure peak RSS empirically on the full kernel.
8. Run the full test suite after each step to verify correctness.
