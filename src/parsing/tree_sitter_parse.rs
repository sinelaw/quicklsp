//! Tree-sitter based parsing for accurate symbol extraction.
//!
//! Languages with tree-sitter grammars get real AST-based parsing,
//! producing both accurate definitions and complete identifier occurrences
//! in a single pass. Languages without grammars fall back to the
//! hand-written tokenizer.

pub mod common;

pub mod c;
pub mod cpp;
pub mod go;
pub mod java;
pub mod javascript;
pub mod python;
pub mod ruby;
pub mod rust;
pub mod typescript;

use std::path::Path;

use super::symbols::Symbol;
use super::tokenizer::{LangFamily, Occurrence};

/// Result of tree-sitter parsing: symbols (definitions) and occurrences (all identifiers).
pub struct ParseResult {
    pub symbols: Vec<Symbol>,
    pub occurrences: Vec<Occurrence>,
}

/// Per-language tree-sitter parser. Adding a language = implement this trait.
pub trait TsParser {
    fn parse(source: &str) -> ParseResult;
}

/// Try to parse a file using the appropriate tree-sitter grammar.
///
/// Returns `Some(ParseResult)` if a tree-sitter grammar is available for the
/// file's language, `None` otherwise (caller should fall back to the tokenizer).
///
/// This is the single dispatch point for all tree-sitter parsers. To add a new
/// language, add a crate dependency, create the parser module, and add an arm here.
pub fn try_parse(path: &Path, source: &str) -> Option<ParseResult> {
    let ext = path.extension()?.to_str()?;
    match ext {
        // C
        "c" | "h" => Some(c::CParser::parse(source)),
        // C++
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" => Some(cpp::CppParser::parse(source)),
        // Rust
        "rs" => Some(rust::RustParser::parse(source)),
        // Go
        "go" => Some(go::GoParser::parse(source)),
        // Python
        "py" | "pyi" => Some(python::PythonParser::parse(source)),
        // JavaScript
        "js" | "jsx" | "mjs" | "cjs" => Some(javascript::JsParser::parse(source)),
        // TypeScript
        "ts" | "mts" => Some(typescript::TypeScriptParser::parse(source)),
        "tsx" => Some(typescript::TsxParser::parse(source)),
        // Java
        "java" => Some(java::JavaParser::parse(source)),
        // Ruby
        "rb" => Some(ruby::RubyParser::parse(source)),
        _ => None,
    }
}

/// Get the tree-sitter Language for a LangFamily, if available.
///
/// Used by `SyntaxCache` for AST queries at cursor position.
pub fn language_for_family(lang: LangFamily) -> Option<tree_sitter::Language> {
    match lang {
        LangFamily::CLike => Some(tree_sitter_c::LANGUAGE.into()),
        LangFamily::Rust => Some(tree_sitter_rust::LANGUAGE.into()),
        LangFamily::Go => Some(tree_sitter_go::LANGUAGE.into()),
        LangFamily::Python => Some(tree_sitter_python::LANGUAGE.into()),
        LangFamily::JsTs => Some(tree_sitter_javascript::LANGUAGE.into()),
        LangFamily::JavaCSharp => Some(tree_sitter_java::LANGUAGE.into()),
        LangFamily::Ruby => Some(tree_sitter_ruby::LANGUAGE.into()),
    }
}

/// Get the tree-sitter Language for a specific file extension.
///
/// More precise than `language_for_family` — distinguishes TypeScript from
/// JavaScript, C++ from C, etc.
pub fn language_for_extension(ext: &str) -> Option<tree_sitter::Language> {
    match ext {
        "c" | "h" => Some(tree_sitter_c::LANGUAGE.into()),
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" => Some(tree_sitter_cpp::LANGUAGE.into()),
        "rs" => Some(tree_sitter_rust::LANGUAGE.into()),
        "go" => Some(tree_sitter_go::LANGUAGE.into()),
        "py" | "pyi" => Some(tree_sitter_python::LANGUAGE.into()),
        "js" | "jsx" | "mjs" | "cjs" => Some(tree_sitter_javascript::LANGUAGE.into()),
        "ts" | "mts" => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        "tsx" => Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
        "java" => Some(tree_sitter_java::LANGUAGE.into()),
        "rb" => Some(tree_sitter_ruby::LANGUAGE.into()),
        _ => None,
    }
}
