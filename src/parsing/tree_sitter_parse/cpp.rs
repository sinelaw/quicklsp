//! Tree-sitter based C++ parser.
//!
//! Extends C parsing with C++-specific constructs: classes, namespaces,
//! access specifiers, templates, and methods.

use tree_sitter::Node;

use crate::parsing::symbols::{Symbol, SymbolKind};
use crate::parsing::tokenizer::Visibility;

use super::common::{
    self, make_contained_symbol, make_symbol, node_text, walk_preproc_conditionals,
};
use super::{ParseResult, TsParser};

const CPP_IDENT_KINDS: &[&str] = &[
    "identifier",
    "type_identifier",
    "field_identifier",
    "namespace_identifier",
    "destructor_name",
];

pub struct CppParser;

impl TsParser for CppParser {
    fn parse(source: &str) -> ParseResult {
        let lang: tree_sitter::Language = tree_sitter_cpp::LANGUAGE.into();
        common::run_parse(source, &lang, CPP_IDENT_KINDS, |root, src, syms| {
            walk_preproc_conditionals(root, src, syms, |n, s, sy| {
                extract_definition(n, s, sy, None, Visibility::Unknown);
            });
        })
    }
}

fn extract_definition(
    node: Node,
    source: &str,
    symbols: &mut Vec<Symbol>,
    container: Option<&str>,
    default_vis: Visibility,
) {
    match node.kind() {
        "function_definition" => {
            if let Some((name, line, col)) = extract_function_name(node, source) {
                let is_static = common::has_child_with_kind_and_text(
                    node, "storage_class_specifier", "static", source,
                );
                let vis = if is_static { Visibility::Private } else { default_vis };
                let (kind, kw) = if container.is_some() {
                    (SymbolKind::Method, "method")
                } else {
                    (SymbolKind::Function, "function")
                };
                if let Some(c) = container {
                    symbols.push(make_contained_symbol(
                        name, kind, line, col, kw, vis, Some(c), 1, None, None,
                    ));
                } else {
                    symbols.push(make_symbol(name, kind, line, col, kw, vis));
                }
            }
        }
        "declaration" => {
            if container.is_none() {
                extract_file_scope_declaration(node, source, symbols);
            }
        }
        "field_declaration" => {
            // Class/struct member variable
            if let Some(decl) = node.child_by_field_name("declarator") {
                let name_node = innermost_declarator_name(decl);
                let kind_str = name_node.kind();
                if kind_str == "field_identifier" || kind_str == "identifier" {
                    let name = node_text(name_node, source).to_string();
                    let type_text = node.child_by_field_name("type")
                        .map(|t| node_text(t, source).to_string());
                    symbols.push(make_contained_symbol(
                        name, SymbolKind::Variable,
                        name_node.start_position().row, name_node.start_position().column,
                        "field", default_vis, container, 1, None, type_text,
                    ));
                }
            }
        }
        "struct_specifier" | "union_specifier" => {
            let struct_name = node.child_by_field_name("name")
                .map(|n| node_text(n, source).to_string());
            if node.child_by_field_name("body").is_some() {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = node_text(name_node, source).to_string();
                    let kw = if node.kind() == "struct_specifier" { "struct" } else { "union" };
                    symbols.push(make_symbol(
                        name, SymbolKind::Struct,
                        name_node.start_position().row, name_node.start_position().column,
                        kw, default_vis,
                    ));
                }
                if let Some(body) = node.child_by_field_name("body") {
                    extract_class_body(body, source, symbols, struct_name.as_deref(), Visibility::Public);
                }
            }
        }
        "class_specifier" => {
            let class_name = node.child_by_field_name("name")
                .map(|n| node_text(n, source).to_string());
            if node.child_by_field_name("body").is_some() {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = node_text(name_node, source).to_string();
                    symbols.push(make_symbol(
                        name, SymbolKind::Class,
                        name_node.start_position().row, name_node.start_position().column,
                        "class", default_vis,
                    ));
                }
                if let Some(body) = node.child_by_field_name("body") {
                    // C++ class defaults to private
                    extract_class_body(body, source, symbols, class_name.as_deref(), Visibility::Private);
                }
            }
        }
        "enum_specifier" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                symbols.push(make_symbol(
                    name, SymbolKind::Enum,
                    name_node.start_position().row, name_node.start_position().column,
                    "enum", default_vis,
                ));
            }
            if let Some(body) = node.child_by_field_name("body") {
                let mut cursor = body.walk();
                for child in body.children(&mut cursor) {
                    if child.kind() == "enumerator" {
                        if let Some(name_node) = child.child_by_field_name("name") {
                            let name = node_text(name_node, source).to_string();
                            symbols.push(make_symbol(
                                name, SymbolKind::Constant,
                                name_node.start_position().row, name_node.start_position().column,
                                "enum", Visibility::Unknown,
                            ));
                        }
                    }
                }
            }
        }
        "namespace_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                symbols.push(make_symbol(
                    name, SymbolKind::Module,
                    name_node.start_position().row, name_node.start_position().column,
                    "namespace", Visibility::Unknown,
                ));
            }
            // Recurse into namespace body
            if let Some(body) = node.child_by_field_name("body") {
                walk_preproc_conditionals(body, source, symbols, |n, s, sy| {
                    extract_definition(n, s, sy, None, Visibility::Unknown);
                });
            }
        }
        "type_definition" => {
            if let Some(decl) = node.child_by_field_name("declarator") {
                let name_node = innermost_declarator_name(decl);
                let name = node_text(name_node, source).to_string();
                symbols.push(make_symbol(
                    name, SymbolKind::TypeAlias,
                    name_node.start_position().row, name_node.start_position().column,
                    "typedef", Visibility::Unknown,
                ));
            }
        }
        "alias_declaration" => {
            // using Name = Type;
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                symbols.push(make_symbol(
                    name, SymbolKind::TypeAlias,
                    name_node.start_position().row, name_node.start_position().column,
                    "using", default_vis,
                ));
            }
        }
        "template_declaration" => {
            // Recurse into the templated declaration
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() != "template_parameter_list" {
                    extract_definition(child, source, symbols, container, default_vis);
                }
            }
        }
        "preproc_def" | "preproc_function_def" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                symbols.push(make_symbol(
                    name, SymbolKind::Constant,
                    name_node.start_position().row, name_node.start_position().column,
                    "define", Visibility::Unknown,
                ));
            }
        }
        _ => {}
    }
}

