//! Shared helpers for tree-sitter language parsers.
//!
//! Provides common utilities for symbol construction, occurrence collection,
//! and AST walking so that per-language parsers only need to define their
//! language-specific definition extraction logic.
//!
//! The **query engine** (`run_query_parse`) uses tree-sitter SCM queries to
//! declaratively match AST patterns and extract symbols. Per-language parsers
//! provide a `QueryParseConfig` with the SCM source, identifier kinds, and
//! optional callbacks for visibility detection and post-processing.

use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator, Tree};

use crate::parsing::symbols::{Symbol, SymbolKind};
use crate::parsing::tokenizer::{Occurrence, OccurrenceRole, Visibility};

use super::ParseResult;

// ── Text helpers ────────────────────────────────────────────────────────

/// Get the text of a node from the source.
pub fn node_text<'a>(node: Node, source: &'a str) -> &'a str {
    &source[node.start_byte()..node.end_byte()]
}

/// Get the name (text) and position of a child identified by `field_name`.
pub fn named_child_text(node: Node, field_name: &str, source: &str) -> Option<(String, usize, usize)> {
    let child = node.child_by_field_name(field_name)?;
    let text = node_text(child, source).to_string();
    Some((text, child.start_position().row, child.start_position().column))
}

// ── Symbol construction ─────────────────────────────────────────────────

/// Build a `Symbol` with common defaults (no doc_comment, no signature, no container,
/// depth=0, no scope_end_line).
pub fn make_symbol(
    name: String,
    kind: SymbolKind,
    line: usize,
    col: usize,
    def_keyword: &str,
    visibility: Visibility,
) -> Symbol {
    Symbol {
        name,
        kind,
        line,
        col,
        def_keyword: def_keyword.to_string(),
        doc_comment: None,
        signature: None,
        visibility,
        container: None,
        depth: 0,
        scope_end_line: None,
    }
}

/// Build a `Symbol` that lives inside a container (e.g., a method in a class,
/// a field in a struct). Sets depth ≥ 1.
pub fn make_contained_symbol(
    name: String,
    kind: SymbolKind,
    line: usize,
    col: usize,
    def_keyword: &str,
    visibility: Visibility,
    container: Option<&str>,
    depth: usize,
    scope_end_line: Option<usize>,
    doc_comment: Option<String>,
) -> Symbol {
    Symbol {
        name,
        kind,
        line,
        col,
        def_keyword: def_keyword.to_string(),
        doc_comment,
        signature: None,
        visibility,
        container: container.map(|s| s.to_string()),
        depth,
        scope_end_line,
    }
}

// ── Occurrence collection ───────────────────────────────────────────────

/// Walk the entire AST and collect all identifier occurrences.
///
/// `identifier_kinds` lists the tree-sitter node kinds that represent
/// identifiers in this language (e.g., `["identifier", "type_identifier",
/// "field_identifier"]` for C-family languages).
pub fn collect_occurrences(
    root: Node,
    source: &str,
    symbols: &[Symbol],
    identifier_kinds: &[&str],
) -> Vec<Occurrence> {
    let mut occurrences = Vec::new();
    collect_occurrences_recursive(root, source, symbols, identifier_kinds, &mut occurrences);
    occurrences
}

fn collect_occurrences_recursive(
    node: Node,
    source: &str,
    symbols: &[Symbol],
    identifier_kinds: &[&str],
    occurrences: &mut Vec<Occurrence>,
) {
    if identifier_kinds.contains(&node.kind()) {
        let start = node.start_byte();
        let end = node.end_byte();
        let len = end - start;
        if len > 0 && len <= u16::MAX as usize {
            let line = node.start_position().row;
            let col = node.start_position().column;

            let role = if symbols.iter().any(|s| s.line == line && s.col == col) {
                OccurrenceRole::Definition
            } else {
                OccurrenceRole::Reference
            };

            occurrences.push(Occurrence {
                word_offset: start as u32,
                word_len: len as u16,
                line,
                col,
                role,
            });
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_occurrences_recursive(child, source, symbols, identifier_kinds, occurrences);
    }
}

// ── Parser setup ────────────────────────────────────────────────────────

/// Parse source with the given tree-sitter language. Returns `None` on failure.
pub fn parse_source(source: &str, language: &tree_sitter::Language) -> Option<Tree> {
    let mut parser = Parser::new();
    parser.set_language(language).ok()?;
    parser.parse(source, None)
}

/// Full parse pipeline: parse source → collect definitions → collect occurrences.
///
/// `language` is the tree-sitter grammar.
/// `identifier_kinds` lists node kinds that are identifiers for occurrence collection.
/// `collect_defs` is the language-specific definition extraction callback.
pub fn run_parse<F>(
    source: &str,
    language: &tree_sitter::Language,
    identifier_kinds: &[&str],
    collect_defs: F,
) -> ParseResult
where
    F: FnOnce(Node, &str, &mut Vec<Symbol>),
{
    let tree = match parse_source(source, language) {
        Some(t) => t,
        None => return ParseResult { symbols: Vec::new(), occurrences: Vec::new() },
    };

    let mut symbols = Vec::new();
    collect_defs(tree.root_node(), source, &mut symbols);

    let occurrences = collect_occurrences(tree.root_node(), source, &symbols, identifier_kinds);

    ParseResult { symbols, occurrences }
}

// ── Query-based parsing engine ──────────────────────────────────────────

/// Configuration for query-based parsing.
pub struct QueryParseConfig {
    /// Tree-sitter language grammar.
    pub language: tree_sitter::Language,
    /// SCM query source with `@name` and `@definition.*` captures.
    /// Optional: `@container` for parent type/class name.
    pub query_source: &'static str,
    /// Node kinds that count as identifiers for occurrence collection.
    pub identifier_kinds: &'static [&'static str],
    /// Map SymbolKind to def_keyword string for this language.
    pub def_keyword: fn(SymbolKind, &str) -> &'static str,
    /// Detect visibility from a definition node.
    pub visibility: fn(Node, &str) -> Visibility,
    /// Optional post-processing on collected symbols (e.g. for local variables).
    /// Receives the parsed tree root, source, and the symbols vec.
    pub post_process: Option<fn(Node, &str, &mut Vec<Symbol>)>,
}

