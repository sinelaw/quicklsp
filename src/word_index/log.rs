//! Append-only log-based word index.
//!
//! Single file format (`index.log`) that supports both cold indexing (write all
//! files sequentially) and incremental updates (append changed files). At load
//! time, a sequential scan builds in-memory query structures.
//!
//! ## Tags
//!
//! - `0x01` define word  — assigns word_id sequentially
//! - `0x02` define path  — assigns path_id sequentially
//! - `0x03` file data    — occurrences + symbols, replaces earlier entry for path_id
//! - `0x04` file removed — drops everything for path_id

use ahash::AHashMap;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use crate::parsing::symbols::{Symbol, SymbolKind};
use crate::parsing::tokenizer::{LangFamily, Visibility};

use super::format::IndexEntry;

const LOG_MAGIC: [u8; 8] = *b"QLSL\x01\x00\x00\x00";

const TAG_WORD: u8 = 0x01;
const TAG_PATH: u8 = 0x02;
const TAG_FILE_DATA: u8 = 0x03;
const TAG_FILE_REMOVED: u8 = 0x04;

/// On-disk occurrence: word_id(4) + line(4) + col(4) + len(2) = 14 bytes.
const OCC_SIZE: usize = 14;

// ── In-memory occurrence ────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct OccEntry {
    pub word_id: u32,
    pub line: u32,
    pub col: u32,
    pub len: u16,
}

// ── Per-file data loaded from the log ───────────────────────────────────

pub struct FileData {
    pub occurrences: Vec<OccEntry>,
    pub symbols: Vec<Symbol>,
    pub lang: Option<LangFamily>,
    pub mtime: u64,
}

// ── LogIndex: the in-memory index built from scanning the log ───────────

pub struct LogIndex {
    pub index_dir: PathBuf,

    // String tables
    pub word_table: Vec<String>,
    pub word_lookup: AHashMap<String, u32>,
    pub path_table: Vec<String>,
    pub path_lookup: AHashMap<String, u32>,

    // Per-file data (last TAG 0x03 per path_id wins)
    pub files: HashMap<u32, FileData>,

    // Posting lists: word_id → [path_ids that contain it]
    pub postings: Vec<Vec<u32>>,
}

