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

## Massif results: drivers/net peak snapshot

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

## What happens at peak

The peak occurs inside `WordIndexBuilder::build()` at the moment both the
original `self.entries` Vec and the new `file_buckets` Vec<Vec<CompactEntry>>
coexist in memory:

### Current flow in build()

1. `self.entries` holds all 7.2M CompactEntry values (20 bytes each) in a
   single flat Vec. This was accumulated during Phase 2 via
   `drain_file_occurrences`. Massif measured this backing allocation at
   **171 MB** (includes amortized over-allocation from `reserve`).

2. `file_buckets: Vec<Vec<CompactEntry>>` is created — one inner Vec per
   file (6,136 files for drivers/net).

3. `self.entries.drain(..)` iterates all entries, pushing each into
   `file_buckets[entry.path_id]`. Critically, `drain(..)` does NOT free the
   backing allocation of `self.entries` — it only drops elements and sets
   length to 0. So during this loop, **both allocations exist**:
   - `self.entries` backing: 171 MB (still allocated, just logically empty)
   - `file_buckets` accumulated: 209 MB (larger than entries because each
     per-file Vec over-allocates independently via amortized doubling)

4. `self.entries = Vec::new()` finally frees the old backing store, but by
   then the peak has already been recorded.

5. Each file bucket is sorted by `(word_id, line)`.

6. `files.v2.bin` is written from `file_buckets` while main thread builds
   `word_postings` from the same data.

7. `drop(file_buckets)`.

The entries + file_buckets duplication accounts for **380 MB out of 550 MB
(69%)** of peak heap on this subset.

### Extrapolation to full kernel

The full kernel has 46.8M entries vs 7.2M for drivers/net (6.5x). The
entries Vec alone would be ~1.1 GB, and file_buckets ~1.35 GB, for a combined
~2.5 GB. This lines up with the observed 2.72 GB peak RSS on the full kernel
(the remaining ~200 MB is symbols, fuzzy index, definitions DashMap, etc).

## Proposed solution: in-place sort

Replace the entries-to-buckets copy with an in-place sort.

### Current code (format.rs:247-259)

```rust
let mut file_buckets: Vec<Vec<CompactEntry>> = Vec::with_capacity(path_count);
file_buckets.resize_with(path_count, Vec::new);
for entry in self.entries.drain(..) {
    file_buckets[entry.path_id as usize].push(entry);
}
self.entries = Vec::new();

for bucket in &mut file_buckets {
    bucket.sort_unstable_by(|a, b| {
        a.word_id.cmp(&b.word_id).then_with(|| a.line.cmp(&b.line))
    });
}
```

### Proposed replacement

```rust
self.entries.sort_unstable_by(|a, b| {
    a.path_id.cmp(&b.path_id)
        .then_with(|| a.word_id.cmp(&b.word_id))
        .then_with(|| a.line.cmp(&b.line))
});
```

After sorting, contiguous slices of `self.entries` with the same `path_id`
are equivalent to the old per-file buckets. The downstream code
(`write_files_bin`, posting list construction) iterates file-by-file, so it
can iterate slices instead of separate Vecs.

### Expected impact

- Eliminates the `file_buckets` allocation entirely (~209 MB on drivers/net,
  ~1.35 GB on full kernel).
- Peak during `build()` becomes just the existing `self.entries` Vec
  (~171 MB on drivers/net, ~1.1 GB on full kernel) plus word_postings and
  I/O buffers.
- In-place sort is also cache-friendlier than scattering entries across
  thousands of separate heap allocations.

### Estimated new peak RSS for full kernel

The 380 MB entries+buckets overlap on drivers/net would become ~171 MB
(entries only). Extrapolating to the full kernel: the ~2.5 GB
entries+buckets cost drops to ~1.1 GB. Combined with the ~200 MB baseline
(symbols, fuzzy, definitions, etc), estimated new peak is roughly **1.3 GB**
— a ~50% reduction from the current 2.7 GB.

### Tradeoffs

- `sort_unstable_by` on 46.8M entries (20 bytes each, ~937 MB) takes
  O(n log n) time. The current approach is O(n) scatter + O(n log n) per-file
  sort, but the per-file sorts are on smaller arrays. In practice the
  difference is small — the sort step on drivers/net already takes 1.5s and
  is not the bottleneck.
- The downstream `write_files_bin` and posting-list builder need minor
  refactoring to iterate slices of a sorted array instead of separate Vecs.
  This is straightforward.

## Alternative solutions considered

### Stream to disk during accumulation

Instead of accumulating all CompactEntries in memory, write them to a
temporary file during Phase 2, then external-sort the file. This would
eliminate the ~1.1 GB entries Vec from peak entirely.

- Pro: largest possible peak reduction.
- Con: significant complexity (external merge sort, temp file management),
  I/O overhead, and the entries Vec is already needed for the sort anyway.
- Verdict: overkill unless the in-place sort isn't enough.

### Shrink CompactEntry

CompactEntry is 20 bytes (word_id: u32, path_id: u32, line: u32, col: u32,
len: u16, _pad: u16). Reducing line/col to u16 would save 4 bytes per entry
(20%) but cap lines at 65,535 — some kernel files exceed this.

- Pro: proportional reduction across all phases.
- Con: requires fallback handling for large files, modest savings.
- Verdict: possible follow-up, not the main win.

### Build fuzzy index lazily

The fuzzy trigram index (23 MB on drivers/net, ~115 MB on full kernel) is
built during scan but only used when the user types a query. Building it
lazily on first use would shave ~115 MB from peak.

- Pro: simple to implement, no algorithmic changes.
- Con: first fuzzy query would be slower.
- Verdict: good complementary optimization, independent of the main fix.

## Plan

1. Implement in-place sort in `WordIndexBuilder::build()`.
2. Refactor `write_files_bin` and posting-list builder to accept sorted
   slices instead of `Vec<Vec<CompactEntry>>`.
3. Run Massif on drivers/net to verify the file_buckets allocation is gone.
4. Run the full kernel benchmark to measure the actual peak RSS reduction.
5. Run the full test suite to verify correctness.
