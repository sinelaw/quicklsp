//! Tree-sitter based TypeScript parser using SCM queries.
//!
//! Extends JavaScript parsing with TypeScript-specific constructs:
//! interfaces, type aliases, enums, and type annotations.

use tree_sitter::Node;

use crate::parsing::symbols::{Symbol, SymbolKind};
use crate::parsing::tokenizer::Visibility;

use super::common::{self, node_text, QueryParseConfig};
use super::{ParseResult, TsParser};

const TS_IDENT_KINDS: &[&str] = &["identifier", "property_identifier", "type_identifier"];

const TS_QUERY: &str = r#"
; ── Function declarations ────────────────────────────────────────────────
(function_declaration
  name: (identifier) @name) @definition.function

; ── Generator functions ──────────────────────────────────────────────────
(generator_function_declaration
  name: (identifier) @name) @definition.function

; ── Class declarations ───────────────────────────────────────────────────
(class_declaration
  name: (type_identifier) @name) @definition.class

; ── Methods inside classes ───────────────────────────────────────────────
(class_declaration
  name: (type_identifier) @container
  body: (class_body
    (method_definition
      name: (property_identifier) @name) @definition.method))

; ── Class properties ─────────────────────────────────────────────────────
(class_declaration
  name: (type_identifier) @container
  body: (class_body
    (public_field_definition
      name: (property_identifier) @name) @definition.field))

; ── Interface declarations ───────────────────────────────────────────────
(interface_declaration
  name: (type_identifier) @name) @definition.interface

; ── Interface method signatures ──────────────────────────────────────────
(interface_declaration
  name: (type_identifier) @container
  body: (interface_body
    (method_signature
      name: (property_identifier) @name) @definition.method))

; ── Interface property signatures ────────────────────────────────────────
(interface_declaration
  name: (type_identifier) @container
  body: (interface_body
    (property_signature
      name: (property_identifier) @name) @definition.field))

; ── Type alias declarations ──────────────────────────────────────────────
(type_alias_declaration
  name: (type_identifier) @name) @definition.type

; ── Enum declarations ────────────────────────────────────────────────────
(enum_declaration
  name: (identifier) @name) @definition.enum

; ── Enum members ─────────────────────────────────────────────────────────
(enum_declaration
  name: (identifier) @container
  body: (enum_body
    (property_identifier) @name) @definition.variant)

; ── Variable declarations (const/let/var) ────────────────────────────────
(lexical_declaration
  (variable_declarator
    name: (identifier) @name) @definition.variable)

(variable_declaration
  (variable_declarator
    name: (identifier) @name) @definition.variable)

; ── Arrow function assigned to variable ──────────────────────────────────
(lexical_declaration
  (variable_declarator
    name: (identifier) @name
    value: (arrow_function)) @definition.function)

(variable_declaration
  (variable_declarator
    name: (identifier) @name
    value: (arrow_function)) @definition.function)

; ── Function expression assigned to variable ─────────────────────────────
(lexical_declaration
  (variable_declarator
    name: (identifier) @name
    value: (function_expression)) @definition.function)

; ── Exported function declarations ───────────────────────────────────────
(export_statement
  (function_declaration
    name: (identifier) @name) @definition.function)

; ── Exported class declarations ──────────────────────────────────────────
(export_statement
  (class_declaration
    name: (type_identifier) @name) @definition.class)

; ── Exported interface declarations ──────────────────────────────────────
(export_statement
  (interface_declaration
    name: (type_identifier) @name) @definition.interface)

; ── Exported type alias declarations ─────────────────────────────────────
(export_statement
  (type_alias_declaration
    name: (type_identifier) @name) @definition.type)

; ── Exported enum declarations ───────────────────────────────────────────
(export_statement
  (enum_declaration
    name: (identifier) @name) @definition.enum)

; ── Exported variable declarations ───────────────────────────────────────
(export_statement
  (lexical_declaration
    (variable_declarator
      name: (identifier) @name) @definition.variable))
"#;

pub struct TypeScriptParser;

impl TsParser for TypeScriptParser {
    fn parse(source: &str) -> ParseResult {
        let lang: tree_sitter::Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
        parse_ts_common(source, lang)
    }
}

pub struct TsxParser;

impl TsParser for TsxParser {
    fn parse(source: &str) -> ParseResult {
        let lang: tree_sitter::Language = tree_sitter_typescript::LANGUAGE_TSX.into();
        parse_ts_common(source, lang)
    }
}

fn parse_ts_common(source: &str, language: tree_sitter::Language) -> ParseResult {
    let mut result = common::run_query_parse(source, &QueryParseConfig {
        language,
        query_source: TS_QUERY,
        identifier_kinds: TS_IDENT_KINDS,
        def_keyword: ts_def_keyword,
        visibility: |_node: Node, _source: &str| Visibility::Unknown,
        post_process: Some(ts_post_process),
    });
    result
}

fn ts_def_keyword(_kind: SymbolKind, suffix: &str) -> &'static str {
    match suffix {
        "function" => "function",
        "method" => "method",
        "class" => "class",
        "interface" => "interface",
        "type" => "type",
        "enum" => "enum",
        "variable" | "field" => "variable",
        "variant" => "enum",
        "constant" => "const",
        _ => "function",
    }
}