impl LogIndex {
    /// Load a log index from disk. Scans the log sequentially,
    /// building in-memory structures. Returns None if no log exists.
    pub fn load(dir: &Path) -> io::Result<Option<Self>> {
        let log_path = dir.join("index.log");
        if !log_path.exists() {
            return Ok(None);
        }

        let mut r = BufReader::with_capacity(1 << 20, File::open(&log_path)?);

        let mut magic = [0u8; 8];
        r.read_exact(&mut magic)?;
        if magic != LOG_MAGIC {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "bad log magic"));
        }
        let mut buf4 = [0u8; 4];
        r.read_exact(&mut buf4)?; // reserved

        let mut word_table = Vec::new();
        let mut word_lookup = AHashMap::new();
        let mut path_table = Vec::new();
        let mut path_lookup = AHashMap::new();
        let mut files: HashMap<u32, FileData> = HashMap::new();

        // Read entries until EOF or truncated entry.
        loop {
            let mut tag = [0u8; 1];
            match r.read_exact(&mut tag) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e),
            }

            match tag[0] {
                TAG_WORD => {
                    let word = match read_word_entry(&mut r) {
                        Ok(w) => w,
                        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                        Err(e) => return Err(e),
                    };
                    let id = word_table.len() as u32;
                    word_lookup.insert(word.clone(), id);
                    word_table.push(word);
                }
                TAG_PATH => {
                    let path = match read_path_entry(&mut r) {
                        Ok(p) => p,
                        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                        Err(e) => return Err(e),
                    };
                    let id = path_table.len() as u32;
                    path_lookup.insert(path.clone(), id);
                    path_table.push(path);
                }
                TAG_FILE_DATA => {
                    let (path_id, file_data) = match read_file_data_entry(&mut r) {
                        Ok(d) => d,
                        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                        Err(e) => return Err(e),
                    };
                    files.insert(path_id, file_data);
                }
                TAG_FILE_REMOVED => {
                    match read_u32(&mut r) {
                        Ok(path_id) => { files.remove(&path_id); }
                        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                        Err(e) => return Err(e),
                    }
                }
                _ => {
                    // Unknown tag — likely corruption or truncation. Stop.
                    tracing::warn!("Unknown tag 0x{:02x} in index.log, stopping scan", tag[0]);
                    break;
                }
            }
        }

        // Build posting lists from file_occs.
        let word_count = word_table.len();
        let mut postings: Vec<Vec<u32>> = vec![Vec::new(); word_count];
        for (&path_id, fd) in &files {
            let mut prev_word_id = u32::MAX;
            for occ in &fd.occurrences {
                if occ.word_id != prev_word_id && (occ.word_id as usize) < word_count {
                    postings[occ.word_id as usize].push(path_id);
                    prev_word_id = occ.word_id;
                }
            }
        }

        Ok(Some(LogIndex {
            index_dir: dir.to_path_buf(),
            word_table,
            word_lookup,
            path_table,
            path_lookup,
            files,
            postings,
        }))
    }

    /// Find all occurrences of a word.
    pub fn find_references(&self, word: &str) -> Vec<IndexEntry> {
        let word_id = match self.word_lookup.get(word) {
            Some(&id) => id,
            None => return Vec::new(),
        };

        let file_ids = match self.postings.get(word_id as usize) {
            Some(ids) => ids,
            None => return Vec::new(),
        };

        let mut results = Vec::new();
        for &file_id in file_ids {
            let fd = match self.files.get(&file_id) {
                Some(fd) => fd,
                None => continue,
            };
            let path = PathBuf::from(&self.path_table[file_id as usize]);

            // Occurrences are sorted by (word_id, line). Binary search.
            let start = fd.occurrences.partition_point(|o| o.word_id < word_id);
            for occ in &fd.occurrences[start..] {
                if occ.word_id != word_id {
                    break;
                }
                results.push(IndexEntry {
                    word: word.to_string(),
                    path: path.clone(),
                    line: occ.line,
                    col: occ.col,
                    len: occ.len,
                });
            }
        }
        results
    }

    pub fn word_count(&self) -> usize { self.word_table.len() }
    pub fn file_count(&self) -> usize { self.files.len() }

    /// Memory usage of the in-memory posting lists + word directory.
    pub fn memory_usage(&self) -> usize {
        let words: usize = self.word_table.iter().map(|w| w.len() + std::mem::size_of::<String>()).sum();
        let paths: usize = self.path_table.iter().map(|p| p.len() + std::mem::size_of::<String>()).sum();
        let postings: usize = self.postings.iter()
            .map(|v| v.len() * 4 + std::mem::size_of::<Vec<u32>>())
            .sum();
        let occs: usize = self.files.values()
            .map(|fd| fd.occurrences.len() * std::mem::size_of::<OccEntry>())
            .sum();
        words + paths + postings + occs
    }
}

// ── LogWriter: append entries to the log ────────────────────────────────

pub struct LogWriter {
    w: BufWriter<File>,
    word_table: Vec<String>,
    word_lookup: AHashMap<String, u32>,
    path_table: Vec<String>,
    path_lookup: AHashMap<String, u32>,
    entry_count: usize,
}

impl LogWriter {
    /// Create a new log file, writing the header.
    pub fn create(dir: &Path) -> io::Result<Self> {
        let log_path = dir.join("index.log");
        let mut w = BufWriter::with_capacity(1 << 20, File::create(&log_path)?);
        w.write_all(&LOG_MAGIC)?;
        w.write_all(&0u32.to_le_bytes())?; // reserved
        Ok(Self {
            w,
            word_table: Vec::new(),
            word_lookup: AHashMap::new(),
            path_table: Vec::new(),
            path_lookup: AHashMap::new(),
            entry_count: 0,
        })
    }

