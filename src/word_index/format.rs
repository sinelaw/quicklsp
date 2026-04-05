//! Word index file format and I/O.
//!
//! ## File layout
//!
//! ```text
//! [header]                    (32 bytes, fixed)
//!   magic: b"QLSP\x01\x00\x00\x00"  (8 bytes)
//!   entry_count: u64
//!   dir_offset: u64           byte offset to word directory
//!   dir_count: u64            number of words in directory
//!
//! [entries section]           (sorted by word, then path, then line)
//!   Each entry is variable-length:
//!     word_len: u16
//!     word: [u8; word_len]
//!     path_len: u16
//!     path: [u8; path_len]
//!     line: u32
//!     col: u32
//!     len: u16
//!
//! [word directory]            (sorted by word, variable-size entries)
//!   Each entry:
//!     word_len: u16
//!     word: [u8; word_len]
//!     first_entry_offset: u64
//!     entry_count: u32
//! ```

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const MAGIC: [u8; 8] = *b"QLSP\x01\x00\x00\x00";
const HEADER_SIZE: u64 = 32;

/// Size of a CompactEntry on disk in the sorted run files.
const COMPACT_ENTRY_SIZE: usize = std::mem::size_of::<CompactEntry>();

/// A single entry in the word index (used for reading / public API).
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
///
/// repr(C) ensures a deterministic byte layout for raw read/write to temp files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
struct CompactEntry {
    word_id: u32,
    path_id: u32,
    line: u32,
    col: u32,
    len: u16,
    /// Padding to make the struct exactly 20 bytes with predictable layout.
    _pad: u16,
}

// Verify the size at compile time.
const _: () = assert!(COMPACT_ENTRY_SIZE == 20);

impl CompactEntry {
    fn new(word_id: u32, path_id: u32, line: u32, col: u32, len: u16) -> Self {
        Self { word_id, path_id, line, col, len, _pad: 0 }
    }

    /// Read from raw bytes. Returns None on EOF.
    fn read_from<R: Read>(reader: &mut R) -> io::Result<Option<Self>> {
        let mut buf = [0u8; COMPACT_ENTRY_SIZE];
        match reader.read_exact(&mut buf) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e),
        }
        Ok(Some(Self {
            word_id: u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]),
            path_id: u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]),
            line: u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]),
            col: u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]),
            len: u16::from_le_bytes([buf[16], buf[17]]),
            _pad: 0,
        }))
    }

    /// Write to raw bytes.
    fn write_to<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        writer.write_all(&self.word_id.to_le_bytes())?;
        writer.write_all(&self.path_id.to_le_bytes())?;
        writer.write_all(&self.line.to_le_bytes())?;
        writer.write_all(&self.col.to_le_bytes())?;
        writer.write_all(&self.len.to_le_bytes())?;
        writer.write_all(&self._pad.to_le_bytes())?;
        Ok(())
    }
}

/// In-memory word directory: maps word → (file_offset, entry_count).
#[derive(Debug, Default)]
pub struct WordDirectory {
    entries: BTreeMap<String, (u64, u32)>,
}

impl WordDirectory {
    pub fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }

    /// Look up a word in the directory.
    pub fn get(&self, word: &str) -> Option<(u64, u32)> {
        self.entries.get(word).copied()
    }

    /// Number of unique words in the directory.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Estimated memory usage in bytes.
    pub fn memory_usage(&self) -> usize {
        // BTreeMap overhead + per-entry: String (24 + len) + (u64, u32) = 12
        self.entries
            .iter()
            .map(|(k, _)| 24 + k.len() + 12 + 48) // 48 for BTreeMap node overhead
            .sum()
    }
}

/// A sorted run of CompactEntry records stored in a temp file.
pub struct SortedRun {
    file: tempfile::NamedTempFile,
}

/// Builder for constructing a word index file.
///
/// Uses compact interned entries internally to minimize memory:
/// words and paths are stored once in lookup tables, and each entry
/// is a 20-byte struct with table indices instead of heap Strings.
///
/// For large indexes, entries are flushed to sorted temp-file runs
/// after each batch and merged during the final build step. This
/// keeps peak memory bounded to one batch's worth of entries (~25 MB)
/// instead of the full dataset (~200+ MB).
pub struct WordIndexBuilder {
    entries: Vec<CompactEntry>,
    /// Sorted runs flushed to temp files.
    runs: Vec<SortedRun>,
    /// Total entries across all flushed runs (not counting self.entries).
    flushed_entry_count: usize,
    /// Interned paths: id → PathBuf.
    path_table: Vec<PathBuf>,
    /// Reverse lookup: PathBuf → id.
    path_lookup: std::collections::HashMap<PathBuf, u32>,
    /// Interned words: id → String.
    word_table: Vec<String>,
    /// Reverse lookup: word → id.
    word_lookup: std::collections::HashMap<String, u32>,
}

