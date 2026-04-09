//! Tree-sitter based C++ parser using SCM queries + procedural helpers.
//!
//! Extends C parsing with C++-specific constructs: classes, namespaces,
//! access specifiers, templates, and methods.

use tree_sitter::Node;

use crate::parsing::symbols::{Symbol, SymbolKind};
use crate::parsing::tokenizer::Visibility;

use super::common::{
    self, find_child_by_kind, make_contained_symbol, make_symbol, node_text,
    walk_preproc_conditionals, QueryParseConfig,
};
use super::{ParseResult, TsParser};

const CPP_IDENT_KINDS: &[&str] = &[
    "identifier",
    "type_identifier",
    "field_identifier",
    "namespace_identifier",
];

const CPP_QUERY: &str = r#"
; ── Function definitions ─────────────────────────────────────────────────
(function_definition
  declarator: (function_declarator
    declarator: (identifier) @name)) @definition.function

; ── Function definitions with pointer return ─────────────────────────────
(function_definition
  declarator: (pointer_declarator
    declarator: (function_declarator
      declarator: (identifier) @name))) @definition.function

; ── Function definitions with reference return ───────────────────────────
(function_definition
  declarator: (reference_declarator
    (function_declarator
      declarator: (identifier) @name))) @definition.function

; ── Qualified function definitions (e.g. Class::method) ──────────────────
(function_definition
  declarator: (function_declarator
    declarator: (qualified_identifier
      name: (identifier) @name))) @definition.function

; ── Class declarations ───────────────────────────────────────────────────
(class_specifier
  name: (type_identifier) @name) @definition.class

; ── Struct declarations ──────────────────────────────────────────────────
(struct_specifier
  name: (type_identifier) @name
  body: (_)) @definition.struct

; ── Union declarations ───────────────────────────────────────────────────
(declaration
  type: (union_specifier
    name: (type_identifier) @name
    body: (_))) @definition.struct

; ── Enum declarations ────────────────────────────────────────────────────
(enum_specifier
  name: (type_identifier) @name) @definition.enum

; ── Enumerators ──────────────────────────────────────────────────────────
(enumerator
  name: (identifier) @name) @definition.constant

; ── Namespace definitions ────────────────────────────────────────────────
(namespace_definition
  name: (namespace_identifier) @name) @definition.module

; ── Typedefs ─────────────────────────────────────────────────────────────
(type_definition
  declarator: (type_identifier) @name) @definition.type

; ── Alias declarations (using X = ...) ───────────────────────────────────
(alias_declaration
  name: (type_identifier) @name) @definition.type

; ── #define ──────────────────────────────────────────────────────────────
(preproc_def
  name: (identifier) @name) @definition.constant

; ── #define function-like macros ─────────────────────────────────────────
(preproc_function_def
  name: (identifier) @name) @definition.constant
"#;

pub struct CppParser;

impl TsParser for CppParser {
    fn parse(source: &str) -> ParseResult {
        let lang: tree_sitter::Language = tree_sitter_cpp::LANGUAGE.into();
        let mut result = common::run_query_parse(
            source,
            &QueryParseConfig {
                language: lang,
                query_source: CPP_QUERY,
                identifier_kinds: CPP_IDENT_KINDS,
                def_keyword: cpp_def_keyword,
                visibility: cpp_visibility,
                post_process: Some(cpp_post_process),
            },
        );
        result
    }
}

fn cpp_def_keyword(_kind: SymbolKind, suffix: &str) -> &'static str {
    match suffix {
        "function" => "function",
        "method" => "method",
        "class" => "class",
        "struct" => "struct",
        "enum" => "enum",
        "constant" => "enum",
        "type" => "typedef",
        "module" => "namespace",
        "field" => "field",
        "variable" => "variable",
        _ => "function",
    }
}

fn cpp_visibility(node: Node, source: &str) -> Visibility {
    if common::has_child_with_kind_and_text(node, "storage_class_specifier", "static", source) {
        Visibility::Private
    } else if node.kind() == "function_definition" {
        Visibility::Public
    } else {
        Visibility::Unknown
    }
}

/// Post-process: extract class bodies (methods, fields with access specifiers),
/// typedef function pointers, file-scope declarations, and function-body
/// locals / parameters.
fn cpp_post_process(root: Node, source: &str, symbols: &mut Vec<Symbol>) {
    extract_class_bodies(root, source, symbols);
    extract_typedef_function_ptrs(root, source, symbols);
    extract_file_scope_vars(root, source, symbols);
    // tree-sitter-cpp shares the `function_definition` / `declaration` /
    // `parameter_declaration` / `compound_statement` node kinds with
    // tree-sitter-c, so the C extractor handles plain C++ function bodies.
    // (Class-member methods are handled by `extract_class_bodies` above.)
    super::c::extract_params_and_locals(root, source, symbols);
}

