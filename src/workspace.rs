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
use std::sync::Arc;

use dashmap::DashMap;
use rayon::iter::{ParallelBridge, ParallelIterator};
use rayon::slice::ParallelSlice;

use tower_lsp::lsp_types::Url;

use crate::cache::state::{build_row, CacheOps, CacheState};
use crate::cache::{word_hash_fnv1a as word_hash, ContentHash, FileUnit, ScanMetrics, PARSER_VERSION};
use crate::fuzzy::deletion_neighborhood::DeletionIndex;
use crate::parsing::symbols::Symbol;
use crate::parsing::tokenizer::{self, LangFamily};
use crate::parsing::tree_sitter_parse;

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

/// Message sent from parallel scan threads to the coordinator.
/// Holds the result of processing one file during a scan.
struct ScanMsg {
    path: PathBuf,
    rel_path: String,
    content_hash: ContentHash,
    size: u64,
    mtime_ns: i128,
    /// Pre-loaded or freshly-parsed FileUnit. Always set.
    unit: FileUnit,
}

/// Unified workspace index. One engine, one path, all operations.
pub struct Workspace {
    /// Per-file parsed state: symbols + metadata (no source text).
    files: DashMap<PathBuf, FileEntry>,

    /// Source text for editor-open files only.
    /// Populated by did_open, updated by did_change, removed by did_close.
    open_sources: DashMap<PathBuf, String>,

    /// Cached tree-sitter parse trees for editor-open files.
    /// Enables accurate AST node queries at cursor positions.
    syntax_cache: crate::syntax_cache::SyntaxCache,

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

    /// Cache v3 runtime state (Layer A + Layer B + in-memory postings).
    /// None until the first scan_directory completes.
    cache: std::sync::RwLock<Option<CacheState>>,

    /// Metric counters exposed to integration tests for objective validation.
    metrics: Arc<ScanMetrics>,
}

impl Workspace {
    pub fn new() -> Self {
        Self {
            files: DashMap::new(),
            open_sources: DashMap::new(),
            syntax_cache: crate::syntax_cache::SyntaxCache::new(),
            definitions: DashMap::new(),
            file_ids: DashMap::new(),
            id_to_path: std::sync::RwLock::new(Vec::new()),
            fuzzy: std::sync::RwLock::new(DeletionIndex::new()),
            fuzzy_dirty: std::sync::atomic::AtomicBool::new(false),
            cache: std::sync::RwLock::new(None),
            metrics: Arc::new(ScanMetrics::new()),
        }
    }

    /// Test-visible scan metrics.
    pub fn metrics(&self) -> &Arc<ScanMetrics> {
        &self.metrics
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
        let lang = path
            .extension()
            .and_then(|e| e.to_str())
            .and_then(LangFamily::from_extension);
        self.syntax_cache.update(&path, &source, lang);
        self.open_sources.insert(path.clone(), source.clone());
        self.index_file_core(path, &source, true);
    }

    /// Core indexing: tokenize, extract symbols, update definition index.
    /// Parse a file: extract symbols + word hashes. Pure function — does not
    /// modify workspace state. Used during cold scan to produce data for the
    /// log writer without accumulating anything in memory.
    fn parse_file(path: &Path, source: &str) -> (Vec<Symbol>, Vec<u32>, Option<LangFamily>) {
        let lang = path
            .extension()
            .and_then(|e| e.to_str())
            .and_then(LangFamily::from_extension);

        let (symbols, occurrences) = if let Some(result) =
            tree_sitter_parse::try_parse(path, source)
        {
            // Tree-sitter grammar available — use AST-based parsing
            let mut symbols = result.symbols;
            if let Some(l) = lang {
                Symbol::enrich_from_source(&mut symbols, source, l);
            }
            (symbols, result.occurrences)
        } else if let Some(l) = lang {
            // No tree-sitter grammar — fall back to hand-written tokenizer
            let (scan_result, def_contexts) = tokenizer::scan_with_contexts(source, l);
            let mut symbols = Symbol::from_tokens_with_contexts(&scan_result.tokens, &def_contexts);
            Symbol::enrich_from_source(&mut symbols, source, l);
            (symbols, scan_result.occurrences)
        } else {
            (Vec::new(), Vec::new())
        };

        let mut hashes: Vec<u32> = occurrences
            .iter()
            .map(|occ| {
                let word = &source
                    [occ.word_offset as usize..(occ.word_offset as usize + occ.word_len as usize)];
                word_hash(word)
            })
            .collect();
        hashes.sort_unstable();
        hashes.dedup();

        (symbols, hashes, lang)
    }