impl WordIndexBuilder {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            runs: Vec::new(),
            flushed_entry_count: 0,
            path_table: Vec::new(),
            path_lookup: std::collections::HashMap::new(),
            word_table: Vec::new(),
            word_lookup: std::collections::HashMap::new(),
        }
    }

    /// Intern a path, returning its table index.
    fn intern_path(&mut self, path: &Path) -> u32 {
        if let Some(&idx) = self.path_lookup.get(path) {
            idx
        } else {
            let idx = self.path_table.len() as u32;
            let owned = path.to_path_buf();
            self.path_lookup.insert(owned.clone(), idx);
            self.path_table.push(owned);
            idx
        }
    }

    /// Intern a word string, returning its table index.
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

    /// Intern a word by moving the String, returning its table index.
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
        self.entries.push(CompactEntry::new(word_id, path_id, entry.line, entry.col, entry.len));
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
            let word = &source[occ.word_offset as usize..(occ.word_offset + occ.word_len as u32) as usize];
            let word_id = self.intern_word(word);
            self.entries.push(CompactEntry::new(
                word_id, path_id, occ.line as u32, occ.col as u32, occ.word_len,
            ));
        }
    }

    /// Drain entries from a file's occurrences, resolving byte offsets against `source`.
    /// The occurrence Vec is consumed to free its memory.
    pub fn drain_file_occurrences(
        &mut self,
        path: &Path,
        occurrences: Vec<crate::parsing::tokenizer::Occurrence>,
        source: &str,
    ) {
        let path_id = self.intern_path(path);
        self.entries.reserve(occurrences.len());
        for occ in occurrences {
            let word = &source[occ.word_offset as usize..(occ.word_offset + occ.word_len as u32) as usize];
            let word_id = self.intern_word(word);
            self.entries.push(CompactEntry::new(
                word_id, path_id, occ.line as u32, occ.col as u32, occ.word_len,
            ));
        }
    }

    /// Total number of entries across all runs and the current buffer.
    pub fn entry_count(&self) -> usize {
        self.flushed_entry_count + self.entries.len()
    }

    /// Number of entries currently in the in-memory buffer (not yet flushed).
    pub fn entries_in_buffer(&self) -> usize {
        self.entries.len()
    }

    /// Sort the current in-memory entries and flush them to a temp file.
    /// Frees the in-memory Vec for reuse by the next batch.
    ///
    /// Call this after each batch's `drain_file_occurrences` to keep
    /// peak memory bounded to one batch.
    pub fn flush_sorted_run(&mut self) -> io::Result<()> {
        if self.entries.is_empty() {
            return Ok(());
        }

        // Compare strings directly instead of building sort-order maps over
        // the entire (and ever-growing) intern tables.  The sort-order
        // approach is O(W log W) setup where W = total accumulated words,
        // which dominates when W >> batch size.
        let word_table = &self.word_table;
        let path_table = &self.path_table;
        self.entries.sort_unstable_by(|a, b| {
            word_table[a.word_id as usize]
                .cmp(&word_table[b.word_id as usize])
                .then_with(|| {
                    path_table[a.path_id as usize].cmp(&path_table[b.path_id as usize])
                })
                .then_with(|| a.line.cmp(&b.line))
        });

        let mut temp = tempfile::NamedTempFile::new()?;
        {
            let mut writer = BufWriter::new(&mut temp);
            for entry in &self.entries {
                entry.write_to(&mut writer)?;
            }
            writer.flush()?;
        }

        let count = self.entries.len();
        self.entries.clear();
        self.flushed_entry_count += count;
        self.runs.push(SortedRun { file: temp });

        Ok(())
    }

    /// Sort entries and write the index file. Returns the word directory.
    ///
    /// If sorted runs have been flushed, performs a K-way merge from the
    /// run files. Otherwise, sorts and writes in memory (for small indexes).
    pub fn build(mut self, path: &Path) -> io::Result<WordDirectory> {
        // If there are unflushed entries remaining, flush them as a final run.
        if !self.entries.is_empty() && !self.runs.is_empty() {
            self.flush_sorted_run()?;
        }

        if self.runs.is_empty() {
            // In-memory path: single sort + sequential write. Used when
            // entries were accumulated without flushing sorted runs.
            self.build_in_memory(path)
        } else {
            // Large index path: K-way merge from sorted runs
            // Free the lookup HashMaps before merge — they're only needed
            // during the accumulation phase. Frees ~50 MB.
            self.word_lookup = std::collections::HashMap::new();
            self.path_lookup = std::collections::HashMap::new();
            self.build_from_runs(path)
        }
    }

    /// In-memory sort and write — single sort over all accumulated entries.
    fn build_in_memory(mut self, path: &Path) -> io::Result<WordDirectory> {
        // Free lookup HashMaps before sort — only needed during accumulation.
        self.word_lookup = std::collections::HashMap::new();
        self.path_lookup = std::collections::HashMap::new();

        let word_sort_order = Self::build_sort_order(&self.word_table);
        let path_sort_order = Self::build_sort_order_path(&self.path_table);

        self.entries.sort_unstable_by(|a, b| {
            Self::compare_entries(a, b, &word_sort_order, &path_sort_order)
        });

        let total_entries = self.entries.len();
        let mut index_writer = IndexFileWriter::new(path)?;

        for entry in &self.entries {
            let word = &self.word_table[entry.word_id as usize];
            let path_str = &self.path_table[entry.path_id as usize];
            index_writer.write_entry(word, path_str, entry)?;
        }

        index_writer.finish(total_entries)
    }

    /// K-way merge from sorted run files.
    fn build_from_runs(mut self, path: &Path) -> io::Result<WordDirectory> {
        let word_sort_order = Self::build_sort_order(&self.word_table);
        let path_sort_order = Self::build_sort_order_path(&self.path_table);

        let total_entries = self.flushed_entry_count;

        // Open a BufReader on each run, seed the heap with the first entry.
        let mut readers: Vec<BufReader<File>> = Vec::with_capacity(self.runs.len());
        for run in &mut self.runs {
            let file = run.file.as_file_mut();
            file.seek(SeekFrom::Start(0))?;
            readers.push(BufReader::new(file.try_clone()?));
        }

        // Min-heap for K-way merge.
        let mut heap: BinaryHeap<Reverse<MergeHead>> = BinaryHeap::with_capacity(readers.len());
        for (i, reader) in readers.iter_mut().enumerate() {
            if let Some(entry) = CompactEntry::read_from(reader)? {
                heap.push(Reverse(MergeHead {
                    sort_key: sort_key_for(&entry, &word_sort_order, &path_sort_order),
                    entry,
                    run_index: i,
                }));
            }
        }

        let mut index_writer = IndexFileWriter::new(path)?;

        while let Some(Reverse(head)) = heap.pop() {
            let word = &self.word_table[head.entry.word_id as usize];
            let path_str = &self.path_table[head.entry.path_id as usize];
            index_writer.write_entry(word, path_str, &head.entry)?;

            // Read next entry from the same run.
            if let Some(next) = CompactEntry::read_from(&mut readers[head.run_index])? {
                heap.push(Reverse(MergeHead {
                    sort_key: sort_key_for(&next, &word_sort_order, &path_sort_order),
                    entry: next,
                    run_index: head.run_index,
                }));
            }
        }

        // Runs are dropped here, deleting temp files.
        drop(self.runs);

        index_writer.finish(total_entries)
    }

    /// Compare two CompactEntries by (word, path, line) using sort-rank mappings.
    fn compare_entries(
        a: &CompactEntry,
        b: &CompactEntry,
        word_order: &[u32],
        path_order: &[u32],
    ) -> std::cmp::Ordering {
        word_order[a.word_id as usize]
            .cmp(&word_order[b.word_id as usize])
            .then_with(|| path_order[a.path_id as usize].cmp(&path_order[b.path_id as usize]))
            .then_with(|| a.line.cmp(&b.line))
    }

    /// Build a sort-order mapping: table_index → sort_rank.
    fn build_sort_order(table: &[String]) -> Vec<u32> {
        let mut indices: Vec<u32> = (0..table.len() as u32).collect();
        indices.sort_unstable_by(|&a, &b| table[a as usize].cmp(&table[b as usize]));
        let mut order = vec![0u32; table.len()];
        for (rank, &idx) in indices.iter().enumerate() {
            order[idx as usize] = rank as u32;
        }
        order
    }

    /// Build a sort-order mapping for PathBuf table.
    fn build_sort_order_path(table: &[PathBuf]) -> Vec<u32> {
        let mut indices: Vec<u32> = (0..table.len() as u32).collect();
        indices.sort_unstable_by(|&a, &b| table[a as usize].cmp(&table[b as usize]));
        let mut order = vec![0u32; table.len()];
        for (rank, &idx) in indices.iter().enumerate() {
            order[idx as usize] = rank as u32;
        }
        order
    }
}

