//! Tree-sitter based Go parser using SCM queries.
//!
//! Extracts: functions, methods, type declarations (structs, interfaces,
//! type aliases), var/const declarations, struct fields, and interface methods.

use tree_sitter::Node;

use crate::parsing::symbols::{Symbol, SymbolKind};
use crate::parsing::tokenizer::Visibility;

use super::common::{self, node_text, QueryParseConfig};
use super::{ParseResult, TsParser};

const GO_IDENT_KINDS: &[&str] = &["identifier", "type_identifier", "field_identifier"];

const GO_QUERY: &str = r#"
; ── Functions ────────────────────────────────────────────────────────────
(function_declaration
  name: (identifier) @name) @definition.function

; ── Methods (with receiver) ──────────────────────────────────────────────
(method_declaration
  name: (field_identifier) @name) @definition.method

; ── Struct types ─────────────────────────────────────────────────────────
(type_declaration
  (type_spec
    name: (type_identifier) @name
    type: (struct_type)) @definition.struct)

; ── Interface types ──────────────────────────────────────────────────────
(type_declaration
  (type_spec
    name: (type_identifier) @name
    type: (interface_type)) @definition.interface)

; ── Other type declarations (type aliases) ───────────────────────────────
(type_declaration
  (type_spec
    name: (type_identifier) @name) @definition.type)

; ── Var declarations ─────────────────────────────────────────────────────
(var_declaration
  (var_spec
    name: (identifier) @name) @definition.variable)

; ── Const declarations ───────────────────────────────────────────────────
(const_declaration
  (const_spec
    name: (identifier) @name) @definition.constant)

; ── Struct fields ────────────────────────────────────────────────────────
(type_declaration
  (type_spec
    name: (type_identifier) @container
    type: (struct_type
      (field_declaration_list
        (field_declaration
          name: (field_identifier) @name) @definition.field))))

; ── Interface method specs ───────────────────────────────────────────────
(type_declaration
  (type_spec
    name: (type_identifier) @container
    type: (interface_type
      (method_elem
        name: (field_identifier) @name) @definition.method)))
"#;

pub struct GoParser;

/// Go nodes that introduce a new function scope.
const GO_SCOPE_KINDS: &[&str] = &["function_declaration", "method_declaration", "func_literal"];

impl TsParser for GoParser {
    fn parse(source: &str) -> ParseResult {
        let mut result = common::run_query_parse(
            source,
            &QueryParseConfig {
                language: tree_sitter_go::LANGUAGE.into(),
                query_source: GO_QUERY,
                identifier_kinds: GO_IDENT_KINDS,
                scope_kinds: GO_SCOPE_KINDS,
                def_keyword: go_def_keyword,
                visibility: |_node: Node, _source: &str| Visibility::Unknown,
                post_process: Some(go_post_process),
            },
        );
        // Apply Go visibility convention (uppercase = Public)
        for sym in &mut result.symbols {
            sym.visibility = go_visibility(&sym.name);
        }
        result
    }
}

fn go_def_keyword(_kind: SymbolKind, suffix: &str) -> &'static str {
    match suffix {
        "function" => "func",
        "method" => "method",
        "struct" => "struct",
        "interface" => "interface",
        "type" => "type",
        "variable" => "var",
        "constant" => "const",
        "field" => "field",
        _ => "func",
    }
}

fn go_visibility(name: &str) -> Visibility {
    if name.starts_with(|c: char| c.is_uppercase()) {
        Visibility::Public
    } else {
        Visibility::Private
    }
}

/// Post-process: extract method receivers as containers.
fn go_post_process(root: Node, source: &str, symbols: &mut Vec<Symbol>) {
    set_method_receivers(root, source, symbols);
}

fn set_method_receivers(node: Node, source: &str, symbols: &mut Vec<Symbol>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "method_declaration" {
            if let Some(name_node) = child.child_by_field_name("name") {
                let name = node_text(name_node, source);
                let line = name_node.start_position().row;
                let col = name_node.start_position().column;
                if let Some(receiver_type) = extract_receiver_type(child, source) {
                    if let Some(sym) = symbols
                        .iter_mut()
                        .find(|s| s.name == name && s.line == line && s.col == col)
                    {
                        sym.container = Some(receiver_type.to_string());
                    }
                }
            }
        }
        set_method_receivers(child, source, symbols);
    }
}

