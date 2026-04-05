//! Unified Workspace Engine
//!
//! Single data structure that indexes all workspace files and serves every
//! LSP operation. No fallback paths, no probabilistic filters, no separate
//! hot/warm/cold paths.
//!
//! ## How it works
//!
//! 1. Every file is tokenized and its definition symbols stored in a symbol table
//! 2. A reverse index maps symbol names → (file, location) for O(1) definition lookup
//! 3. References are found by word-boundary text search across indexed file contents
//! 4. Fuzzy matching uses precomputed deletion neighborhoods
//! 5. All data is exact — no false positives, no false negatives for definitions

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use dashmap::DashMap;
use rayon::iter::{ParallelBridge, ParallelIterator};
use rayon::slice::ParallelSlice;

use tower_lsp::lsp_types::Url;

use crate::fuzzy::deletion_neighborhood::DeletionIndex;
use crate::parsing::symbols::Symbol;
use crate::parsing::tokenizer::{self, LangFamily};
use crate::parsing::tree_sitter_parse::{self, TsParser};
use crate::word_index::{
    IndexMeta, LogIndex, LogWriter, word_hash,
    collect_file_mtimes, index_dir_for_project,
};

/// A symbol definition with its file location.
#[derive(Debug, Clone)]
pub struct SymbolLocation {
    pub file: PathBuf,
    pub symbol: Symbol,
}

/// A reference (usage) of a name found via text search.
#[derive(Debug, Clone)]
pub struct Reference {
    pub file: PathBuf,
    pub line: usize,
    /// Column as a byte offset from line start.
    pub col: usize,
    /// Length in bytes.
    pub len: usize,
}

/// Compact file identifier used internally to avoid cloning PathBuf.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct FileId(u32);

/// Lightweight reference into the `files` table, stored in the definitions index.
/// Resolves to a full `SymbolLocation` on demand via `files[path].symbols[idx]`.
#[derive(Debug, Clone, Copy)]
struct SymbolRef {
    file_id: FileId,
    symbol_idx: u32,
}

/// Per-file state stored in the workspace.
struct FileEntry {
    symbols: Vec<Symbol>,
    /// Language family detected from file extension.
    #[allow(dead_code)]
    lang: Option<LangFamily>,
}

/// Message sent from parallel scan threads to the log writer thread.
/// Contains everything needed to write one file's data to the log.
struct LogWriteMsg {
    path: PathBuf,
    word_hashes: Vec<u32>,
    symbols: Vec<Symbol>,
    lang: Option<LangFamily>,
    mtime: u64,
}

/// Unified workspace index. One engine, one path, all operations.
pub struct Workspace {
    /// Per-file parsed state: symbols + metadata (no source text).
    files: DashMap<PathBuf, FileEntry>,

    /// Source text for editor-open files only.
    /// Populated by did_open, updated by did_change, removed by did_close.
    open_sources: DashMap<PathBuf, String>,

    /// Reverse index: symbol name → list of (file_id, symbol_index) refs.
    /// This is the primary lookup structure for go-to-definition.
    /// Refs are resolved on demand via `files` to avoid duplicating Symbol data.
    definitions: DashMap<String, Vec<SymbolRef>>,

    /// Maps PathBuf → FileId for O(1) lookup.
    file_ids: DashMap<PathBuf, FileId>,

    /// Maps FileId → PathBuf for reverse lookup. Protected by RwLock for append.
    id_to_path: std::sync::RwLock<Vec<PathBuf>>,

    /// Fuzzy index for typo-tolerant workspace symbol search and completion.
    /// Built lazily on first query to avoid holding memory during scan.
    fuzzy: std::sync::RwLock<DeletionIndex>,
    /// Set to true when definitions change; cleared when fuzzy index is rebuilt.
    fuzzy_dirty: std::sync::atomic::AtomicBool,

    /// On-disk word index for memory-efficient reference lookups.
    /// None until the first scan_directory completes.
    word_index: std::sync::RwLock<Option<LogIndex>>,
}

impl Workspace {
    pub fn new() -> Self {
        Self {
            files: DashMap::new(),
            open_sources: DashMap::new(),
            definitions: DashMap::new(),
            file_ids: DashMap::new(),
            id_to_path: std::sync::RwLock::new(Vec::new()),
            fuzzy: std::sync::RwLock::new(DeletionIndex::new()),
            fuzzy_dirty: std::sync::atomic::AtomicBool::new(false),
            word_index: std::sync::RwLock::new(None),
        }
    }

    /// Get or create a FileId for a path. Thread-safe.
    fn get_or_create_file_id(&self, path: &Path) -> FileId {
        if let Some(id) = self.file_ids.get(path) {
            return *id;
        }
        let mut table = self.id_to_path.write().unwrap();
        // Double-check after acquiring write lock
        if let Some(id) = self.file_ids.get(path) {
            return *id;
        }
        let id = FileId(table.len() as u32);
        table.push(path.to_path_buf());
        self.file_ids.insert(path.to_path_buf(), id);
        id
    }

    /// Resolve a list of SymbolRefs into SymbolLocations by looking up
    /// the actual Symbol data from the `files` table.
    fn resolve_refs(&self, refs: &[SymbolRef]) -> Vec<SymbolLocation> {
        let id_table = self.id_to_path.read().unwrap();
        let mut result = Vec::with_capacity(refs.len());
        for r in refs {
            let path = &id_table[r.file_id.0 as usize];
            if let Some(entry) = self.files.get(path) {
                if let Some(sym) = entry.symbols.get(r.symbol_idx as usize) {
                    result.push(SymbolLocation {
                        file: path.clone(),
                        symbol: sym.clone(),
                    });
                }
            }
        }
        result
    }

    // ── Indexing ─────────────────────────────────────────────────────────

    /// Index a file from the editor: tokenize, extract symbols, store source text.
    pub fn index_file(&self, path: PathBuf, source: String) {
        self.open_sources.insert(path.clone(), source.clone());
        let _ = self.index_file_core(path, &source, true);
    }

    /// Core indexing: tokenize, extract symbols, update definition index.
    /// Parse a file and update symbols + definitions. Returns unique sorted
    /// word hashes for the caller to send to the log writer.
    /// Source text is NOT stored in FileEntry — only open_sources holds it.
    fn index_file_core(
        &self,
        path: PathBuf,
        source: &str,
        update_fuzzy: bool,
    ) -> Vec<u32> {
        let lang = path
            .extension()
            .and_then(|e| e.to_str())
            .and_then(LangFamily::from_extension);

        let (symbols, occurrences) = match lang {
            Some(LangFamily::CLike) => {
                let result = tree_sitter_parse::c::CParser::parse(source);
                let mut symbols = result.symbols;
                Symbol::enrich_from_source(&mut symbols, source, LangFamily::CLike);
                (symbols, result.occurrences)
            }
            Some(l) => {
                let (scan_result, def_contexts) = tokenizer::scan_with_contexts(source, l);
                let mut symbols =
                    Symbol::from_tokens_with_contexts(&scan_result.tokens, &def_contexts);
                Symbol::enrich_from_source(&mut symbols, source, l);
                (symbols, scan_result.occurrences)
            }
            None => (Vec::new(), Vec::new()),
        };

        // Compute unique word hashes from occurrences while we have the source.
        let mut hashes: Vec<u32> = occurrences.iter()
            .map(|occ| {
                let word = &source[occ.word_offset as usize
                    ..(occ.word_offset as usize + occ.word_len as usize)];
                word_hash(word)
            })
            .collect();
        hashes.sort_unstable();
        hashes.dedup();

        // Remove old definitions for this file before inserting new ones
        self.remove_definitions_for_file(&path);

        // Insert into reverse definition index using compact SymbolRefs
        let file_id = self.get_or_create_file_id(&path);
        for (idx, sym) in symbols.iter().enumerate() {
            let sym_ref = SymbolRef {
                file_id,
                symbol_idx: idx as u32,
            };
            self.definitions
                .entry(sym.name.clone())
                .or_default()
                .push(sym_ref);
        }

        // Mark fuzzy index dirty for lazy rebuild on next query.
        if update_fuzzy {
            self.fuzzy_dirty.store(true, std::sync::atomic::Ordering::Release);
        }

        self.files.insert(path, FileEntry { symbols, lang });
        hashes
    }

    /// Rebuild the fuzzy index from all currently indexed symbols if it's dirty.
    fn ensure_fuzzy_built(&self) {
        if !self.fuzzy_dirty.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        let mut fuzzy = self.fuzzy.write().unwrap();
        // Double-check after acquiring write lock.
        if !self.fuzzy_dirty.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        fuzzy.clear();
        for entry in self.definitions.iter() {
            fuzzy.insert(entry.key());
        }
        self.fuzzy_dirty.store(false, std::sync::atomic::Ordering::Release);
    }

