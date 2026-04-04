//! Lightweight tokenizer for extracting identifiers and keywords from source code.
//!
//! This is a hand-written scanner (not a parser) that identifies:
//! - Keywords that introduce definitions (fn, def, class, struct, etc.)
//! - The identifier immediately following such keywords
//! - String/comment boundaries (to skip false matches inside them)
//!
//! It operates as a single-pass state machine with O(n) time and O(1) memory
//! per file (no AST, no grammar, no allocations beyond the output Vec).
//!
//! ## Unicode Support
//!
//! The scanner correctly handles Unicode identifiers (e.g., `données`, `名前`,
//! `über_config`) by switching to char-boundary-aware iteration when it encounters
//! non-ASCII bytes in identifier positions. Definition keywords are always ASCII,
//! so the hot path for keyword matching stays byte-level for performance.

/// Thread-local tokenizer counters for profiling diagnostics.
/// Plain integer increments — no atomics, no cache line contention.
/// Call `stats::summary()` to collect totals from all threads.
pub mod stats {
    use std::cell::Cell;
    use std::sync::Mutex;

    /// Per-thread counters.
    #[derive(Default)]
    struct Counters {
        scan_calls: u64,
        total_bytes: u64,
        ident_calls: u64,
        ident_ascii_only: u64,
        ident_hit_unicode: u64,
        ident_unicode_chars: u64,
        skipped_comment_bytes: u64,
        skipped_string_bytes: u64,
        non_ident_unicode_skips: u64,
    }

    /// Registry of all thread-local counter snapshots, collected on drop.
    static REGISTRY: Mutex<Vec<Counters>> = Mutex::new(Vec::new());

    thread_local! {
        static LOCAL: Cell<Counters> = const { Cell::new(Counters {
            scan_calls: 0,
            total_bytes: 0,
            ident_calls: 0,
            ident_ascii_only: 0,
            ident_hit_unicode: 0,
            ident_unicode_chars: 0,
            skipped_comment_bytes: 0,
            skipped_string_bytes: 0,
            non_ident_unicode_skips: 0,
        }) };
    }

    /// Flush the current thread's counters into the global registry.
    /// Called automatically, but can be called explicitly before `summary()`.
    pub fn flush() {
        LOCAL.with(|cell| {
            let c = cell.take();
            if c.scan_calls > 0 || c.total_bytes > 0 {
                if let Ok(mut reg) = REGISTRY.lock() {
                    reg.push(c);
                }
            }
        });
    }

    pub fn reset() {
        LOCAL.with(|cell| cell.set(Counters::default()));
        if let Ok(mut reg) = REGISTRY.lock() {
            reg.clear();
        }
    }

    pub fn summary() -> String {
        // Flush current thread first
        flush();

        let reg = REGISTRY.lock().unwrap();
        let mut total_bytes = 0u64;
        let mut scan_calls = 0u64;
        let mut ident_calls = 0u64;
        let mut ident_ascii = 0u64;
        let mut ident_unicode = 0u64;
        let mut unicode_chars = 0u64;
        let mut comment = 0u64;
        let mut string = 0u64;
        let mut non_ident_skips = 0u64;

        for c in reg.iter() {
            scan_calls += c.scan_calls;
            total_bytes += c.total_bytes;
            ident_calls += c.ident_calls;
            ident_ascii += c.ident_ascii_only;
            ident_unicode += c.ident_hit_unicode;
            unicode_chars += c.ident_unicode_chars;
            comment += c.skipped_comment_bytes;
            string += c.skipped_string_bytes;
            non_ident_skips += c.non_ident_unicode_skips;
        }

        let pct = |n: u64, d: u64| if d == 0 { 0.0 } else { n as f64 / d as f64 * 100.0 };

        format!(
            "Scans: {}, Bytes: {} ({:.1} MB)\n\
             Skip: comments {} ({:.1}%), strings {} ({:.1}%)\n\
             Idents: {} total, {} ascii ({:.1}%), {} unicode ({:.1}%), {} unicode chars\n\
             Non-ident unicode skips: {}",
            scan_calls,
            total_bytes, total_bytes as f64 / 1_048_576.0,
            comment, pct(comment, total_bytes), string, pct(string, total_bytes),
            ident_calls, ident_ascii, pct(ident_ascii, ident_calls),
            ident_unicode, pct(ident_unicode, ident_calls),
            unicode_chars, non_ident_skips,
        )
    }

    // --- Inline increment helpers (no atomics, just Cell::get/set) ---

    macro_rules! inc {
        ($field:ident, $val:expr) => {
            LOCAL.with(|cell| {
                let mut c = cell.take();
                c.$field += $val as u64;
                cell.set(c);
            });
        };
    }