    /// Open an existing log for appending. Seeds intern tables from a loaded LogIndex.
    pub fn open_append(dir: &Path, index: &LogIndex) -> io::Result<Self> {
        let log_path = dir.join("index.log");
        let file = std::fs::OpenOptions::new().append(true).open(&log_path)?;
        let w = BufWriter::with_capacity(1 << 20, file);
        Ok(Self {
            w,
            word_table: index.word_table.clone(),
            word_lookup: index.word_lookup.clone(),
            path_table: index.path_table.clone(),
            path_lookup: index.path_lookup.clone(),
            entry_count: 0,
        })
    }

    /// Intern a word, writing TAG 0x01 if it's new. Returns word_id.
    pub fn intern_word(&mut self, word: &str) -> io::Result<u32> {
        if let Some(&id) = self.word_lookup.get(word) {
            return Ok(id);
        }
        let id = self.word_table.len() as u32;
        // Write TAG 0x01
        self.w.write_all(&[TAG_WORD])?;
        self.w.write_all(&(word.len() as u16).to_le_bytes())?;
        self.w.write_all(word.as_bytes())?;

        self.word_lookup.insert(word.to_string(), id);
        self.word_table.push(word.to_string());
        Ok(id)
    }

    /// Intern a path, writing TAG 0x02 if it's new. Returns path_id.
    pub fn intern_path(&mut self, path: &Path) -> io::Result<u32> {
        let path_str = path.to_string_lossy();
        if let Some(&id) = self.path_lookup.get(path_str.as_ref()) {
            return Ok(id);
        }
        let id = self.path_table.len() as u32;
        let bytes = path_str.as_bytes();
        // Write TAG 0x02
        self.w.write_all(&[TAG_PATH])?;
        self.w.write_all(&(bytes.len() as u32).to_le_bytes())?;
        self.w.write_all(bytes)?;

        let owned = path_str.into_owned();
        self.path_lookup.insert(owned.clone(), id);
        self.path_table.push(owned);
        Ok(id)
    }

    /// Write a file's occurrences + symbols as TAG 0x03.
    pub fn write_file_data(
        &mut self,
        path_id: u32,
        mtime: u64,
        occurrences: &[OccEntry],
        symbols: &[Symbol],
        lang: Option<LangFamily>,
    ) -> io::Result<()> {
        self.w.write_all(&[TAG_FILE_DATA])?;
        self.w.write_all(&path_id.to_le_bytes())?;
        self.w.write_all(&mtime.to_le_bytes())?;

        // Occurrences
        self.w.write_all(&(occurrences.len() as u32).to_le_bytes())?;
        for occ in occurrences {
            self.w.write_all(&occ.word_id.to_le_bytes())?;
            self.w.write_all(&occ.line.to_le_bytes())?;
            self.w.write_all(&occ.col.to_le_bytes())?;
            self.w.write_all(&occ.len.to_le_bytes())?;
        }

        // Symbols
        self.w.write_all(&(symbols.len() as u32).to_le_bytes())?;
        for sym in symbols {
            write_symbol(&mut self.w, sym)?;
        }

        // Lang
        self.w.write_all(&[lang_to_u8(lang)])?;

        self.entry_count += 1;
        Ok(())
    }

    /// Write TAG 0x04 (file removed).
    pub fn write_file_removed(&mut self, path_id: u32) -> io::Result<()> {
        self.w.write_all(&[TAG_FILE_REMOVED])?;
        self.w.write_all(&path_id.to_le_bytes())?;
        Ok(())
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.w.flush()
    }

    pub fn entry_count(&self) -> usize { self.entry_count }
    pub fn word_count(&self) -> usize { self.word_table.len() }
    pub fn path_count(&self) -> usize { self.path_table.len() }
}

// ── Binary helpers ──────────────────────────────────────────────────────

