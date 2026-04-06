//! Word index: three-file format for compact storage and incremental update.
//!
//! ## Files (in `~/.cache/quicklsp/<project-hash>/`)
//!
//! ### `words.v2.bin` — shared string tables
//! ```text
//! [header: 24 bytes]  magic + word_count(u32) + path_count(u32) + reserved(u64)
//! [word offsets]      (word_count+1) × u32 — byte offset into word data
//! [word data]         concatenated word strings
//! [path offsets]      (path_count+1) × u32 — byte offset into path data
//! [path data]         concatenated path strings
//! ```
//!
//! ### `files.v2.bin` — per-file occurrences (unit of incremental update)
//! ```text
//! [header: 16 bytes]  magic + file_count(u32) + reserved(u32)
//! [file table]        file_count × 12 bytes: occ_offset(u64) + occ_count(u32)
//! [occurrence data]   per file, sorted by (word_id, line):
//!                     word_id(u32) + line(u32) + col(u32) + len(u16) = 14 bytes
//! ```
//!
//! ### `index.v2.bin` — inverted posting lists for queries
//! ```text
//! [header: 16 bytes]  magic + word_count(u32) + reserved(u32)
//! [word directory]    word_count × 8 bytes: posting_offset(u32) + posting_count(u32)
//! [posting data]      file_id(u32) per posting, grouped by word
//! ```

use ahash::AHashMap;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const MAGIC_WORDS: [u8; 8] = *b"QLSW\x02\x00\x00\x00";
const MAGIC_FILES: [u8; 8] = *b"QLSF\x02\x00\x00\x00";
const MAGIC_INDEX: [u8; 8] = *b"QLSI\x02\x00\x00\x00";

/// Size of a single on-disk occurrence: word_id(4) + line(4) + col(4) + len(2) = 14.
const OCC_SIZE: usize = 14;

/// Size of a single CompactEntry on disk (same as in-memory due to repr(C)).
const ENTRY_SIZE: usize = 20;

/// A single entry in the word index (public API for query results).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct IndexEntry {
    pub word: String,
    pub path: PathBuf,
    pub line: u32,
    pub col: u32,
    pub len: u16,
}

/// Compact in-memory entry used during index building.
/// Stores intern-table IDs instead of full strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
struct CompactEntry {
    word_id: u32,
    path_id: u32,
    line: u32,
    col: u32,
    len: u16,
    _pad: u16,
}

const _: () = assert!(std::mem::size_of::<CompactEntry>() == 20);

impl CompactEntry {
    fn new(word_id: u32, path_id: u32, line: u32, col: u32, len: u16) -> Self {
        Self {
            word_id,
            path_id,
            line,
            col,
            len,
            _pad: 0,
        }
    }
}

// ── WordDirectory ─────────────────────────────────────────────────────────

/// Compact in-memory word directory: sorted flat array + string pool.
///
/// Supports two lookup modes:
/// - `get(word)` → (posting_offset, posting_count) for find_references queries
/// - `get_word_id(word)` → word_id for building/updating the index
#[derive(Debug, Default)]
pub struct WordDirectory {
    pool: String,
    entries: Vec<DirEntry>,
}

#[derive(Debug, Clone, Copy)]
struct DirEntry {
    pool_offset: u32,
    word_len: u16,
    posting_offset: u32,
    posting_count: u32,
}

impl WordDirectory {
    pub fn new() -> Self {
        Self {
            pool: String::new(),
            entries: Vec::new(),
        }
    }

    fn insert(&mut self, word: &str, posting_offset: u32, posting_count: u32) {
        let pool_offset = self.pool.len() as u32;
        self.pool.push_str(word);
        self.entries.push(DirEntry {
            pool_offset,
            word_len: word.len() as u16,
            posting_offset,
            posting_count,
        });
    }

    fn word_at(&self, e: &DirEntry) -> &str {
        &self.pool[e.pool_offset as usize..(e.pool_offset as usize + e.word_len as usize)]
    }