    #[inline(always)]
    pub(super) fn scan_call(bytes: u64) {
        inc!(scan_calls, 1);
        inc!(total_bytes, bytes);
    }
    #[inline(always)]
    pub(super) fn comment_bytes(n: u64) { inc!(skipped_comment_bytes, n); }
    #[inline(always)]
    pub(super) fn string_bytes(n: u64) { inc!(skipped_string_bytes, n); }
    #[inline(always)]
    pub(super) fn ident_ascii() { inc!(ident_ascii_only, 1); }
    #[inline(always)]
    pub(super) fn ident_unicode(chars: u64) {
        inc!(ident_hit_unicode, 1);
        inc!(ident_unicode_chars, chars);
    }
    #[inline(always)]
    pub(super) fn ident_call() { inc!(ident_calls, 1); }
    #[inline(always)]
    pub(super) fn non_ident_unicode_skip() { inc!(non_ident_unicode_skips, 1); }
}

/// A token extracted by the scanner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub text: String,
    pub line: usize,
    pub col: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// A keyword that introduces a definition (fn, def, class, struct, etc.)
    DefKeyword,
    /// An identifier (variable name, function name, etc.)
    Ident,
}

/// Visibility of a definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Visibility {
    Public,
    Private,
    Unknown,
}

/// Role of an identifier occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OccurrenceRole {
    Definition,
    Reference,
}

/// A single identifier occurrence found during scanning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    pub word: String,
    pub line: usize,
    pub col: usize,
    pub len: usize,
    pub role: OccurrenceRole,
}

/// Result of scanning a source file.
#[derive(Debug, Clone)]
pub struct ScanResult {
    /// Definition keyword+ident pairs (existing behavior).
    pub tokens: Vec<Token>,
    /// Every identifier occurrence (new).
    pub occurrences: Vec<Occurrence>,
}

/// Context tracked per definition during scanning.
#[derive(Debug, Clone)]
pub struct DefContext {
    pub visibility: Visibility,
    pub container: Option<String>,
    pub depth: usize,
}

/// Language family determines comment/string syntax and definition keywords.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LangFamily {
    /// Rust: fn, struct, enum, trait, type, const, static, mod
    Rust,
    /// C/C++: struct, enum, class, typedef, define
    CLike,
    /// Go: func, type, var, const
    Go,
    /// Python: def, class
    Python,
    /// JavaScript/TypeScript: function, class, const, let, var, interface, type, enum
    JsTs,
    /// Java/C#: class, interface, enum, record
    JavaCSharp,
    /// Ruby: def, class, module
    Ruby,
}

impl LangFamily {
    /// Detect language family from file extension.
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext {
            "rs" => Some(Self::Rust),
            "c" | "h" | "cpp" | "cc" | "cxx" | "hpp" | "hxx" => Some(Self::CLike),
            "go" => Some(Self::Go),
            "py" | "pyi" => Some(Self::Python),
            "js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs" | "mts" => Some(Self::JsTs),
            "java" | "cs" => Some(Self::JavaCSharp),
            "rb" => Some(Self::Ruby),
            _ => None,
        }
    }

    /// Keywords that introduce a named definition in this language family.
    fn def_keywords(&self) -> &[&str] {
        match self {
            Self::Rust => &[
                "fn", "struct", "enum", "trait", "type", "const", "static", "mod",
            ],
            Self::CLike => &["struct", "enum", "class", "union", "typedef", "namespace"],
            Self::Go => &["func", "type", "var", "const"],
            Self::Python => &["def", "class"],
            Self::JsTs => &[
                "function",
                "class",
                "const",
                "let",
                "var",
                "interface",
                "type",
                "enum",
            ],
            Self::JavaCSharp => &["class", "interface", "enum", "record", "struct"],
            Self::Ruby => &["def", "class", "module"],
        }
    }

    /// Does this language use `#` for line comments?
    fn hash_line_comment(&self) -> bool {
        matches!(self, Self::Python | Self::Ruby)
    }

    /// Does this language use `//` for line comments?
    fn slash_line_comment(&self) -> bool {
        !matches!(self, Self::Python | Self::Ruby)
    }

    /// Does this language use `/* */` block comments?
    fn block_comments(&self) -> bool {
        !matches!(self, Self::Python | Self::Ruby)
    }

    /// Does this language have triple-quoted strings?
    fn triple_quote_strings(&self) -> bool {
        matches!(self, Self::Python)
    }

    /// Does this language use indentation-based scoping (Python)?
    fn indent_scoping(&self) -> bool {
        matches!(self, Self::Python)
    }

    /// Keywords that open named scopes (containers).
    fn scope_openers(&self) -> &[&str] {
        match self {
            Self::Rust => &["impl", "struct", "enum", "trait", "mod"],
            Self::CLike => &["class", "struct", "enum", "namespace"],
            Self::Go => &[],
            Self::Python => &["class"],
            Self::JsTs => &["class", "interface", "enum", "namespace"],
            Self::JavaCSharp => &["class", "interface", "enum"],
            Self::Ruby => &["class", "module"],
        }
    }

    /// Check if a token is a visibility keyword, returning the visibility.
    fn check_visibility(&self, word: &str) -> Option<Visibility> {
        match self {
            Self::Rust => match word {
                "pub" => Some(Visibility::Public),
                _ => None,
            },
            Self::JsTs => match word {
                "export" => Some(Visibility::Public),
                _ => None,
            },
            Self::JavaCSharp => match word {
                "public" => Some(Visibility::Public),
                "private" | "protected" => Some(Visibility::Private),
                _ => None,
            },
            Self::CLike => match word {
                // In C++ class context: public/private/protected are markers
                "public" => Some(Visibility::Public),
                "private" | "protected" => Some(Visibility::Private),
                _ => None,
            },
            Self::Ruby => match word {
                "public" => Some(Visibility::Public),
                "private" | "protected" => Some(Visibility::Private),
                _ => None,
            },
            Self::Go | Self::Python => None,
        }
    }
}