/// Post-process: detect exports and const vs let/var.
fn ts_post_process(root: Node, source: &str, symbols: &mut Vec<Symbol>) {
    walk_for_export_and_const(root, source, symbols);
}

fn walk_for_export_and_const(node: Node, source: &str, symbols: &mut Vec<Symbol>) {
    match node.kind() {
        "export_statement" => {
            let start_line = node.start_position().row;
            let end_line = node.end_position().row;
            for sym in symbols.iter_mut() {
                if sym.line >= start_line && sym.line <= end_line {
                    sym.visibility = Visibility::Public;
                }
            }
        }
        "lexical_declaration" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if node_text(child, source) == "const" {
                    let start_line = node.start_position().row;
                    let end_line = node.end_position().row;
                    for sym in symbols.iter_mut() {
                        if sym.line >= start_line && sym.line <= end_line
                            && sym.kind == SymbolKind::Variable
                        {
                            sym.kind = SymbolKind::Constant;
                            sym.def_keyword = "const".to_string();
                        }
                    }
                    break;
                }
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_for_export_and_const(child, source, symbols);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_typescript_parser_basic() {
        let source = r#"
function hello(name: string): string {
    return "Hello, " + name;
}

const MAX_SIZE: number = 100;

interface Handler {
    handle(data: string): void;
    name: string;
}

class Config {
    private value: number;

    constructor(public name: string) {}

    getName(): string {
        return this.name;
    }
}

type Result<T> = { ok: true; value: T } | { ok: false; error: string };

enum Status {
    Active,
    Inactive,
}

export function exported(): void {}

const handler = (x: number): number => x + 1;
"#;
        let result = TypeScriptParser::parse(source);
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();

        assert!(names.contains(&"hello"), "should find function hello, got: {:?}", names);
        assert!(names.contains(&"MAX_SIZE"), "should find const MAX_SIZE, got: {:?}", names);
        assert!(names.contains(&"Handler"), "should find interface Handler, got: {:?}", names);
        assert!(names.contains(&"Config"), "should find class Config, got: {:?}", names);
        assert!(names.contains(&"getName"), "should find method getName, got: {:?}", names);
        assert!(names.contains(&"Result"), "should find type alias Result, got: {:?}", names);
        assert!(names.contains(&"Status"), "should find enum Status, got: {:?}", names);
        assert!(names.contains(&"exported"), "should find exported function, got: {:?}", names);
        assert!(names.contains(&"handler"), "should find arrow function handler, got: {:?}", names);

        // Check exported visibility
        let exported_sym = result.symbols.iter().find(|s| s.name == "exported").unwrap();
        assert_eq!(exported_sym.visibility, Visibility::Public);

        // Check interface method container
        let handle_sym = result.symbols.iter().find(|s| s.name == "handle").unwrap();
        assert_eq!(handle_sym.container.as_deref(), Some("Handler"));

        assert!(!result.occurrences.is_empty());
    }

    #[test]
    fn test_typescript_parser_fixture() {
        let source = include_str!("../../../tests/fixtures/sample_typescript.ts");
        let result = TypeScriptParser::parse(source);
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();

        // Constants
        assert!(names.contains(&"MAX_RETRIES"), "got: {:?}", names);
        assert!(names.contains(&"DEFAULT_TIMEOUT"));

        // Interfaces
        assert!(names.contains(&"Config"));
        assert!(names.contains(&"Handler"));

        // Type aliases
        assert!(names.contains(&"StatusCode"));
        assert!(names.contains(&"HandlerResult"));

        // Classes
        assert!(names.contains(&"Request"));
        assert!(names.contains(&"Response"));
        assert!(names.contains(&"Server"));

        // Functions
        assert!(names.contains(&"createConfig"));
        assert!(names.contains(&"processRequest"));
        assert!(names.contains(&"validateRequest"));

        // Enum
        assert!(names.contains(&"Status"));

        // Class methods with containers
        let add_handler = result.symbols.iter().find(|s| s.name == "addHandler");
        assert!(add_handler.is_some(), "should find Server.addHandler");
        if let Some(s) = add_handler {
            assert_eq!(s.container.as_deref(), Some("Server"));
        }

        // Variables
        assert!(names.contains(&"globalCounter"));
    }

    #[test]
    fn test_tsx_parser_basic() {
        let source = r#"
interface Props {
    name: string;
    onClick: () => void;
}

function Greeting(props: Props) {
    return props.name;
}

const App = () => {
    return "hello";
};

export default App;
"#;
        let result = TsxParser::parse(source);
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Props"), "should find interface Props, got: {:?}", names);
        assert!(names.contains(&"Greeting"), "should find function Greeting, got: {:?}", names);
        assert!(names.contains(&"App"), "should find const App, got: {:?}", names);
    }

    #[test]
    fn test_typescript_empty_file() {
        let result = TypeScriptParser::parse("");
        assert!(result.symbols.is_empty());
    }
}