    /// Parse a file and update workspace state (symbols, definitions, fuzzy).
    /// Used for editor-driven indexing (did_open/did_change).
    fn index_file_core(&self, path: PathBuf, source: &str, update_fuzzy: bool) {
        let (symbols, _hashes, lang) = Self::parse_file(&path, source);
        self.insert_file_entry(path, symbols, lang, update_fuzzy);
    }

    /// Insert pre-parsed symbols into workspace state.
    fn insert_file_entry(
        &self,
        path: PathBuf,
        symbols: Vec<Symbol>,
        lang: Option<LangFamily>,
        update_fuzzy: bool,
    ) {
        self.remove_definitions_for_file(&path);

        let file_id = self.get_or_create_file_id(&path);
        for (idx, sym) in symbols.iter().enumerate() {
            // Skip local variables, function parameters, and struct fields from the
            // global definitions map. These are kept in the per-file symbol list and
            // looked up via find_local_definitions(). We check both depth and keyword
            // to avoid filtering out Rust methods in impl blocks (depth > 0, keyword "fn").
            let is_local = sym.depth > 0
                && matches!(sym.def_keyword.as_str(), "variable" | "parameter" | "field");
            if !is_local {
                let sym_ref = SymbolRef {
                    file_id,
                    symbol_idx: idx as u32,
                };
                self.definitions
                    .entry(sym.name.clone())
                    .or_default()
                    .push(sym_ref);
            }
        }

        if update_fuzzy {
            self.fuzzy_dirty
                .store(true, std::sync::atomic::Ordering::Release);
        }

        self.files.insert(path, FileEntry { symbols, lang });
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
        self.fuzzy_dirty
            .store(false, std::sync::atomic::Ordering::Release);
    }

    /// Populate in-memory `files` / `definitions` / `file_ids` for one file.
    fn install_file_unit(&self, path: PathBuf, unit: &FileUnit) {
        let fid = self.get_or_create_file_id(&path);
        for (idx, sym) in unit.symbols.iter().enumerate() {
            let is_local = sym.depth > 0
                && matches!(sym.def_keyword.as_str(), "variable" | "parameter" | "field");
            if !is_local {
                let sym_ref = SymbolRef {
                    file_id: fid,
                    symbol_idx: idx as u32,
                };
                self.definitions
                    .entry(sym.name.clone())
                    .or_default()
                    .push(sym_ref);
            }
        }
        self.files.insert(
            path,
            FileEntry {
                symbols: unit.symbols.clone(),
                lang: unit.lang,
            },
        );
    }

    /// After a scan, walk over any editor-open files that were skipped by
    /// `collect_paths` (because they were already in `self.files`) and add
    /// their word hashes to the in-memory posting list so `find_references`
    /// covers them. Does not write to Layer A — editor overlays are
    /// per-process (see design §5.5).
    ///
    /// `scanned_rels` is the set of rel_paths that were processed this scan
    /// and thus already have their word_hashes in postings.
    fn install_open_sources_not_yet_cached(
        &self,
        state: &mut CacheState,
        scanned_rels: &std::collections::HashSet<String>,
    ) {
        for entry in self.open_sources.iter() {
            let path = entry.key();
            let Some(rel) = state.rel_path(path) else {
                continue;
            };
            if scanned_rels.contains(&rel) {
                continue;
            }
            let source = entry.value().clone();
            let (_syms, word_hashes, _lang) = Self::parse_file(path, &source);
            if !word_hashes.is_empty() {
                state.add_to_postings(&rel, &word_hashes);
            }
        }
    }