impl Default for WordIndexBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ── Merge heap entry ────────────────────────────────────────────────────

/// Sort key for the merge heap: (word_rank, path_rank, line).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct SortKey(u32, u32, u32);

fn sort_key_for(entry: &CompactEntry, word_order: &[u32], path_order: &[u32]) -> SortKey {
    SortKey(
        word_order[entry.word_id as usize],
        path_order[entry.path_id as usize],
        entry.line,
    )
}

/// Entry in the K-way merge heap. Ordered by sort key.
#[derive(Debug, Eq, PartialEq)]
struct MergeHead {
    sort_key: SortKey,
    entry: CompactEntry,
    run_index: usize,
}

impl Ord for MergeHead {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.sort_key.cmp(&other.sort_key)
    }
}

impl PartialOrd for MergeHead {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

// ── Index file writer ──────────────────────────────────────────────────

/// Helper that writes the final index file format, tracking word boundaries.
struct IndexFileWriter {
    writer: BufWriter<File>,
    dir: WordDirectory,
    current_word: Option<String>,
    word_start_offset: u64,
    word_entry_count: u32,
    offset: u64,
}

impl IndexFileWriter {
    fn new(path: &Path) -> io::Result<Self> {
        let file = File::create(path)?;
        let mut writer = BufWriter::new(file);

        // Write placeholder header.
        let header_buf = [0u8; HEADER_SIZE as usize];
        writer.write_all(&header_buf)?;

        Ok(Self {
            writer,
            dir: WordDirectory::new(),
            current_word: None,
            word_start_offset: HEADER_SIZE,
            word_entry_count: 0,
            offset: HEADER_SIZE,
        })
    }

