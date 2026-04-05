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

When any file changes, the entire word index is rebuilt from scratch.
All 6030 files are re-read, re-tokenized, and re-merged — even if only
one file changed. On our test corpus this takes 2.3s. On the full Linux
kernel it would take much longer.

The symbol cache (symbols.bin) already avoids re-extracting definitions
for unchanged files. But the word index rebuild re-tokenizes every file
because it needs occurrences (word_id, line, col, len) from every file
to produce the three output files.

### Key insight: files.v2.bin is already seekable per-file

The existing on-disk format stores occurrences grouped by file_id with
a file table for random access:

```
file_table[file_id] = (occ_offset: u64, occ_count: u32)
```

We can read back any unchanged file's occurrences by seeking to
`occ_offset` and reading `occ_count × 14` bytes. No source text or
tokenization needed. The `find_references()` query path already does
exactly this.

### Approach

When the PartiallyStale path detects N changed files out of F total:

#### Phase A: Read unchanged occurrences from disk

For each unchanged file_id:
1. Look up `(occ_offset, occ_count)` from the old file table
   (already in memory as `WordIndex.file_table`).
2. Seek into old `files.v2.bin`, read `occ_count × 14` bytes.
3. Parse into CompactEntry values using the existing word_id/path_id
   namespace (word and path tables from the old index).
4. Feed these entries into the builder as a sorted chunk (they're
   already sorted by (word_id, line) within each file).

This is O(unchanged_entries) in I/O but zero CPU for tokenization.

#### Phase B: Re-tokenize changed files

For each changed file:
1. Read source from disk.
2. Tokenize to get occurrences.
3. Intern words — some may be new (not in old word table), some
   existing. The builder's intern tables handle this naturally.
4. Intern path — may be a new file (new path_id) or existing.
5. Feed entries into the builder.

For removed files: skip them (don't read from old files.v2.bin,
don't tokenize).

For added files: tokenize them (they won't be in old files.v2.bin).

#### Phase C: Build new word index

The builder now has entries from both sources (disk + fresh tokenize).
Proceed with the normal build path: flush to disk, k-way merge,
write new files.v2.bin + index.v2.bin + words.v2.bin.

#### Handling word_id / path_id identity

The old word index has a word_table and path_table baked into
words.v2.bin. When reading unchanged occurrences, the word_ids
refer to this old table. The new builder has its own intern tables.

Two options:

**Option A: Seed builder with old tables.**

Before processing any files, seed the builder's intern tables with
the old word_table and path_table (loaded from words.v2.bin during
warm startup — already in `WordIndex.word_table` / `path_table`).
This ensures old word_ids and path_ids are valid in the new builder.
New words from changed files get new IDs appended to the end.

This is the simplest approach. The builder's `intern_word` /
`intern_path` already deduplicate, so seeding just pre-fills the
tables. Old CompactEntry values can be fed directly without
re-interning.

**Option B: Remap IDs.**

Build fresh intern tables and remap old entries' word_id/path_id
to new IDs. More complex, no benefit.

**Verdict: Option A.** Seed the builder from the old index.

### What changes

1. **New method: `WordIndexBuilder::seed_from_index(word_table, path_table)`**
   — Pre-fills intern tables so old IDs remain valid.

2. **New method: `WordIndexBuilder::ingest_unchanged_file(file_id, occurrences)`**
   — Accepts pre-parsed occurrences from disk (already has valid IDs).

3. **New function: `read_file_occurrences(files_bin_path, file_table, file_id) → Vec<CompactEntry>`**
   — Reads one file's occurrences from files.v2.bin using the seekable
   file table.

4. **PartiallyStale path in `scan_directory`:**
   ```
   // 1. Seed builder from old index
   builder.seed_from_index(&old_word_table, &old_path_table);

   // 2. Read unchanged files' occurrences from disk
   for file_id in 0..old_file_count {
       let path = &old_path_table[file_id];
       if !changed_set.contains(path) {
           let occs = read_file_occurrences(&files_bin, &file_table, file_id);
           builder.ingest_unchanged_file(file_id, occs);
       }
   }

   // 3. Re-tokenize changed files only
   for path in &changed_files {
       let source = read_to_string(path)?;
       index_file_core(path, &source, false);  // updates symbols
       let occs = take(&mut files[path].occurrences);
       builder.drain_file_occurrences(path, occs, &source);
   }

   // 4. Build new index (same as cold path)
   builder.flush_to_disk()?;
   finish_word_index(root, &file_mtimes, builder);
   ```

5. **`scan_directory` no longer calls par_chunks for PartiallyStale.**
   The reading-from-disk loop replaces the parallel tokenize loop for
   unchanged files. Only changed files are read + tokenized.

### Expected performance

If 1 file out of 6030 changes:
- Read ~1.7M occurrences from files.v2.bin: sequential I/O, ~33 MB
  at 14 bytes/entry. On SSD: ~50ms. On warm OS page cache: ~10ms.
- Tokenize 1 file: <1ms.
- Build new word index from seeded builder: ~150ms (same as cold).
- **Total: ~200ms** vs current 2.3s for the full re-tokenize.

If 100 files change: ~200ms + ~50ms tokenize = ~250ms.

The dominant cost is always the k-way merge + write, which is O(total
entries) regardless. Reading unchanged occurrences from disk is much
faster than reading + tokenizing source files.

### Tradeoffs

- **Disk I/O vs CPU**: Reading 33 MB from files.v2.bin replaces
  reading ~hundreds of MB of source files + tokenizing each one.
  Net I/O is much less; CPU savings are large.
- **Complexity**: The seed-from-old-index path is a new code path
  that must maintain word_id/path_id consistency. Bugs here would
  produce corrupted indexes.
- **New files with new words**: Handled naturally by the builder's
  intern tables — new words get IDs after the seeded range.
- **Removed files**: Their old entries are simply not read from disk.
  Their word_ids may become orphaned (no entries reference them), but
  this is harmless — the word table just has a few unused entries.

### Verification

- Run the full test suite.
- Benchmark: cold index, then modify 1 file, measure incremental.
- Verify that `find_references("word_in_unchanged_file")` still works.
- Verify that `find_references("word_in_changed_file")` reflects the
  new content.
- Verify that `find_references("new_word_only_in_changed_file")` works.
- Compare output of incremental build vs full rebuild to verify they
  produce identical index files.

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