/// Walk the tree to find class/struct bodies and extract their members
/// with proper access specifier tracking.
fn extract_class_bodies(node: Node, source: &str, symbols: &mut Vec<Symbol>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "class_specifier" | "struct_specifier" => {
                let name = child
                    .child_by_field_name("name")
                    .map(|n| node_text(n, source).to_string());
                if let Some(body) = child.child_by_field_name("body") {
                    let default_vis = if child.kind() == "class_specifier" {
                        Visibility::Private
                    } else {
                        Visibility::Public
                    };
                    extract_class_body(body, source, symbols, name.as_deref(), default_vis);
                }
            }
            "template_declaration" => {
                // Recurse into templates to find the inner class/struct/function
                extract_class_bodies(child, source, symbols);
            }
            "namespace_definition" => {
                if let Some(body) = child.child_by_field_name("body") {
                    extract_class_bodies(body, source, symbols);
                }
            }
            "preproc_ifdef" | "preproc_if" | "preproc_elif" | "preproc_else" | "preproc_ifndef" => {
                extract_class_bodies(child, source, symbols);
            }
            _ => {
                extract_class_bodies(child, source, symbols);
            }
        }
    }
}

fn extract_class_body(
    body: Node,
    source: &str,
    symbols: &mut Vec<Symbol>,
    class_name: Option<&str>,
    default_vis: Visibility,
) {
    let mut current_vis = default_vis;
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        match child.kind() {
            "access_specifier" => {
                let text = node_text(child, source);
                if text.starts_with("public") {
                    current_vis = Visibility::Public;
                } else if text.starts_with("private") {
                    current_vis = Visibility::Private;
                } else if text.starts_with("protected") {
                    current_vis = Visibility::Unknown;
                }
            }
            "function_definition" | "declaration" => {
                extract_class_member(child, source, symbols, class_name, current_vis);
            }
            "field_declaration" => {
                extract_class_field(child, source, symbols, class_name, current_vis);
            }
            _ => {}
        }
    }
}

fn extract_class_member(
    node: Node,
    source: &str,
    symbols: &mut Vec<Symbol>,
    class_name: Option<&str>,
    vis: Visibility,
) {
    if node.kind() == "function_definition" {
        if let Some(decl) = node.child_by_field_name("declarator") {
            let name_node = innermost_declarator_name(decl);
            if name_node.kind() == "identifier" || name_node.kind() == "field_identifier" {
                let name = node_text(name_node, source).to_string();
                let line = name_node.start_position().row;
                let col = name_node.start_position().column;
                if !symbols.iter().any(|s| s.line == line && s.col == col) {
                    symbols.push(make_contained_symbol(
                        name,
                        SymbolKind::Method,
                        line,
                        col,
                        "method",
                        vis,
                        class_name,
                        1,
                        None,
                        None,
                    ));
                }
            }
        }
    } else if node.kind() == "declaration" {
        // Method declaration (without body) in class
        if let Some(decl) = node.child_by_field_name("declarator") {
            if is_function_prototype(&decl) {
                let name_node = innermost_declarator_name(decl);
                if name_node.kind() == "identifier" || name_node.kind() == "field_identifier" {
                    let name = node_text(name_node, source).to_string();
                    let line = name_node.start_position().row;
                    let col = name_node.start_position().column;
                    if !symbols.iter().any(|s| s.line == line && s.col == col) {
                        symbols.push(make_contained_symbol(
                            name,
                            SymbolKind::Method,
                            line,
                            col,
                            "method",
                            vis,
                            class_name,
                            1,
                            None,
                            None,
                        ));
                    }
                }
            }
        }
    }
}

fn extract_class_field(
    node: Node,
    source: &str,
    symbols: &mut Vec<Symbol>,
    class_name: Option<&str>,
    vis: Visibility,
) {
    if let Some(decl) = node.child_by_field_name("declarator") {
        let name_node = innermost_declarator_name(decl);
        if name_node.kind() == "field_identifier" || name_node.kind() == "identifier" {
            let name = node_text(name_node, source).to_string();
            let line = name_node.start_position().row;
            let col = name_node.start_position().column;
            if !symbols.iter().any(|s| s.line == line && s.col == col) {
                let type_text = node
                    .child_by_field_name("type")
                    .map(|t| node_text(t, source).to_string());
                symbols.push(make_contained_symbol(
                    name,
                    SymbolKind::Variable,
                    line,
                    col,
                    "field",
                    vis,
                    class_name,
                    1,
                    None,
                    type_text,
                ));
            }
        }
    }
}

