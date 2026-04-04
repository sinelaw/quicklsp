# Word Index + Scope-Aware Tokenizer Plan

## Goal

Replace in-memory source text storage with an on-disk word index.
Extend the tokenizer to emit all identifiers (not just definitions)
with lightweight scope/visibility context.

Current state: 3.1 GB RSS for the linux kernel (65K files). Target: ~85 MB.

## Architecture overview

```
                    ┌─────────────────┐
                    │   Index File    │  on disk, seek-based reads
                    │  (sorted word   │
                    │   occurrences)  │
                    └────────┬────────┘
                             │ seek + read
    ┌────────────────────────┼────────────────────────┐
    │                        │                        │
┌───┴────┐            ┌──────┴──────┐          ┌──────┴──────┐
│  Defs  │            │   Word Dir  │          │  Open Files │
│DashMap │ in memory  │  in memory  │          │  DashMap    │ editor-open only
│name→loc│            │ word→offset │          │ path→source │
└────────┘            └─────────────┘          └─────────────┘
```

## Phase 1: Scope-aware tokenizer

Extend the existing single-pass tokenizer to track:

### Brace depth

Increment on `{`, decrement on `}`. Already skip braces inside
strings/comments, so this is ~5 lines of state.

- depth 0 = module level
- depth 1+ = nested (function body, impl block, class body, etc.)

For Python: track indentation level instead. Maintain a stack of
indentation widths. Dedent = scope pop. ~30 extra lines.

### Visibility

Detect visibility keywords immediately before definitions:

| Language | Public | Private |
|----------|--------|---------|
| Rust | `pub`, `pub(crate)`, `pub(super)` | (default) |
| JS/TS | `export` | (default) |
| Go | first char uppercase | first char lowercase |
| Python | no `_` prefix | `_` prefix |
| C/C++ | `public:` (in class) | `private:`, `protected:` |
| Java/C# | `public` | `private`, `protected`, (default) |
| Ruby | after `public`/`private` marker | (default) |

Implementation: when scanning identifiers, check if the previous
non-whitespace token was a visibility keyword. Store as a simple
enum: `Public`, `Private`, `Unknown`. Don't try to get it 100% right —
`Unknown` is a safe default that doesn't affect ranking negatively.

### Container names

Track the most recent named scope opener:

```
impl Config {        → push container "Config"
    fn new() {}      → container = "Config"
}                    → pop container

class Handler {      → push container "Handler"
    process() {}     → container = "Handler"
}                    → pop
```

Implementation: maintain a small stack (capacity ~8) of
`(name, brace_depth)` pairs. On `}` that matches a named scope's
depth, pop it. Current container = top of stack or None.

Named scope openers: `impl`, `class`, `struct`, `enum`, `trait`,
`interface`, `module`, `namespace`, `mod`. When we see one of these
followed by a name followed by `{`, push `(name, current_depth)`.

### All-identifier emission

Currently the tokenizer only emits `DefKeyword` + `Ident` pairs.
Extend to also emit a stream of all identifier occurrences:

```rust
pub struct ScanResult {
    pub tokens: Vec<Token>,              // definition keyword+ident pairs (existing)
    pub occurrences: Vec<Occurrence>,     // every identifier occurrence (new)
}

pub struct Occurrence {
    pub word: String,       // or interned index
    pub line: usize,
    pub col: usize,         // byte offset from line start
    pub len: usize,
    pub role: OccurrenceRole,
}

pub enum OccurrenceRole {
    Definition,
    Reference,
}
```

This is gathered alongside the existing token extraction — same loop,
same pass. Every time `consume_identifier` runs, emit an `Occurrence`.
Definition-introducing identifiers also get their `Token` as before.

### Updated Symbol struct

```rust
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub line: usize,
    pub col: usize,
    pub def_keyword: String,
    pub doc_comment: Option<String>,
    pub signature: Option<String>,
    // new fields:
    pub visibility: Visibility,
    pub container: Option<String>,
    pub depth: usize,
}

pub enum Visibility {
    Public,
    Private,
    Unknown,
}
```

### Complexity estimate

All changes are within the existing `scan()` function and its helpers.
No new passes, no AST allocation, no grammar files.

- Brace depth: ~5 lines
- Visibility detection: ~30 lines
- Container stack: ~40 lines
- Python indentation scope: ~30 lines
- All-identifier emission: ~20 lines
- New structs/enums: ~30 lines

Total: ~150 lines of new code in the tokenizer.

## Phase 2: On-disk word index

### Index file format

```
[header]                    (fixed size, 32 bytes)
  magic: b"QLSP\x01\x00\x00\x00"
  entry_count: u64
  dir_offset: u64           byte offset to word directory
  dir_count: u64            number of words in directory

[entries section]           (sorted by word, then path, then line)
  Each entry is variable-length:
    word_len: u16
    word: [u8; word_len]
    path_len: u16
    path: [u8; path_len]
    line: u32
    col: u32
    len: u16

[word directory]            (sorted by word, fixed-size entries)
  Each entry:
    word_len: u16
    word: [u8; word_len]
    first_entry_offset: u64     byte offset into entries section
    entry_count: u32
```