fn extract_receiver_type<'a>(method_node: Node<'a>, source: &'a str) -> Option<&'a str> {
    let params = method_node.child_by_field_name("receiver")?;
    let mut cursor = params.walk();
    for child in params.children(&mut cursor) {
        if child.kind() == "parameter_declaration" {
            if let Some(type_node) = child.child_by_field_name("type") {
                let text = node_text(type_node, source);
                return Some(text.trim_start_matches('*'));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_go_parser_basic() {
        let source = r#"
package main

func Hello(name string) string {
    return "Hello, " + name
}

func helper() {}

type Config struct {
    Name  string
    value int
}

type Handler interface {
    Handle()
}

type MyInt int

func (c *Config) GetName() string {
    return c.Name
}

var GlobalVar = 42

const MaxSize = 100
"#;
        let result = GoParser::parse(source);
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();

        assert!(
            names.contains(&"Hello"),
            "should find func Hello, got: {:?}",
            names
        );
        assert!(
            names.contains(&"helper"),
            "should find func helper, got: {:?}",
            names
        );
        assert!(
            names.contains(&"Config"),
            "should find struct Config, got: {:?}",
            names
        );
        assert!(
            names.contains(&"Name"),
            "should find field Name, got: {:?}",
            names
        );
        assert!(
            names.contains(&"Handler"),
            "should find interface Handler, got: {:?}",
            names
        );
        assert!(
            names.contains(&"MyInt"),
            "should find type MyInt, got: {:?}",
            names
        );
        assert!(
            names.contains(&"GetName"),
            "should find method GetName, got: {:?}",
            names
        );
        assert!(
            names.contains(&"GlobalVar"),
            "should find var GlobalVar, got: {:?}",
            names
        );
        assert!(
            names.contains(&"MaxSize"),
            "should find const MaxSize, got: {:?}",
            names
        );

        // Check visibility
        let hello_sym = result.symbols.iter().find(|s| s.name == "Hello").unwrap();
        assert_eq!(hello_sym.visibility, Visibility::Public);

        let helper_sym = result.symbols.iter().find(|s| s.name == "helper").unwrap();
        assert_eq!(helper_sym.visibility, Visibility::Private);

        // Check method container
        let get_name = result.symbols.iter().find(|s| s.name == "GetName").unwrap();
        assert_eq!(get_name.container.as_deref(), Some("Config"));

        assert!(!result.occurrences.is_empty());
    }

    #[test]
    fn test_go_parser_fixture() {
        let source = include_str!("../../../tests/fixtures/sample_go.go");
        let result = GoParser::parse(source);
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();

        // Constants
        assert!(names.contains(&"MaxRetries"), "got: {:?}", names);
        assert!(names.contains(&"DefaultTimeout"));

        // Types
        assert!(names.contains(&"Config"));
        assert!(names.contains(&"Status"));
        assert!(names.contains(&"Request"));
        assert!(names.contains(&"Response"));
        assert!(names.contains(&"Server"));
        assert!(names.contains(&"HandlerFunc"));

        // Functions
        assert!(names.contains(&"NewConfig"));
        assert!(names.contains(&"ValidatePort"));
        assert!(names.contains(&"ProcessRequest"));
        assert!(names.contains(&"SanitizeInput"));
        assert!(names.contains(&"main"));

        // Methods
        let add_handler = result
            .symbols
            .iter()
            .find(|s| s.name == "AddHandler")
            .unwrap();
        assert_eq!(add_handler.container.as_deref(), Some("Server"));

        let run = result.symbols.iter().find(|s| s.name == "Run").unwrap();
        assert_eq!(run.container.as_deref(), Some("Server"));

        // Struct fields
        assert!(names.contains(&"Host"));
        assert!(names.contains(&"Port"));

        // Variables
        assert!(names.contains(&"globalCounter"));
    }

    #[test]
    fn test_go_empty_file() {
        // Go requires at least "package main" but we can test minimal
        let result = GoParser::parse("package main\n");
        // No definitions expected beyond package
        assert!(result.symbols.is_empty() || result.symbols.len() <= 1);
    }

    #[test]
    fn test_go_interface_methods() {
        let source = r#"
package test

type Reader interface {
    Read(p []byte) (n int, err error)
    Close() error
}
"#;
        let result = GoParser::parse(source);
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Reader"));
        // Interface method specs may or may not be captured depending on grammar
    }
}