/// Map a capture name suffix (after "definition.") to a SymbolKind.
fn capture_to_kind(capture_name: &str) -> Option<SymbolKind> {
    let suffix = capture_name.strip_prefix("definition.")?;
    match suffix {
        "function" => Some(SymbolKind::Function),
        "method" => Some(SymbolKind::Method),
        "class" => Some(SymbolKind::Class),
        "struct" => Some(SymbolKind::Struct),
        "enum" => Some(SymbolKind::Enum),
        "interface" => Some(SymbolKind::Interface),
        "trait" => Some(SymbolKind::Trait),
        "type" => Some(SymbolKind::TypeAlias),
        "constant" => Some(SymbolKind::Constant),
        "variable" => Some(SymbolKind::Variable),
        "module" => Some(SymbolKind::Module),
        "macro" => Some(SymbolKind::Function),
        "field" => Some(SymbolKind::Variable),
        "variant" => Some(SymbolKind::Constant),
        "constructor" => Some(SymbolKind::Function),
        _ => None,
    }
}

/// Default def_keyword: use the capture suffix directly.
pub fn default_def_keyword(kind: SymbolKind, capture_suffix: &str) -> &'static str {
    // Use the capture suffix as a hint, falling back to kind name
    match capture_suffix {
        "function" => "fn",
        "method" => "method",
        "class" => "class",
        "struct" => "struct",
        "enum" => "enum",
        "interface" => "interface",
        "trait" => "trait",
        "type" => "type",
        "constant" | "variant" => "const",
        "variable" | "field" => "variable",
        "module" => "mod",
        "macro" => "macro",
        "constructor" => "constructor",
        _ => match kind {
            SymbolKind::Function => "fn",
            SymbolKind::Method => "method",
            SymbolKind::Class => "class",
            SymbolKind::Struct => "struct",
            SymbolKind::Enum => "enum",
            SymbolKind::Interface => "interface",
            SymbolKind::Trait => "trait",
            SymbolKind::TypeAlias => "type",
            SymbolKind::Constant => "const",
            SymbolKind::Variable => "variable",
            SymbolKind::Module => "mod",
            SymbolKind::Unknown => "unknown",
        },
    }
}

/// Default visibility: Unknown.
pub fn default_visibility(_node: Node, _source: &str) -> Visibility {
    Visibility::Unknown
}