    /// Try to load cached symbols from a persisted index.
    ///
    /// Returns:
    /// - `WarmResult::FullyFresh` if all files match and nothing needs re-indexing.
    /// - `WarmResult::PartiallyStale(changed)` if symbols were loaded but some files
    ///   changed and need re-indexing (the word index must be rebuilt).
    /// - `WarmResult::Cold` if no usable cache exists.
    fn try_warm_startup(
        &self,
        root: &Path,
        file_mtimes: &[(PathBuf, std::time::SystemTime)],
    ) -> WarmResult {
        let index_dir = match index_dir_for_project(root) {
            Some(d) => d,
            None => return WarmResult::Cold,
        };

        let meta = match IndexMeta::load(&index_dir) {
            Ok(m) => m,
            Err(_) => return WarmResult::Cold,
        };

        if meta.version != crate::word_index::persistence::CURRENT_VERSION {
            return WarmResult::Cold;
        }

        // Load the log index (contains words, paths, occurrences, symbols).
        let index = match LogIndex::load(&index_dir) {
            Ok(Some(idx)) => idx,
            Ok(None) => return WarmResult::Cold,
            Err(e) => {
                tracing::warn!("Failed to load log index: {e}");
                return WarmResult::Cold;
            }
        };

        // Populate workspace files/definitions/file_ids from the loaded log.
        self.populate_from_log_index(&index);
        self.fuzzy_dirty.store(true, std::sync::atomic::Ordering::Release);

        let changed = changed_files(&meta, file_mtimes);

        if changed.is_empty() {
            tracing::info!(
                "Warm startup: loaded {} hashes, {} files, {} definitions from {}",
                index.unique_hash_count(), index.file_count(),
                self.definitions.len(), index_dir.display(),
            );
            *self.word_index.write().unwrap() = Some(index);
            WarmResult::FullyFresh
        } else {
            tracing::info!(
                "Warm startup: {} cached files loaded, {} files changed",
                index.file_count(), changed.len(),
            );
            // Don't store the index — it will be rebuilt after re-parsing changed files.
            WarmResult::PartiallyStale(changed)
        }
    }

    /// Populate workspace data structures from a loaded LogIndex.
    fn populate_from_log_index(&self, index: &LogIndex) {
        let mut id_table = self.id_to_path.write().unwrap();
        for (path_id, fd) in &index.files {
            let path_str = &index.path_table[*path_id as usize];
            let path = PathBuf::from(path_str);

            let fid = FileId(id_table.len() as u32);
            id_table.push(path.clone());
            self.file_ids.insert(path.clone(), fid);

            // Insert symbols into definitions index.
            for (idx, sym) in fd.symbols.iter().enumerate() {
                let sym_ref = SymbolRef { file_id: fid, symbol_idx: idx as u32 };
                self.definitions.entry(sym.name.clone()).or_default().push(sym_ref);
            }

            self.files.insert(path, FileEntry {
                symbols: fd.symbols.clone(),
                lang: fd.lang,
            });
        }
    }

    /// Scan a directory tree and index all files with supported extensions.
    ///
    /// Phase 0: Compute content hash and try warm startup from persisted index.
    /// Phase 1: Sequential directory walk to collect file paths.
    /// Phase 2: Parallel read + tokenize + index (using rayon).
    /// Phase 3: Rebuild fuzzy index from collected symbols.
    /// Phase 4: Build on-disk word index from all occurrences.
    ///
    /// Files already in the index (e.g., from a prior `did_open`) are skipped.
    pub fn scan_directory(&self, root: &Path) -> ScanStats {
        // Phase 0: collect file mtimes and try warm startup
        let file_mtimes = collect_file_mtimes(root, &should_skip_dir);
        let warm_result = self.try_warm_startup(root, &file_mtimes);

        if matches!(warm_result, WarmResult::FullyFresh) {
            let stats = ScanStats {
                indexed: self.files.len(),
                skipped: 0,
                errors: 0,
            };
            tracing::info!(
                "Workspace scan complete (warm): {} files loaded from cache",
                stats.indexed,
            );
            return stats;
        }

        match warm_result {
            WarmResult::PartiallyStale(changed) => {
                // Truly incremental: append only changed files to the log.
                self.incremental_update(root, &file_mtimes, changed);

                let stats = ScanStats {
                    indexed: self.files.len(),
                    skipped: 0,
                    errors: 0,
                };
                tracing::info!(
                    "Workspace scan complete (incremental): {} files",
                    stats.indexed,
                );
                return stats;
            }
            WarmResult::Cold => {
                // Full cold index.
            }
            WarmResult::FullyFresh => unreachable!(),
        };

        // Cold path: parallel parse → stream occurrences to writer thread → log.
        let mut paths = Vec::new();
        let mut skipped = 0usize;
        Self::collect_paths(root, &self.files, &mut paths, &mut skipped, 0);

        // Build mtime lookup for the writer.
        let mtime_map: std::collections::HashMap<String, u64> = file_mtimes
            .iter()
            .map(|(p, mt)| {
                let secs = mt.duration_since(std::time::SystemTime::UNIX_EPOCH)
                    .map(|d| d.as_secs()).unwrap_or(0);
                (p.to_string_lossy().into_owned(), secs)
            })
            .collect();

        // Set up channel: parallel producers → single writer consumer.
        let (tx, rx) = std::sync::mpsc::sync_channel::<LogWriteMsg>(128);

        // Spawn writer thread.
        let index_dir = index_dir_for_project(root);
        let writer_handle = if let Some(ref idx_dir) = index_dir {
            let _ = std::fs::create_dir_all(idx_dir);
            let idx_dir = idx_dir.clone();
            Some(std::thread::spawn(move || -> std::io::Result<()> {
                let t0 = std::time::Instant::now();
                let mut w = LogWriter::create(&idx_dir)?;
                let mut files_written = 0usize;
                let mut total_hashes = 0usize;

                for msg in rx {
                    let path_id = w.intern_path(&msg.path)?;
                    total_hashes += msg.word_hashes.len();
                    w.write_file_data(path_id, msg.mtime, &msg.word_hashes, &msg.symbols, msg.lang)?;

                    files_written += 1;
                    if files_written % 10000 == 0 {
                        tracing::info!(
                            "log writer progress: {}/{} files, {} hashes, {:.1}s, {}",
                            files_written, "?", total_hashes, t0.elapsed().as_secs_f64(),
                            rss_summary(),
                        );
                    }
                }

                w.flush()?;
                tracing::info!(
                    "log writer done: {} files, {} hashes, {:.1}s, {}",
                    files_written, total_hashes, t0.elapsed().as_secs_f64(),
                    rss_summary(),
                );
                Ok(())
            }))
        } else {
            // No index dir — just drain the channel.
            Some(std::thread::spawn(move || -> std::io::Result<()> {
                for _ in rx {}
                Ok(())
            }))
        };

        // Parallel scan: parse files, store symbols, send occurrences to writer.
        let indexed = AtomicUsize::new(0);
        let errors = AtomicUsize::new(0);

        paths.par_chunks(100).for_each(|chunk| {
            for path in chunk {
                match std::fs::read_to_string(path) {
                    Ok(source) => {
                        let word_hashes = self.index_file_core(path.clone(), &source, false);
                        let lang = path.extension()
                            .and_then(|e| e.to_str())
                            .and_then(LangFamily::from_extension);
                        let mtime = mtime_map.get(&path.to_string_lossy().into_owned())
                            .copied().unwrap_or(0);
                        // Send to writer. If writer died, just drop silently.
                        let _ = tx.send(LogWriteMsg {
                            path: path.clone(),
                            word_hashes,
                            symbols: self.files.get(path).map(|e| e.symbols.clone()).unwrap_or_default(),
                            lang,
                            mtime,
                        });
                        indexed.fetch_add(1, Relaxed);
                    }
                    Err(_) => {
                        errors.fetch_add(1, Relaxed);
                    }
                }
            }
        });

        // Close the channel and wait for writer to finish.
        drop(tx);
        tracing::info!("Indexed {} files, waiting for log writer, {}", indexed.load(Relaxed), Self::rss_summary());

        if let Some(handle) = writer_handle {
            match handle.join() {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::warn!("Log writer failed: {e}"),
                Err(_) => tracing::warn!("Log writer thread panicked"),
            }
        }

        // Return freed memory to the OS before the large LogIndex allocation.
        // Rayon worker threads and the writer thread accumulate freed pages in
        // glibc's per-thread arenas that won't be returned without this.
        #[cfg(target_os = "linux")]
        unsafe { libc::malloc_trim(0); }
        tracing::info!("After malloc_trim (pre-reload): {}", Self::rss_summary());

        // Load the written log index.
        if let Some(ref idx_dir) = index_dir {
            let tl = std::time::Instant::now();
            match LogIndex::load(idx_dir) {
                Ok(Some(index)) => {
                    let meta = IndexMeta {
                        version: crate::word_index::persistence::CURRENT_VERSION,
                        file_count: index.file_count() as u64,
                        entry_count: 0,
                        word_count: index.unique_hash_count() as u64,
                        built_at: std::time::SystemTime::now()
                            .duration_since(std::time::SystemTime::UNIX_EPOCH)
                            .map(|d| d.as_secs()).unwrap_or(0),
                        file_mtimes: IndexMeta::build_mtime_map(&file_mtimes),
                    };
                    if let Err(e) = meta.save(idx_dir) {
                        tracing::warn!("Failed to save index metadata: {e}");
                    }
                    tracing::info!(
                        "Log index loaded: {} files, {} hashes, {:.1}s, {}",
                        index.file_count(), index.unique_hash_count(),
                        tl.elapsed().as_secs_f64(), Self::rss_summary(),
                    );
                    *self.word_index.write().unwrap() = Some(index);
                }
                Ok(None) => tracing::warn!("Log not found after write"),
                Err(e) => tracing::warn!("Failed to load log index: {e}"),
            }
        }

        self.fuzzy_dirty.store(true, std::sync::atomic::Ordering::Release);
        tracing::info!("Log index written, {}", Self::rss_summary());

        // Strip doc_comment and signature from symbols to reduce resident memory.
        for mut entry in self.files.iter_mut() {
            for sym in &mut entry.value_mut().symbols {
                sym.doc_comment = None;
                sym.signature = None;
            }
        }

        #[cfg(target_os = "linux")]
        unsafe {
            libc::malloc_trim(0);
        }

        let stats = ScanStats {
            indexed: indexed.load(Relaxed),
            skipped,
            errors: errors.load(Relaxed),
        };
        tracing::info!(
            "Workspace scan complete: {} files indexed, {} skipped, {} errors{}",
            stats.indexed,
            stats.skipped,
            stats.errors,
            "",
        );
        stats
    }