    /// Look up a word → (posting_offset, posting_count).
    pub fn get(&self, word: &str) -> Option<(u32, u32)> {
        let pool = self.pool.as_bytes();
        self.entries
            .binary_search_by(|e| {
                pool[e.pool_offset as usize..(e.pool_offset as usize + e.word_len as usize)]
                    .cmp(word.as_bytes())
            })
            .ok()
            .map(|i| {
                (
                    self.entries[i].posting_offset,
                    self.entries[i].posting_count,
                )
            })
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn memory_usage(&self) -> usize {
        self.pool.len() + self.entries.len() * std::mem::size_of::<DirEntry>()
    }
}

// ── WordIndexBuilder ──────────────────────────────────────────────────────

/// Builder for constructing the three-file word index.
pub struct WordIndexBuilder {
    entries: Vec<CompactEntry>,
    path_table: Vec<String>,
    path_lookup: AHashMap<String, u32>,
    word_table: Vec<String>,
    word_lookup: AHashMap<String, u32>,
    /// Sorted chunk files spilled to disk.
    chunk_files: Vec<PathBuf>,
    /// Total entries spilled to disk across all chunk files.
    spilled_count: usize,
    /// Temp directory for chunk files (kept alive for cleanup).
    spill_dir: Option<tempfile::TempDir>,
}

impl WordIndexBuilder {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            path_table: Vec::new(),
            path_lookup: AHashMap::new(),
            word_table: Vec::new(),
            word_lookup: AHashMap::new(),
            chunk_files: Vec::new(),
            spilled_count: 0,
            spill_dir: None,
        }
    }

    fn intern_path(&mut self, path: &Path) -> u32 {
        let path_str = path.to_string_lossy();
        if let Some(&idx) = self.path_lookup.get(path_str.as_ref()) {
            idx
        } else {
            let idx = self.path_table.len() as u32;
            let owned = path_str.into_owned();
            self.path_lookup.insert(owned.clone(), idx);
            self.path_table.push(owned);
            idx
        }
    }

    fn intern_word(&mut self, word: &str) -> u32 {
        if let Some(&idx) = self.word_lookup.get(word) {
            idx
        } else {
            let idx = self.word_table.len() as u32;
            self.word_lookup.insert(word.to_string(), idx);
            self.word_table.push(word.to_string());
            idx
        }
    }

    fn intern_word_owned(&mut self, word: String) -> u32 {
        if let Some(&idx) = self.word_lookup.get(&word) {
            idx
        } else {
            let idx = self.word_table.len() as u32;
            self.word_lookup.insert(word.clone(), idx);
            self.word_table.push(word);
            idx
        }
    }

    /// Add an entry to the builder.
    pub fn add(&mut self, entry: IndexEntry) {
        let word_id = self.intern_word_owned(entry.word);
        let path_id = self.intern_path(&entry.path);
        self.entries.push(CompactEntry::new(
            word_id, path_id, entry.line, entry.col, entry.len,
        ));
    }

    /// Add entries from a file's occurrences, resolving byte offsets against `source`.
    pub fn add_file_occurrences(
        &mut self,
        path: &Path,
        occurrences: &[crate::parsing::tokenizer::Occurrence],
        source: &str,
    ) {
        let path_id = self.intern_path(path);
        for occ in occurrences {
            let word =
                &source[occ.word_offset as usize..(occ.word_offset + occ.word_len as u32) as usize];
            let word_id = self.intern_word(word);
            self.entries.push(CompactEntry::new(
                word_id,
                path_id,
                occ.line as u32,
                occ.col as u32,
                occ.word_len,
            ));
        }
    }

    /// Drain entries from a file's occurrences, resolving byte offsets against `source`.
    pub fn drain_file_occurrences(
        &mut self,
        path: &Path,
        occurrences: Vec<crate::parsing::tokenizer::Occurrence>,
        source: &str,
    ) {
        let path_id = self.intern_path(path);
        self.entries.reserve(occurrences.len());
        for occ in occurrences {
            let word =
                &source[occ.word_offset as usize..(occ.word_offset + occ.word_len as u32) as usize];
            let word_id = self.intern_word(word);
            self.entries.push(CompactEntry::new(
                word_id,
                path_id,
                occ.line as u32,
                occ.col as u32,
                occ.word_len,
            ));
        }
    }

    pub fn entry_count(&self) -> usize {
        self.spilled_count + self.entries.len()
    }

    /// Sort the in-memory entries and flush them to a temp chunk file on disk.
    /// Clears the entries Vec afterward, freeing its memory.
    pub fn flush_to_disk(&mut self) -> io::Result<()> {
        if self.entries.is_empty() {
            return Ok(());
        }

        // Lazily create temp directory.
        if self.spill_dir.is_none() {
            self.spill_dir = Some(tempfile::tempdir()?);
        }
        let dir = self.spill_dir.as_ref().unwrap().path();

        // Sort this chunk by (path_id, word_id, line) — same order as final output.
        self.entries.sort_unstable_by(|a, b| {
            a.path_id
                .cmp(&b.path_id)
                .then_with(|| a.word_id.cmp(&b.word_id))
                .then_with(|| a.line.cmp(&b.line))
        });

        // Write as raw bytes.
        let chunk_path = dir.join(format!("chunk_{}.bin", self.chunk_files.len()));
        let mut w = BufWriter::with_capacity(1 << 20, File::create(&chunk_path)?);
        let raw: &[u8] = unsafe {
            std::slice::from_raw_parts(
                self.entries.as_ptr() as *const u8,
                self.entries.len() * ENTRY_SIZE,
            )
        };
        w.write_all(raw)?;
        w.flush()?;

        self.spilled_count += self.entries.len();
        self.chunk_files.push(chunk_path);
        self.entries = Vec::new(); // free the backing allocation
        Ok(())
    }

    /// Build the three-file index. `dir` is the output directory.
    ///
    /// Returns (WordDirectory, word_table, path_table) for constructing a WordIndex.
    pub fn build(mut self, dir: &Path) -> io::Result<(WordDirectory, Vec<String>, Vec<String>)> {
        use std::time::Instant;

        // Free lookup tables — only needed during accumulation.
        self.word_lookup = AHashMap::new();
        self.path_lookup = AHashMap::new();

        let total_entries = self.entry_count();
        let word_count = self.word_table.len();
        let path_count = self.path_table.len();

        let t0 = Instant::now();
        log_rss("build: start");

        // Flush any remaining in-memory entries to disk.
        if !self.entries.is_empty() {
            self.flush_to_disk()?;
        }
        log_rss("build: after final flush");

        let files_path = dir.join("files.v2.bin");
        let index_path = dir.join("index.v2.bin");
        let words_path = dir.join("words.v2.bin");

        // ── 1. Streaming merge of sorted chunk files ────────────────────
        //
        // Each chunk file is already sorted by (path_id, word_id, line).
        // K-way merge them, collecting entries per-file, and simultaneously
        // write files.v2.bin and build posting lists.
        //
        // Peak memory = one file's entries + merge readers + posting lists.

        let mut word_postings: Vec<Vec<u32>> = Vec::with_capacity(word_count);
        word_postings.resize_with(word_count, Vec::new);

        // Streaming merge → files.v2.bin + posting lists
        streaming_build_files_and_postings(
            &files_path,
            &self.chunk_files,
            path_count,
            &mut word_postings,
        )?;

        let t1 = Instant::now();
        tracing::info!("word index build: merge + files.v2.bin + postings: {:.2?} ({} entries, {} files, {} chunks)",
            t1 - t0, total_entries, path_count, self.chunk_files.len());
        log_rss("build: after merge");

        // Drop chunk files and temp dir.
        self.chunk_files.clear();
        drop(self.spill_dir.take());

        // ── 2. Write index.v2.bin and words.v2.bin concurrently ─────────

        let mut sorted_word_ids: Vec<u32> = (0..word_count as u32).collect();
        sorted_word_ids.sort_unstable_by(|&a, &b| {
            self.word_table[a as usize].cmp(&self.word_table[b as usize])
        });

        let mut word_dir = WordDirectory::new();
        let words_write_err: std::sync::Mutex<Option<io::Error>> = std::sync::Mutex::new(None);

        std::thread::scope(|s| {
            let word_table = &self.word_table;
            let path_table = &self.path_table;
            s.spawn(|| {
                if let Err(e) = write_words_bin(&words_path, word_table, path_table) {
                    *words_write_err.lock().unwrap() = Some(e);
                }
            });

            let result = write_index_bin(
                &index_path,
                &sorted_word_ids,
                &word_postings,
                &self.word_table,
                &mut word_dir,
            );
            if let Err(e) = result {
                tracing::warn!("Failed to write index.v2.bin: {e}");
            }
        });

        if let Some(e) = words_write_err.into_inner().unwrap() {
            return Err(e);
        }

        let t2 = Instant::now();
        tracing::info!(
            "word index build: index.v2.bin + words.v2.bin: {:.2?}",
            t2 - t1
        );
        tracing::info!(
            "word index build: total: {:.2?} ({} entries, {} words, {} files)",
            t2 - t0,
            total_entries,
            word_count,
            path_count
        );

        Ok((word_dir, self.word_table, self.path_table))
    }
}