/// Check if a character can continue an identifier (Unicode-aware).
/// Matches: ASCII alphanumerics, `_`, and Unicode letters/numbers.
#[inline]
fn is_ident_continue_char(ch: char) -> bool {
    ch == '_' || ch.is_alphanumeric()
}

/// Check if a byte can start an identifier (ASCII fast path).
/// Returns true for ASCII letters, `_`, and UTF-8 leading bytes (0xC0+).
/// Continuation bytes (0x80..0xBF) are NOT identifier starts.
#[inline]
fn is_ident_start_byte(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b >= 0xC0
}

/// Check if a byte can continue an identifier (ASCII fast path).
/// Returns true for ASCII alphanumerics, `_`, and any high byte (>= 0x80)
/// since continuation bytes are valid inside a multi-byte identifier.
#[inline]
fn is_ident_continue_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// Scan source code and extract definition-introducing keyword + identifier pairs.
///
/// Returns tokens in document order. Each `DefKeyword` token is followed by
/// an `Ident` token if an identifier was found after the keyword.
pub fn scan(source: &str, lang: LangFamily) -> Vec<Token> {
    scan_full(source, lang).0.tokens
}

/// Scan source code and return full results including all identifier occurrences,
/// scope depth, visibility, and container context, plus definition contexts.
pub fn scan_full(source: &str, lang: LangFamily) -> (ScanResult, Vec<DefContext>) {
    let bytes = source.as_bytes();
    let len = bytes.len();
    let def_keywords = lang.def_keywords();
    let scope_openers = lang.scope_openers();

    stats::scan_call(len as u64);

    let mut tokens = Vec::new();
    let mut occurrences = Vec::new();
    let mut i = 0;
    let mut line = 0usize;
    let mut line_start = 0usize;

    // Scope tracking
    let mut brace_depth: usize = 0;

    // Container stack: (name, brace_depth_when_opened)
    let mut container_stack: Vec<(String, usize)> = Vec::with_capacity(8);

    // Visibility: the last visibility keyword seen (reset after consuming a def)
    let mut pending_visibility: Option<Visibility> = None;

    // For detecting scope openers: track last identifier that could be a scope name
    // Format: (keyword, after_keyword_byte_pos)
    let mut pending_scope_opener: Option<String> = None;

    // Python indentation tracking
    let mut indent_stack: Vec<usize> = if lang.indent_scoping() {
        vec![0]
    } else {
        Vec::new()
    };
    let mut py_at_line_start = true;

    // Def context map: indexed by token pair index (tokens.len() / 2 at time of push)
    // Stored parallel to definition tokens
    let mut def_contexts: Vec<DefContext> = Vec::new();

    while i < len {
        let b = bytes[i];

        // Track line numbers
        if b == b'\n' {
            line += 1;
            line_start = i + 1;
            i += 1;
            if lang.indent_scoping() {
                py_at_line_start = true;
            }
            continue;
        }

        // Python indentation tracking at line start
        if lang.indent_scoping() && py_at_line_start && !b.is_ascii_whitespace() && b != b'#' {
            let indent = i - line_start;
            // Pop scopes that have ended (dedented)
            while indent_stack.len() > 1 && indent <= indent_stack[indent_stack.len() - 1] {
                indent_stack.pop();
                if let Some(pos) = container_stack.iter().rposition(|(_,d)| *d >= indent_stack.len()) {
                    container_stack.truncate(pos);
                }
            }
            brace_depth = indent_stack.len().saturating_sub(1);
            py_at_line_start = false;
        }

        // Skip whitespace (ASCII whitespace only)
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }

        // Skip line comments
        if lang.hash_line_comment() && b == b'#' {
            let before = i;
            i = skip_to_eol(bytes, i);
            stats::comment_bytes((i - before) as u64);
            continue;
        }
        if lang.slash_line_comment() && b == b'/' && i + 1 < len && bytes[i + 1] == b'/' {
            let before = i;
            i = skip_to_eol(bytes, i);
            stats::comment_bytes((i - before) as u64);
            continue;
        }

        // Skip block comments
        if lang.block_comments() && b == b'/' && i + 1 < len && bytes[i + 1] == b'*' {
            let before = i;
            i = skip_block_comment(bytes, i + 2, &mut line, &mut line_start);
            stats::comment_bytes((i - before) as u64);
            continue;
        }

        // Skip triple-quoted strings (Python)
        if lang.triple_quote_strings()
            && i + 2 < len
            && ((b == b'"' && bytes[i + 1] == b'"' && bytes[i + 2] == b'"')
                || (b == b'\'' && bytes[i + 1] == b'\'' && bytes[i + 2] == b'\''))
        {
            let before = i;
            let quote = b;
            i = skip_triple_quote(bytes, i + 3, quote, &mut line, &mut line_start);
            stats::string_bytes((i - before) as u64);
            continue;
        }

        // Skip strings
        if b == b'"' || b == b'\'' {
            if b == b'\'' && matches!(lang, LangFamily::Rust) {
                // Rust: single quote is lifetime or char literal
                i += 1;
                if i < len && bytes[i] == b'\\' {
                    i += 2;
                }
                if i < len {
                    i += 1;
                }
                if i < len && bytes[i] == b'\'' {
                    i += 1;
                }
                continue;
            }
            let before = i;
            i = skip_string(bytes, i + 1, b, &mut line, &mut line_start);
            stats::string_bytes((i - before) as u64);
            continue;
        }

        // Skip backtick template literals (JS/TS)
        if b == b'`' && matches!(lang, LangFamily::JsTs) {
            let before = i;
            i = skip_string(bytes, i + 1, b'`', &mut line, &mut line_start);
            stats::string_bytes((i - before) as u64);
            continue;
        }

        // Track brace depth (not for Python)
        if !lang.indent_scoping() {
            if b == b'{' {
                // If we have a pending scope opener + name, push container
                if let Some(ref name) = pending_scope_opener {
                    container_stack.push((name.clone(), brace_depth));
                    pending_scope_opener = None;
                }
                brace_depth += 1;
                i += 1;
                continue;
            }
            if b == b'}' {
                brace_depth = brace_depth.saturating_sub(1);
                // Pop containers whose depth matches
                while let Some((_, d)) = container_stack.last() {
                    if *d >= brace_depth {
                        container_stack.pop();
                    } else {
                        break;
                    }
                }
                i += 1;
                continue;
            }
        }

        // Python colon starts a new scope
        if lang.indent_scoping() && b == b':' {
            if let Some(ref name) = pending_scope_opener {
                // Next indented block will be inside this container
                container_stack.push((name.clone(), indent_stack.len()));
                // Push a new indent level (will be adjusted when we see the actual indent)
                indent_stack.push(indent_stack.last().copied().unwrap_or(0) + 1);
                brace_depth = indent_stack.len().saturating_sub(1);
                pending_scope_opener = None;
            }
            i += 1;
            continue;
        }

        // Identifier or keyword
        if is_ident_start_byte(b) {
            let start = i;
            let col = i - line_start;

            i = consume_identifier(source, i);

            // Non-alphanumeric Unicode chars
            if i == start {
                stats::non_ident_unicode_skip();
                i += utf8_char_len(b);
                continue;
            }

            let word = &source[start..i];
            let is_ascii_word = word.is_ascii();

            // Check for visibility keywords
            if is_ascii_word {
                if let Some(vis) = lang.check_visibility(word) {
                    pending_visibility = Some(vis);
                    // Don't emit visibility keywords as occurrences
                    continue;
                }
            }

            // Check for scope opener keywords (impl, class, struct, etc.)
            if is_ascii_word && scope_openers.contains(&word) {
                // The next identifier after this keyword is the container name
                let saved_i = i;
                let saved_line = line;
                let saved_line_start = line_start;
                let peek_i = skip_to_ident(bytes, i, lang, &mut line, &mut line_start);
                if peek_i < len && is_ident_start_byte(bytes[peek_i]) {
                    let name_start = peek_i;
                    let peek_end = consume_identifier(source, peek_i);
                    let name = &source[name_start..peek_end];
                    if !(name.is_ascii() && is_noise_word(name, lang)) {
                        pending_scope_opener = Some(name.to_string());
                    }
                }
                // Restore position — we only peeked
                i = saved_i;
                line = saved_line;
                line_start = saved_line_start;
            }

            // Go visibility: uppercase first char = public
            let go_visibility = if matches!(lang, LangFamily::Go) && is_ascii_word {
                if word.starts_with(|c: char| c.is_ascii_uppercase()) {
                    Some(Visibility::Public)
                } else {
                    Some(Visibility::Private)
                }
            } else {
                None
            };

            // Python visibility: _ prefix = private
            let py_visibility = if matches!(lang, LangFamily::Python) {
                if word.starts_with('_') {
                    Some(Visibility::Private)
                } else {
                    Some(Visibility::Public)
                }
            } else {
                None
            };

            if is_ascii_word && def_keywords.contains(&word) {
                tokens.push(Token {
                    kind: TokenKind::DefKeyword,
                    text: word.to_string(),
                    line,
                    col,
                });

                // Skip whitespace/punctuation to find the identifier name
                i = skip_to_ident(bytes, i, lang, &mut line, &mut line_start);
                if i < len && is_ident_start_byte(bytes[i]) {
                    let name_start = i;
                    let name_col = i - line_start;
                    i = consume_identifier(source, i);
                    let name = &source[name_start..i];
                    // Skip language noise identifiers
                    if !(name.is_ascii() && is_noise_word(name, lang)) {
                        tokens.push(Token {
                            kind: TokenKind::Ident,
                            text: name.to_string(),
                            line,
                            col: name_col,
                        });

                        // Determine visibility for this definition
                        let vis = pending_visibility
                            .take()
                            .or(go_visibility)
                            .or(py_visibility)
                            .unwrap_or(Visibility::Unknown);

                        let container = container_stack.last().map(|(n, _)| n.clone());

                        def_contexts.push(DefContext {
                            visibility: vis,
                            container,
                            depth: brace_depth,
                        });

                        // Emit definition occurrence
                        occurrences.push(Occurrence {
                            word: name.to_string(),
                            line,
                            col: name_col,
                            len: name.len(),
                            role: OccurrenceRole::Definition,
                        });
                    } else {
                        pending_visibility = None;
                    }
                } else {
                    pending_visibility = None;
                }
            } else {
                // Not a definition keyword — emit as reference occurrence
                pending_visibility = None;
                occurrences.push(Occurrence {
                    word: word.to_string(),
                    line,
                    col,
                    len: word.len(),
                    role: OccurrenceRole::Reference,
                });
            }
            continue;
        }

        // Skip non-ASCII bytes that aren't identifier starts
        if b >= 0x80 {
            i += utf8_char_len(b);
            continue;
        }

        // Skip everything else (ASCII operators, punctuation, etc.)
        i += 1;
    }

    (ScanResult { tokens, occurrences }, def_contexts)
}

