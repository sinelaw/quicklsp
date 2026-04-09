//! Tree-sitter based Python parser using SCM queries.
//!
//! Extracts: functions, classes, methods, decorators, and module-level assignments.

use tree_sitter::Node;

use crate::parsing::symbols::{Symbol, SymbolKind};
use crate::parsing::tokenizer::Visibility;

use super::common::{self, make_contained_symbol, node_text, QueryParseConfig};
use super::{ParseResult, TsParser};

const PYTHON_IDENT_KINDS: &[&str] = &["identifier"];

const PYTHON_QUERY: &str = r#"
; ── Functions ────────────────────────────────────────────────────────────
(function_definition
  name: (identifier) @name) @definition.function

; ── Methods inside classes ───────────────────────────────────────────────
(class_definition
  name: (identifier) @container
  body: (block
    (function_definition
      name: (identifier) @name) @definition.method))

; ── Decorated methods inside classes ─────────────────────────────────────
(class_definition
  name: (identifier) @container
  body: (block
    (decorated_definition
      definition: (function_definition
        name: (identifier) @name) @definition.method)))

; ── Classes ──────────────────────────────────────────────────────────────
(class_definition
  name: (identifier) @name) @definition.class

; ── Decorated functions (top-level) ──────────────────────────────────────
(decorated_definition
  definition: (function_definition
    name: (identifier) @name) @definition.function)

; ── Module-level assignments ─────────────────────────────────────────────
(module
  (expression_statement
    (assignment
      left: (identifier) @name) @definition.variable))
"#;

pub struct PythonParser;

impl TsParser for PythonParser {
    fn parse(source: &str) -> ParseResult {
        let mut result = common::run_query_parse(
            source,
            &QueryParseConfig {
                language: tree_sitter_python::LANGUAGE.into(),
                query_source: PYTHON_QUERY,
                identifier_kinds: PYTHON_IDENT_KINDS,
                def_keyword: python_def_keyword,
                visibility: |_node: Node, _source: &str| Visibility::Unknown,
                post_process: Some(python_post_process),
            },
        );
        // Post-process: apply name-based visibility and constant detection
        for sym in &mut result.symbols {
            sym.visibility = python_visibility(&sym.name);
            if sym.kind == SymbolKind::Variable
                && sym.container.is_none()
                && sym.name.chars().all(|c| c.is_uppercase() || c == '_')
            {
                sym.kind = SymbolKind::Constant;
                sym.def_keyword = "const".to_string();
            }
        }
        result
    }
}

/// Post-process: extract Python function parameters and assignment
/// locals inside function bodies so `find_local_definition_at` can
/// resolve them.
fn python_post_process(root: Node, source: &str, symbols: &mut Vec<Symbol>) {
    walk_python(root, source, symbols, None, 0, None);
}