impl Default for WordIndexBuilder {
    fn default() -> Self {
        Self::new()
    }
}

fn log_rss(label: &str) {
    if let Ok(statm) = std::fs::read_to_string("/proc/self/statm") {
        if let Some(rss_pages) = statm
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse::<usize>().ok())
        {
            let rss_mb = rss_pages * 4096 / (1024 * 1024);
            tracing::info!("RSS [{}]: {} MB", label, rss_mb);
        }
    }
}

// ── Streaming merge + build ───────────────────────────────────────────────

/// Read a chunk file into a Vec<CompactEntry>.
fn read_chunk_file(path: &Path) -> io::Result<Vec<CompactEntry>> {
    let data = std::fs::read(path)?;
    assert!(
        data.len() % ENTRY_SIZE == 0,
        "chunk file size not a multiple of entry size"
    );
    let count = data.len() / ENTRY_SIZE;
    let mut entries = Vec::with_capacity(count);
    // SAFETY: CompactEntry is repr(C), 20 bytes, all fields are plain integers,
    // _pad is always written as 0. The chunk files were written from the same layout.
    unsafe {
        let src = data.as_ptr() as *const CompactEntry;
        entries.set_len(count);
        std::ptr::copy_nonoverlapping(src, entries.as_mut_ptr(), count);
    }
    Ok(entries)
}

