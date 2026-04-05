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

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const MAGIC: [u8; 8] = *b"QLSP\x01\x00\x00\x00";
const HEADER_SIZE: u64 = 32;

/// A single entry in the word index.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct IndexEntry {
    pub word: String,
    pub path: PathBuf,
    pub line: u32,
    pub col: u32,
    pub len: u16,
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

/// Builder for constructing a word index file.
pub struct WordIndexBuilder {
    entries: Vec<IndexEntry>,
    /// Interned paths: map from path to index, and the reverse list.
    path_table: Vec<PathBuf>,
    path_lookup: std::collections::HashMap<PathBuf, u32>,
}

impl WordIndexBuilder {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            path_table: Vec::new(),
            path_lookup: std::collections::HashMap::new(),
        }
    }

    /// Intern a path, returning a cheap-to-clone PathBuf (shared via the table).
    fn intern_path(&mut self, path: &Path) -> PathBuf {
        if let Some(&idx) = self.path_lookup.get(path) {
            self.path_table[idx as usize].clone()
        } else {
            let idx = self.path_table.len() as u32;
            let owned = path.to_path_buf();
            self.path_lookup.insert(owned.clone(), idx);
            self.path_table.push(owned.clone());
            owned
        }
    }

    /// Add an entry to the builder.
    pub fn add(&mut self, entry: IndexEntry) {
        self.entries.push(entry);
    }

    /// Add entries from a file's occurrences (borrowed — clones strings).
    pub fn add_file_occurrences(
        &mut self,
        path: &Path,
        occurrences: &[crate::parsing::tokenizer::Occurrence],
    ) {
        let interned = self.intern_path(path);
        for occ in occurrences {
            self.entries.push(IndexEntry {
                word: occ.word.clone(),
                path: interned.clone(),
                line: occ.line as u32,
                col: occ.col as u32,
                len: occ.len as u16,
            });
        }
    }

    /// Drain entries from a file's occurrences (takes ownership — moves strings).
    pub fn drain_file_occurrences(
        &mut self,
        path: &Path,
        occurrences: Vec<crate::parsing::tokenizer::Occurrence>,
    ) {
        let interned = self.intern_path(path);
        self.entries.reserve(occurrences.len());
        for occ in occurrences {
            self.entries.push(IndexEntry {
                word: occ.word, // moved, not cloned
                path: interned.clone(),
                line: occ.line as u32,
                col: occ.col as u32,
                len: occ.len as u16,
            });
        }
    }

    /// Number of entries accumulated so far.
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Sort entries and write the index file. Returns the word directory.
    pub fn build(mut self, path: &Path) -> io::Result<WordDirectory> {
        // Sort by (word, path, line)
        self.entries.sort();

        let file = File::create(path)?;
        let mut writer = BufWriter::new(file);

        // Write placeholder header (we'll fill in dir_offset later)
        let header_buf = [0u8; HEADER_SIZE as usize];
        writer.write_all(&header_buf)?;

        // Write entries section, tracking word boundaries for the directory
        let mut dir = WordDirectory::new();
        let mut current_word: Option<String> = None;
        let mut word_start_offset = HEADER_SIZE;
        let mut word_entry_count = 0u32;
        let mut offset = HEADER_SIZE;

        for entry in &self.entries {
            if current_word.as_deref() != Some(&entry.word) {
                // Flush previous word
                if let Some(ref prev_word) = current_word {
                    dir.entries
                        .insert(prev_word.clone(), (word_start_offset, word_entry_count));
                }
                current_word = Some(entry.word.clone());
                word_start_offset = offset;
                word_entry_count = 0;
            }

            let entry_size = write_entry(&mut writer, entry)?;
            offset += entry_size as u64;
            word_entry_count += 1;
        }

        // Flush last word
        if let Some(ref prev_word) = current_word {
            dir.entries
                .insert(prev_word.clone(), (word_start_offset, word_entry_count));
        }

        let dir_offset = offset;
        let dir_count = dir.entries.len() as u64;

        // Write word directory
        for (word, (first_offset, count)) in &dir.entries {
            let word_bytes = word.as_bytes();
            writer.write_all(&(word_bytes.len() as u16).to_le_bytes())?;
            writer.write_all(word_bytes)?;
            writer.write_all(&first_offset.to_le_bytes())?;
            writer.write_all(&(*count).to_le_bytes())?;
        }

        writer.flush()?;

        // Go back and write the real header
        let mut file = writer.into_inner()?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&MAGIC)?;
        file.write_all(&(self.entries.len() as u64).to_le_bytes())?;
        file.write_all(&dir_offset.to_le_bytes())?;
        file.write_all(&dir_count.to_le_bytes())?;
        file.flush()?;

        Ok(dir)
    }
}

impl Default for WordIndexBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Write a single entry to the writer, returning bytes written.
fn write_entry<W: Write>(writer: &mut W, entry: &IndexEntry) -> io::Result<usize> {
    let word_bytes = entry.word.as_bytes();
    let path_bytes = entry.path.to_string_lossy();
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
}
