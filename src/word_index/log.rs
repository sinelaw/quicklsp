//! Append-only log-based word index with hash-based posting lists.
//!
//! Single file format (`index.log`) that supports both cold indexing (write all
//! files sequentially) and incremental updates (append changed files). At load
//! time, a sequential scan builds in-memory posting lists.
//!
//! Words are identified by 32-bit FNV-1a hashes — no intern table needed.
//! Only posting lists (word_hash → [file_ids]) are kept in memory;
//! exact positions are resolved by re-scanning source files at query time.
//!
//! ## Tags
//!
//! - `0x02` define path  — assigns path_id sequentially
//! - `0x03` file data    — word hashes + symbols, replaces earlier entry for path_id
//! - `0x04` file removed — drops everything for path_id

use ahash::AHashMap;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use crate::parsing::symbols::{Symbol, SymbolKind};
use crate::parsing::tokenizer::{LangFamily, Visibility};

const LOG_MAGIC: [u8; 8] = *b"QLSL\x02\x00\x00\x00"; // v2 format

const TAG_PATH: u8 = 0x02;
const TAG_FILE_DATA: u8 = 0x03;
const TAG_FILE_REMOVED: u8 = 0x04;

// ── Word hashing ───────────────────────────────────────────────────────

/// Deterministic 32-bit FNV-1a hash for word identification.
pub fn word_hash(word: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for &b in word.as_bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

// ── Per-file data loaded from the log ───────────────────────────────────

pub struct FileData {
    pub word_hashes: Vec<u32>,
    pub symbols: Vec<Symbol>,
    pub lang: Option<LangFamily>,
    pub mtime: u64,
}

// ── LogIndex: the in-memory index built from scanning the log ───────────

pub struct LogIndex {
    pub index_dir: PathBuf,

    // Path tables
    pub path_table: Vec<String>,
    pub path_lookup: AHashMap<String, u32>,

    // Per-file data (last TAG 0x03 per path_id wins) — symbols + lang for warm startup
    pub files: HashMap<u32, FileData>,

    // Posting lists: word_hash → [path_ids that contain it]
    pub postings: AHashMap<u32, Vec<u32>>,
}

impl LogIndex {
    /// Load a log index from disk. Scans the log sequentially,
    /// building in-memory structures. Returns None if no log exists.
    pub fn load(dir: &Path) -> io::Result<Option<Self>> {
        let log_path = dir.join("index.log");
        if !log_path.exists() {
            return Ok(None);
        }

        let t0 = std::time::Instant::now();
        let file_size = std::fs::metadata(&log_path)?.len();
        let mut r = BufReader::with_capacity(1 << 20, File::open(&log_path)?);

        let mut magic = [0u8; 8];
        r.read_exact(&mut magic)?;
        if magic != LOG_MAGIC {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "bad log magic"));
        }
        let mut buf4 = [0u8; 4];
        r.read_exact(&mut buf4)?; // reserved

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
                    tracing::warn!("Unknown tag 0x{:02x} in index.log, stopping scan", tag[0]);
                    break;
                }
            }
        }

        let scan_elapsed = t0.elapsed();
        tracing::info!(
            "LogIndex::load scan done: {} paths, {} files, \
             log_size={:.1} MB, {:.1}s, {}",
            path_table.len(), files.len(),
            file_size as f64 / (1024.0 * 1024.0), scan_elapsed.as_secs_f64(),
            rss_summary(),
        );

        // Build posting lists from per-file word hashes.
        let tp = std::time::Instant::now();
        let mut postings: AHashMap<u32, Vec<u32>> = AHashMap::new();
        for (&path_id, fd) in &files {
            for &wh in &fd.word_hashes {
                postings.entry(wh).or_default().push(path_id);
            }
        }
        let unique_hashes = postings.len();
        tracing::info!(
            "LogIndex::load postings built: {} unique hashes, {:.1}s, {}",
            unique_hashes, tp.elapsed().as_secs_f64(), rss_summary(),
        );

        Ok(Some(LogIndex {
            index_dir: dir.to_path_buf(),
            path_table,
            path_lookup,
            files,
            postings,
        }))
    }

    /// Find all files containing a word (by hash).
    pub fn find_files(&self, word: &str) -> Vec<PathBuf> {
        let wh = word_hash(word);
        match self.postings.get(&wh) {
            Some(file_ids) => {
                file_ids.iter()
                    .filter_map(|&fid| {
                        self.path_table.get(fid as usize)
                            .map(|s| PathBuf::from(s))
                    })
                    .collect()
            }
            None => Vec::new(),
        }
    }

    pub fn unique_hash_count(&self) -> usize { self.postings.len() }
    pub fn file_count(&self) -> usize { self.files.len() }

    /// Memory usage of the in-memory posting lists.
    pub fn memory_usage(&self) -> usize {
        let paths: usize = self.path_table.iter()
            .map(|p| p.len() + std::mem::size_of::<String>())
            .sum();
        let postings: usize = self.postings.iter()
            .map(|(_, v)| std::mem::size_of::<u32>() + v.len() * 4 + std::mem::size_of::<Vec<u32>>())
            .sum();
        // Per-file word_hashes kept for incremental updates
        let file_hashes: usize = self.files.values()
            .map(|fd| fd.word_hashes.len() * 4)
            .sum();
        let file_symbols: usize = self.files.values()
            .map(|fd| fd.symbols.len() * std::mem::size_of::<Symbol>())
            .sum();
        paths + postings + file_hashes + file_symbols
    }
}

