//! Shared helpers for tree-sitter language parsers.
//!
//! Provides common utilities for symbol construction, occurrence collection,
//! and AST walking so that per-language parsers only need to define their
//! language-specific definition extraction logic.

use tree_sitter::{Node, Parser, Tree};

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