fn extract_typedef_function_ptrs(node: Node, source: &str, symbols: &mut Vec<Symbol>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "type_definition" => {
                if let Some(decl) = child.child_by_field_name("declarator") {
                    if decl.kind() != "type_identifier" {
                        let name_node = innermost_declarator_name(decl);
                        let name = node_text(name_node, source);
                        let line = name_node.start_position().row;
                        let col = name_node.start_position().column;
                        if !symbols.iter().any(|s| s.line == line && s.col == col) {
                            symbols.push(make_symbol(
                                name.to_string(),
                                SymbolKind::TypeAlias,
                                line,
                                col,
                                "typedef",
                                Visibility::Unknown,
                            ));
                        }
                    }
                }
            }
            "preproc_ifdef" | "preproc_if" | "preproc_elif" | "preproc_else" | "preproc_ifndef" => {
                extract_typedef_function_ptrs(child, source, symbols);
            }
            _ => {}
        }
    }
}

fn extract_file_scope_vars(node: Node, source: &str, symbols: &mut Vec<Symbol>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "declaration" => {
                let declarator = match child.child_by_field_name("declarator") {
                    Some(d) => d,
                    None => continue,
                };
                if is_function_prototype(&declarator) {
                    continue;
                }
                let name_node = innermost_declarator_name(declarator);
                if name_node.kind() == "identifier" || name_node.kind() == "type_identifier" {
                    let name = node_text(name_node, source).to_string();
                    let line = name_node.start_position().row;
                    let col = name_node.start_position().column;
                    if !symbols.iter().any(|s| s.line == line && s.col == col) {
                        let is_static = common::has_child_with_kind_and_text(
                            child,
                            "storage_class_specifier",
                            "static",
                            source,
                        );
                        symbols.push(make_symbol(
                            name,
                            SymbolKind::Variable,
                            line,
                            col,
                            "variable",
                            if is_static {
                                Visibility::Private
                            } else {
                                Visibility::Unknown
                            },
                        ));
                    }
                }
            }
            "preproc_ifdef" | "preproc_if" | "preproc_elif" | "preproc_else" | "preproc_ifndef" => {
                extract_file_scope_vars(child, source, symbols);
            }
            _ => {}
        }
    }
}

fn innermost_declarator_name(mut node: Node) -> Node {
    loop {
        match node.kind() {
            "function_declarator"
            | "array_declarator"
            | "parenthesized_declarator"
            | "pointer_declarator"
            | "reference_declarator"
            | "init_declarator" => {
                if let Some(decl) = node.child_by_field_name("declarator") {
                    node = decl;
                } else {
                    let mut cursor = node.walk();
                    let mut found = false;
                    for child in node.named_children(&mut cursor) {
                        match child.kind() {
                            "identifier"
                            | "type_identifier"
                            | "field_identifier"
                            | "qualified_identifier"
                            | "pointer_declarator"
                            | "reference_declarator"
                            | "function_declarator"
                            | "array_declarator"
                            | "parenthesized_declarator" => {
                                node = child;
                                found = true;
                                break;
                            }
                            _ => {}
                        }
                    }
                    if !found {
                        break;
                    }
                }
            }
            "qualified_identifier" => {
                if let Some(name) = node.child_by_field_name("name") {
                    node = name;
                } else {
                    break;
                }
            }
            _ => break,
        }
    }
    node
}

fn is_function_prototype(node: &Node) -> bool {
    match node.kind() {
        "function_declarator" => true,
        "pointer_declarator" | "reference_declarator" => {
            if let Some(decl) = node.child_by_field_name("declarator") {
                is_function_prototype(&decl)
            } else {
                false
            }
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cpp_parser_basic() {
        let source = r#"
#define MAX_SIZE 100

namespace mylib {

class Config {
public:
    Config(const std::string& name);
    std::string getName() const;
private:
    std::string name_;
    int value_;
};

struct Point {
    double x;
    double y;
};

enum Color { RED, GREEN, BLUE };

typedef unsigned int uint32;
using StringVec = std::vector<std::string>;

void helper() {}

}
"#;
        let result = CppParser::parse(source);
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();

        assert!(
            names.contains(&"MAX_SIZE"),
            "should find #define MAX_SIZE, got: {:?}",
            names
        );
        assert!(
            names.contains(&"mylib"),
            "should find namespace mylib, got: {:?}",
            names
        );
        assert!(
            names.contains(&"Config"),
            "should find class Config, got: {:?}",
            names
        );
        assert!(
            names.contains(&"Point"),
            "should find struct Point, got: {:?}",
            names
        );
        assert!(
            names.contains(&"Color"),
            "should find enum Color, got: {:?}",
            names
        );
        assert!(
            names.contains(&"RED"),
            "should find enumerator RED, got: {:?}",
            names
        );
        assert!(
            names.contains(&"uint32"),
            "should find typedef uint32, got: {:?}",
            names
        );
        assert!(
            names.contains(&"StringVec"),
            "should find alias StringVec, got: {:?}",
            names
        );
        assert!(
            names.contains(&"helper"),
            "should find function helper, got: {:?}",
            names
        );

        // Check class members
        assert!(
            names.contains(&"name_"),
            "should find field name_, got: {:?}",
            names
        );

        assert!(!result.occurrences.is_empty());
    }
}