    /// Maximum directory depth for workspace scanning.
    const MAX_SCAN_DEPTH: usize = 20;

    /// Collect file paths eligible for indexing (sequential directory walk).
    fn collect_paths(
        dir: &Path,
        existing_files: &DashMap<PathBuf, FileEntry>,
        paths: &mut Vec<PathBuf>,
        skipped: &mut usize,
        depth: usize,
    ) {
        if depth > Self::MAX_SCAN_DEPTH {
            return;
        }

        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };

        for entry in entries.flatten() {
            let path = entry.path();

            if path.is_dir() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if should_skip_dir(name) {
                        continue;
                    }
                }
                Self::collect_paths(&path, existing_files, paths, skipped, depth + 1);
            } else if path.is_file() {
                let has_lang = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .and_then(LangFamily::from_extension)
                    .is_some();

                if !has_lang {
                    continue;
                }

                // Skip files already opened by the editor (they have fresher content)
                if existing_files.contains_key(&path) {
                    *skipped += 1;
                    continue;
                }

                paths.push(path);
            }
        }
    }

    /// Re-index a file after edits (same as index_file, just a clearer name).
    pub fn update_file(&self, path: PathBuf, source: String) {
        self.index_file(path, source);
    }

    /// Remove a file from all indices.
    pub fn remove_file(&self, path: &Path) {
        self.remove_definitions_for_file(path);
        self.files.remove(path);
        self.open_sources.remove(path);
    }

    /// Remove all definition entries for a given file from the reverse index.
    fn remove_definitions_for_file(&self, path: &Path) {
        // Look up old symbols for this file to do targeted removal instead of
        // scanning all definitions. Only touches DashMap entries we know about.
        let old_names: Vec<String> = match self.files.get(path) {
            Some(entry) => entry.symbols.iter().map(|s| s.name.clone()).collect(),
            None => return,
        };
        let file_id = match self.file_ids.get(path) {
            Some(id) => *id,
            None => return,
        };
        for name in old_names {
            if let Some(mut entry) = self.definitions.get_mut(&name) {
                entry.value_mut().retain(|r| r.file_id != file_id);
                if entry.value().is_empty() {
                    drop(entry);
                    self.definitions.remove(&name);
                }
            }
        }
    }

    // ── Queries ─────────────────────────────────────────────────────────

    /// Find all definitions of a symbol name. O(1) hash lookup.
    pub fn find_definitions(&self, name: &str) -> Vec<SymbolLocation> {
        self.definitions
            .get(name)
            .map(|v| self.resolve_refs(v.value()))
            .unwrap_or_default()
    }

    /// Rank definitions so the most contextually relevant one comes first.
    ///
    /// Scoring heuristic (higher = better):
    ///  - Same file as cursor: +2
    ///  - Qualifier matches a container near the definition (e.g., the word
    ///    `Workspace` appears on an `impl`/`struct`/`class` line within 20
    ///    lines above the definition): +4
    ///
    /// This is a heuristic, not a type system — but it handles the most
    /// common cases (e.g., `Workspace::new` vs `Mutex::new`) well.
    pub fn rank_definitions(
        &self,
        defs: &mut Vec<SymbolLocation>,
        current_file: Option<&Path>,
        qualifier: Option<&str>,
    ) {
        if defs.len() <= 1 {
            return;
        }

        defs.sort_by(|a, b| {
            let score_a = self.definition_score(a, current_file, qualifier);
            let score_b = self.definition_score(b, current_file, qualifier);
            score_b.cmp(&score_a) // higher score first
        });
    }

    fn definition_score(
        &self,
        loc: &SymbolLocation,
        current_file: Option<&Path>,
        qualifier: Option<&str>,
    ) -> i32 {
        let mut score = 0;

        // Prefer definitions in the same file
        if let Some(cur) = current_file {
            if loc.file == cur {
                score += 2;
            }
        }

        // If there's a qualifier (e.g., "Workspace" from "Workspace::new"),
        // check whether the definition lives inside a matching container.
        if let Some(qual) = qualifier {
            if self.definition_matches_qualifier(loc, qual) {
                score += 4;
            }
        }

        score
    }

    /// Check whether a definition is inside a container matching the qualifier.
    ///
    /// Uses the container name tracked by the scope-aware tokenizer.
    /// Falls back to source text scanning if container is not available.
    fn definition_matches_qualifier(&self, loc: &SymbolLocation, qualifier: &str) -> bool {
        // Use the container name from the scope-aware tokenizer (Phase 1)
        if let Some(ref container) = loc.symbol.container {
            return container == qualifier;
        }

        // Fallback: scan source text (for files indexed before Phase 1)
        let source = match self.file_source(&loc.file) {
            Some(s) => s,
            None => return false,
        };

        let lines: Vec<&str> = source.lines().collect();
        let def_line = loc.symbol.line;

        let start = def_line.saturating_sub(30);
        for line_idx in (start..def_line).rev() {
            if let Some(line) = lines.get(line_idx) {
                let trimmed = line.trim_start();
                for keyword in &[
                    "impl",
                    "struct",
                    "class",
                    "trait",
                    "enum",
                    "interface",
                    "object",
                ] {
                    if let Some(rest) = trimmed.strip_prefix(keyword) {
                        let rest = rest.trim_start();
                        let rest = if rest.starts_with('<') {
                            match rest.find('>') {
                                Some(pos) => rest[pos + 1..].trim_start(),
                                None => rest,
                            }
                        } else {
                            rest
                        };
                        if rest.starts_with(qualifier) {
                            let after = &rest[qualifier.len()..];
                            if after.is_empty()
                                || after.starts_with(|c: char| !c.is_alphanumeric() && c != '_')
                            {
                                return true;
                            }
                        }
                    }
                }
            }
        }

        false
    }

    /// Find all references (usages) of a symbol name across all indexed files.
    ///
    /// If an on-disk word index is available, uses seek-based lookup (O(1) I/O).
    /// Otherwise falls back to word-boundary text search across all files.
    pub fn find_references(&self, name: &str) -> Vec<Reference> {
        // Use the log-based word index to narrow down to candidate files,
        // then re-scan those files for exact positions.
        let candidate_files: Option<Vec<PathBuf>> = if let Ok(guard) = self.word_index.read() {
            guard.as_ref().map(|index| index.find_files(name))
        } else {
            None
        };

        let files_to_scan: Vec<PathBuf> = match candidate_files {
            Some(files) if !files.is_empty() => files,
            _ => {
                // No index or no hits — fall back to scanning all files.
                self.files.iter().map(|e| e.key().clone()).collect()
            }
        };

        files_to_scan
            .into_iter()
            .flat_map(|path| {
                let source = self.open_sources.get(&path)
                    .map(|s| s.clone())
                    .or_else(|| std::fs::read_to_string(&path).ok());
                let mut refs = Vec::new();
                if let Some(source) = source {
                    find_word_occurrences(name, &source, &path, &mut refs);
                }
                refs
            })
            .collect()
    }

    /// Get all symbols defined in a specific file.
    pub fn file_symbols(&self, path: &Path) -> Vec<Symbol> {
        self.files
            .get(path)
            .map(|e| e.symbols.clone())
            .unwrap_or_default()
    }

    /// Get the source text for a file.
    ///
    /// Checks editor-open files first, then falls back to reading from disk.
    pub fn file_source(&self, path: &Path) -> Option<String> {
        // Check open_sources first (editor-open files have the freshest content)
        if let Some(source) = self.open_sources.get(path) {
            return Some(source.clone());
        }
        // Fall back to reading from disk
        std::fs::read_to_string(path).ok()
    }

    /// Get the source text for a file by LSP URI.
    pub fn file_source_from_uri(&self, uri: &Url) -> Option<String> {
        let path = uri.to_file_path().ok()?;
        self.file_source(&path)
    }

    /// Search for symbols by name, with fuzzy/typo tolerance.
    /// Returns (symbol_name, locations) pairs.
    pub fn search_symbols(&self, query: &str) -> Vec<SymbolLocation> {
        self.ensure_fuzzy_built();
        let names = if let Ok(fuzzy) = self.fuzzy.read() {
            fuzzy
                .resolve(query)
                .into_iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        } else {
            return Vec::new();
        };

        let mut results = Vec::new();
        for name in names {
            if let Some(refs) = self.definitions.get(&name) {
                results.extend(self.resolve_refs(refs.value()));
            }
        }
        results
    }

    /// Get completion candidates matching a prefix.
    ///
    /// Uses case-insensitive prefix matching against all definitions, then
    /// falls back to fuzzy search if no prefix matches are found (handles
    /// typos in nearly-complete names).
    pub fn completions(&self, prefix: &str) -> Vec<SymbolLocation> {
        let lower = prefix.to_ascii_lowercase();
        let mut results = Vec::new();
        for entry in self.definitions.iter() {
            if entry.key().to_ascii_lowercase().starts_with(&lower) {
                results.extend(self.resolve_refs(entry.value()));
            }
        }
        if results.is_empty() {
            // Fall back to fuzzy search for typo tolerance on near-complete names
            return self.search_symbols(prefix);
        }
        results
    }

    /// Get hover information for a symbol: signature + doc comment.
    /// Returns (signature, doc_comment) if found.
    ///
    /// After bulk scan, doc_comment and signature are stripped from in-memory
    /// symbols to save RAM. This method re-extracts them from source on demand.
    pub fn hover_info(&self, name: &str) -> Option<(Option<String>, Option<String>)> {
        let defs = self.find_definitions(name);
        let loc = defs.first()?;

        // Fast path: if fields are already populated (e.g. single-file re-index),
        // return them directly.
        if loc.symbol.signature.is_some() || loc.symbol.doc_comment.is_some() {
            return Some((loc.symbol.signature.clone(), loc.symbol.doc_comment.clone()));
        }

        // Re-extract from source file.
        let source = std::fs::read_to_string(&loc.file).ok()?;
        let lang = loc.file.extension()
            .and_then(|e| e.to_str())
            .and_then(LangFamily::from_extension)?;

        let lines: Vec<&str> = source.lines().collect();
        let doc = crate::parsing::symbols::extract_doc_comment(&lines, loc.symbol.line, lang);
        let sig = crate::parsing::symbols::extract_signature(
            &lines, loc.symbol.line, loc.symbol.col, lang,
        );
        Some((sig, doc))
    }

    /// Incremental update: append only changed files to the existing log.
    /// O(changed files), not O(total files).
    fn incremental_update(
        &self,
        root: &Path,
        file_mtimes: &[(PathBuf, std::time::SystemTime)],
        changed: Vec<PathBuf>,
    ) {
        let index_dir = match index_dir_for_project(root) {
            Some(d) => d,
            None => {
                tracing::warn!("Cannot determine cache directory");
                return;
            }
        };

        // Load the existing log index to seed the append writer.
        let index = match LogIndex::load(&index_dir) {
            Ok(Some(idx)) => idx,
            _ => {
                tracing::warn!("Cannot load existing log for incremental update");
                return;
            }
        };

        // Open log for appending.
        let mut w = match LogWriter::open_append(&index_dir, &index) {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!("Cannot open log for append: {e}");
                return;
            }
        };

        let mtime_map: std::collections::HashMap<String, u64> = file_mtimes
            .iter()
            .map(|(p, mt)| {
                let secs = mt.duration_since(std::time::SystemTime::UNIX_EPOCH)
                    .map(|d| d.as_secs()).unwrap_or(0);
                (p.to_string_lossy().into_owned(), secs)
            })
            .collect();

        let mut appended = 0usize;
        for path in &changed {
            // Check if file still exists (could be deleted).
            if !path.exists() {
                // File was removed.
                if let Some(&pid) = index.path_lookup.get(&path.to_string_lossy().into_owned()) {
                    if let Err(e) = w.write_file_removed(pid) {
                        tracing::warn!("Failed to write file removal: {e}");
                    }
                    // Update in-memory workspace.
                    self.remove_definitions_for_file(path);
                    self.files.remove(path);
                }
                continue;
            }

            let source = match std::fs::read_to_string(path) {
                Ok(s) => s,
                Err(_) => continue,
            };

            // Re-parse for symbols, get word hashes.
            let word_hashes = self.index_file_core(path.clone(), &source, false);

            let path_id = match w.intern_path(path) {
                Ok(id) => id,
                Err(e) => { tracing::warn!("intern_path failed: {e}"); continue; }
            };
            let mtime = mtime_map.get(&path.to_string_lossy().into_owned()).copied().unwrap_or(0);

            let (symbols, lang) = match self.files.get(path) {
                Some(entry) => (entry.symbols.clone(), entry.lang),
                None => continue,
            };

            if let Err(e) = w.write_file_data(path_id, mtime, &word_hashes, &symbols, lang) {
                tracing::warn!("Failed to write file data: {e}");
            }
            appended += 1;
        }

        if let Err(e) = w.flush() {
            tracing::warn!("Failed to flush log: {e}");
            return;
        }
        drop(w);

        tracing::info!("Incremental: appended {} files to log", appended);

        // Reload the log to get updated in-memory index.
        match LogIndex::load(&index_dir) {
            Ok(Some(new_index)) => {
                // Update meta.json with new mtimes.
                let meta = IndexMeta {
                    version: crate::word_index::persistence::CURRENT_VERSION,
                    file_count: new_index.file_count() as u64,
                    entry_count: 0,
                    word_count: new_index.unique_hash_count() as u64,
                    built_at: std::time::SystemTime::now()
                        .duration_since(std::time::SystemTime::UNIX_EPOCH)
                        .map(|d| d.as_secs()).unwrap_or(0),
                    file_mtimes: IndexMeta::build_mtime_map(file_mtimes),
                };
                if let Err(e) = meta.save(&index_dir) {
                    tracing::warn!("Failed to save meta: {e}");
                }
                *self.word_index.write().unwrap() = Some(new_index);
            }
            Ok(None) => tracing::warn!("Log not found after incremental append"),
            Err(e) => tracing::warn!("Failed to reload log: {e}"),
        }

        self.fuzzy_dirty.store(true, std::sync::atomic::Ordering::Release);
    }

    /// Re-extract doc_comment and signature from source if they were stripped.
    fn enrich_symbol_if_needed(&self, loc: &mut SymbolLocation) {
        if loc.symbol.signature.is_some() {
            return;
        }
        if let Ok(source) = std::fs::read_to_string(&loc.file) {
            let lang = loc.file.extension()
                .and_then(|e| e.to_str())
                .and_then(LangFamily::from_extension);
            if let Some(lang) = lang {
                let lines: Vec<&str> = source.lines().collect();
                loc.symbol.doc_comment = crate::parsing::symbols::extract_doc_comment(
                    &lines, loc.symbol.line, lang,
                );
                loc.symbol.signature = crate::parsing::symbols::extract_signature(
                    &lines, loc.symbol.line, loc.symbol.col, lang,
                );
            }
        }
    }

    /// Find the function symbol being called at a given position.
    ///
    /// Scans backwards from the cursor position to find the function name
    /// before the opening parenthesis, then returns the symbol's signature
    /// and the active parameter index.
    pub fn signature_help_at(
        &self,
        source: &str,
        line: usize,
        col: usize,
    ) -> Option<(SymbolLocation, usize)> {
        let lines: Vec<&str> = source.lines().collect();
        let current_line = lines.get(line)?;
        let chars: Vec<char> = current_line.chars().collect();

        // Count commas and find the function name by scanning backwards
        let mut comma_count = 0usize;
        let mut paren_depth = 0i32;
        let mut scan_col = col.min(chars.len());

        // First scan backwards on the current line to find matching '('
        while scan_col > 0 {
            scan_col -= 1;
            match chars[scan_col] {
                ')' => paren_depth += 1,
                '(' => {
                    if paren_depth == 0 {
                        // Found the opening paren — now find the function name
                        let mut name_end = scan_col;
                        while name_end > 0 && chars[name_end - 1] == ' ' {
                            name_end -= 1;
                        }
                        let mut name_start = name_end;
                        while name_start > 0
                            && (chars[name_start - 1] == '_'
                                || chars[name_start - 1].is_alphanumeric())
                        {
                            name_start -= 1;
                        }
                        if name_start < name_end {
                            let func_name: String = chars[name_start..name_end].iter().collect();
                            let mut defs = self.find_definitions(&func_name);
                            // Extract qualifier before the function name
                            // (e.g., "Workspace" from "Workspace::new(")
                            let qualifier = extract_qualifier_before(&chars, name_start);
                            self.rank_definitions(&mut defs, None, qualifier.as_deref());
                            if let Some(mut loc) = defs.into_iter().next() {
                                self.enrich_symbol_if_needed(&mut loc);
                                return Some((loc, comma_count));
                            }
                        }
                        return None;
                    }
                    paren_depth -= 1;
                }
                ',' if paren_depth == 0 => comma_count += 1,
                _ => {}
            }
        }

        None
    }

    /// Number of indexed files.
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// Total number of definition refs across all files.
    pub fn definition_count(&self) -> usize {
        self.definitions.iter().map(|e| e.value().len()).sum()
    }

    /// Total number of unique symbol names.
    pub fn unique_symbol_count(&self) -> usize {
        self.definitions.len()
    }

    /// Collect up to `max` unique symbol names from the index.
    pub fn sample_symbol_names(&self, max: usize) -> Vec<String> {
        self.definitions
            .iter()
            .take(max)
            .map(|e| e.key().clone())
            .collect()
    }

    /// Compute a detailed memory breakdown of all in-memory data structures.
    /// Returns a list of (component_name, bytes) pairs.
    pub fn memory_breakdown(&self) -> Vec<(&'static str, usize)> {
        let mut breakdown = Vec::new();

        // 1. files DashMap: PathBuf keys + FileEntry values (symbols only, no occurrences)
        let mut files_keys = 0usize;
        let mut files_symbols = 0usize;
        let mut total_symbols = 0usize;
        for entry in self.files.iter() {
            files_keys += entry.key().as_os_str().len() + std::mem::size_of::<PathBuf>();
            for sym in &entry.value().symbols {
                files_symbols += Self::symbol_deep_size(sym);
                total_symbols += 1;
            }
        }
        breakdown.push(("files: path keys", files_keys));
        breakdown.push(("files: symbols", files_symbols));

        // 2. definitions DashMap: String keys + Vec<SymbolRef> values
        let mut defs_keys = 0usize;
        let mut defs_values = 0usize;
        let mut total_defs = 0usize;
        for entry in self.definitions.iter() {
            defs_keys += std::mem::size_of::<String>() + entry.key().len();
            defs_values += std::mem::size_of::<Vec<SymbolRef>>()
                + entry.value().len() * std::mem::size_of::<SymbolRef>();
            total_defs += entry.value().len();
        }
        breakdown.push(("definitions: keys", defs_keys));
        breakdown.push(("definitions: values", defs_values));

        // 2b. file_ids + id_to_path overhead
        let mut file_id_overhead = 0usize;
        for entry in self.file_ids.iter() {
            file_id_overhead += entry.key().as_os_str().len()
                + std::mem::size_of::<PathBuf>()
                + std::mem::size_of::<FileId>();
        }
        if let Ok(table) = self.id_to_path.read() {
            file_id_overhead += table.len() * std::mem::size_of::<PathBuf>();
            for p in table.iter() {
                file_id_overhead += p.as_os_str().len();
            }
        }
        breakdown.push(("file id tables", file_id_overhead));

        // 3. fuzzy index
        let fuzzy_size = if let Ok(fuzzy) = self.fuzzy.read() {
            let syms: usize = fuzzy.symbols().iter().map(|s| std::mem::size_of::<String>() + s.len()).sum();
            let trigrams: usize = fuzzy.trigram_count() * (3 + std::mem::size_of::<Vec<u32>>())
                + fuzzy.trigram_entry_count() * std::mem::size_of::<u32>();
            syms + trigrams
        } else {
            0
        };
        breakdown.push(("fuzzy index", fuzzy_size));

        // 4. log index (in-memory postings + occurrences)
        let word_index_size = if let Ok(guard) = self.word_index.read() {
            guard.as_ref().map(|wi| wi.memory_usage()).unwrap_or(0)
        } else {
            0
        };
        breakdown.push(("log index (postings)", word_index_size));

        // 5. open_sources (should be 0 for scan_directory benchmarks)
        let open_sources_size: usize = self.open_sources.iter()
            .map(|e| e.key().as_os_str().len() + e.value().len())
            .sum();
        breakdown.push(("open sources", open_sources_size));

        // Summary stats
        breakdown.push(("(count) files", self.files.len()));
        breakdown.push(("(count) symbols in files", total_symbols));
        breakdown.push(("(count) definitions", total_defs));
        breakdown.push(("(count) unique def names", self.definitions.len()));

        breakdown
    }

    fn rss_summary() -> String { rss_summary() }

    fn symbol_deep_size(sym: &Symbol) -> usize {
        std::mem::size_of::<Symbol>()
            + sym.name.len()
            + sym.def_keyword.len()
            + sym.doc_comment.as_ref().map_or(0, |s| s.len())
            + sym.signature.as_ref().map_or(0, |s| s.len())
            + sym.container.as_ref().map_or(0, |s| s.len())
    }
}