    /// Scan a directory tree and index all files with supported extensions.
    ///
    /// Scan a directory tree and index all files with supported extensions.
    ///
    /// Flow (cache v3):
    ///   1. Detect repo+worktree identity; open/create cache state (Layer A + B).
    ///   2. Validate parser_version and stat-freshness against manifest.
    ///      - If all rows fresh: load FileUnits from Layer A; done.
    ///   3. Collect candidate paths; partition into fresh (manifest-match) and dirty.
    ///   4. In parallel, for each dirty path:
    ///        stat → hash → check Layer A → parse-if-miss → emit ScanMsg.
    ///   5. Install FileUnits in workspace; bulk-upsert manifest rows; bump generation.
    pub fn scan_directory(
        &self,
        root: &Path,
        progress: Option<&(dyn Fn(usize, usize) + Send + Sync)>,
    ) -> ScanStats {
        // ── Phase 1: open cache ──────────────────────────────────────────
        let mut state = match CacheState::open(root, self.metrics.clone()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("Cannot open cache at {}: {e}", root.display());
                // Degenerate fall-through: treat every file as a cold parse,
                // no persistence. (Layer A unavailable → parse everything.)
                return self.scan_directory_no_cache(root, progress);
            }
        };

        // Parser version check: if bumped, treat manifest as empty.
        let parser_ok = state.check_parser_version(PARSER_VERSION).unwrap_or(false);

        // ── Phase 2: collect paths and classify against manifest ─────────
        let mut paths = Vec::new();
        let mut skipped = 0usize;
        Self::collect_paths(root, &self.files, &mut paths, &mut skipped, 0);
        let total_files = paths.len();