/// Alias for scan_full for backward compatibility.
pub fn scan_with_contexts(source: &str, lang: LangFamily) -> (ScanResult, Vec<DefContext>) {
    scan_full(source, lang)
}

/// Consume a full identifier starting at byte position `i`, returning the new
/// byte position after the identifier.
///
/// Handles both ASCII-only identifiers (fast path, byte-level) and identifiers
/// containing Unicode characters (char-level iteration).
fn consume_identifier(source: &str, start: usize) -> usize {
    let bytes = source.as_bytes();
    let mut i = start;

    stats::ident_call();

    // Fast path: pure ASCII identifier
    while i < bytes.len() && bytes[i] < 0x80 {
        if is_ident_continue_byte(bytes[i]) {
            i += 1;
        } else {
            stats::ident_ascii();
            return i;
        }
    }

    // If we hit a high byte, switch to char-level iteration for the rest
    if i < bytes.len() && bytes[i] >= 0x80 {
        let mut uc = 0u64;
        // Continue consuming from position i using chars
        for ch in source[i..].chars() {
            if is_ident_continue_char(ch) {
                uc += 1;
                i += ch.len_utf8();
            } else {
                break;
            }
        }
        stats::ident_unicode(uc);
    } else {
        stats::ident_ascii();
    }

    i
}