The word directory is loaded into memory at startup (~2-4 MB for
linux kernel). The entries section stays on disk.

### Build process

During `scan_directory`:

1. Parallel tokenize all files (same as now)
2. Each thread collects `Occurrence` tuples into a thread-local Vec
3. After parallel phase: merge all Vecs, sort by (word, path, line)
4. Sequential write to index file (entries section)
5. Build word directory from sorted entries, append to file
6. Load word directory into memory

### Seek-based lookup

```rust
fn find_references(&self, name: &str) -> Vec<Reference> {
    // 1. Look up word in in-memory directory
    let (offset, count) = match self.word_dir.get(name) {
        Some(entry) => (entry.offset, entry.count),
        None => return Vec::new(),
    };

    // 2. Seek to offset in index file
    let mut file = File::open(&self.index_path).unwrap();
    file.seek(SeekFrom::Start(offset)).unwrap();

    // 3. Read exactly `count` entries
    let mut refs = Vec::with_capacity(count as usize);
    let mut reader = BufReader::new(file);
    for _ in 0..count {
        refs.push(read_entry(&mut reader));
    }
    refs
}
```

RAM used per query: just the result set. I/O: one seek + sequential
read of matching entries.

### Index location

Store in the project directory:

```
.quicklsp/
  index.qlsp          word index file
  meta.json            index version, project mtime, file count
```

Or in a global cache dir:

```
~/.cache/quicklsp/<project-hash>/
  index.qlsp
  meta.json
```

Global cache is cleaner — doesn't pollute the project directory.

## Phase 3: Drop in-memory source text

Remove `source: String` from `FileEntry`. Replace with:

```rust
struct FileEntry {
    symbols: Vec<Symbol>,
    lang: Option<LangFamily>,
    mtime: SystemTime,
}
```

Source text is only kept for editor-open files:

```rust
struct Workspace {
    files: DashMap<PathBuf, FileEntry>,       // metadata + symbols
    open_sources: DashMap<PathBuf, String>,    // editor-open files only
    definitions: DashMap<String, Vec<SymbolLocation>>,
    fuzzy: RwLock<DeletionIndex>,
    word_dir: WordDirectory,                   // in-memory directory
    index_path: PathBuf,                       // path to index file
}
```

Operations that need source text:
- `word_at_position`: reads from `open_sources` (always available — editor sent it via did_open)
- `signature_help_at`: reads from `open_sources`
- `find_references`: reads from on-disk index (no source needed)
- `hover`: reads pre-extracted signature/doc_comment from Symbol

## Phase 4: Persistence

On startup:
1. Check if `meta.json` exists and project mtime matches
2. If yes: load word directory from index file, rebuild in-memory
   definitions from the entries (or store definitions separately
   in the index). Skip tokenization entirely. ~100-500ms startup.
3. If no: full re-index (current behavior)

Incremental updates (`did_change` / `did_save`):
- Re-tokenize the changed file
- Update in-memory definitions
- Mark old entries in index as stale (don't compact immediately)
- Append new entries to end of file
- Update word directory in memory
- Periodic compaction: rewrite index file without stale entries

## Phase 5: Use visibility + container in LSP responses

With the enriched symbol data:

- `document_symbol`: return `containerName` field (LSP spec supports it)
- `workspace_symbol`: rank public symbols higher than private
- `completion`: rank visible symbols higher (same-file private, public from other files)
- `goto_definition`: prefer same-container definitions when qualifier matches

## Memory budget (projected)

| Component | Linux kernel | Normal project |
|-----------|-------------|----------------|
| Definitions | ~50 MB | ~2 MB |
| Fuzzy trigram index | ~20 MB | ~1 MB |
| File metadata | ~10 MB | ~0.1 MB |
| Word directory | ~4 MB | ~0.1 MB |
| Open file sources | ~1 MB | ~1 MB |
| **Total RSS** | **~85 MB** | **~5 MB** |
| On-disk index | ~300-500 MB | ~5-20 MB |

## Implementation order

```
Phase 1 ──→ Phase 2 ──→ Phase 3 ──→ Phase 4
  │              │           │           │
  │ tokenizer    │ on-disk   │ drop RAM  │ persistence
  │ scope+vis    │ word idx  │ source    │ warm startup
  │ all idents   │ seek read │           │ incremental
  │              │           │           │
  └──── can ship ┘─── can ship ─── can ship ────┘
        alone          alone        alone

Phase 5 (use visibility in LSP) can happen anytime after Phase 1.
```

Each phase is independently shippable and improves the system.
Phase 1 improves symbol quality even without the word index.
Phase 2+3 solve the memory problem.
Phase 4 solves startup latency.
