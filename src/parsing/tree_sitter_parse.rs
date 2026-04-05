//! Tree-sitter based parsing for accurate symbol extraction.
//!
//! Languages with tree-sitter grammars get real AST-based parsing,
//! producing both accurate definitions and complete identifier occurrences
//! in a single pass. Languages without grammars fall back to the
//! hand-written tokenizer.

pub mod c;

use super::symbols::Symbol;
use super::tokenizer::Occurrence;

/// Result of tree-sitter parsing: symbols (definitions) and occurrences (all identifiers).
pub struct ParseResult {
    pub symbols: Vec<Symbol>,
    pub occurrences: Vec<Occurrence>,
}

/// Per-language tree-sitter parser. Adding a language = implement this trait.
pub trait TsParser {
    fn parse(source: &str) -> ParseResult;
}
