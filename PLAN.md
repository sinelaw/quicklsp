# Plan: Hash-based posting-list-only index

## Problem
LogIndex holds 47M occurrences in memory (828 MB) plus word/path intern tables.
Writer builds a 487 MB intern table during scan.

## Solution
Replace word_id-based occurrence index with hash-based posting lists.
No word strings stored. No per-file occurrences stored. Just:
  word_hash_u32 → [file_ids that contain this word]

## Steps

- [ ] 1. Change LogWriteMsg: compute unique word hashes per file during scan (no source needed in msg)
- [ ] 2. Change LogWriter: write per-file word hashes instead of OccEntry. Remove intern tables
- [ ] 3. Change log format: TAG_FILE_DATA stores [word_hash_u32, ...] + symbols (no OccEntry)
- [ ] 4. Change LogIndex: postings as HashMap<u32, Vec<u32>>, drop files/word_table/word_lookup
- [ ] 5. Change LogIndex::load: build hash-based postings from log
- [ ] 6. Change find_references: posting lookup → file list → re-scan files for positions
- [ ] 7. Update incremental path
- [ ] 8. Update memory_usage, tests