/// Return the length of a UTF-8 character from its first byte.
#[inline]
fn utf8_char_len(first_byte: u8) -> usize {
    match first_byte {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xFF => 4,
        _ => 1, // invalid, advance 1 to avoid infinite loop
    }
}

// --- Scanning helpers (byte-level, only used for ASCII constructs) ---

fn skip_to_eol(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i] != b'\n' {
        i += 1;
    }
    i
}

fn skip_block_comment(
    bytes: &[u8],
    mut i: usize,
    line: &mut usize,
    line_start: &mut usize,
) -> usize {
    while i + 1 < bytes.len() {
        if bytes[i] == b'\n' {
            *line += 1;
            *line_start = i + 1;
        }
        if bytes[i] == b'*' && bytes[i + 1] == b'/' {
            return i + 2;
        }
        i += 1;
    }
    bytes.len()
}

fn skip_string(
    bytes: &[u8],
    mut i: usize,
    quote: u8,
    line: &mut usize,
    line_start: &mut usize,
) -> usize {
    while i < bytes.len() {
        if bytes[i] == b'\n' {
            *line += 1;
            *line_start = i + 1;
            if quote != b'`' {
                return i;
            }
        }
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == quote {
            return i + 1;
        }
        i += 1;
    }
    bytes.len()
}