/// Full query-based parse pipeline:
/// 1. Parse source with tree-sitter
/// 2. Run SCM query to extract definitions
/// 3. Map captures to Symbol objects
/// 4. Run optional post-processing
/// 5. Collect all identifier occurrences
pub fn run_query_parse(source: &str, config: &QueryParseConfig) -> ParseResult {
    let tree = match parse_source(source, &config.language) {
        Some(t) => t,
        None => return ParseResult { symbols: Vec::new(), occurrences: Vec::new() },
    };

    let query = match Query::new(&config.language, config.query_source) {
        Ok(q) => q,
        Err(e) => {
            tracing::error!("Failed to compile tree-sitter query: {e:?}");
            return ParseResult { symbols: Vec::new(), occurrences: Vec::new() };
        }
    };

    // Resolve capture indices
    let name_idx = query.capture_index_for_name("name");
    let container_idx = query.capture_index_for_name("container");

    // Find all definition capture indices
    let capture_names = query.capture_names();
    let def_captures: Vec<(u32, SymbolKind, &str)> = capture_names
        .iter()
        .enumerate()
        .filter_map(|(i, &name)| {
            let suffix = name.strip_prefix("definition.")?;
            let kind = capture_to_kind(name)?;
            Some((i as u32, kind, suffix))
        })
        .collect();

    let mut symbols = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());

    while let Some(m) = matches.next() {
        // Find the @name capture
        let name_capture = name_idx.and_then(|idx| {
            m.captures.iter().find(|c| c.index == idx)
        });

        // Find the @definition.* capture
        let def_capture = m.captures.iter().find_map(|c| {
            def_captures.iter().find(|(idx, _, _)| *idx == c.index)
                .map(|(_, kind, suffix)| (c, *kind, *suffix))
        });

        // Find the @container capture
        let container_capture = container_idx.and_then(|idx| {
            m.captures.iter().find(|c| c.index == idx)
        });

        if let (Some(name_cap), Some((def_cap, kind, suffix))) = (name_capture, def_capture) {
            let name = node_text(name_cap.node, source).to_string();
            let line = name_cap.node.start_position().row;
            let col = name_cap.node.start_position().column;
            let def_keyword = (config.def_keyword)(kind, suffix);
            let visibility = (config.visibility)(def_cap.node, source);

            let container = container_capture
                .map(|c| node_text(c.node, source).to_string());

            let has_container = container.is_some();
            symbols.push(Symbol {
                name,
                kind,
                line,
                col,
                def_keyword: def_keyword.to_string(),
                doc_comment: None,
                signature: None,
                visibility,
                container,
                depth: if has_container { 1 } else { 0 },
                scope_end_line: None,
            });
        }
    }

    // Deduplicate: when the same name node is matched by multiple patterns,
    // keep the one with a container (more specific) over the one without.
    deduplicate_symbols(&mut symbols);

    // Run optional post-processing (e.g. local variable extraction)
    if let Some(post) = config.post_process {
        post(tree.root_node(), source, &mut symbols);
    }

    let occurrences = collect_occurrences(
        tree.root_node(),
        source,
        &symbols,
        config.identifier_kinds,
    );

    ParseResult { symbols, occurrences }
}

/// Remove duplicate symbols at the same position, preferring the one with
/// a container (more specific match from a nested pattern) over one without.
fn deduplicate_symbols(symbols: &mut Vec<Symbol>) {
    // Sort by (line, col) so duplicates are adjacent
    symbols.sort_by(|a, b| a.line.cmp(&b.line).then(a.col.cmp(&b.col)));
    let mut i = 0;
    while i + 1 < symbols.len() {
        if symbols[i].line == symbols[i + 1].line && symbols[i].col == symbols[i + 1].col {
            // Same position — keep the one with a container (more specific)
            if symbols[i].container.is_some() && symbols[i + 1].container.is_none() {
                symbols.remove(i + 1);
            } else {
                symbols.remove(i);
            }
        } else {
            i += 1;
        }
    }
}

// ── AST walking helpers ─────────────────────────────────────────────────

/// Check if any direct child has a specific storage class specifier (e.g., "static", "pub").
pub fn has_child_with_kind_and_text(node: Node, kind: &str, text: &str, source: &str) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == kind && node_text(child, source) == text {
            return true;
        }
    }
    false
}

/// Find the first direct child with a given kind.
pub fn find_child_by_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == kind {
            return Some(child);
        }
    }
    None
}

/// Iterate direct children, calling `f` on each that matches any of the given kinds.
pub fn for_each_child_of_kind<F>(node: Node, kinds: &[&str], mut f: F)
where
    F: FnMut(Node),
{
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if kinds.contains(&child.kind()) {
            f(child);
        }
    }
}

/// Recursively walk into preprocessor conditional blocks to find definitions.
/// Used by C/C++ parsers for `#ifdef`, `#ifndef`, `#if`, etc.
pub fn walk_preproc_conditionals<F>(node: Node, source: &str, symbols: &mut Vec<Symbol>, mut extract: F)
where
    F: FnMut(Node, &str, &mut Vec<Symbol>),
{
    walk_preproc_conditionals_inner(node, source, symbols, &mut extract);
}

fn walk_preproc_conditionals_inner<F>(node: Node, source: &str, symbols: &mut Vec<Symbol>, extract: &mut F)
where
    F: FnMut(Node, &str, &mut Vec<Symbol>),
{
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "preproc_ifdef" | "preproc_if" | "preproc_elif" | "preproc_else"
            | "preproc_ifndef" => {
                walk_preproc_conditionals_inner(child, source, symbols, extract);
            }
            _ => {
                extract(child, source, symbols);
            }
        }
    }
}