fn walk_python(
    node: Node,
    source: &str,
    symbols: &mut Vec<Symbol>,
    container: Option<&str>,
    depth: usize,
    scope_end: Option<usize>,
) {
    match node.kind() {
        "function_definition" => {
            let name_text = node
                .child_by_field_name("name")
                .map(|n| node_text(n, source).to_string());
            let body_end = node
                .child_by_field_name("body")
                .map(|b| b.end_position().row)
                .unwrap_or_else(|| node.end_position().row);

            if let Some(params) = node.child_by_field_name("parameters") {
                extract_python_params(
                    params,
                    source,
                    symbols,
                    name_text.as_deref(),
                    depth + 1,
                    body_end,
                );
            }
            if let Some(body) = node.child_by_field_name("body") {
                let new_container = name_text.as_deref().or(container);
                let mut cursor = body.walk();
                for child in body.children(&mut cursor) {
                    walk_python(
                        child,
                        source,
                        symbols,
                        new_container,
                        depth + 1,
                        Some(body_end),
                    );
                }
            }
            return;
        }
        // An assignment inside a function body introduces a local binding
        // (only when we're actually inside a function — depth > 0 — so
        // that module-level assignments stay globals handled by the SCM query).
        "assignment" if depth > 0 => {
            if let Some(left) = node.child_by_field_name("left") {
                let end = scope_end.unwrap_or_else(|| node.end_position().row);
                collect_python_assignment_targets(left, source, symbols, container, depth, end);
            }
            if let Some(right) = node.child_by_field_name("right") {
                walk_python(right, source, symbols, container, depth, scope_end);
            }
            return;
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_python(child, source, symbols, container, depth, scope_end);
    }
}

fn extract_python_params(
    params: Node,
    source: &str,
    symbols: &mut Vec<Symbol>,
    fn_name: Option<&str>,
    depth: usize,
    scope_end: usize,
) {
    let mut cursor = params.walk();
    for child in params.children(&mut cursor) {
        // Collect identifiers inside the parameter node. Python has a few
        // variants: bare `identifier`, `typed_parameter`, `default_parameter`,
        // `typed_default_parameter`, `list_splat_pattern`, `dictionary_splat_pattern`.
        match child.kind() {
            "identifier" => {
                push_python_local(
                    child,
                    source,
                    symbols,
                    fn_name,
                    depth,
                    scope_end,
                    "parameter",
                );
            }
            "typed_parameter"
            | "default_parameter"
            | "typed_default_parameter"
            | "list_splat_pattern"
            | "dictionary_splat_pattern" => {
                // The bound name is the first `identifier` descendant.
                if let Some(ident) = first_python_identifier(child) {
                    push_python_local(
                        ident,
                        source,
                        symbols,
                        fn_name,
                        depth,
                        scope_end,
                        "parameter",
                    );
                }
            }
            _ => {}
        }
    }
}

fn collect_python_assignment_targets(
    target: Node,
    source: &str,
    symbols: &mut Vec<Symbol>,
    container: Option<&str>,
    depth: usize,
    scope_end: usize,
) {
    match target.kind() {
        "identifier" => {
            push_python_local(
                target, source, symbols, container, depth, scope_end, "variable",
            );
        }
        "pattern_list" | "tuple_pattern" | "list_pattern" => {
            let mut cursor = target.walk();
            for child in target.children(&mut cursor) {
                collect_python_assignment_targets(
                    child, source, symbols, container, depth, scope_end,
                );
            }
        }
        // attribute (self.x = …) / subscript (a[0] = …) / etc. are not new
        // bindings — they mutate an existing object.
        _ => {}
    }
}

fn push_python_local(
    ident: Node,
    source: &str,
    symbols: &mut Vec<Symbol>,
    container: Option<&str>,
    depth: usize,
    scope_end: usize,
    def_keyword: &str,
) {
    let name = node_text(ident, source).to_string();
    if name.is_empty() || name == "_" {
        return;
    }
    let line = ident.start_position().row;
    let col = ident.start_position().column;
    if symbols.iter().any(|s| s.line == line && s.col == col) {
        return;
    }
    symbols.push(make_contained_symbol(
        name,
        SymbolKind::Variable,
        line,
        col,
        def_keyword,
        Visibility::Private,
        container,
        depth,
        Some(scope_end),
        None,
    ));
}

fn first_python_identifier(node: Node) -> Option<Node> {
    if node.kind() == "identifier" {
        return Some(node);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(found) = first_python_identifier(child) {
            return Some(found);
        }
    }
    None
}

fn python_def_keyword(_kind: SymbolKind, suffix: &str) -> &'static str {
    match suffix {
        "function" => "def",
        "method" => "method",
        "class" => "class",
        "variable" => "variable",
        _ => "def",
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

        assert!(
            names.contains(&"MAX_SIZE"),
            "should find constant MAX_SIZE, got: {:?}",
            names
        );
        assert!(
            names.contains(&"_private_var"),
            "should find _private_var, got: {:?}",
            names
        );
        assert!(
            names.contains(&"hello"),
            "should find function hello, got: {:?}",
            names
        );
        assert!(
            names.contains(&"_helper"),
            "should find function _helper, got: {:?}",
            names
        );
        assert!(
            names.contains(&"Config"),
            "should find class Config, got: {:?}",
            names
        );
        assert!(
            names.contains(&"__init__"),
            "should find method __init__, got: {:?}",
            names
        );
        assert!(
            names.contains(&"get_name"),
            "should find method get_name, got: {:?}",
            names
        );
        assert!(
            names.contains(&"_PrivateClass"),
            "should find _PrivateClass, got: {:?}",
            names
        );

        // Check visibility
        let hello_sym = result.symbols.iter().find(|s| s.name == "hello").unwrap();
        assert_eq!(hello_sym.visibility, Visibility::Public);

        let helper_sym = result.symbols.iter().find(|s| s.name == "_helper").unwrap();
        assert_eq!(helper_sym.visibility, Visibility::Private);

        // Check method container
        let init_sym = result
            .symbols
            .iter()
            .find(|s| s.name == "__init__")
            .unwrap();
        assert_eq!(init_sym.container.as_deref(), Some("Config"));

        // Check constant detection
        let max_sym = result
            .symbols
            .iter()
            .find(|s| s.name == "MAX_SIZE")
            .unwrap();
        assert_eq!(max_sym.kind, SymbolKind::Constant);

        assert!(!result.occurrences.is_empty());
    }

    #[test]
    fn test_python_parser_fixture() {
        let source = include_str!("../../../tests/fixtures/sample_python.py");
        let result = PythonParser::parse(source);
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();

        // Constants
        assert!(names.contains(&"MAX_RETRIES"), "got: {:?}", names);
        assert!(names.contains(&"DEFAULT_TIMEOUT"));

        // Classes
        assert!(names.contains(&"Config"));
        assert!(names.contains(&"Server"));
        assert!(names.contains(&"Handler"));

        // Methods
        let init = result
            .symbols
            .iter()
            .find(|s| s.name == "__init__" && s.container.as_deref() == Some("Config"));
        assert!(init.is_some(), "should find Config.__init__");

        let display = result
            .symbols
            .iter()
            .find(|s| s.name == "display" && s.container.as_deref() == Some("Config"));
        assert!(display.is_some(), "should find Config.display");

        let add_handler = result
            .symbols
            .iter()
            .find(|s| s.name == "add_handler" && s.container.as_deref() == Some("Server"));
        assert!(add_handler.is_some(), "should find Server.add_handler");

        // Functions
        assert!(names.contains(&"process_request"));
        assert!(names.contains(&"validate_input"));
    }

    #[test]
    fn test_python_empty_file() {
        let result = PythonParser::parse("");
        assert!(result.symbols.is_empty());
    }

    #[test]
    fn test_python_decorated_class_method() {
        let source = r#"
class MyClass:
    @staticmethod
    def static_method():
        pass

    @classmethod
    def class_method(cls):
        pass

    @property
    def prop(self):
        return self._prop
"#;
        let result = PythonParser::parse(source);
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"MyClass"));
        assert!(names.contains(&"static_method"), "got: {:?}", names);
        assert!(names.contains(&"class_method"), "got: {:?}", names);
        assert!(names.contains(&"prop"), "got: {:?}", names);
    }
}