// ── LogWriter: append entries to the log ────────────────────────────────

pub struct LogWriter {
    w: BufWriter<File>,
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
            path_table: Vec::new(),
            path_lookup: AHashMap::new(),
            entry_count: 0,
        })
    }

    /// Open an existing log for appending. Seeds path tables from a loaded LogIndex.
    pub fn open_append(dir: &Path, index: &LogIndex) -> io::Result<Self> {
        let log_path = dir.join("index.log");
        let file = std::fs::OpenOptions::new().append(true).open(&log_path)?;
        let w = BufWriter::with_capacity(1 << 20, file);
        Ok(Self {
            w,
            path_table: index.path_table.clone(),
            path_lookup: index.path_lookup.clone(),
            entry_count: 0,
        })
    }

    /// Intern a path, writing TAG 0x02 if it's new. Returns path_id.
    pub fn intern_path(&mut self, path: &Path) -> io::Result<u32> {
        let path_str = path.to_string_lossy();
        if let Some(&id) = self.path_lookup.get(path_str.as_ref()) {
            return Ok(id);
        }
        let id = self.path_table.len() as u32;
        let bytes = path_str.as_bytes();
        self.w.write_all(&[TAG_PATH])?;
        self.w.write_all(&(bytes.len() as u32).to_le_bytes())?;
        self.w.write_all(bytes)?;

        let owned = path_str.into_owned();
        self.path_lookup.insert(owned.clone(), id);
        self.path_table.push(owned);
        Ok(id)
    }

    /// Write a file's word hashes + symbols as TAG 0x03.
    pub fn write_file_data(
        &mut self,
        path_id: u32,
        mtime: u64,
        word_hashes: &[u32],
        symbols: &[Symbol],
        lang: Option<LangFamily>,
    ) -> io::Result<()> {
        self.w.write_all(&[TAG_FILE_DATA])?;
        self.w.write_all(&path_id.to_le_bytes())?;
        self.w.write_all(&mtime.to_le_bytes())?;

        // Word hashes (sorted, unique)
        self.w.write_all(&(word_hashes.len() as u32).to_le_bytes())?;
        for &wh in word_hashes {
            self.w.write_all(&wh.to_le_bytes())?;
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

fn read_path_entry(r: &mut impl Read) -> io::Result<String> {
    let len = read_u32(r)? as usize;
    read_string(r, len)
}

fn read_file_data_entry(r: &mut impl Read) -> io::Result<(u32, FileData)> {
    let path_id = read_u32(r)?;
    let mtime = read_u64(r)?;

    // Word hashes
    let hash_count = read_u32(r)? as usize;
    let mut word_hashes = Vec::with_capacity(hash_count);
    for _ in 0..hash_count {
        word_hashes.push(read_u32(r)?);
    }

    // Symbols
    let sym_count = read_u32(r)? as usize;
    let mut symbols = Vec::with_capacity(sym_count);
    for _ in 0..sym_count {
        symbols.push(read_symbol(r)?);
    }

    // Lang
    let lang = u8_to_lang(read_u8(r)?);

    Ok((path_id, FileData { word_hashes, symbols, lang, mtime }))
}

// ── Symbol serialization ────────────────────────────────────────────────

fn write_symbol(w: &mut impl Write, sym: &Symbol) -> io::Result<()> {
    w.write_all(&(sym.name.len() as u16).to_le_bytes())?;
    w.write_all(sym.name.as_bytes())?;
    w.write_all(&[symbol_kind_to_u8(sym.kind)])?;
    w.write_all(&(sym.line as u32).to_le_bytes())?;
    w.write_all(&(sym.col as u32).to_le_bytes())?;
    w.write_all(&(sym.def_keyword.len() as u16).to_le_bytes())?;
    w.write_all(sym.def_keyword.as_bytes())?;
    w.write_all(&[visibility_to_u8(sym.visibility)])?;
    match &sym.container {
        Some(c) => {
            w.write_all(&[1u8])?;
            w.write_all(&(c.len() as u16).to_le_bytes())?;
            w.write_all(c.as_bytes())?;
        }
        None => w.write_all(&[0u8])?,
    }
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
        scope_end_line: None,
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

/// Read RSS from /proc/self/statm (Linux only).
fn rss_summary() -> String {
    let Ok(statm) = std::fs::read_to_string("/proc/self/statm") else {
        return "rss=N/A".to_string();
    };
    let mut fields = statm.split_whitespace();
    let vm_pages: usize = fields.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let rss_pages: usize = fields.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let page_size = 4096usize;
    format!(
        "rss={} MB, vm={} MB",
        rss_pages * page_size / (1024 * 1024),
        vm_pages * page_size / (1024 * 1024),
    )
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_and_load_log() {
        let dir = tempfile::tempdir().unwrap();

        {
            let mut w = LogWriter::create(dir.path()).unwrap();
            let pid_main = w.intern_path(Path::new("/src/main.rs")).unwrap();
            let pid_lib = w.intern_path(Path::new("/src/lib.rs")).unwrap();

            let mut hashes_main = vec![word_hash("foo"), word_hash("bar")];
            hashes_main.sort();
            hashes_main.dedup();

            w.write_file_data(
                pid_main, 1000,
                &hashes_main,
                &[Symbol {
                    name: "foo".into(), kind: SymbolKind::Function,
                    line: 0, col: 3, def_keyword: "fn".into(),
                    doc_comment: None, signature: None,
                    visibility: Visibility::Public, container: None, depth: 0,
                    scope_end_line: None,
                }],
                Some(LangFamily::Rust),
            ).unwrap();

            let hashes_lib = vec![word_hash("foo")];
            w.write_file_data(
                pid_lib, 1001,
                &hashes_lib,
                &[],
                Some(LangFamily::Rust),
            ).unwrap();

            w.flush().unwrap();
        }

        let idx = LogIndex::load(dir.path()).unwrap().unwrap();
        assert_eq!(idx.file_count(), 2);

        let files = idx.find_files("foo");
        assert_eq!(files.len(), 2);

        let files = idx.find_files("bar");
        assert_eq!(files.len(), 1);

        assert!(idx.find_files("baz").is_empty());

        // Check symbols loaded.
        let main_id = *idx.path_lookup.get("/src/main.rs").unwrap();
        let main_data = idx.files.get(&main_id).unwrap();
        assert_eq!(main_data.symbols.len(), 1);
        assert_eq!(main_data.symbols[0].name, "foo");
    }

    #[test]
    fn incremental_append() {
        let dir = tempfile::tempdir().unwrap();

        {
            let mut w = LogWriter::create(dir.path()).unwrap();
            let pid = w.intern_path(Path::new("/a.rs")).unwrap();
            w.write_file_data(
                pid, 100,
                &[word_hash("alpha")],
                &[], None,
            ).unwrap();
            w.flush().unwrap();
        }

        let idx = LogIndex::load(dir.path()).unwrap().unwrap();
        assert_eq!(idx.find_files("alpha").len(), 1);

        // Incremental: modify /a.rs, add beta.
        {
            let mut w = LogWriter::open_append(dir.path(), &idx).unwrap();
            let pid = w.intern_path(Path::new("/a.rs")).unwrap();
            let mut hashes = vec![word_hash("alpha"), word_hash("beta")];
            hashes.sort();
            hashes.dedup();
            w.write_file_data(pid, 200, &hashes, &[], None).unwrap();
            w.flush().unwrap();
        }

        let idx2 = LogIndex::load(dir.path()).unwrap().unwrap();
        assert_eq!(idx2.find_files("alpha").len(), 1);
        assert_eq!(idx2.find_files("beta").len(), 1);
    }

    #[test]
    fn file_removal() {
        let dir = tempfile::tempdir().unwrap();

        {
            let mut w = LogWriter::create(dir.path()).unwrap();
            let pid = w.intern_path(Path::new("/a.rs")).unwrap();
            w.write_file_data(pid, 100, &[word_hash("x")], &[], None).unwrap();
            w.flush().unwrap();
        }

        let idx = LogIndex::load(dir.path()).unwrap().unwrap();
        assert_eq!(idx.find_files("x").len(), 1);

        {
            let mut w = LogWriter::open_append(dir.path(), &idx).unwrap();
            w.write_file_removed(0).unwrap();
            w.flush().unwrap();
        }

        let idx2 = LogIndex::load(dir.path()).unwrap().unwrap();
        assert!(idx2.find_files("x").is_empty());
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
        assert_eq!(idx.file_count(), 0);
        assert!(idx.find_files("anything").is_empty());
    }

    #[test]
    fn hash_determinism() {
        // Same word always produces same hash
        assert_eq!(word_hash("mutex_lock"), word_hash("mutex_lock"));
        // Different words produce different hashes (not guaranteed but should hold for these)
        assert_ne!(word_hash("foo"), word_hash("bar"));
    }
}