/// A cursor into a sorted chunk file for k-way merging.
struct ChunkCursor {
    entries: Vec<CompactEntry>,
    pos: usize,
}

impl ChunkCursor {
    fn new(path: &Path) -> io::Result<Self> {
        let entries = read_chunk_file(path)?;
        Ok(Self { entries, pos: 0 })
    }

    fn peek(&self) -> Option<&CompactEntry> {
        self.entries.get(self.pos)
    }

    fn advance(&mut self) {
        self.pos += 1;
    }

    fn is_exhausted(&self) -> bool {
        self.pos >= self.entries.len()
    }
}

/// Streaming k-way merge of sorted chunk files.
/// Writes files.v2.bin and builds posting lists simultaneously.
///
/// Each chunk is loaded fully into memory one at a time via ChunkCursor.
/// Only one chunk needs to be "active" for the merge at any given time per stream,
/// but all cursors are alive simultaneously for the k-way merge.
///
/// For bounded memory, we use a min-heap to pick the smallest entry across
/// all cursors, then group entries by path_id to write files.v2.bin and
/// build posting lists file-by-file.
fn streaming_build_files_and_postings(
    files_path: &Path,
    chunk_files: &[PathBuf],
    path_count: usize,
    word_postings: &mut [Vec<u32>],
) -> io::Result<()> {
    use std::cmp::Reverse;
    use std::collections::BinaryHeap;

    if chunk_files.is_empty() {
        // No entries at all — write an empty files.v2.bin.
        return write_empty_files_bin(files_path, path_count);
    }

    // Load all chunk cursors. Each cursor holds one sorted chunk in memory.
    // Total memory = sum of all chunk sizes = total entries. However, the key
    // win is that during the *accumulation* phase, entries were flushed to disk
    // per-chunk, so peak during accumulation was bounded to one chunk.
    let mut cursors: Vec<ChunkCursor> = Vec::with_capacity(chunk_files.len());
    for cf in chunk_files {
        let c = ChunkCursor::new(cf)?;
        if !c.is_exhausted() {
            cursors.push(c);
        }
    }

    // Min-heap: (entry_sort_key, cursor_index)
    // We use a Reverse wrapper so BinaryHeap (max-heap) acts as min-heap.
    let mut heap: BinaryHeap<Reverse<(u32, u32, u32, usize)>> = BinaryHeap::new();
    for (i, c) in cursors.iter().enumerate() {
        if let Some(e) = c.peek() {
            heap.push(Reverse((e.path_id, e.word_id, e.line, i)));
        }
    }

    // Prepare files.v2.bin writer.
    let mut w = BufWriter::with_capacity(1 << 20, File::create(files_path)?);
    w.write_all(&MAGIC_FILES)?;
    w.write_all(&(path_count as u32).to_le_bytes())?;
    w.write_all(&0u32.to_le_bytes())?;

    let file_table_offset = 16u64;
    let file_table_size = path_count as u64 * 12;
    w.write_all(&vec![0u8; file_table_size as usize])?;
    let mut file_table: Vec<(u64, u32)> = Vec::with_capacity(path_count);
    let mut occ_offset = file_table_offset + file_table_size;

    let mut cur_path_id: u32 = 0;
    let mut cur_file_entries: Vec<CompactEntry> = Vec::new();

    // Flush one file's entries to files.v2.bin and update posting lists.
    let mut flush_file = |file_entries: &[CompactEntry],
                          file_id: u32,
                          w: &mut BufWriter<File>,
                          occ_offset: &mut u64,
                          file_table: &mut Vec<(u64, u32)>,
                          word_postings: &mut [Vec<u32>]|
     -> io::Result<()> {
        // Pad file_table for any skipped path_ids (files with no entries).
        while file_table.len() < file_id as usize {
            file_table.push((*occ_offset, 0));
        }
        file_table.push((*occ_offset, file_entries.len() as u32));

        // Write occurrences.
        for e in file_entries {
            w.write_all(&e.word_id.to_le_bytes())?;
            w.write_all(&e.line.to_le_bytes())?;
            w.write_all(&e.col.to_le_bytes())?;
            w.write_all(&e.len.to_le_bytes())?;
        }
        *occ_offset += (file_entries.len() * OCC_SIZE) as u64;

        // Build posting entries for this file.
        let mut prev_word_id = u32::MAX;
        for e in file_entries {
            if e.word_id != prev_word_id {
                word_postings[e.word_id as usize].push(file_id);
                prev_word_id = e.word_id;
            }
        }
        Ok(())
    };

    while let Some(Reverse((path_id, _word_id, _line, ci))) = heap.pop() {
        let entry = *cursors[ci].peek().unwrap();
        cursors[ci].advance();

        // Re-insert cursor's next entry into the heap.
        if let Some(next) = cursors[ci].peek() {
            heap.push(Reverse((next.path_id, next.word_id, next.line, ci)));
        }

        // If we've moved to a new file, flush the previous one.
        if path_id != cur_path_id || cur_file_entries.is_empty() {
            if !cur_file_entries.is_empty() {
                flush_file(
                    &cur_file_entries,
                    cur_path_id,
                    &mut w,
                    &mut occ_offset,
                    &mut file_table,
                    word_postings,
                )?;
                cur_file_entries.clear();
            }
            cur_path_id = path_id;
        }
        cur_file_entries.push(entry);
    }

    // Flush the last file.
    if !cur_file_entries.is_empty() {
        flush_file(
            &cur_file_entries,
            cur_path_id,
            &mut w,
            &mut occ_offset,
            &mut file_table,
            word_postings,
        )?;
    }

    // Pad file_table for any trailing empty files.
    while file_table.len() < path_count {
        file_table.push((occ_offset, 0));
    }

    // Back-patch the file table.
    w.flush()?;
    let mut file = w.into_inner()?;
    file.seek(SeekFrom::Start(file_table_offset))?;
    let mut tw = BufWriter::new(file);
    for &(off, cnt) in &file_table {
        tw.write_all(&off.to_le_bytes())?;
        tw.write_all(&cnt.to_le_bytes())?;
    }
    tw.flush()?;
    Ok(())
}