fn skip_triple_quote(
    bytes: &[u8],
    mut i: usize,
    quote: u8,
    line: &mut usize,
    line_start: &mut usize,
) -> usize {
    while i + 2 < bytes.len() {
        if bytes[i] == b'\n' {
            *line += 1;
            *line_start = i + 1;
        }
        if bytes[i] == quote && bytes[i + 1] == quote && bytes[i + 2] == quote {
            return i + 3;
        }
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        i += 1;
    }
    bytes.len()
}

/// Skip whitespace, newlines, and minor punctuation between a keyword and its name.
fn skip_to_ident(
    bytes: &[u8],
    mut i: usize,
    lang: LangFamily,
    line: &mut usize,
    line_start: &mut usize,
) -> usize {
    // Skip whitespace first
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        if bytes[i] == b'\n' {
            *line += 1;
            *line_start = i + 1;
        }
        i += 1;
    }

    // In Go, `func (r *Receiver) Name()` — skip the receiver
    if matches!(lang, LangFamily::Go) && i < bytes.len() && bytes[i] == b'(' {
        let mut depth = 0;
        while i < bytes.len() {
            if bytes[i] == b'(' {
                depth += 1;
            } else if bytes[i] == b')' {
                depth -= 1;
                if depth == 0 {
                    i += 1;
                    break;
                }
            } else if bytes[i] == b'\n' {
                *line += 1;
                *line_start = i + 1;
            }
            i += 1;
        }
    }

    // Skip whitespace again
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        if bytes[i] == b'\n' {
            *line += 1;
            *line_start = i + 1;
        }
        i += 1;
    }

    // Skip pointer/reference modifiers: `*`, `&`
    while i < bytes.len() && matches!(bytes[i], b'*' | b'&') {
        i += 1;
    }

    // Skip whitespace again
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        if bytes[i] == b'\n' {
            *line += 1;
            *line_start = i + 1;
        }
        i += 1;
    }

    i
}

