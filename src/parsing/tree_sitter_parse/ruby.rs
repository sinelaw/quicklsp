//! Tree-sitter based Ruby parser using SCM queries.
//!
//! Extracts: methods, classes, modules, singleton methods, and module-level
//! constant assignments.

use tree_sitter::Node;

use crate::parsing::symbols::SymbolKind;
use crate::parsing::tokenizer::Visibility;

use super::common::{self, QueryParseConfig};
use super::{ParseResult, TsParser};

const RUBY_IDENT_KINDS: &[&str] = &["identifier", "constant"];

const RUBY_QUERY: &str = r#"
; ── Methods ──────────────────────────────────────────────────────────────
(method
  name: (identifier) @name) @definition.function

; ── Methods inside classes ───────────────────────────────────────────────
(class
  name: (constant) @container
  (body_statement
    (method
      name: (identifier) @name) @definition.method))

; ── Singleton methods inside classes ─────────────────────────────────────
(class
  name: (constant) @container
  (body_statement
    (singleton_method
      name: (identifier) @name) @definition.method))

; ── Methods inside modules ───────────────────────────────────────────────
(module
  name: (constant) @container
  (body_statement
    (method
      name: (identifier) @name) @definition.method))

; ── Classes ──────────────────────────────────────────────────────────────
(class
  name: (constant) @name) @definition.class

; ── Modules ──────────────────────────────────────────────────────────────
(module
  name: (constant) @name) @definition.module

; ── Singleton methods (top-level) ────────────────────────────────────────
(singleton_method
  name: (identifier) @name) @definition.function

; ── Constant assignments ─────────────────────────────────────────────────
(assignment
  left: (constant) @name) @definition.constant
"#;

pub struct RubyParser;

impl TsParser for RubyParser {
    fn parse(source: &str) -> ParseResult {
        let mut result = common::run_query_parse(
            source,
            &QueryParseConfig {
                language: tree_sitter_ruby::LANGUAGE.into(),
                query_source: RUBY_QUERY,
                identifier_kinds: RUBY_IDENT_KINDS,
                def_keyword: ruby_def_keyword,
                visibility: |_node: Node, _source: &str| Visibility::Unknown,
                post_process: None,
            },
        );
        // Apply Ruby naming convention for visibility
        for sym in &mut result.symbols {
            sym.visibility = ruby_visibility(&sym.name);
        }
        result
    }
}

fn ruby_def_keyword(_kind: SymbolKind, suffix: &str) -> &'static str {
    match suffix {
        "function" | "method" => "def",
        "class" => "class",
        "module" => "module",
        "constant" => "constant",
        _ => "def",
    }
}

fn ruby_visibility(name: &str) -> Visibility {
    if name.starts_with('_') {
        Visibility::Private
    } else {
        Visibility::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ruby_parser_basic() {
        let source = r#"
MAX_SIZE = 100

def hello(name)
    "Hello, #{name}"
end

def _helper
    nil
end

class Config
    def initialize(name)
        @name = name
    end

    def get_name
        @name
    end

    def self.create(name)
        new(name)
    end

    def _internal
        nil
    end
end

module Utils
    def format(data)
        data.to_s
    end
end
"#;
        let result = RubyParser::parse(source);
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();

        assert!(
            names.contains(&"MAX_SIZE"),
            "should find constant MAX_SIZE, got: {:?}",
            names
        );
        assert!(
            names.contains(&"hello"),
            "should find method hello, got: {:?}",
            names
        );
        assert!(
            names.contains(&"_helper"),
            "should find method _helper, got: {:?}",
            names
        );
        assert!(
            names.contains(&"Config"),
            "should find class Config, got: {:?}",
            names
        );
        assert!(
            names.contains(&"initialize"),
            "should find method initialize, got: {:?}",
            names
        );
        assert!(
            names.contains(&"get_name"),
            "should find method get_name, got: {:?}",
            names
        );
        assert!(
            names.contains(&"create"),
            "should find singleton method create, got: {:?}",
            names
        );
        assert!(
            names.contains(&"Utils"),
            "should find module Utils, got: {:?}",
            names
        );

        // Check visibility
        let helper_sym = result.symbols.iter().find(|s| s.name == "_helper").unwrap();
        assert_eq!(helper_sym.visibility, Visibility::Private);

        // Check method container
        let get_name = result
            .symbols
            .iter()
            .find(|s| s.name == "get_name")
            .unwrap();
        assert_eq!(get_name.container.as_deref(), Some("Config"));

        assert!(!result.occurrences.is_empty());
    }
}