    fn write_entry(
        &mut self,
        word: &str,
        path: &Path,
        entry: &CompactEntry,
    ) -> io::Result<()> {
        if self.current_word.as_deref() != Some(word) {
            // Flush previous word.
            if let Some(ref prev_word) = self.current_word {
                self.dir.entries.insert(
                    prev_word.clone(),
                    (self.word_start_offset, self.word_entry_count),
                );
            }
            self.current_word = Some(word.to_string());
            self.word_start_offset = self.offset;
            self.word_entry_count = 0;
        }

        let entry_size = write_resolved_entry(&mut self.writer, word, path, entry)?;
        self.offset += entry_size as u64;
        self.word_entry_count += 1;
        Ok(())
    }

    fn finish(mut self, total_entries: usize) -> io::Result<WordDirectory> {
        // Flush last word.
        if let Some(ref prev_word) = self.current_word {
            self.dir.entries.insert(
                prev_word.clone(),
                (self.word_start_offset, self.word_entry_count),
            );
        }

        let dir_offset = self.offset;
        let dir_count = self.dir.entries.len() as u64;

        // Write word directory.
        for (word, (first_offset, count)) in &self.dir.entries {
            let word_bytes = word.as_bytes();
            self.writer.write_all(&(word_bytes.len() as u16).to_le_bytes())?;
            self.writer.write_all(word_bytes)?;
            self.writer.write_all(&first_offset.to_le_bytes())?;
            self.writer.write_all(&(*count).to_le_bytes())?;
        }

        self.writer.flush()?;

        // Write real header.
        let mut file = self.writer.into_inner()?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&MAGIC)?;
        file.write_all(&(total_entries as u64).to_le_bytes())?;
        file.write_all(&dir_offset.to_le_bytes())?;
        file.write_all(&dir_count.to_le_bytes())?;
        file.flush()?;

        Ok(self.dir)
    }
}

// ─��� Entry I/O ──────────────────────────────────────────────────────────

/// Write a resolved compact entry to the writer, returning bytes written.
fn write_resolved_entry<W: Write>(
    writer: &mut W,
    word: &str,
    path: &Path,
    entry: &CompactEntry,
) -> io::Result<usize> {
    let word_bytes = word.as_bytes();
    let path_bytes = path.to_string_lossy();
    let path_bytes = path_bytes.as_bytes();

    let mut written = 0;
    writer.write_all(&(word_bytes.len() as u16).to_le_bytes())?;
    written += 2;
    writer.write_all(word_bytes)?;
    written += word_bytes.len();
    writer.write_all(&(path_bytes.len() as u16).to_le_bytes())?;
    written += 2;
    writer.write_all(path_bytes)?;
    written += path_bytes.len();
    writer.write_all(&entry.line.to_le_bytes())?;
    written += 4;
    writer.write_all(&entry.col.to_le_bytes())?;
    written += 4;
    writer.write_all(&entry.len.to_le_bytes())?;
    written += 2;

    Ok(written)
}