impl Default for Workspace {
    fn default() -> Self {
        Self::new()
    }
}

// ── Warm startup types ────────────────────────────────────────────────

enum WarmResult {
    /// All files match — no re-indexing needed.
    FullyFresh,
    /// Some files changed — these need re-parsing + word index rebuild.
    PartiallyStale(Vec<PathBuf>),
    /// No usable cache — full cold index.
    Cold,
}

/// Read RSS and VM from /proc/self/statm.
fn rss_summary() -> String {
    let Ok(statm) = std::fs::read_to_string("/proc/self/statm") else {
        return "rss=N/A".to_string();
    };
    let mut fields = statm.split_whitespace();
    let vm_pages: usize = fields.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let rss_pages: usize = fields.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let page_size = 4096;
    format!(
        "rss={:.0} MB, vm={:.0} MB",
        (rss_pages * page_size) as f64 / (1024.0 * 1024.0),
        (vm_pages * page_size) as f64 / (1024.0 * 1024.0),
    )
}

/// Compare current file mtimes against the cached meta to find changed files.
/// Returns paths of files that were added, modified, or removed.
fn changed_files(
    meta: &IndexMeta,
    current_mtimes: &[(PathBuf, std::time::SystemTime)],
) -> Vec<PathBuf> {
    let mut changed = Vec::new();

    // Check for new or modified files.
    for (path, mtime) in current_mtimes {
        let secs = mtime
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let path_str = path.to_string_lossy();
        match meta.file_mtimes.get(path_str.as_ref()) {
            Some(&stored) if stored == secs => {} // unchanged
            _ => changed.push(path.clone()),       // new or modified
        }
    }

    // Check for removed files (in meta but not in current).
    let current_set: std::collections::HashSet<String> = current_mtimes
        .iter()
        .map(|(p, _)| p.to_string_lossy().into_owned())
        .collect();
    for path_str in meta.file_mtimes.keys() {
        if !current_set.contains(path_str) {
            changed.push(PathBuf::from(path_str));
        }
    }

    changed
}

