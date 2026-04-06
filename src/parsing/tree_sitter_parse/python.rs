//! Tree-sitter based Python parser.
//!
//! Extracts: functions, classes, methods, decorators, and module-level assignments.

use tree_sitter::Node;

use crate::parsing::symbols::{Symbol, SymbolKind};
use crate::parsing::tokenizer::Visibility;

use super::common::{self, make_contained_symbol, make_symbol, node_text};
use super::{ParseResult, TsParser};

const PYTHON_IDENT_KINDS: &[&str] = &["identifier"];

pub struct PythonParser;

impl TsParser for PythonParser {
    fn parse(source: &str) -> ParseResult {
        let lang: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
        common::run_parse(source, &lang, PYTHON_IDENT_KINDS, collect_definitions)
    }
}

fn collect_definitions(root: Node, source: &str, symbols: &mut Vec<Symbol>) {
    collect_definitions_recursive(root, source, symbols, None);
}

fn collect_definitions_recursive(
    node: Node,
    source: &str,
    symbols: &mut Vec<Symbol>,
    container: Option<&str>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        extract_definition(child, source, symbols, container);
    }
}

fn extract_definition(node: Node, source: &str, symbols: &mut Vec<Symbol>, container: Option<&str>) {
    match node.kind() {
        "function_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                let vis = python_visibility(&name);
                let (kind, kw) = if container.is_some() {
                    (SymbolKind::Method, "method")
                } else {
                    (SymbolKind::Function, "def")
                };
                if let Some(c) = container {
                    symbols.push(make_contained_symbol(
                        name, kind,
                        name_node.start_position().row, name_node.start_position().column,
                        kw, vis, Some(c), 1, None, None,
                    ));
                } else {
                    symbols.push(make_symbol(
                        name, kind,
                        name_node.start_position().row, name_node.start_position().column,
                        kw, vis,
                    ));
                }
            }
        }
        "class_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                let vis = python_visibility(&name);
                symbols.push(make_symbol(
                    name.clone(), SymbolKind::Class,
                    name_node.start_position().row, name_node.start_position().column,
                    "class", vis,
                ));
                // Class body is in the "body" field (a block node)
                if let Some(body) = node.child_by_field_name("body") {
                    collect_definitions_recursive(body, source, symbols, Some(&name));
                }
            }
        }
        "decorated_definition" => {
            // Recurse into the actual definition inside the decorator
            if let Some(def) = node.child_by_field_name("definition") {
                extract_definition(def, source, symbols, container);
            }
        }
        "expression_statement" => {
            // Module-level assignments: `x = 5` at top level (only when no container)
            if container.is_none() {
                // expression_statement has a single child
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if child.kind() == "assignment" {
                        extract_assignment(child, source, symbols);
                    }
                }
            }
        }
        _ => {}
    }
}

fn extract_assignment(node: Node, source: &str, symbols: &mut Vec<Symbol>) {
    if let Some(left) = node.child_by_field_name("left") {
        if left.kind() == "identifier" {
            let name = node_text(left, source).to_string();
            let vis = python_visibility(&name);
            let (kind, kw) = if name.chars().all(|c| c.is_uppercase() || c == '_') {
                (SymbolKind::Constant, "const")
            } else {
                (SymbolKind::Variable, "variable")
            };
            symbols.push(make_symbol(
                name, kind,
                left.start_position().row, left.start_position().column,
                kw, vis,
            ));
        }
    }
}

fn python_visibility(name: &str) -> Visibility {
    if name.starts_with('_') {
        Visibility::Private
    } else {
        Visibility::Public
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_python_parser_basic() {
        let source = r#"
MAX_SIZE = 100
_private_var = "hidden"

def hello(name):
    return f"Hello, {name}"

def _helper():
    pass

class Config:
    def __init__(self, name):
        self.name = name

    def get_name(self):
        return self.name

    def _internal(self):
        pass

@staticmethod
def decorated():
    pass

class _PrivateClass:
    pass
"#;
        let result = PythonParser::parse(source);
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();

        assert!(names.contains(&"MAX_SIZE"), "should find constant MAX_SIZE, got: {:?}", names);
        assert!(names.contains(&"_private_var"), "should find _private_var, got: {:?}", names);
        assert!(names.contains(&"hello"), "should find function hello, got: {:?}", names);
        assert!(names.contains(&"_helper"), "should find function _helper, got: {:?}", names);
        assert!(names.contains(&"Config"), "should find class Config, got: {:?}", names);
        assert!(names.contains(&"__init__"), "should find method __init__, got: {:?}", names);
        assert!(names.contains(&"get_name"), "should find method get_name, got: {:?}", names);
        assert!(names.contains(&"_PrivateClass"), "should find _PrivateClass, got: {:?}", names);

        // Check visibility
        let hello_sym = result.symbols.iter().find(|s| s.name == "hello").unwrap();
        assert_eq!(hello_sym.visibility, Visibility::Public);

        let helper_sym = result.symbols.iter().find(|s| s.name == "_helper").unwrap();
        assert_eq!(helper_sym.visibility, Visibility::Private);

        // Check method container
        let init_sym = result.symbols.iter().find(|s| s.name == "__init__").unwrap();
        assert_eq!(init_sym.container.as_deref(), Some("Config"));

        // Check constant detection
        let max_sym = result.symbols.iter().find(|s| s.name == "MAX_SIZE").unwrap();
        assert_eq!(max_sym.kind, SymbolKind::Constant);

        assert!(!result.occurrences.is_empty());
    }
}