fn write_empty_files_bin(path: &Path, path_count: usize) -> io::Result<()> {
    let mut w = BufWriter::new(File::create(path)?);
    w.write_all(&MAGIC_FILES)?;
    w.write_all(&(path_count as u32).to_le_bytes())?;
    w.write_all(&0u32.to_le_bytes())?;
    let file_table_offset = 16u64;
    let file_table_size = path_count as u64 * 12;
    let occ_offset = file_table_offset + file_table_size;
    for _ in 0..path_count {
        w.write_all(&occ_offset.to_le_bytes())?;
        w.write_all(&0u32.to_le_bytes())?;
    }
    w.flush()?;
    Ok(())
}

// ── File writers ──────────────────────────────────────────────────────────

fn write_index_bin(
    path: &Path,
    sorted_word_ids: &[u32],
    word_postings: &[Vec<u32>],
    word_table: &[String],
    word_dir: &mut WordDirectory,
) -> io::Result<()> {
    let word_count = word_table.len();
    let mut w = BufWriter::with_capacity(1 << 20, File::create(path)?);
    w.write_all(&MAGIC_INDEX)?;
    w.write_all(&(word_count as u32).to_le_bytes())?;
    w.write_all(&0u32.to_le_bytes())?;

    let dir_table_offset = 16u64;
    let dir_table_size = word_count as u64 * 8;
    w.write_all(&vec![0u8; dir_table_size as usize])?;

    let mut posting_offset: u32 = 0;
    let mut dir_by_word_id: Vec<(u32, u32)> = vec![(0, 0); word_count];
    for &word_id in sorted_word_ids {
        let postings = &word_postings[word_id as usize];
        dir_by_word_id[word_id as usize] = (posting_offset, postings.len() as u32);
        word_dir.insert(
            &word_table[word_id as usize],
            posting_offset,
            postings.len() as u32,
        );
        for &file_id in postings {
            w.write_all(&file_id.to_le_bytes())?;
        }
        posting_offset += postings.len() as u32;
    }
    w.flush()?;

    let mut file = w.into_inner()?;
    file.seek(SeekFrom::Start(dir_table_offset))?;
    let mut tw = BufWriter::new(file);
    for &(off, cnt) in &dir_by_word_id {
        tw.write_all(&off.to_le_bytes())?;
        tw.write_all(&cnt.to_le_bytes())?;
    }
    tw.flush()?;
    Ok(())
}