// ── Symbol persistence for warm startup ───────────────────────────────

use crate::parsing::symbols::SymbolKind;
use crate::parsing::tokenizer::Visibility;
use std::io::{BufReader, BufWriter, Read as IoRead, Write as IoWrite};

const SYMBOLS_MAGIC: &[u8; 8] = b"QLSY\x01\x00\x00\x00";

/// Save all file symbols + definitions to a compact binary file.
/// Excludes doc_comment and signature (already stripped).
fn save_symbols(
    files: &DashMap<PathBuf, FileEntry>,
    index_dir: &Path,
) -> std::io::Result<()> {
    let path = index_dir.join("symbols.bin");
    let mut w = BufWriter::with_capacity(1 << 20, std::fs::File::create(&path)?);

    w.write_all(SYMBOLS_MAGIC)?;
    let file_count = files.len() as u32;
    w.write_all(&file_count.to_le_bytes())?;

    for entry in files.iter() {
        let path_bytes = entry.key().to_string_lossy();
        let path_bytes = path_bytes.as_bytes();
        w.write_all(&(path_bytes.len() as u32).to_le_bytes())?;
        w.write_all(path_bytes)?;

        let lang_byte = match entry.value().lang {
            None => 0u8,
            Some(LangFamily::CLike) => 1,
            Some(LangFamily::Rust) => 2,
            Some(LangFamily::Python) => 3,
            Some(LangFamily::JsTs) => 4,
            Some(LangFamily::Go) => 5,
            Some(LangFamily::JavaCSharp) => 6,
            Some(LangFamily::Ruby) => 7,
        };
        w.write_all(&[lang_byte])?;

        let sym_count = entry.value().symbols.len() as u32;
        w.write_all(&sym_count.to_le_bytes())?;

        for sym in &entry.value().symbols {
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
        }
    }
    w.flush()?;
    Ok(())
}

/// Load symbols from disk and populate files, definitions, file_ids, id_to_path.
fn load_symbols(
    files: &DashMap<PathBuf, FileEntry>,
    definitions: &DashMap<String, Vec<SymbolRef>>,
    file_ids: &DashMap<PathBuf, FileId>,
    id_to_path: &std::sync::RwLock<Vec<PathBuf>>,
    index_dir: &Path,
) -> std::io::Result<()> {
    let path = index_dir.join("symbols.bin");
    let mut r = BufReader::with_capacity(1 << 20, std::fs::File::open(&path)?);

    let mut magic = [0u8; 8];
    r.read_exact(&mut magic)?;
    if &magic != SYMBOLS_MAGIC {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "bad symbols magic"));
    }

    let mut buf4 = [0u8; 4];
    r.read_exact(&mut buf4)?;
    let file_count = u32::from_le_bytes(buf4);

    let mut id_table = id_to_path.write().unwrap();

    for _ in 0..file_count {
        // path
        r.read_exact(&mut buf4)?;
        let path_len = u32::from_le_bytes(buf4) as usize;
        let mut path_bytes = vec![0u8; path_len];
        r.read_exact(&mut path_bytes)?;
        let file_path = PathBuf::from(String::from_utf8_lossy(&path_bytes).into_owned());

        // lang
        let mut lang_byte = [0u8; 1];
        r.read_exact(&mut lang_byte)?;
        let lang = match lang_byte[0] {
            1 => Some(LangFamily::CLike),
            2 => Some(LangFamily::Rust),
            3 => Some(LangFamily::Python),
            4 => Some(LangFamily::JsTs),
            5 => Some(LangFamily::Go),
            6 => Some(LangFamily::JavaCSharp),
            7 => Some(LangFamily::Ruby),
            _ => None,
        };

        // sym_count
        r.read_exact(&mut buf4)?;
        let sym_count = u32::from_le_bytes(buf4) as usize;

        // Assign file_id
        let fid = FileId(id_table.len() as u32);
        id_table.push(file_path.clone());
        file_ids.insert(file_path.clone(), fid);

        let mut symbols = Vec::with_capacity(sym_count);
        for sym_idx in 0..sym_count {
            let sym = read_symbol(&mut r)?;

            // Insert into definitions
            let sym_ref = SymbolRef {
                file_id: fid,
                symbol_idx: sym_idx as u32,
            };
            definitions
                .entry(sym.name.clone())
                .or_default()
                .push(sym_ref);

            symbols.push(sym);
        }

        files.insert(file_path, FileEntry {
            symbols,
            lang,
        });
    }

    Ok(())
}