/// Read a single entry from the reader.
fn read_entry<R: Read>(reader: &mut R) -> io::Result<IndexEntry> {
    let mut buf2 = [0u8; 2];
    let mut buf4 = [0u8; 4];

    reader.read_exact(&mut buf2)?;
    let word_len = u16::from_le_bytes(buf2) as usize;
    let mut word_buf = vec![0u8; word_len];
    reader.read_exact(&mut word_buf)?;
    let word = String::from_utf8(word_buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    reader.read_exact(&mut buf2)?;
    let path_len = u16::from_le_bytes(buf2) as usize;
    let mut path_buf = vec![0u8; path_len];
    reader.read_exact(&mut path_buf)?;
    let path_str = String::from_utf8(path_buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let path = PathBuf::from(path_str);

    reader.read_exact(&mut buf4)?;
    let line = u32::from_le_bytes(buf4);

    reader.read_exact(&mut buf4)?;
    let col = u32::from_le_bytes(buf4);

    reader.read_exact(&mut buf2)?;
    let len = u16::from_le_bytes(buf2);

    Ok(IndexEntry {
        word,
        path,
        line,
        col,
        len,
    })
}

// ── WordIndex ──────────────────────────────────────────────────────────

/// On-disk word index with in-memory directory for seek-based lookups.
pub struct WordIndex {
    index_path: PathBuf,
    word_dir: WordDirectory,
}

impl WordIndex {
    /// Build a new word index from a builder and store at the given path.
    pub fn build(builder: WordIndexBuilder, path: &Path) -> io::Result<Self> {
        let word_dir = builder.build(path)?;
        Ok(Self {
            index_path: path.to_path_buf(),
            word_dir,
        })
    }

    /// Load an existing word index from disk.
    pub fn load(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);

        // Read header
        let mut magic = [0u8; 8];
        reader.read_exact(&mut magic)?;
        if magic != MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid word index magic",
            ));
        }

        let mut buf8 = [0u8; 8];
        reader.read_exact(&mut buf8)?;
        let _entry_count = u64::from_le_bytes(buf8);

        reader.read_exact(&mut buf8)?;
        let dir_offset = u64::from_le_bytes(buf8);

        reader.read_exact(&mut buf8)?;
        let dir_count = u64::from_le_bytes(buf8);

        // Seek to word directory and read it
        reader.seek(SeekFrom::Start(dir_offset))?;
        let mut word_dir = WordDirectory::new();

        for _ in 0..dir_count {
            let mut buf2 = [0u8; 2];
            let mut buf4 = [0u8; 4];

            reader.read_exact(&mut buf2)?;
            let word_len = u16::from_le_bytes(buf2) as usize;
            let mut word_buf = vec![0u8; word_len];
            reader.read_exact(&mut word_buf)?;
            let word = String::from_utf8(word_buf)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

            reader.read_exact(&mut buf8)?;
            let first_offset = u64::from_le_bytes(buf8);

            reader.read_exact(&mut buf4)?;
            let count = u32::from_le_bytes(buf4);

            word_dir.entries.insert(word, (first_offset, count));
        }

        Ok(Self {
            index_path: path.to_path_buf(),
            word_dir,
        })
    }

    /// Find all occurrences of a word in the index.
    pub fn find_references(&self, word: &str) -> io::Result<Vec<IndexEntry>> {
        let (offset, count) = match self.word_dir.get(word) {
            Some(entry) => entry,
            None => return Ok(Vec::new()),
        };

        let mut file = File::open(&self.index_path)?;
        file.seek(SeekFrom::Start(offset))?;
        let mut reader = BufReader::new(file);

        let mut refs = Vec::with_capacity(count as usize);
        for _ in 0..count {
            refs.push(read_entry(&mut reader)?);
        }
        Ok(refs)
    }

    /// Get the word directory (for memory stats).
    pub fn word_dir(&self) -> &WordDirectory {
        &self.word_dir
    }

    /// Path to the index file on disk.
    pub fn index_path(&self) -> &Path {
        &self.index_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_and_query_index() {
        let dir = tempfile::tempdir().unwrap();
        let index_path = dir.path().join("test.qlsp");

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

        let index = WordIndex::build(builder, &index_path).unwrap();

        // Query foo
        let refs = index.find_references("foo").unwrap();
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].path, PathBuf::from("/src/lib.rs"));
        assert_eq!(refs[1].path, PathBuf::from("/src/main.rs"));

        // Query bar
        let refs = index.find_references("bar").unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].line, 2);

        // Query nonexistent
        let refs = index.find_references("baz").unwrap();
        assert!(refs.is_empty());

        // Reload from disk
        let loaded = WordIndex::load(&index_path).unwrap();
        let refs = loaded.find_references("foo").unwrap();
        assert_eq!(refs.len(), 2);
    }

    #[test]
    fn empty_index() {
        let dir = tempfile::tempdir().unwrap();
        let index_path = dir.path().join("empty.qlsp");

        let builder = WordIndexBuilder::new();
        let index = WordIndex::build(builder, &index_path).unwrap();

        assert!(index.word_dir().is_empty());
        let refs = index.find_references("anything").unwrap();
        assert!(refs.is_empty());
    }

    #[test]
    fn unicode_words() {
        let dir = tempfile::tempdir().unwrap();
        let index_path = dir.path().join("unicode.qlsp");

        let mut builder = WordIndexBuilder::new();
        builder.add(IndexEntry {
            word: "über_config".to_string(),
            path: PathBuf::from("/src/main.rs"),
            line: 0,
            col: 3,
            len: 13, // UTF-8 byte length
        });

        let index = WordIndex::build(builder, &index_path).unwrap();
        let refs = index.find_references("über_config").unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].word, "über_config");
    }

    #[test]
    fn merge_multiple_sorted_runs() {
        // Verify that flushing runs and merging produces the same result
        // as the in-memory path.
        let dir = tempfile::tempdir().unwrap();

        // Build with runs (external merge path)
        let merge_path = dir.path().join("merged.qlsp");
        let mut builder = WordIndexBuilder::new();

        // Run 1: entries for "foo" and "bar"
        builder.add(IndexEntry {
            word: "foo".to_string(), path: PathBuf::from("/a.rs"), line: 1, col: 0, len: 3,
        });
        builder.add(IndexEntry {
            word: "bar".to_string(), path: PathBuf::from("/a.rs"), line: 2, col: 0, len: 3,
        });
        builder.flush_sorted_run().unwrap();

        // Run 2: more entries for "foo" and a new word "zap"
        builder.add(IndexEntry {
            word: "foo".to_string(), path: PathBuf::from("/b.rs"), line: 5, col: 0, len: 3,
        });
        builder.add(IndexEntry {
            word: "zap".to_string(), path: PathBuf::from("/b.rs"), line: 6, col: 0, len: 3,
        });
        builder.flush_sorted_run().unwrap();

        // Run 3: bar again in a different file
        builder.add(IndexEntry {
            word: "bar".to_string(), path: PathBuf::from("/c.rs"), line: 10, col: 0, len: 3,
        });
        builder.flush_sorted_run().unwrap();

        assert_eq!(builder.entry_count(), 5);
        let merged = WordIndex::build(builder, &merge_path).unwrap();

        // Verify results
        let foo_refs = merged.find_references("foo").unwrap();
        assert_eq!(foo_refs.len(), 2);
        assert_eq!(foo_refs[0].path, PathBuf::from("/a.rs"));
        assert_eq!(foo_refs[1].path, PathBuf::from("/b.rs"));

        let bar_refs = merged.find_references("bar").unwrap();
        assert_eq!(bar_refs.len(), 2);
        assert_eq!(bar_refs[0].path, PathBuf::from("/a.rs"));
        assert_eq!(bar_refs[1].path, PathBuf::from("/c.rs"));

        let zap_refs = merged.find_references("zap").unwrap();
        assert_eq!(zap_refs.len(), 1);
        assert_eq!(zap_refs[0].path, PathBuf::from("/b.rs"));

        // Reload from disk to verify persistence
        let reloaded = WordIndex::load(&merge_path).unwrap();
        assert_eq!(reloaded.find_references("foo").unwrap().len(), 2);
        assert_eq!(reloaded.find_references("bar").unwrap().len(), 2);
        assert_eq!(reloaded.find_references("zap").unwrap().len(), 1);
        assert!(reloaded.find_references("missing").unwrap().is_empty());
    }
}