        // Load existing rows keyed by rel_path (for stat-freshness check).
        let mut existing_rows: std::collections::HashMap<String, crate::cache::ManifestRow> =
            if parser_ok {
                let m = state.manifest.lock().unwrap();
                m.all_rows()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|r| (r.rel_path.clone(), r))
                    .collect()
            } else {
                std::collections::HashMap::new()
            };

        // Manifest subsumption (design §5.2): pull in rows from prior
        // manifests under the same repo_id whose working_dir is an ancestor
        // or descendant of ours. Our own manifest rows take precedence.
        let subsumed = state.collect_subsumable_rows();
        let subsumed_count = subsumed.len() as u64;
        for r in subsumed {
            existing_rows.entry(r.rel_path.clone()).or_insert(r);
        }
        if subsumed_count > 0 {
            self.metrics
                .manifest_rows_copied
                .fetch_add(subsumed_count, Relaxed);
            tracing::info!(
                "manifest subsumption: borrowed {subsumed_count} rows from sibling worktrees"
            );
        }

        // ── Phase 3: parallel scan → Vec<ScanMsg> ────────────────────────
        let indexed = AtomicUsize::new(0);
        let errors = AtomicUsize::new(0);
        let last_progress = AtomicUsize::new(0);

        let working_dir = state.identity.working_dir.clone();
        let content_store = state.content_store.clone();
        let metrics = self.metrics.clone();
        let existing_rows = Arc::new(existing_rows);

        let scan_results: Vec<ScanMsg> = paths
            .par_chunks(100)
            .flat_map_iter(|chunk| {
                let mut out = Vec::with_capacity(chunk.len());
                for path in chunk {
                    let rel_path = rel_path_for(path, &working_dir);

                    let meta = match std::fs::metadata(path) {
                        Ok(m) => m,
                        Err(_) => {
                            errors.fetch_add(1, Relaxed);
                            continue;
                        }
                    };
                    metrics.files_stat_called.fetch_add(1, Relaxed);
                    let size = meta.len();
                    let mtime_ns = meta
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_nanos() as i128)
                        .unwrap_or(0);

                    // Fast path: stat match → load FileUnit from Layer A.
                    if let Some(row) = existing_rows.get(&rel_path) {
                        if row.size == size && row.mtime_ns == mtime_ns {
                            if let Some(unit) =
                                content_store.get(&row.content_hash, PARSER_VERSION)
                            {
                                out.push(ScanMsg {
                                    path: path.clone(),
                                    rel_path: rel_path.clone(),
                                    content_hash: row.content_hash,
                                    size,
                                    mtime_ns,
                                    unit,
                                });
                                Self::progress_bump(
                                    &indexed,
                                    &last_progress,
                                    progress,
                                    total_files,
                                );
                                continue;
                            }
                        }
                    }

                    // Slow path: read + hash + (maybe) parse.
                    let bytes = match std::fs::read(path) {
                        Ok(b) => b,
                        Err(_) => {
                            errors.fetch_add(1, Relaxed);
                            continue;
                        }
                    };
                    metrics.files_bytes_read.fetch_add(bytes.len() as u64, Relaxed);
                    metrics.files_blake3_hashed.fetch_add(1, Relaxed);
                    let content_hash = ContentHash::of_bytes(&bytes);

                    let source = String::from_utf8_lossy(&bytes).into_owned();

                    let parse_path = path.clone();
                    let source_ref = &source;
                    let unit = match CacheOps::ensure_file_unit(
                        &content_store,
                        &metrics,
                        &content_hash,
                        PARSER_VERSION,
                        || {
                            let (symbols, word_hashes, lang) =
                                Self::parse_file(&parse_path, source_ref);
                            FileUnit {
                                parser_version: PARSER_VERSION,
                                lang,
                                symbols,
                                word_hashes,
                            }
                        },
                    ) {
                        Ok(u) => u,
                        Err(e) => {
                            tracing::warn!(
                                "content_store write failed for {}: {e}",
                                path.display()
                            );
                            errors.fetch_add(1, Relaxed);
                            continue;
                        }
                    };

                    out.push(ScanMsg {
                        path: path.clone(),
                        rel_path,
                        content_hash,
                        size,
                        mtime_ns,
                        unit,
                    });

                    Self::progress_bump(&indexed, &last_progress, progress, total_files);
                }
                out
            })
            .collect();

        // ── Phase 4: install in memory + collect rows ────────────────────
        let generation = {
            let m = state.manifest.lock().unwrap();
            m.generation().unwrap_or(0) + 1
        };
        let mut rows = Vec::with_capacity(scan_results.len());
        for msg in scan_results {
            let lang_u32 = msg.unit.lang.map(lang_to_u32);
            rows.push(build_row(
                msg.rel_path.clone(),
                msg.content_hash,
                lang_u32,
                msg.size,
                msg.mtime_ns,
                generation,
            ));
            state.add_to_postings(&msg.rel_path, &msg.unit.word_hashes);
            self.install_file_unit(msg.path, &msg.unit);
        }

        // Detect rows in manifest that no longer exist on disk → delete.
        let live_rels: std::collections::HashSet<&str> =
            rows.iter().map(|r| r.rel_path.as_str()).collect();
        let mut dead: Vec<String> = Vec::new();
        if parser_ok {
            for (rel, _) in existing_rows.iter() {
                if !live_rels.contains(rel.as_str()) {
                    // File is in manifest but not in current scan → removed.
                    // But only delete if it's actually missing (not just outside
                    // the collect_paths subtree because of skip dirs).
                    let abs = state.abs_path(rel);
                    if !abs.exists() {
                        dead.push(rel.clone());
                    }
                }
            }
        }

        // ── Phase 5: persist manifest changes ────────────────────────────
        {
            let mut m = state.manifest.lock().unwrap();
            if !parser_ok {
                // Parser version mismatch: wipe all stale rows first.
                let stale: Vec<String> = existing_rows.keys().cloned().collect();
                let _ = m.delete_rows(&stale);
                let _ = m.set_parser_version(PARSER_VERSION);
            }
            if !rows.is_empty() {
                if let Err(e) = m.put_rows(&rows) {
                    tracing::warn!("manifest upsert failed: {e}");
                }
            }
            if !dead.is_empty() {
                let _ = m.delete_rows(&dead);
                // Clean up in-memory state too.
                for rel in &dead {
                    let abs = state.abs_path(rel);
                    self.remove_definitions_for_file(&abs);
                    self.files.remove(&abs);
                }
                drop(m);
                for rel in &dead {
                    state.remove_from_postings(rel);
                }
            } else {
                drop(m);
            }
            // Re-lock to bump generation.
            let m = state.manifest.lock().unwrap();
            let _ = m.set_meta("generation", &generation.to_string());
        }

        // Fold in didOpen files whose rel_path was not processed this scan
        // (because collect_paths skipped them as already-indexed).
        let scanned_rels: std::collections::HashSet<String> =
            rows.iter().map(|r| r.rel_path.clone()).collect();
        self.install_open_sources_not_yet_cached(&mut state, &scanned_rels);

        self.fuzzy_dirty
            .store(true, std::sync::atomic::Ordering::Release);

        // Strip doc_comment/signature to reduce resident memory.
        for mut entry in self.files.iter_mut() {
            for sym in &mut entry.value_mut().symbols {
                sym.doc_comment = None;
                sym.signature = None;
            }
        }

        // Return freed memory to the OS.
        #[cfg(target_os = "linux")]
        unsafe {
            libc::malloc_trim(0);
        }

        *self.cache.write().unwrap() = Some(state);

        let stats = ScanStats {
            indexed: indexed.load(Relaxed),
            skipped,
            errors: errors.load(Relaxed),
        };
        tracing::info!(
            "Workspace scan complete: {} indexed, {} skipped, {} errors, {} removed",
            stats.indexed,
            stats.skipped,
            stats.errors,
            dead.len(),
        );
        stats
    }

    /// Progress reporting helper.
    fn progress_bump(
        indexed: &AtomicUsize,
        last_progress: &AtomicUsize,
        progress: Option<&(dyn Fn(usize, usize) + Send + Sync)>,
        total_files: usize,
    ) {
        let count = indexed.fetch_add(1, Relaxed) + 1;
        if let Some(cb) = progress {
            let prev = last_progress.load(Relaxed);
            if count >= prev + 500 || count == total_files {
                if last_progress
                    .compare_exchange(prev, count, Relaxed, Relaxed)
                    .is_ok()
                {
                    cb(count, total_files);
                }
            }
        }
    }

    /// Degraded scan when the cache cannot be opened (no XDG_CACHE, no HOME).
    /// Parses every file directly into `self.files` without persistence.
    fn scan_directory_no_cache(
        &self,
        root: &Path,
        progress: Option<&(dyn Fn(usize, usize) + Send + Sync)>,
    ) -> ScanStats {
        let mut paths = Vec::new();
        let mut skipped = 0usize;
        Self::collect_paths(root, &self.files, &mut paths, &mut skipped, 0);
        let total_files = paths.len();
        let indexed = AtomicUsize::new(0);
        let errors = AtomicUsize::new(0);
        let last_progress = AtomicUsize::new(0);

        paths.par_chunks(100).for_each(|chunk| {
            for path in chunk {
                match std::fs::read_to_string(path) {
                    Ok(source) => {
                        let (symbols, _wh, lang) = Self::parse_file(path, &source);
                        let fid = self.get_or_create_file_id(path);
                        for (idx, sym) in symbols.iter().enumerate() {
                            let is_local = sym.depth > 0
                                && matches!(
                                    sym.def_keyword.as_str(),
                                    "variable" | "parameter" | "field"
                                );
                            if !is_local {
                                self.definitions
                                    .entry(sym.name.clone())
                                    .or_default()
                                    .push(SymbolRef {
                                        file_id: fid,
                                        symbol_idx: idx as u32,
                                    });
                            }
                        }
                        self.files
                            .insert(path.clone(), FileEntry { symbols, lang });
                        Self::progress_bump(&indexed, &last_progress, progress, total_files);
                    }
                    Err(_) => {
                        errors.fetch_add(1, Relaxed);
                    }
                }
            }
        });

        self.fuzzy_dirty
            .store(true, std::sync::atomic::Ordering::Release);

        ScanStats {
            indexed: indexed.load(Relaxed),
            skipped,
            errors: errors.load(Relaxed),
        }
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
        self.syntax_cache.remove(path);
    }

    /// Close an editor-open file: remove source text and cached parse tree,
    /// but keep the indexed symbols (they're still valid from the last save).
    pub fn close_file(&self, path: &Path) {
        self.open_sources.remove(path);
        self.syntax_cache.remove(path);
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

    /// Find local definitions (depth > 0) of a symbol name within a specific file.
    ///
    /// Searches the file's symbol list for local variables, parameters, and struct
    /// fields that match the given name. These are not in the global definitions map
    /// to avoid bloating it with common names like `i`, `result`, etc.
    pub fn find_local_definitions(&self, name: &str, file: &Path) -> Vec<SymbolLocation> {
        self.files
            .get(file)
            .map(|entry| {
                entry
                    .symbols
                    .iter()
                    .filter(|s| s.depth > 0 && s.name == name)
                    .map(|s| SymbolLocation {
                        file: file.to_path_buf(),
                        symbol: s.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Find the best-scoped local definition for a name at a given cursor position.
    ///
    /// When multiple locals share the same name (e.g., shadowed variables in nested
    /// blocks), picks the **nearest definition that precedes the cursor**, preferring
    /// deeper scopes (inner blocks shadow outer ones).
    ///
    /// For example, with cursor on line 5:
    /// ```c
    /// void foo(void) {
    ///     int x = 1;        // line 2, depth 1 — candidate
    ///     if (cond) {
    ///         int x = 2;    // line 4, depth 2 — better candidate (deeper, still before cursor)
    ///         use(x);       // line 5 — cursor here → picks line 4
    ///     }
    /// }
    /// ```
    pub fn find_local_definition_at(
        &self,
        name: &str,
        file: &Path,
        cursor_line: usize,
    ) -> Option<SymbolLocation> {
        self.files.get(file).and_then(|entry| {
            entry
                .symbols
                .iter()
                .filter(|s| {
                    s.depth > 0
                        && s.name == name
                        && s.line <= cursor_line
                        // If scope_end_line is set, the cursor must be within scope
                        && s.scope_end_line.map_or(true, |end| cursor_line <= end)
                })
                .max_by_key(|s| {
                    // Prefer: (1) deeper scope, (2) closer to cursor line
                    (s.depth, s.line)
                })
                .map(|s| SymbolLocation {
                    file: file.to_path_buf(),
                    symbol: s.clone(),
                })
        })
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
        // Use the posting list to narrow down to candidate files,
        // then re-scan those files for exact positions.
        let candidate_files: Option<Vec<PathBuf>> = if let Ok(guard) = self.cache.read() {
            guard.as_ref().map(|state| state.candidate_files(name))
        } else {
            None
        };

        let files_to_scan: Vec<PathBuf> = match candidate_files {
            Some(files) if !files.is_empty() => files,
            _ => {
                // No cache or no hits — fall back to scanning all files.
                self.files.iter().map(|e| e.key().clone()).collect()
            }
        };

        files_to_scan
            .into_iter()
            .flat_map(|path| {
                let source = self
                    .open_sources
                    .get(&path)
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

    /// Access the syntax cache for AST node queries.
    pub fn syntax_cache(&self) -> &crate::syntax_cache::SyntaxCache {
        &self.syntax_cache
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
        let lang = loc
            .file
            .extension()
            .and_then(|e| e.to_str())
            .and_then(LangFamily::from_extension)?;

        let lines: Vec<&str> = source.lines().collect();
        let doc = crate::parsing::symbols::extract_doc_comment(&lines, loc.symbol.line, lang);
        let sig = crate::parsing::symbols::extract_signature(
            &lines,
            loc.symbol.line,
            loc.symbol.col,
            lang,
        );
        Some((sig, doc))
    }


    /// Re-extract doc_comment and signature from source if they were stripped.
    pub fn enrich_symbol_if_needed(&self, loc: &mut SymbolLocation) {
        if loc.symbol.signature.is_some() {
            return;
        }
        if let Ok(source) = std::fs::read_to_string(&loc.file) {
            let lang = loc
                .file
                .extension()
                .and_then(|e| e.to_str())
                .and_then(LangFamily::from_extension);
            if let Some(lang) = lang {
                let lines: Vec<&str> = source.lines().collect();
                loc.symbol.doc_comment =
                    crate::parsing::symbols::extract_doc_comment(&lines, loc.symbol.line, lang);
                loc.symbol.signature = crate::parsing::symbols::extract_signature(
                    &lines,
                    loc.symbol.line,
                    loc.symbol.col,
                    lang,
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
            let syms: usize = fuzzy
                .symbols()
                .iter()
                .map(|s| std::mem::size_of::<String>() + s.len())
                .sum();
            let trigrams: usize = fuzzy.trigram_count() * (3 + std::mem::size_of::<Vec<u32>>())
                + fuzzy.trigram_entry_count() * std::mem::size_of::<u32>();
            syms + trigrams
        } else {
            0
        };
        breakdown.push(("fuzzy index", fuzzy_size));

        // 4. cache v3 postings (word_hash → rel_paths)
        let postings_size = if let Ok(guard) = self.cache.read() {
            guard
                .as_ref()
                .map(|state| {
                    state
                        .postings
                        .iter()
                        .map(|(_, v)| {
                            std::mem::size_of::<Vec<String>>()
                                + v.iter()
                                    .map(|s| std::mem::size_of::<String>() + s.len())
                                    .sum::<usize>()
                        })
                        .sum()
                })
                .unwrap_or(0)
        } else {
            0
        };
        breakdown.push(("cache postings", postings_size));

        // 5. open_sources (should be 0 for scan_directory benchmarks)
        let open_sources_size: usize = self
            .open_sources
            .iter()
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


/// Resolve `path` to a manifest-relative path string rooted at `working_dir`.
/// Falls back to the absolute path if the file is outside the worktree root
/// (e.g. symlinked from elsewhere).
fn rel_path_for(path: &Path, working_dir: &Path) -> String {
    if let Ok(stripped) = path.strip_prefix(working_dir) {
        return stripped.to_string_lossy().into_owned();
    }
    if let Ok(canon) = std::fs::canonicalize(path) {
        if let Ok(stripped) = canon.strip_prefix(working_dir) {
            return stripped.to_string_lossy().into_owned();
        }
    }
    path.to_string_lossy().into_owned()
}

/// Serialize LangFamily as a u32 for the manifest table.
fn lang_to_u32(l: LangFamily) -> u32 {
    match l {
        LangFamily::CLike => 1,
        LangFamily::Rust => 2,
        LangFamily::Python => 3,
        LangFamily::JsTs => 4,
        LangFamily::Go => 5,
        LangFamily::JavaCSharp => 6,
        LangFamily::Ruby => 7,
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
        ws.scan_directory(dir.path(), None);
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
        let stats = ws.scan_directory(dir.path(), None);
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
        ws.scan_directory(dir.path(), None);
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
        ws.scan_directory(dir.path(), None);

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
        ws.scan_directory(dir.path(), None);

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
        let stats = ws.scan_directory(dir.path(), None);

        assert_eq!(stats.indexed, file_count);
        assert_eq!(stats.errors, 0);

        // Every unique_N should have exactly 1 definition
        for i in 0..file_count {
            let name = format!("unique_{i}");
            let defs = ws.find_definitions(&name);
            assert_eq!(
                defs.len(),
                1,
                "unique_{i} should have exactly 1 definition, got {}",
                defs.len()
            );
        }

        // shared_func defined in every file
        let shared_defs = ws.find_definitions("shared_func");
        assert_eq!(
            shared_defs.len(),
            file_count,
            "shared_func should have {} definitions, got {}",
            file_count,
            shared_defs.len()
        );

        // References should find shared_func across files via word index
        let refs = ws.find_references("shared_func");
        assert!(
            refs.len() >= file_count,
            "shared_func should have >= {} references, got {}",
            file_count,
            refs.len()
        );

        // Fuzzy search should still work
        let results = ws.search_symbols("unique_0");
        assert!(
            results.iter().any(|r| r.symbol.name == "unique_0"),
            "Fuzzy search should find unique_0"
        );
    }

    #[test]
    fn cross_file_references_at_scale() {
        // Verifies find_references is correct when many files share an
        // identifier — exercises the full posting-list pipeline.
        let dir = tempfile::tempdir().unwrap();
        for i in 0..510 {
            let name = format!("f_{i}.rs");
            let content = format!(
                "fn func_{i}() {{ shared_name(); }}\nfn shared_name() {{}}",
                i = i,
            );
            std::fs::write(dir.path().join(&name), content).unwrap();
        }

        let ws = Workspace::new();
        ws.scan_directory(dir.path(), None);

        let refs = ws.find_references("shared_name");
        assert_eq!(refs.len(), 510 * 2, "2 refs per file (call + def)");
        assert_eq!(ws.find_definitions("shared_name").len(), 510);
        assert_eq!(ws.find_references("func_505").len(), 1);
    }

    #[test]
    fn single_file_scan_produces_correct_references() {
        // Edge case: only 1 file (well under BATCH_SIZE).
        // Ensures batching logic works when there's only one partial batch.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("only.rs"),
            "fn sole_function() {}\nfn caller() { sole_function(); }",
        )
        .unwrap();

        let ws = Workspace::new();
        let stats = ws.scan_directory(dir.path(), None);
        assert_eq!(stats.indexed, 1);

        let defs = ws.find_definitions("sole_function");
        assert_eq!(defs.len(), 1);

        let refs = ws.find_references("sole_function");
        assert_eq!(refs.len(), 2, "sole_function: 1 def + 1 call = 2 refs");
    }

    /// Issue 2: When a file is opened via didOpen (index_file) BEFORE
    /// scan_directory runs, collect_paths skips it. That file is then
    /// missing from the word index, so find_references can't find
    /// cross-file references through that file.
    #[test]
    fn did_open_before_scan_still_included_in_word_index() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.rs");
        let b = dir.path().join("b.rs");
        let c = dir.path().join("c.rs");
        std::fs::write(&a, "fn shared() {}\nfn only_a() {}").unwrap();
        std::fs::write(&b, "fn caller_b() { shared(); }").unwrap();
        std::fs::write(&c, "fn caller_c() { shared(); }").unwrap();

        let ws = Workspace::new();

        // Simulate the editor race: didOpen for a.rs arrives before scan.
        ws.index_file(a, "fn shared() {}\nfn only_a() {}".to_string());

        // scan_directory skips a.rs (already in self.files).
        ws.scan_directory(dir.path(), None);

        // Definitions for all files should be present.
        assert_eq!(ws.find_definitions("shared").len(), 1);
        assert_eq!(ws.find_definitions("caller_b").len(), 1);
        assert_eq!(ws.find_definitions("caller_c").len(), 1);

        // Critical: find_references for "shared" must include ALL 3 files.
        // Before the fix, a.rs was missing from the word index because
        // collect_paths skipped it, so only b.rs and c.rs were found.
        let refs = ws.find_references("shared");
        assert!(
            refs.len() >= 3,
            "shared should have >= 3 references (def in a + call in b + call in c), got {}",
            refs.len()
        );
    }
}