/// Filter out noise words that aren't real symbol names.
fn is_noise_word(word: &str, lang: LangFamily) -> bool {
    match lang {
        LangFamily::JsTs => matches!(
            word,
            "async" | "export" | "default" | "abstract" | "static" | "readonly" | "declare" | "new"
        ),
        LangFamily::Rust => matches!(
            word,
            "pub" | "crate" | "super" | "self" | "unsafe" | "async" | "mut" | "ref" | "dyn"
        ),
        LangFamily::CLike => matches!(
            word,
            "static"
                | "inline"
                | "extern"
                | "volatile"
                | "const"
                | "unsigned"
                | "signed"
                | "long"
                | "short"
        ),
        LangFamily::Go => matches!(word, "chan" | "map" | "error"),
        LangFamily::Python => matches!(word, "async"),
        LangFamily::JavaCSharp => matches!(
            word,
            "public"
                | "private"
                | "protected"
                | "static"
                | "final"
                | "abstract"
                | "sealed"
                | "partial"
                | "virtual"
                | "override"
                | "new"
        ),
        LangFamily::Ruby => matches!(word, "self"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract_names(source: &str, lang: LangFamily) -> Vec<String> {
        scan(source, lang)
            .into_iter()
            .filter(|t| t.kind == TokenKind::Ident)
            .map(|t| t.text)
            .collect()
    }

    #[test]
    fn rust_functions() {
        let src = r#"
fn main() {}
pub fn process_data(x: i32) -> bool { true }
fn helper() {}
"#;
        assert_eq!(
            extract_names(src, LangFamily::Rust),
            vec!["main", "process_data", "helper"]
        );
    }

    #[test]
    fn rust_structs_enums() {
        let src = r#"
struct Config { field: u32 }
enum Color { Red, Green, Blue }
trait Drawable { fn draw(&self); }
type Alias = Vec<u8>;
"#;
        let names = extract_names(src, LangFamily::Rust);
        assert!(names.contains(&"Config".to_string()));
        assert!(names.contains(&"Color".to_string()));
        assert!(names.contains(&"Drawable".to_string()));
        assert!(names.contains(&"Alias".to_string()));
    }

    #[test]
    fn python_defs() {
        let src = r#"
def process_data(x):
    pass

class MyClass:
    def method(self):
        pass
"#;
        let names = extract_names(src, LangFamily::Python);
        assert_eq!(names, vec!["process_data", "MyClass", "method"]);
    }

    #[test]
    fn python_skips_comments_and_strings() {
        let src = r#"
# def not_a_function():
"""def also_not()"""
def real_function():
    x = "def fake_in_string()"
    pass
"#;
        let names = extract_names(src, LangFamily::Python);
        assert_eq!(names, vec!["real_function"]);
    }

    #[test]
    fn javascript_functions() {
        let src = r#"
function processData(input) {}
class EventEmitter {}
const MAX_SIZE = 100;
let counter = 0;
"#;
        let names = extract_names(src, LangFamily::JsTs);
        assert_eq!(
            names,
            vec!["processData", "EventEmitter", "MAX_SIZE", "counter"]
        );
    }

    #[test]
    fn go_functions() {
        let src = r#"
func main() {}
func (s *Server) handleRequest(w http.ResponseWriter) {}
type Config struct {}
var globalState int
const MaxRetries = 3
"#;
        let names = extract_names(src, LangFamily::Go);
        assert!(names.contains(&"main".to_string()));
        assert!(names.contains(&"handleRequest".to_string()));
        assert!(names.contains(&"Config".to_string()));
        assert!(names.contains(&"globalState".to_string()));
        assert!(names.contains(&"MaxRetries".to_string()));
    }

    #[test]
    fn c_structs() {
        let src = r#"
struct Point { int x; int y; };
enum Color { RED, GREEN, BLUE };
class Widget { public: void draw(); };
"#;
        let names = extract_names(src, LangFamily::CLike);
        assert!(names.contains(&"Point".to_string()));
        assert!(names.contains(&"Color".to_string()));
        assert!(names.contains(&"Widget".to_string()));
    }

    #[test]
    fn skips_block_comments() {
        let src = r#"
/* fn not_a_function() {} */
fn real_function() {}
"#;
        let names = extract_names(src, LangFamily::Rust);
        assert_eq!(names, vec!["real_function"]);
    }

    #[test]
    fn skips_strings() {
        let src = r#"
fn real() {}
let x = "fn fake()";
fn also_real() {}
"#;
        let names = extract_names(src, LangFamily::Rust);
        assert!(names.contains(&"real".to_string()));
        assert!(names.contains(&"also_real".to_string()));
        assert!(!names.contains(&"fake".to_string()));
    }

    #[test]
    fn line_numbers_correct() {
        let src = "fn first() {}\n\nfn second() {}";
        let tokens = scan(src, LangFamily::Rust);
        let idents: Vec<_> = tokens
            .iter()
            .filter(|t| t.kind == TokenKind::Ident)
            .collect();
        assert_eq!(idents[0].text, "first");
        assert_eq!(idents[0].line, 0);
        assert_eq!(idents[1].text, "second");
        assert_eq!(idents[1].line, 2);
    }

    #[test]
    fn typescript_interface() {
        let src = r#"
interface UserProps {
    name: string;
}
type UserId = string;
enum Status { Active, Inactive }
"#;
        let names = extract_names(src, LangFamily::JsTs);
        assert!(names.contains(&"UserProps".to_string()));
        assert!(names.contains(&"UserId".to_string()));
        assert!(names.contains(&"Status".to_string()));
    }

    // --- Unicode tests ---

    #[test]
    fn unicode_identifier_latin_extended() {
        let src = "fn über_config() {}\nstruct Données {}";
        let names = extract_names(src, LangFamily::Rust);
        assert!(
            names.contains(&"über_config".to_string()),
            "Should extract Latin extended identifier: {names:?}"
        );
        assert!(
            names.contains(&"Données".to_string()),
            "Should extract accented identifier: {names:?}"
        );
    }

    #[test]
    fn unicode_identifier_cjk() {
        let src = "def 処理する():\n    pass\nclass 名前:\n    pass";
        let names = extract_names(src, LangFamily::Python);
        assert!(
            names.contains(&"処理する".to_string()),
            "Should extract CJK function name: {names:?}"
        );
        assert!(
            names.contains(&"名前".to_string()),
            "Should extract CJK class name: {names:?}"
        );
    }

    #[test]
    fn unicode_identifier_cyrillic() {
        let src = "fn обработка() {}\nstruct Данные {}";
        let names = extract_names(src, LangFamily::Rust);
        assert!(
            names.contains(&"обработка".to_string()),
            "Should extract Cyrillic identifier: {names:?}"
        );
        assert!(
            names.contains(&"Данные".to_string()),
            "Should extract Cyrillic struct name: {names:?}"
        );
    }

    #[test]
    fn unicode_mixed_ascii_and_extended() {
        let src = "fn café_résumé() {}\nfn plain_ascii() {}";
        let names = extract_names(src, LangFamily::Rust);
        assert!(names.contains(&"café_résumé".to_string()));
        assert!(names.contains(&"plain_ascii".to_string()));
    }

    #[test]
    fn unicode_in_strings_not_extracted() {
        let src = r#"
fn real_fn() {}
let x = "fn 偽物()";
fn also_real() {}
"#;
        let names = extract_names(src, LangFamily::Rust);
        assert!(names.contains(&"real_fn".to_string()));
        assert!(names.contains(&"also_real".to_string()));
        assert!(
            !names.contains(&"偽物".to_string()),
            "Should not extract from strings"
        );
    }

    #[test]
    fn unicode_in_comments_not_extracted() {
        let src = "# def コメント():\ndef 本物():\n    pass";
        let names = extract_names(src, LangFamily::Python);
        assert_eq!(names, vec!["本物"]);
    }

    #[test]
    fn unicode_column_offset() {
        // "fn á() {}" — á is 2 bytes in UTF-8, column should be char-based
        let src = "fn café() {}";
        let tokens = scan(src, LangFamily::Rust);
        let ident = tokens.iter().find(|t| t.kind == TokenKind::Ident).unwrap();
        assert_eq!(ident.text, "café");
        assert_eq!(ident.col, 3); // "fn " = 3 chars
    }

    #[test]
    fn unicode_emoji_in_string_skipped() {
        let src = "fn real() {}\nlet s = \"hello 🌍 world\";\nfn also_real() {}";
        let names = extract_names(src, LangFamily::Rust);
        assert!(names.contains(&"real".to_string()));
        assert!(names.contains(&"also_real".to_string()));
    }

    #[test]
    fn unicode_does_not_corrupt_scanning() {
        // Ensure random Unicode scattered through a file doesn't cause panics
        // or corrupt subsequent ASCII scanning
        let src = "fn a() {} // →→→\nstruct Ω {} /* αβγ */\nfn b() {}";
        let names = extract_names(src, LangFamily::Rust);
        assert!(names.contains(&"a".to_string()));
        assert!(names.contains(&"Ω".to_string()));
        assert!(names.contains(&"b".to_string()));
    }

    #[test]
    fn javascript_unicode_identifier() {
        let src = "function données() {}\nclass Ñoño {}";
        let names = extract_names(src, LangFamily::JsTs);
        assert!(names.contains(&"données".to_string()));
        assert!(names.contains(&"Ñoño".to_string()));
    }

    #[test]
    fn go_unicode_identifier() {
        let src = "func Обработать() {}\ntype Конфиг struct {}";
        let names = extract_names(src, LangFamily::Go);
        assert!(names.contains(&"Обработать".to_string()));
        assert!(names.contains(&"Конфиг".to_string()));
    }

    #[test]
    fn non_alphanumeric_unicode_does_not_hang() {
        // Box-drawing chars like │ (U+2502, UTF-8: 0xE2 0x94 0x82) have a leading
        // byte >= 0xC0 which passes is_ident_start_byte, but the char itself is not
        // alphanumeric. The tokenizer must not loop forever on these.
        //
        // Place the Unicode punctuation outside strings/comments so it actually
        // reaches the identifier check in the main scan loop.
        let src = "let x = 1\n│\nfn real() {}";
        let names = extract_names(src, LangFamily::Rust);
        assert!(names.contains(&"real".to_string()));

        // Em-dash outside strings
        let src2 = "let x = 1\n──\nfunction foo() {}";
        let names2 = extract_names(src2, LangFamily::JsTs);
        assert!(names2.contains(&"foo".to_string()));
    }
}