fn write_words_bin(path: &Path, word_table: &[String], path_table: &[String]) -> io::Result<()> {
    let mut w = BufWriter::with_capacity(1 << 20, File::create(path)?);
    w.write_all(&MAGIC_WORDS)?;
    w.write_all(&(word_table.len() as u32).to_le_bytes())?;
    w.write_all(&(path_table.len() as u32).to_le_bytes())?;
    w.write_all(&0u64.to_le_bytes())?;

    let mut offset = 0u32;
    for word in word_table {
        w.write_all(&offset.to_le_bytes())?;
        offset += word.len() as u32;
    }
    w.write_all(&offset.to_le_bytes())?;
    for word in word_table {
        w.write_all(word.as_bytes())?;
    }

    offset = 0;
    for path in path_table {
        w.write_all(&offset.to_le_bytes())?;
        offset += path.len() as u32;
    }
    w.write_all(&offset.to_le_bytes())?;
    for path in path_table {
        w.write_all(path.as_bytes())?;
    }

    w.flush()?;
    Ok(())
}

// ── WordIndex ─────────────────────────────────────────────────────────────

/// On-disk word index backed by three files.
pub struct WordIndex {
    index_dir: PathBuf,
    word_dir: WordDirectory,
    /// file_id → (occ_offset, occ_count) in files.v2.bin
    file_table: Vec<(u64, u32)>,
    /// file_id → path string
    path_table: Vec<String>,
    /// word_id → word string (used for resolving query results)
    word_table: Vec<String>,
}

impl WordIndex {
    /// Build a new word index from a builder, writing to `dir`.
    pub fn build(builder: WordIndexBuilder, dir: &Path) -> io::Result<Self> {
        let total = builder.entry_count();
        let (word_dir, word_table, path_table) = builder.build(dir)?;

        // Read back the file table we just wrote
        let file_table = Self::load_file_table(&dir.join("files.v2.bin"))?;

        tracing::info!(
            "Word index built: {} entries, {} unique words, dir memory ~{} KB, path: {}",
            total,
            word_dir.len(),
            word_dir.memory_usage() / 1024,
            dir.display(),
        );

        Ok(Self {
            index_dir: dir.to_path_buf(),
            word_dir,
            file_table,
            path_table,
            word_table,
        })
    }

    /// Load an existing word index from disk.
    pub fn load(dir: &Path) -> io::Result<Self> {
        let (word_table, path_table) = Self::load_words_file(&dir.join("words.v2.bin"))?;
        let file_table = Self::load_file_table(&dir.join("files.v2.bin"))?;
        let word_dir = Self::load_index_file(&dir.join("index.v2.bin"), &word_table)?;

        Ok(Self {
            index_dir: dir.to_path_buf(),
            word_dir,
            file_table,
            path_table,
            word_table,
        })
    }