/// Walk class/struct body, tracking access specifier sections.
fn extract_class_body(
    body: Node,
    source: &str,
    symbols: &mut Vec<Symbol>,
    class_name: Option<&str>,
    initial_vis: Visibility,
) {
    let mut current_vis = initial_vis;
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        match child.kind() {
            "access_specifier" => {
                let text = node_text(child, source).trim_end_matches(':').trim();
                current_vis = match text {
                    "public" => Visibility::Public,
                    "private" => Visibility::Private,
                    _ => Visibility::Unknown, // protected
                };
            }
            _ => {
                extract_definition(child, source, symbols, class_name, current_vis);
            }
        }
    }
}

fn extract_function_name(node: Node, source: &str) -> Option<(String, usize, usize)> {
    let declarator = node.child_by_field_name("declarator")?;
    let name_node = innermost_declarator_name(declarator);
    let name = node_text(name_node, source).to_string();
    Some((name, name_node.start_position().row, name_node.start_position().column))
}

fn innermost_declarator_name(mut node: Node) -> Node {
    loop {
        match node.kind() {
            "function_declarator" | "array_declarator" | "parenthesized_declarator"
            | "pointer_declarator" | "reference_declarator" | "init_declarator"
            | "structured_binding_declarator" => {
                if let Some(decl) = node.child_by_field_name("declarator") {
                    node = decl;
                } else {
                    break;
                }
            }
            "qualified_identifier" => {
                // namespace::name — take the rightmost name
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

fn extract_file_scope_declaration(node: Node, source: &str, symbols: &mut Vec<Symbol>) {
    let declarator = match node.child_by_field_name("declarator") {
        Some(d) => d,
        None => return,
    };

    if is_function_prototype(&declarator) {
        return;
    }

    let name_node = innermost_declarator_name(declarator);
    if name_node.kind() == "identifier" || name_node.kind() == "type_identifier" {
        let name = node_text(name_node, source).to_string();
        let is_static = common::has_child_with_kind_and_text(
            node, "storage_class_specifier", "static", source,
        );
        symbols.push(make_symbol(
            name, SymbolKind::Variable,
            name_node.start_position().row, name_node.start_position().column,
            "variable",
            if is_static { Visibility::Private } else { Visibility::Unknown },
        ));
    }
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
#include <string>
#define MAX_SIZE 100

namespace mylib {

class Config {
public:
    std::string name;

    Config(const std::string& n) : name(n) {}

    std::string getName() const {
        return name;
    }

private:
    int value;

    void helper() {}
};

struct Point {
    int x;
    int y;
};

enum Color { RED, GREEN, BLUE };

void free_function() {}

template<typename T>
class Container {
public:
    void add(T item) {}
};

} // namespace mylib
"#;
        let result = CppParser::parse(source);
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();

        assert!(names.contains(&"MAX_SIZE"), "should find #define, got: {:?}", names);
        assert!(names.contains(&"mylib"), "should find namespace, got: {:?}", names);
        assert!(names.contains(&"Config"), "should find class, got: {:?}", names);
        assert!(names.contains(&"getName"), "should find method, got: {:?}", names);
        assert!(names.contains(&"helper"), "should find private method, got: {:?}", names);
        assert!(names.contains(&"Point"), "should find struct, got: {:?}", names);
        assert!(names.contains(&"Color"), "should find enum, got: {:?}", names);
        assert!(names.contains(&"RED"), "should find enumerator, got: {:?}", names);
        assert!(names.contains(&"free_function"), "should find free function, got: {:?}", names);
        assert!(names.contains(&"Container"), "should find template class, got: {:?}", names);
        assert!(names.contains(&"add"), "should find template class method, got: {:?}", names);

        // Check access specifier visibility
        let get_name = result.symbols.iter().find(|s| s.name == "getName").unwrap();
        assert_eq!(get_name.visibility, Visibility::Public);

        let helper_sym = result.symbols.iter().find(|s| s.name == "helper").unwrap();
        assert_eq!(helper_sym.visibility, Visibility::Private);

        assert!(!result.occurrences.is_empty());
    }
}