fn read_symbol(r: &mut impl IoRead) -> std::io::Result<Symbol> {
    let mut buf2 = [0u8; 2];
    let mut buf4 = [0u8; 4];
    let mut buf1 = [0u8; 1];

    // name
    r.read_exact(&mut buf2)?;
    let name_len = u16::from_le_bytes(buf2) as usize;
    let mut name_bytes = vec![0u8; name_len];
    r.read_exact(&mut name_bytes)?;
    let name = String::from_utf8_lossy(&name_bytes).into_owned();

    // kind
    r.read_exact(&mut buf1)?;
    let kind = u8_to_symbol_kind(buf1[0]);

    // line, col
    r.read_exact(&mut buf4)?;
    let line = u32::from_le_bytes(buf4) as usize;
    r.read_exact(&mut buf4)?;
    let col = u32::from_le_bytes(buf4) as usize;

    // def_keyword
    r.read_exact(&mut buf2)?;
    let kw_len = u16::from_le_bytes(buf2) as usize;
    let mut kw_bytes = vec![0u8; kw_len];
    r.read_exact(&mut kw_bytes)?;
    let def_keyword = String::from_utf8_lossy(&kw_bytes).into_owned();

    // visibility
    r.read_exact(&mut buf1)?;
    let visibility = u8_to_visibility(buf1[0]);

    // container
    r.read_exact(&mut buf1)?;
    let container = if buf1[0] == 1 {
        r.read_exact(&mut buf2)?;
        let c_len = u16::from_le_bytes(buf2) as usize;
        let mut c_bytes = vec![0u8; c_len];
        r.read_exact(&mut c_bytes)?;
        Some(String::from_utf8_lossy(&c_bytes).into_owned())
    } else {
        None
    };

    // depth
    r.read_exact(&mut buf4)?;
    let depth = u32::from_le_bytes(buf4) as usize;

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

fn symbol_kind_to_u8(k: SymbolKind) -> u8 {
    match k {
        SymbolKind::Function => 0,
        SymbolKind::Method => 1,
        SymbolKind::Class => 2,
        SymbolKind::Struct => 3,
        SymbolKind::Enum => 4,
        SymbolKind::Interface => 5,
        SymbolKind::Constant => 6,
        SymbolKind::Variable => 7,
        SymbolKind::Module => 8,
        SymbolKind::TypeAlias => 9,
        SymbolKind::Trait => 10,
        SymbolKind::Unknown => 255,
    }
}

fn u8_to_symbol_kind(b: u8) -> SymbolKind {
    match b {
        0 => SymbolKind::Function,
        1 => SymbolKind::Method,
        2 => SymbolKind::Class,
        3 => SymbolKind::Struct,
        4 => SymbolKind::Enum,
        5 => SymbolKind::Interface,
        6 => SymbolKind::Constant,
        7 => SymbolKind::Variable,
        8 => SymbolKind::Module,
        9 => SymbolKind::TypeAlias,
        10 => SymbolKind::Trait,
        _ => SymbolKind::Unknown,
    }
}

fn visibility_to_u8(v: Visibility) -> u8 {
    match v {
        Visibility::Public => 0,
        Visibility::Private => 1,
        Visibility::Unknown => 2,
    }
}

fn u8_to_visibility(b: u8) -> Visibility {
    match b {
        0 => Visibility::Public,
        1 => Visibility::Private,
        _ => Visibility::Unknown,
    }
}

// ── Workspace scanning ─────────────────────────────────────────────────

/// Statistics from a workspace directory scan.
#[derive(Debug, Default)]
pub struct ScanStats {
    pub indexed: usize,
    pub skipped: usize,
    pub errors: usize,
}

/// Directories to skip during workspace scanning.
fn should_skip_dir(name: &str) -> bool {
    matches!(
        name,
        "target"
            | "node_modules"
            | ".git"
            | ".hg"
            | ".svn"
            | "__pycache__"
            | ".venv"
            | "venv"
            | ".env"
            | "env"
            | "build"
            | "dist"
            | ".next"
            | ".nuxt"
            | "vendor"
            | ".idea"
            | ".vscode"
    ) || name.starts_with('.')
}

// ── Qualifier extraction ────────────────────────────────────────────────

/// Extract the qualifier identifier before a separator (`::`, `.`, `->`)
/// that appears immediately before position `pos` in `chars`.
///
/// For example, given `Workspace::new(` with `pos` pointing to the start of
/// `new`, returns `Some("Workspace")`.
fn extract_qualifier_before(chars: &[char], pos: usize) -> Option<String> {
    let mut i = pos;

    // Check for separator: `::`, `.`, or `->`
    if i >= 2 && chars[i - 2] == ':' && chars[i - 1] == ':' {
        i -= 2;
    } else if i >= 2 && chars[i - 2] == '-' && chars[i - 1] == '>' {
        i -= 2;
    } else if i >= 1 && chars[i - 1] == '.' {
        i -= 1;
    } else {
        return None;
    }

    // Skip whitespace
    while i > 0 && chars[i - 1] == ' ' {
        i -= 1;
    }

    // Extract identifier
    let end = i;
    while i > 0 && (chars[i - 1] == '_' || chars[i - 1].is_alphanumeric()) {
        i -= 1;
    }
    if i == end {
        return None;
    }
    Some(chars[i..end].iter().collect())
}

// ── Word-boundary text search ───────────────────────────────────────────

/// Find all occurrences of `word` in `source` that are at word boundaries.
/// Appends results to `out`. Unicode-aware.
fn find_word_occurrences(word: &str, source: &str, file: &Path, out: &mut Vec<Reference>) {
    if word.is_empty() {
        return;
    }

    let mut line = 0usize;
    let mut search_from = 0usize;

    while let Some(byte_pos) = source[search_from..].find(word) {
        let abs_pos = search_from + byte_pos;

        // Count newlines between last position and this match
        for &b in &source.as_bytes()[search_from..abs_pos] {
            if b == b'\n' {
                line += 1;
            }
        }
        // Find the start of the current line
        let line_start_byte = source[..abs_pos].rfind('\n').map(|p| p + 1).unwrap_or(0);

        let end_pos = abs_pos + word.len();

        // Check word boundaries
        let start_ok = abs_pos == 0 || !is_ident_char_at(source, abs_pos - 1);
        let end_ok = end_pos >= source.len() || !is_ident_char_at(source, end_pos);

        if start_ok && end_ok {
            out.push(Reference {
                file: file.to_path_buf(),
                line,
                col: abs_pos - line_start_byte,
                len: word.len(),
            });
        }

        search_from = abs_pos + word.len().max(1);
    }
}

/// Check if the character at byte position `pos` in `source` is an identifier char.
fn is_ident_char_at(source: &str, pos: usize) -> bool {
    // Get the char that starts at or contains this byte position
    if pos >= source.len() {
        return false;
    }
    // Find the char boundary at or before pos
    let s = &source[pos..];
    if let Some(ch) = s.chars().next() {
        ch == '_' || ch.is_alphanumeric()
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_and_find_definitions() {
        let ws = Workspace::new();
        ws.index_file(
            PathBuf::from("/src/main.rs"),
            "fn main() {}\nfn process_data() {}".to_string(),
        );
        ws.index_file(
            PathBuf::from("/src/lib.rs"),
            "fn helper() {}\nfn process_data() {}".to_string(),
        );

        let defs = ws.find_definitions("process_data");
        assert_eq!(defs.len(), 2);
        assert!(defs.iter().any(|d| d.file == Path::new("/src/main.rs")));
        assert!(defs.iter().any(|d| d.file == Path::new("/src/lib.rs")));

        let defs = ws.find_definitions("main");
        assert_eq!(defs.len(), 1);
    }

    #[test]
    fn find_references_across_files() {
        let ws = Workspace::new();
        ws.index_file(
            PathBuf::from("/src/main.rs"),
            "fn main() { process_data(); }".to_string(),
        );
        ws.index_file(
            PathBuf::from("/src/lib.rs"),
            "fn process_data() {}\nfn other() { process_data(); }".to_string(),
        );

        let refs = ws.find_references("process_data");
        assert_eq!(refs.len(), 3); // 1 in main.rs, 2 in lib.rs
    }

    #[test]
    fn references_respect_word_boundaries() {
        let ws = Workspace::new();
        ws.index_file(
            PathBuf::from("/test.rs"),
            "fn process() {}\nfn process_data() {}\nlet preprocessed = 1;".to_string(),
        );

        let refs = ws.find_references("process");
        // Should match "process" in fn process() but NOT "process_data" or "preprocessed"
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].line, 0);
    }

    #[test]
    fn update_file_replaces_old_symbols() {
        let ws = Workspace::new();
        ws.index_file(
            PathBuf::from("/src/main.rs"),
            "fn old_function() {}".to_string(),
        );
        assert_eq!(ws.find_definitions("old_function").len(), 1);

        ws.update_file(
            PathBuf::from("/src/main.rs"),
            "fn new_function() {}".to_string(),
        );
        assert_eq!(ws.find_definitions("old_function").len(), 0);
        assert_eq!(ws.find_definitions("new_function").len(), 1);
    }

    #[test]
    fn remove_file_clears_all_data() {
        let ws = Workspace::new();
        ws.index_file(PathBuf::from("/src/main.rs"), "fn foo() {}".to_string());
        assert_eq!(ws.file_count(), 1);
        assert_eq!(ws.find_definitions("foo").len(), 1);

        ws.remove_file(Path::new("/src/main.rs"));
        assert_eq!(ws.file_count(), 0);
        assert_eq!(ws.find_definitions("foo").len(), 0);
    }

    #[test]
    fn file_symbols_returns_all_symbols() {
        let ws = Workspace::new();
        ws.index_file(
            PathBuf::from("/src/lib.rs"),
            "fn a() {}\nstruct B {}\nenum C {}".to_string(),
        );

        let syms = ws.file_symbols(Path::new("/src/lib.rs"));
        assert_eq!(syms.len(), 3);
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"a"));
        assert!(names.contains(&"B"));
        assert!(names.contains(&"C"));
    }

    #[test]
    fn fuzzy_search_finds_typos() {
        let ws = Workspace::new();
        ws.index_file(
            PathBuf::from("/src/main.rs"),
            "fn process_data() {}".to_string(),
        );

        // Transposition typo
        let results = ws.search_symbols("process_dtaa");
        assert!(
            results.iter().any(|r| r.symbol.name == "process_data"),
            "Should find process_data via fuzzy: {results:?}"
        );
    }

    #[test]
    fn unicode_definitions_and_references() {
        let ws = Workspace::new();
        ws.index_file(
            PathBuf::from("/src/main.rs"),
            "fn über_config() {}\nfn test() { über_config(); }".to_string(),
        );

        let defs = ws.find_definitions("über_config");
        assert_eq!(defs.len(), 1);

        let refs = ws.find_references("über_config");
        assert_eq!(refs.len(), 2); // definition + usage
    }

    #[test]
    fn word_boundary_unicode() {
        let ws = Workspace::new();
        ws.index_file(
            PathBuf::from("/test.py"),
            "def données(): pass\ndef données_extra(): pass".to_string(),
        );

        let refs = ws.find_references("données");
        // Should match "données" but not "données_extra"
        assert_eq!(refs.len(), 1);
    }

    #[test]
    fn reference_column_is_char_offset() {
        let ws = Workspace::new();
        // "fn x() { café(); }" — café starts at char 9
        ws.index_file(PathBuf::from("/test.rs"), "fn café() {}".to_string());

        let refs = ws.find_references("café");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].col, 3); // "fn " = 3 chars
    }

    #[test]
    fn cross_language_workspace() {
        let ws = Workspace::new();
        ws.index_file(PathBuf::from("/src/main.rs"), "fn process() {}".to_string());
        ws.index_file(
            PathBuf::from("/src/app.py"),
            "def process():\n    pass".to_string(),
        );
        ws.index_file(
            PathBuf::from("/src/app.js"),
            "function process() {}".to_string(),
        );

        let defs = ws.find_definitions("process");
        assert_eq!(defs.len(), 3);
    }

    #[test]
    fn completions_return_results() {
        let ws = Workspace::new();
        ws.index_file(
            PathBuf::from("/src/main.rs"),
            "fn process_data() {}\nfn process_request() {}".to_string(),
        );

        let results = ws.completions("process_dat");
        assert!(
            results.iter().any(|r| r.symbol.name == "process_data"),
            "Completions should include process_data"
        );
    }

    /// Integration test: index the quicklsp crate's own source files.
    /// Runs in CI without any external repo downloads.
    #[test]
    fn index_own_source() {
        let ws = Workspace::new();
        let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");

        let mut file_count = 0;
        index_dir_recursive(&ws, &src_dir, &mut file_count);

        assert!(
            file_count > 5,
            "Should index at least 5 source files, got {file_count}"
        );
        assert!(
            ws.definition_count() > 20,
            "Should find >20 definitions in own source, got {}",
            ws.definition_count()
        );
        assert!(
            ws.unique_symbol_count() > 10,
            "Should find >10 unique symbols, got {}",
            ws.unique_symbol_count()
        );

        // Should find our own types
        let defs = ws.find_definitions("Workspace");
        assert!(!defs.is_empty(), "Should find Workspace definition");

        let defs = ws.find_definitions("QuickLspServer");
        assert!(!defs.is_empty(), "Should find QuickLspServer definition");

        // References should find usages across files
        let refs = ws.find_references("Workspace");
        assert!(
            refs.len() >= 2,
            "Should find >=2 references to Workspace, got {}",
            refs.len()
        );

        // Fuzzy search should work
        let results = ws.search_symbols("Workspce"); // typo
        assert!(
            results.iter().any(|r| r.symbol.name == "Workspace"),
            "Fuzzy search should resolve typo 'Workspce' to 'Workspace'"
        );
    }

    fn index_dir_recursive(ws: &Workspace, dir: &Path, count: &mut usize) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                index_dir_recursive(ws, &path, count);
            } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if ext == "rs" {
                    if let Ok(source) = std::fs::read_to_string(&path) {
                        ws.index_file(path, source);
                        *count += 1;
                    }
                }
            }
        }
    }

    // ── Scan + didOpen ordering integration tests ──────────────────────

    /// Helper: create a temp directory with two Rust source files.
    fn make_scan_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.rs");
        let b = dir.path().join("b.rs");
        std::fs::write(&a, "/// Doc for alpha.\nfn alpha() {}").unwrap();
        std::fs::write(&b, "/// Doc for beta.\nfn beta() { alpha(); }").unwrap();
        (dir, a, b)
    }

    #[test]
    fn scan_then_did_open_replaces_with_editor_version() {
        let (dir, a, _b) = make_scan_fixture();
        let ws = Workspace::new();

        // Phase 1: scan indexes both files from disk
        ws.scan_directory(dir.path());
        assert_eq!(ws.find_definitions("alpha").len(), 1);
        assert_eq!(ws.find_definitions("beta").len(), 1);

        // Phase 2: editor opens a.rs with different content (renamed function)
        ws.index_file(a, "fn alpha_v2() {}".to_string());

        // Editor version wins: alpha is gone, alpha_v2 is present
        assert_eq!(ws.find_definitions("alpha").len(), 0);
        assert_eq!(ws.find_definitions("alpha_v2").len(), 1);
        // b.rs still has beta from the scan
        assert_eq!(ws.find_definitions("beta").len(), 1);
    }

    #[test]
    fn did_open_then_scan_preserves_editor_version() {
        let (dir, a, _b) = make_scan_fixture();
        let ws = Workspace::new();

        // Phase 1: editor opens a.rs with modified content
        ws.index_file(a, "fn alpha_edited() {}".to_string());
        assert_eq!(ws.find_definitions("alpha_edited").len(), 1);

        // Phase 2: scan runs — should SKIP a.rs (already in index)
        let stats = ws.scan_directory(dir.path());
        assert_eq!(stats.skipped, 1); // a.rs skipped
        assert_eq!(stats.indexed, 1); // b.rs indexed

        // Editor version preserved
        assert_eq!(ws.find_definitions("alpha_edited").len(), 1);
        assert_eq!(ws.find_definitions("alpha").len(), 0);
        // b.rs picked up from scan
        assert_eq!(ws.find_definitions("beta").len(), 1);
    }

    #[test]
    fn scan_then_did_change_replaces_scanned_version() {
        let (dir, a, _b) = make_scan_fixture();
        let ws = Workspace::new();

        // Phase 1: scan indexes from disk
        ws.scan_directory(dir.path());
        assert_eq!(ws.find_definitions("alpha").len(), 1);
        let info = ws.hover_info("alpha");
        assert!(info.is_some());

        // Phase 2: editor sends didChange (update_file) with new content
        ws.update_file(a, "fn alpha_changed() {}".to_string());

        // Changed version wins
        assert_eq!(ws.find_definitions("alpha").len(), 0);
        assert_eq!(ws.find_definitions("alpha_changed").len(), 1);
    }

    #[test]
    fn scan_only_enables_cross_file_resolution() {
        let (dir, _a, _b) = make_scan_fixture();
        let ws = Workspace::new();

        // No files opened via didOpen — scan is the only source
        ws.scan_directory(dir.path());

        // Both files indexed from scan
        assert_eq!(ws.find_definitions("alpha").len(), 1);
        assert_eq!(ws.find_definitions("beta").len(), 1);

        // Hover works on scanned symbols
        let (sig, doc) = ws.hover_info("alpha").unwrap();
        assert!(sig.unwrap().contains("alpha"));
        assert!(doc.unwrap().contains("Doc for alpha"));

        // Cross-file: beta references alpha
        let refs = ws.find_references("alpha");
        assert!(refs.len() >= 2, "alpha should appear in both files");
    }

    #[test]
    fn scan_skips_excluded_directories() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let target = dir.path().join("target");
        let node_modules = dir.path().join("node_modules");
        let git = dir.path().join(".git");

        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::fs::create_dir_all(&node_modules).unwrap();
        std::fs::create_dir_all(&git).unwrap();

        std::fs::write(src.join("lib.rs"), "fn real() {}").unwrap();
        std::fs::write(target.join("gen.rs"), "fn generated() {}").unwrap();
        std::fs::write(node_modules.join("dep.js"), "function dep() {}").unwrap();
        std::fs::write(git.join("hook.rs"), "fn hook() {}").unwrap();

        let ws = Workspace::new();
        ws.scan_directory(dir.path());

        assert_eq!(ws.find_definitions("real").len(), 1);
        assert_eq!(ws.find_definitions("generated").len(), 0);
        assert_eq!(ws.find_definitions("dep").len(), 0);
        assert_eq!(ws.find_definitions("hook").len(), 0);
    }

    #[test]
    fn extract_qualifier_before_double_colon() {
        let chars: Vec<char> = "Workspace::new()".chars().collect();
        // "new" starts at index 11 (W=0..e=8, :=9, :=10, n=11)
        assert_eq!(
            extract_qualifier_before(&chars, 11),
            Some("Workspace".to_string())
        );
    }

    #[test]
    fn extract_qualifier_before_dot() {
        let chars: Vec<char> = "self.workspace.scan_directory()".chars().collect();
        // "scan_directory" starts at index 15
        // s=0,e=1,l=2,f=3,.=4,w=5..e=13,.=14,s=15
        assert_eq!(
            extract_qualifier_before(&chars, 15),
            Some("workspace".to_string())
        );
    }

    #[test]
    fn extract_qualifier_before_arrow() {
        let chars: Vec<char> = "ptr->method()".chars().collect();
        // "method" starts at index 5 (p=0,t=1,r=2,-=3,>=4,m=5)
        assert_eq!(extract_qualifier_before(&chars, 5), Some("ptr".to_string()));
    }

    #[test]
    fn extract_qualifier_bare_ident() {
        let chars: Vec<char> = "some_function()".chars().collect();
        assert_eq!(extract_qualifier_before(&chars, 0), None);
    }

    #[test]
    fn rank_definitions_prefers_qualifier_match() {
        let ws = Workspace::new();
        ws.index_file(
            PathBuf::from("/src/mutex.rs"),
            "impl Mutex {\n    pub fn new() {}\n}".to_string(),
        );
        ws.index_file(
            PathBuf::from("/src/workspace.rs"),
            "impl Workspace {\n    pub fn new() {}\n}".to_string(),
        );

        let mut defs = ws.find_definitions("new");
        assert_eq!(defs.len(), 2);

        // With qualifier "Workspace", the Workspace::new should rank first
        ws.rank_definitions(&mut defs, None, Some("Workspace"));
        assert_eq!(defs[0].file, PathBuf::from("/src/workspace.rs"));

        // With qualifier "Mutex", the Mutex::new should rank first
        ws.rank_definitions(&mut defs, None, Some("Mutex"));
        assert_eq!(defs[0].file, PathBuf::from("/src/mutex.rs"));
    }

    #[test]
    fn rank_definitions_prefers_same_file() {
        let ws = Workspace::new();
        ws.index_file(PathBuf::from("/src/a.rs"), "fn helper() {}".to_string());
        ws.index_file(PathBuf::from("/src/b.rs"), "fn helper() {}".to_string());

        let mut defs = ws.find_definitions("helper");
        assert_eq!(defs.len(), 2);

        // Without qualifier, prefer same-file
        ws.rank_definitions(&mut defs, Some(Path::new("/src/b.rs")), None);
        assert_eq!(defs[0].file, PathBuf::from("/src/b.rs"));

        ws.rank_definitions(&mut defs, Some(Path::new("/src/a.rs")), None);
        assert_eq!(defs[0].file, PathBuf::from("/src/a.rs"));
    }

    #[test]
    fn rank_definitions_qualifier_beats_same_file() {
        let ws = Workspace::new();
        // File a.rs has a "new" inside impl Mutex
        ws.index_file(
            PathBuf::from("/src/a.rs"),
            "impl Mutex {\n    pub fn new() {}\n}".to_string(),
        );
        // File b.rs has a "new" inside impl Workspace
        ws.index_file(
            PathBuf::from("/src/b.rs"),
            "impl Workspace {\n    pub fn new() {}\n}".to_string(),
        );

        let mut defs = ws.find_definitions("new");
        assert_eq!(defs.len(), 2);

        // Even though current_file is a.rs, qualifier "Workspace" should win
        ws.rank_definitions(&mut defs, Some(Path::new("/src/a.rs")), Some("Workspace"));
        assert_eq!(defs[0].file, PathBuf::from("/src/b.rs"));
    }

    #[test]
    fn scan_multi_batch_correctness() {
        // Create enough files to span multiple batches (BATCH_SIZE=500).
        // Verifies that definitions, references, and the word index are
        // correct when occurrences are drained across batch boundaries.
        let dir = tempfile::tempdir().unwrap();
        let file_count = 600; // > 500 = at least 2 batches

        for i in 0..file_count {
            let name = format!("mod_{i}.rs");
            // Each file defines a unique fn and references a shared name
            let content = format!(
                "fn unique_{i}() {{}}\nfn shared_func() {{ unique_{i}(); }}",
                i = i
            );
            std::fs::write(dir.path().join(&name), content).unwrap();
        }

        let ws = Workspace::new();
        let stats = ws.scan_directory(dir.path());

        assert_eq!(stats.indexed, file_count);
        assert_eq!(stats.errors, 0);

        // Every unique_N should have exactly 1 definition
        for i in 0..file_count {
            let name = format!("unique_{i}");
            let defs = ws.find_definitions(&name);
            assert_eq!(
                defs.len(), 1,
                "unique_{i} should have exactly 1 definition, got {}",
                defs.len()
            );
        }

        // shared_func defined in every file
        let shared_defs = ws.find_definitions("shared_func");
        assert_eq!(
            shared_defs.len(), file_count,
            "shared_func should have {} definitions, got {}",
            file_count, shared_defs.len()
        );

        // References should find shared_func across files via word index
        let refs = ws.find_references("shared_func");
        assert!(
            refs.len() >= file_count,
            "shared_func should have >= {} references, got {}",
            file_count, refs.len()
        );

        // Fuzzy search should still work
        let results = ws.search_symbols("unique_0");
        assert!(
            results.iter().any(|r| r.symbol.name == "unique_0"),
            "Fuzzy search should find unique_0"
        );
    }

    #[test]
    fn word_index_references_correct_across_batches() {
        // Verifies the word index (compact entry sort + write + read-back)
        // produces correct find_references results when entries span
        // multiple batches. Tests the full pipeline: tokenize → drain →
        // compact intern → sort → write → disk read.
        let dir = tempfile::tempdir().unwrap();

        // Create files that share identifiers across batch boundaries.
        // File 0-499 (batch 0) and 500-509 (batch 1) all reference "cross_batch_name".
        for i in 0..510 {
            let name = format!("f_{i}.rs");
            let content = format!(
                "fn func_{i}() {{ cross_batch_name(); }}\nfn cross_batch_name() {{}}",
                i = i,
            );
            std::fs::write(dir.path().join(&name), content).unwrap();
        }

        let ws = Workspace::new();
        ws.scan_directory(dir.path());

        // find_references reads from the on-disk word index built by scan_directory.
        // This exercises the full compact entry pipeline.
        let refs = ws.find_references("cross_batch_name");
        // Each file has 2 occurrences of cross_batch_name (call + def)
        assert_eq!(
            refs.len(), 510 * 2,
            "cross_batch_name should have {} references (2 per file), got {}",
            510 * 2, refs.len()
        );

        // Verify a function that only exists in batch 1 (file 505)
        let refs = ws.find_references("func_505");
        assert_eq!(refs.len(), 1, "func_505 should have 1 reference, got {}", refs.len());

        // Verify a function from batch 0 (file 10)
        let refs = ws.find_references("func_10");
        assert_eq!(refs.len(), 1, "func_10 should have 1 reference, got {}", refs.len());

        // Verify definitions work across batches too
        let defs = ws.find_definitions("func_505");
        assert_eq!(defs.len(), 1);
        let defs = ws.find_definitions("cross_batch_name");
        assert_eq!(defs.len(), 510);
    }

    #[test]
    fn word_index_sort_order_matches_lexicographic() {
        // The compact entry builder uses intern IDs and a sort-rank mapping.
        // Verify that the final on-disk order matches the expected lexicographic
        // order by querying words that would sort differently by ID vs string.
        let dir = tempfile::tempdir().unwrap();

        // Create files with identifiers that have a different insertion order
        // than lexicographic order: "zebra" inserted before "alpha".
        std::fs::write(
            dir.path().join("first.rs"),
            "fn zebra() {}\nfn alpha() {}",
        ).unwrap();
        std::fs::write(
            dir.path().join("second.rs"),
            "fn middle() {}\nfn alpha() {}",
        ).unwrap();

        let ws = Workspace::new();
        ws.scan_directory(dir.path());

        // All three words should be findable via the word index
        let refs_alpha = ws.find_references("alpha");
        assert!(refs_alpha.len() >= 2, "alpha should appear in both files");

        let refs_middle = ws.find_references("middle");
        assert_eq!(refs_middle.len(), 1);

        let refs_zebra = ws.find_references("zebra");
        assert_eq!(refs_zebra.len(), 1);

        // Definitions should also work (these go through the SymbolRef path)
        assert_eq!(ws.find_definitions("alpha").len(), 2);
        assert_eq!(ws.find_definitions("middle").len(), 1);
        assert_eq!(ws.find_definitions("zebra").len(), 1);
    }

    #[test]
    fn single_file_scan_produces_correct_references() {
        // Edge case: only 1 file (well under BATCH_SIZE).
        // Ensures batching logic works when there's only one partial batch.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("only.rs"),
            "fn sole_function() {}\nfn caller() { sole_function(); }",
        ).unwrap();

        let ws = Workspace::new();
        let stats = ws.scan_directory(dir.path());
        assert_eq!(stats.indexed, 1);

        let defs = ws.find_definitions("sole_function");
        assert_eq!(defs.len(), 1);

        let refs = ws.find_references("sole_function");
        assert_eq!(refs.len(), 2, "sole_function: 1 def + 1 call = 2 refs");
    }
}