    fn load_words_file(path: &Path) -> io::Result<(Vec<String>, Vec<String>)> {
        let mut r = BufReader::new(File::open(path)?);
        let mut magic = [0u8; 8];
        r.read_exact(&mut magic)?;
        if magic != MAGIC_WORDS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid words magic",
            ));
        }
        let mut buf4 = [0u8; 4];
        r.read_exact(&mut buf4)?;
        let word_count = u32::from_le_bytes(buf4) as usize;
        r.read_exact(&mut buf4)?;
        let path_count = u32::from_le_bytes(buf4) as usize;
        let mut buf8 = [0u8; 8];
        r.read_exact(&mut buf8)?; // reserved

        // Read word offsets + data
        let word_table = Self::read_string_table(&mut r, word_count)?;
        let path_table = Self::read_string_table(&mut r, path_count)?;

        Ok((word_table, path_table))
    }

    fn read_string_table<R: Read>(r: &mut R, count: usize) -> io::Result<Vec<String>> {
        let mut buf4 = [0u8; 4];
        let mut offsets = Vec::with_capacity(count + 1);
        for _ in 0..=count {
            r.read_exact(&mut buf4)?;
            offsets.push(u32::from_le_bytes(buf4));
        }
        let total_len = *offsets.last().unwrap() as usize;
        let mut data = vec![0u8; total_len];
        r.read_exact(&mut data)?;

        let mut table = Vec::with_capacity(count);
        for i in 0..count {
            let start = offsets[i] as usize;
            let end = offsets[i + 1] as usize;
            let s = String::from_utf8(data[start..end].to_vec())
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            table.push(s);
        }
        Ok(table)
    }

    fn load_file_table(path: &Path) -> io::Result<Vec<(u64, u32)>> {
        let mut r = BufReader::new(File::open(path)?);
        let mut magic = [0u8; 8];
        r.read_exact(&mut magic)?;
        if magic != MAGIC_FILES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid files magic",
            ));
        }
        let mut buf4 = [0u8; 4];
        r.read_exact(&mut buf4)?;
        let file_count = u32::from_le_bytes(buf4) as usize;
        r.read_exact(&mut buf4)?; // reserved

        let mut table = Vec::with_capacity(file_count);
        let mut buf8 = [0u8; 8];
        for _ in 0..file_count {
            r.read_exact(&mut buf8)?;
            let occ_offset = u64::from_le_bytes(buf8);
            r.read_exact(&mut buf4)?;
            let occ_count = u32::from_le_bytes(buf4);
            table.push((occ_offset, occ_count));
        }
        Ok(table)
    }

    fn load_index_file(path: &Path, word_table: &[String]) -> io::Result<WordDirectory> {
        let mut r = BufReader::new(File::open(path)?);
        let mut magic = [0u8; 8];
        r.read_exact(&mut magic)?;
        if magic != MAGIC_INDEX {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid index magic",
            ));
        }
        let mut buf4 = [0u8; 4];
        r.read_exact(&mut buf4)?;
        let word_count = u32::from_le_bytes(buf4) as usize;
        r.read_exact(&mut buf4)?; // reserved

        // Read directory entries (posting_offset, posting_count) per word_id
        let mut raw_dir: Vec<(u32, u32)> = Vec::with_capacity(word_count);
        for _ in 0..word_count {
            let mut a = [0u8; 4];
            let mut b = [0u8; 4];
            r.read_exact(&mut a)?;
            r.read_exact(&mut b)?;
            raw_dir.push((u32::from_le_bytes(a), u32::from_le_bytes(b)));
        }

        // Build sorted WordDirectory from word_table + raw_dir
        // raw_dir is indexed by word_id (insertion order), but WordDirectory
        // needs entries in sorted word order.
        let mut sorted_ids: Vec<u32> = (0..word_count as u32).collect();
        sorted_ids.sort_unstable_by(|&a, &b| word_table[a as usize].cmp(&word_table[b as usize]));

        let mut word_dir = WordDirectory::new();
        word_dir.entries.reserve(word_count);
        for &word_id in &sorted_ids {
            let (posting_offset, posting_count) = raw_dir[word_id as usize];
            word_dir.insert(&word_table[word_id as usize], posting_offset, posting_count);
        }

        Ok(word_dir)
    }

    /// Find all occurrences of a word in the index.
    pub fn find_references(&self, word: &str) -> io::Result<Vec<IndexEntry>> {
        let (posting_offset, posting_count) = match self.word_dir.get(word) {
            Some(entry) => entry,
            None => return Ok(Vec::new()),
        };

        if posting_count == 0 {
            return Ok(Vec::new());
        }

        // Read posting list (file_ids) from index.v2.bin
        let index_path = self.index_dir.join("index.v2.bin");
        let mut index_file = File::open(&index_path)?;
        // Posting data starts after header (16) + word_dir (word_count * 8)
        let posting_data_offset = 16 + self.word_dir.len() as u64 * 8;
        index_file.seek(SeekFrom::Start(
            posting_data_offset + posting_offset as u64 * 4,
        ))?;
        let mut reader = BufReader::new(index_file);
        let mut file_ids = Vec::with_capacity(posting_count as usize);
        let mut buf4 = [0u8; 4];
        for _ in 0..posting_count {
            reader.read_exact(&mut buf4)?;
            file_ids.push(u32::from_le_bytes(buf4));
        }

        // For each file, binary search its occurrences for this word
        let files_path = self.index_dir.join("files.v2.bin");
        let mut files_file = BufReader::new(File::open(&files_path)?);

        // We need the word_id to search occurrences. Find it via binary search
        // in the word directory — we already found the word, now we need its id
        // to match against the occurrence word_id field.
        // Since we have word_table, just scan for the word. For large tables
        // this could be optimized with a reverse map, but it's only done once per query.
        let word_id = self
            .word_table
            .iter()
            .position(|w| w == word)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "word not in table"))?
            as u32;

        let mut results = Vec::new();
        for &file_id in &file_ids {
            let (occ_offset, occ_count) = self.file_table[file_id as usize];
            if occ_count == 0 {
                continue;
            }

            // Read this file's occurrences and binary search for word_id
            files_file.seek(SeekFrom::Start(occ_offset))?;
            let path = PathBuf::from(&self.path_table[file_id as usize]);

            // Read all occurrences for this file (14 bytes each)
            // Binary search for the start of word_id entries
            let mut occs = Vec::with_capacity(occ_count as usize);
            for _ in 0..occ_count {
                let mut buf = [0u8; OCC_SIZE];
                files_file.read_exact(&mut buf)?;
                let wid = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
                let line = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
                let col = u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]);
                let len = u16::from_le_bytes([buf[12], buf[13]]);
                occs.push((wid, line, col, len));
            }

            // Since sorted by word_id, find the range via binary search
            let start = occs.partition_point(|o| o.0 < word_id);
            for &(wid, line, col, len) in &occs[start..] {
                if wid != word_id {
                    break;
                }
                results.push(IndexEntry {
                    word: word.to_string(),
                    path: path.clone(),
                    line,
                    col,
                    len,
                });
            }
        }

        Ok(results)
    }

    pub fn word_dir(&self) -> &WordDirectory {
        &self.word_dir
    }
    pub fn index_path(&self) -> &Path {
        &self.index_dir
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_and_query_index() {
        let dir = tempfile::tempdir().unwrap();

        let mut builder = WordIndexBuilder::new();
        builder.add(IndexEntry {
            word: "foo".to_string(),
            path: PathBuf::from("/src/main.rs"),
            line: 0,
            col: 3,
            len: 3,
        });
        builder.add(IndexEntry {
            word: "foo".to_string(),
            path: PathBuf::from("/src/lib.rs"),
            line: 5,
            col: 10,
            len: 3,
        });
        builder.add(IndexEntry {
            word: "bar".to_string(),
            path: PathBuf::from("/src/main.rs"),
            line: 2,
            col: 0,
            len: 3,
        });

        let index = WordIndex::build(builder, dir.path()).unwrap();

        let refs = index.find_references("foo").unwrap();
        assert_eq!(refs.len(), 2);
        // Results should include both files
        let paths: Vec<_> = refs.iter().map(|r| r.path.to_str().unwrap()).collect();
        assert!(paths.contains(&"/src/lib.rs"));
        assert!(paths.contains(&"/src/main.rs"));

        let refs = index.find_references("bar").unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].line, 2);

        let refs = index.find_references("baz").unwrap();
        assert!(refs.is_empty());

        // Reload from disk
        let loaded = WordIndex::load(dir.path()).unwrap();
        let refs = loaded.find_references("foo").unwrap();
        assert_eq!(refs.len(), 2);
    }

    #[test]
    fn empty_index() {
        let dir = tempfile::tempdir().unwrap();

        let builder = WordIndexBuilder::new();
        let index = WordIndex::build(builder, dir.path()).unwrap();

        assert!(index.word_dir().is_empty());
        let refs = index.find_references("anything").unwrap();
        assert!(refs.is_empty());
    }

    #[test]
    fn unicode_words() {
        let dir = tempfile::tempdir().unwrap();

        let mut builder = WordIndexBuilder::new();
        builder.add(IndexEntry {
            word: "über_config".to_string(),
            path: PathBuf::from("/src/main.rs"),
            line: 0,
            col: 3,
            len: 13,
        });

        let index = WordIndex::build(builder, dir.path()).unwrap();
        let refs = index.find_references("über_config").unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].len, 13);

        let loaded = WordIndex::load(dir.path()).unwrap();
        let refs = loaded.find_references("über_config").unwrap();
        assert_eq!(refs.len(), 1);
    }

    #[test]
    fn many_files_many_words() {
        let dir = tempfile::tempdir().unwrap();

        let mut builder = WordIndexBuilder::new();
        for i in 0..100 {
            let path = PathBuf::from(format!("/src/file_{}.rs", i));
            builder.add(IndexEntry {
                word: "shared".to_string(),
                path: path.clone(),
                line: i,
                col: 0,
                len: 6,
            });
            builder.add(IndexEntry {
                word: format!("unique_{}", i),
                path,
                line: i + 1,
                col: 0,
                len: 8,
            });
        }

        let index = WordIndex::build(builder, dir.path()).unwrap();

        let refs = index.find_references("shared").unwrap();
        assert_eq!(refs.len(), 100);

        let refs = index.find_references("unique_42").unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].line, 43);

        let refs = index.find_references("nonexistent").unwrap();
        assert!(refs.is_empty());

        // Reload
        let loaded = WordIndex::load(dir.path()).unwrap();
        let refs = loaded.find_references("shared").unwrap();
        assert_eq!(refs.len(), 100);
    }
}