fn read_u32(r: &mut impl Read) -> io::Result<u32> {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

fn read_u16(r: &mut impl Read) -> io::Result<u16> {
    let mut buf = [0u8; 2];
    r.read_exact(&mut buf)?;
    Ok(u16::from_le_bytes(buf))
}

fn read_u64(r: &mut impl Read) -> io::Result<u64> {
    let mut buf = [0u8; 8];
    r.read_exact(&mut buf)?;
    Ok(u64::from_le_bytes(buf))
}

fn read_u8(r: &mut impl Read) -> io::Result<u8> {
    let mut buf = [0u8; 1];
    r.read_exact(&mut buf)?;
    Ok(buf[0])
}

fn read_string(r: &mut impl Read, len: usize) -> io::Result<String> {
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    String::from_utf8(buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

fn read_word_entry(r: &mut impl Read) -> io::Result<String> {
    let len = read_u16(r)? as usize;
    read_string(r, len)
}

fn read_path_entry(r: &mut impl Read) -> io::Result<String> {
    let len = read_u32(r)? as usize;
    read_string(r, len)
}

fn read_file_data_entry(r: &mut impl Read) -> io::Result<(u32, FileData)> {
    let path_id = read_u32(r)?;
    let mtime = read_u64(r)?;

    // Occurrences
    let occ_count = read_u32(r)? as usize;
    let mut occurrences = Vec::with_capacity(occ_count);
    for _ in 0..occ_count {
        let word_id = read_u32(r)?;
        let line = read_u32(r)?;
        let col = read_u32(r)?;
        let len = read_u16(r)?;
        occurrences.push(OccEntry { word_id, line, col, len });
    }

    // Symbols
    let sym_count = read_u32(r)? as usize;
    let mut symbols = Vec::with_capacity(sym_count);
    for _ in 0..sym_count {
        symbols.push(read_symbol(r)?);
    }

    // Lang
    let lang = u8_to_lang(read_u8(r)?);

    Ok((path_id, FileData { occurrences, symbols, lang, mtime }))
}

// ── Symbol serialization ────────────────────────────────────────────────

fn write_symbol(w: &mut impl Write, sym: &Symbol) -> io::Result<()> {
    // name
    w.write_all(&(sym.name.len() as u16).to_le_bytes())?;
    w.write_all(sym.name.as_bytes())?;
    // kind
    w.write_all(&[symbol_kind_to_u8(sym.kind)])?;
    // line, col
    w.write_all(&(sym.line as u32).to_le_bytes())?;
    w.write_all(&(sym.col as u32).to_le_bytes())?;
    // def_keyword
    w.write_all(&(sym.def_keyword.len() as u16).to_le_bytes())?;
    w.write_all(sym.def_keyword.as_bytes())?;
    // visibility
    w.write_all(&[visibility_to_u8(sym.visibility)])?;
    // container
    match &sym.container {
        Some(c) => {
            w.write_all(&[1u8])?;
            w.write_all(&(c.len() as u16).to_le_bytes())?;
            w.write_all(c.as_bytes())?;
        }
        None => w.write_all(&[0u8])?,
    }
    // depth
    w.write_all(&(sym.depth as u32).to_le_bytes())?;
    Ok(())
}

fn read_symbol(r: &mut impl Read) -> io::Result<Symbol> {
    let name_len = read_u16(r)? as usize;
    let name = read_string(r, name_len)?;
    let kind = u8_to_symbol_kind(read_u8(r)?);
    let line = read_u32(r)? as usize;
    let col = read_u32(r)? as usize;
    let kw_len = read_u16(r)? as usize;
    let def_keyword = read_string(r, kw_len)?;
    let visibility = u8_to_visibility(read_u8(r)?);
    let has_container = read_u8(r)?;
    let container = if has_container == 1 {
        let c_len = read_u16(r)? as usize;
        Some(read_string(r, c_len)?)
    } else {
        None
    };
    let depth = read_u32(r)? as usize;

    Ok(Symbol {
        name,
        kind,
        line,
        col,
        def_keyword,
        doc_comment: None,
        signature: None,
        visibility,
        container,
        depth,
    })
}

// ── Enum conversions ────────────────────────────────────────────────────

fn symbol_kind_to_u8(k: SymbolKind) -> u8 {
    match k {
        SymbolKind::Function => 0, SymbolKind::Method => 1,
        SymbolKind::Class => 2, SymbolKind::Struct => 3,
        SymbolKind::Enum => 4, SymbolKind::Interface => 5,
        SymbolKind::Constant => 6, SymbolKind::Variable => 7,
        SymbolKind::Module => 8, SymbolKind::TypeAlias => 9,
        SymbolKind::Trait => 10, SymbolKind::Unknown => 255,
    }
}

fn u8_to_symbol_kind(b: u8) -> SymbolKind {
    match b {
        0 => SymbolKind::Function, 1 => SymbolKind::Method,
        2 => SymbolKind::Class, 3 => SymbolKind::Struct,
        4 => SymbolKind::Enum, 5 => SymbolKind::Interface,
        6 => SymbolKind::Constant, 7 => SymbolKind::Variable,
        8 => SymbolKind::Module, 9 => SymbolKind::TypeAlias,
        10 => SymbolKind::Trait, _ => SymbolKind::Unknown,
    }
}

fn visibility_to_u8(v: Visibility) -> u8 {
    match v { Visibility::Public => 0, Visibility::Private => 1, Visibility::Unknown => 2 }
}

fn u8_to_visibility(b: u8) -> Visibility {
    match b { 0 => Visibility::Public, 1 => Visibility::Private, _ => Visibility::Unknown }
}

fn lang_to_u8(lang: Option<LangFamily>) -> u8 {
    match lang {
        None => 0,
        Some(LangFamily::CLike) => 1, Some(LangFamily::Rust) => 2,
        Some(LangFamily::Python) => 3, Some(LangFamily::JsTs) => 4,
        Some(LangFamily::Go) => 5, Some(LangFamily::JavaCSharp) => 6,
        Some(LangFamily::Ruby) => 7,
    }
}

fn u8_to_lang(b: u8) -> Option<LangFamily> {
    match b {
        1 => Some(LangFamily::CLike), 2 => Some(LangFamily::Rust),
        3 => Some(LangFamily::Python), 4 => Some(LangFamily::JsTs),
        5 => Some(LangFamily::Go), 6 => Some(LangFamily::JavaCSharp),
        7 => Some(LangFamily::Ruby), _ => None,
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_and_load_log() {
        let dir = tempfile::tempdir().unwrap();

        // Write a log with two files.
        {
            let mut w = LogWriter::create(dir.path()).unwrap();
            let wid_foo = w.intern_word("foo").unwrap();
            let wid_bar = w.intern_word("bar").unwrap();
            let pid_main = w.intern_path(Path::new("/src/main.rs")).unwrap();
            let pid_lib = w.intern_path(Path::new("/src/lib.rs")).unwrap();

            w.write_file_data(
                pid_main, 1000,
                &[
                    OccEntry { word_id: wid_foo, line: 0, col: 3, len: 3 },
                    OccEntry { word_id: wid_bar, line: 2, col: 0, len: 3 },
                ],
                &[Symbol {
                    name: "foo".into(), kind: SymbolKind::Function,
                    line: 0, col: 3, def_keyword: "fn".into(),
                    doc_comment: None, signature: None,
                    visibility: Visibility::Public, container: None, depth: 0,
                }],
                Some(LangFamily::Rust),
            ).unwrap();

            w.write_file_data(
                pid_lib, 1001,
                &[OccEntry { word_id: wid_foo, line: 5, col: 10, len: 3 }],
                &[],
                Some(LangFamily::Rust),
            ).unwrap();

            w.flush().unwrap();
        }

        // Load and query.
        let idx = LogIndex::load(dir.path()).unwrap().unwrap();
        assert_eq!(idx.word_count(), 2);
        assert_eq!(idx.file_count(), 2);

        let refs = idx.find_references("foo");
        assert_eq!(refs.len(), 2);
        let paths: Vec<_> = refs.iter().map(|r| r.path.to_str().unwrap().to_string()).collect();
        assert!(paths.contains(&"/src/main.rs".to_string()));
        assert!(paths.contains(&"/src/lib.rs".to_string()));

        let refs = idx.find_references("bar");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].line, 2);

        assert!(idx.find_references("baz").is_empty());

        // Check symbols loaded.
        let main_id = *idx.path_lookup.get("/src/main.rs").unwrap();
        let main_data = idx.files.get(&main_id).unwrap();
        assert_eq!(main_data.symbols.len(), 1);
        assert_eq!(main_data.symbols[0].name, "foo");
    }

    #[test]
    fn incremental_append() {
        let dir = tempfile::tempdir().unwrap();

        // Cold index.
        {
            let mut w = LogWriter::create(dir.path()).unwrap();
            let wid = w.intern_word("alpha").unwrap();
            let pid = w.intern_path(Path::new("/a.rs")).unwrap();
            w.write_file_data(
                pid, 100,
                &[OccEntry { word_id: wid, line: 1, col: 0, len: 5 }],
                &[], None,
            ).unwrap();
            w.flush().unwrap();
        }

        let idx = LogIndex::load(dir.path()).unwrap().unwrap();
        assert_eq!(idx.find_references("alpha").len(), 1);
        assert_eq!(idx.find_references("alpha")[0].line, 1);

        // Incremental: modify /a.rs (line changes from 1 to 42).
        {
            let mut w = LogWriter::open_append(dir.path(), &idx).unwrap();
            let wid = w.intern_word("alpha").unwrap(); // already exists
            let wid2 = w.intern_word("beta").unwrap(); // new word
            let pid = w.intern_path(Path::new("/a.rs")).unwrap(); // already exists
            w.write_file_data(
                pid, 200,
                &[
                    OccEntry { word_id: wid, line: 42, col: 0, len: 5 },
                    OccEntry { word_id: wid2, line: 43, col: 0, len: 4 },
                ],
                &[], None,
            ).unwrap();
            w.flush().unwrap();
        }

        // Reload — should see updated data.
        let idx2 = LogIndex::load(dir.path()).unwrap().unwrap();
        assert_eq!(idx2.word_count(), 2); // alpha, beta
        let refs = idx2.find_references("alpha");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].line, 42); // updated!

        let refs = idx2.find_references("beta");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].line, 43);
    }

    #[test]
    fn file_removal() {
        let dir = tempfile::tempdir().unwrap();

        {
            let mut w = LogWriter::create(dir.path()).unwrap();
            let wid = w.intern_word("x").unwrap();
            let pid = w.intern_path(Path::new("/a.rs")).unwrap();
            w.write_file_data(pid, 100, &[OccEntry { word_id: wid, line: 1, col: 0, len: 1 }], &[], None).unwrap();
            w.flush().unwrap();
        }

        let idx = LogIndex::load(dir.path()).unwrap().unwrap();
        assert_eq!(idx.find_references("x").len(), 1);

        // Remove the file.
        {
            let mut w = LogWriter::open_append(dir.path(), &idx).unwrap();
            w.write_file_removed(0).unwrap();
            w.flush().unwrap();
        }

        let idx2 = LogIndex::load(dir.path()).unwrap().unwrap();
        assert!(idx2.find_references("x").is_empty());
        assert_eq!(idx2.file_count(), 0);
    }

    #[test]
    fn empty_log() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut w = LogWriter::create(dir.path()).unwrap();
            w.flush().unwrap();
        }
        let idx = LogIndex::load(dir.path()).unwrap().unwrap();
        assert_eq!(idx.word_count(), 0);
        assert_eq!(idx.file_count(), 0);
        assert!(idx.find_references("anything").is_empty());
    }
}
