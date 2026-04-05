//! Tree-sitter based C parser.
//!
//! Extracts accurate definitions (functions, structs, enums, enum values,
//! typedefs, #defines, file-scope globals) and all identifier occurrences
//! from a single AST walk.

use tree_sitter::{Node, Parser};

use crate::parsing::symbols::{Symbol, SymbolKind};
use crate::parsing::tokenizer::Visibility;
use crate::parsing::tokenizer::{Occurrence, OccurrenceRole};

use super::{ParseResult, TsParser};

pub struct CParser;

impl TsParser for CParser {
    fn parse(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_c::LANGUAGE.into())
            .expect("failed to load tree-sitter-c grammar");

        let tree = match parser.parse(source, None) {
            Some(t) => t,
            None => return ParseResult { symbols: Vec::new(), occurrences: Vec::new() },
        };

        let mut symbols = Vec::new();
        let mut occurrences = Vec::new();

        collect_definitions(tree.root_node(), source, &mut symbols);
        collect_occurrences(tree.root_node(), source, &symbols, &mut occurrences);

        ParseResult { symbols, occurrences }
    }
}

/// Walk top-level nodes and extract definitions.
/// Recurses into preprocessor conditional blocks (#ifdef, #ifndef, #if, #else,
/// #elif) so that definitions guarded by conditionals are still indexed.
fn collect_definitions(root: Node, source: &str, symbols: &mut Vec<Symbol>) {
    collect_definitions_recursive(root, source, symbols);
}

fn collect_definitions_recursive(node: Node, source: &str, symbols: &mut Vec<Symbol>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "preproc_ifdef" | "preproc_if" | "preproc_elif" | "preproc_else"
            | "preproc_ifndef" => {
                // Recurse into preprocessor conditional blocks to find
                // definitions nested inside #ifdef CONFIG_*, #if, etc.
                collect_definitions_recursive(child, source, symbols);
            }
            _ => {
                extract_definition(child, source, symbols, false);
            }
        }
    }
}

/// Extract a definition from a node, if it represents one.
/// `inside_enum` is true when we're walking children of an enum_specifier body.
fn extract_definition(node: Node, source: &str, symbols: &mut Vec<Symbol>, inside_enum: bool) {
    match node.kind() {
        "function_definition" => {
            let func_name = if let Some((name, line, col)) = extract_function_name(node, source) {
                let is_static = has_storage_class(node, source, "static");
                let n = name.clone();
                symbols.push(Symbol {
                    name,
                    kind: SymbolKind::Function,
                    line,
                    col,
                    def_keyword: "function".to_string(),
                    doc_comment: None,
                    signature: None,
                    visibility: if is_static { Visibility::Private } else { Visibility::Public },
                    container: None,
                    depth: 0,
                });
                Some(n)
            } else {
                None
            };
            // Extract function parameters
            if let Some(ref fname) = func_name {
                extract_function_params(node, source, symbols, fname);
            }
            // Extract local variable declarations from the function body
            if let Some(body) = node.child_by_field_name("body") {
                extract_locals_from_compound(body, source, symbols, func_name.as_deref(), 1);
            }
        }
        "declaration" => {
            // File-scope variable declarations (not inside function bodies)
            // Skip if this is a function prototype (has a function_declarator with parameter_list)
            if !inside_enum {
                extract_file_scope_declaration(node, source, symbols);
            }
        }
        "struct_specifier" | "union_specifier" => {
            let struct_name = node.child_by_field_name("name")
                .map(|n| node_text(n, source).to_string());
            // Only index if it has a body (field_declaration_list) — it's a definition, not just a reference
            if node.child_by_field_name("body").is_some() {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = node_text(name_node, source).to_string();
                    let kind = if node.kind() == "struct_specifier" {
                        SymbolKind::Struct
                    } else {
                        SymbolKind::Struct // union — no dedicated kind, use Struct
                    };
                    symbols.push(Symbol {
                        name,
                        kind,
                        line: name_node.start_position().row,
                        col: name_node.start_position().column,
                        def_keyword: if node.kind() == "struct_specifier" { "struct" } else { "union" }.to_string(),
                        doc_comment: None,
                        signature: None,
                        visibility: Visibility::Unknown,
                        container: None,
                        depth: 0,
                    });
                }
                // Extract struct/union fields
                if let Some(body) = node.child_by_field_name("body") {
                    extract_struct_fields(body, source, symbols, struct_name.as_deref());
                }
            }
        }
        "enum_specifier" => {
            // Index the enum name
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                symbols.push(Symbol {
                    name,
                    kind: SymbolKind::Enum,
                    line: name_node.start_position().row,
                    col: name_node.start_position().column,
                    def_keyword: "enum".to_string(),
                    doc_comment: None,
                    signature: None,
                    visibility: Visibility::Unknown,
                    container: None,
                    depth: 0,
                });
            }
            // Index individual enumerators
            if let Some(body) = node.child_by_field_name("body") {
                let mut body_cursor = body.walk();
                for child in body.children(&mut body_cursor) {
                    if child.kind() == "enumerator" {
                        if let Some(name_node) = child.child_by_field_name("name") {
                            let name = node_text(name_node, source).to_string();
                            symbols.push(Symbol {
                                name,
                                kind: SymbolKind::Constant,
                                line: name_node.start_position().row,
                                col: name_node.start_position().column,
                                def_keyword: "enum".to_string(),
                                doc_comment: None,
                                signature: None,
                                visibility: Visibility::Unknown,
                                container: None,
                                depth: 0,
                            });
                        }
                    }
                }
            }
        }
        "type_definition" => {
            // typedef — the alias name is the `declarator` field (type_identifier)
            if let Some(decl) = node.child_by_field_name("declarator") {
                let name_node = innermost_declarator_name(decl);
                let name = node_text(name_node, source).to_string();
                symbols.push(Symbol {
                    name,
                    kind: SymbolKind::TypeAlias,
                    line: name_node.start_position().row,
                    col: name_node.start_position().column,
                    def_keyword: "typedef".to_string(),
                    doc_comment: None,
                    signature: None,
                    visibility: Visibility::Unknown,
                    container: None,
                    depth: 0,
                });
            }
        }
        "preproc_def" => {
            // #define NAME value
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                symbols.push(Symbol {
                    name,
                    kind: SymbolKind::Constant,
                    line: name_node.start_position().row,
                    col: name_node.start_position().column,
                    def_keyword: "define".to_string(),
                    doc_comment: None,
                    signature: None,
                    visibility: Visibility::Unknown,
                    container: None,
                    depth: 0,
                });
            }
        }
        "preproc_function_def" => {
            // #define NAME(args) body — function-like macros
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                symbols.push(Symbol {
                    name,
                    kind: SymbolKind::Constant,
                    line: name_node.start_position().row,
                    col: name_node.start_position().column,
                    def_keyword: "define".to_string(),
                    doc_comment: None,
                    signature: None,
                    visibility: Visibility::Unknown,
                    container: None,
                    depth: 0,
                });
            }
        }
        _ => {}
    }
}

/// Extract function name from a function_definition node.
/// Handles: `void foo()`, `static int bar()`, `char *baz()`, etc.
fn extract_function_name(node: Node, source: &str) -> Option<(String, usize, usize)> {
    let declarator = node.child_by_field_name("declarator")?;
    let name_node = innermost_declarator_name(declarator);
    let name = node_text(name_node, source).to_string();
    Some((name, name_node.start_position().row, name_node.start_position().column))
}

/// Drill into nested declarators (pointer_declarator, function_declarator, etc.)
/// to find the actual identifier node.
fn innermost_declarator_name(mut node: Node) -> Node {
    loop {
        match node.kind() {
            "function_declarator" | "array_declarator" | "parenthesized_declarator"
            | "pointer_declarator" | "init_declarator" => {
                if let Some(decl) = node.child_by_field_name("declarator") {
                    node = decl;
                } else {
                    break;
                }
            }
            _ => break,
        }
    }
    node
}

/// Check if a function_definition has a specific storage class (e.g., "static").
fn has_storage_class(node: Node, source: &str, specifier: &str) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "storage_class_specifier" && node_text(child, source) == specifier {
            return true;
        }
    }
    false
}

/// Extract file-scope variable declarations.
/// Skips function prototypes (declarations with function_declarator that have no body).
fn extract_file_scope_declaration(node: Node, source: &str, symbols: &mut Vec<Symbol>) {
    let declarator = match node.child_by_field_name("declarator") {
        Some(d) => d,
        None => return,
    };

    // If it's a function declarator (prototype), skip it
    if is_function_prototype(&declarator) {
        return;
    }

    let name_node = innermost_declarator_name(declarator);
    if name_node.kind() == "identifier" || name_node.kind() == "type_identifier" {
        let name = node_text(name_node, source).to_string();
        let is_static = has_storage_class_in_declaration(node, source, "static");
        symbols.push(Symbol {
            name,
            kind: SymbolKind::Variable,
            line: name_node.start_position().row,
            col: name_node.start_position().column,
            def_keyword: "variable".to_string(),
            doc_comment: None,
            signature: None,
            visibility: if is_static { Visibility::Private } else { Visibility::Unknown },
            container: None,
            depth: 0,
        });
    }
}

/// Extract function parameters from a function_definition node.
fn extract_function_params(func_node: Node, source: &str, symbols: &mut Vec<Symbol>, func_name: &str) {
    let declarator = match func_node.child_by_field_name("declarator") {
        Some(d) => d,
        None => return,
    };
    // Find the function_declarator (may be nested under pointer_declarator)
    let func_decl = find_function_declarator(declarator);
    let func_decl = match func_decl {
        Some(d) => d,
        None => return,
    };
    if let Some(params) = func_decl.child_by_field_name("parameters") {
        let mut cursor = params.walk();
        for child in params.children(&mut cursor) {
            if child.kind() == "parameter_declaration" {
                if let Some(decl) = child.child_by_field_name("declarator") {
                    let name_node = innermost_declarator_name(decl);
                    if name_node.kind() == "identifier" {
                        let name = node_text(name_node, source).to_string();
                        // Get the type from the node text before the declarator
                        let type_text = child.child_by_field_name("type")
                            .map(|t| node_text(t, source).to_string());
                        symbols.push(Symbol {
                            name,
                            kind: SymbolKind::Variable,
                            line: name_node.start_position().row,
                            col: name_node.start_position().column,
                            def_keyword: "parameter".to_string(),
                            doc_comment: type_text,
                            signature: None,
                            visibility: Visibility::Private,
                            container: Some(func_name.to_string()),
                            depth: 1,
                        });
                    }
                }
            }
        }
    }
}

/// Find the function_declarator node, drilling through pointer_declarator etc.
fn find_function_declarator(mut node: Node) -> Option<Node> {
    loop {
        match node.kind() {
            "function_declarator" => return Some(node),
            "pointer_declarator" | "parenthesized_declarator" => {
                node = node.child_by_field_name("declarator")?;
            }
            _ => return None,
        }
    }
}

/// Extract local variable declarations from a compound_statement (function body).
/// Recurses into nested blocks (if/for/while bodies).
fn extract_locals_from_compound(
    node: Node,
    source: &str,
    symbols: &mut Vec<Symbol>,
    func_name: Option<&str>,
    depth: usize,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "declaration" => {
                extract_local_declaration(child, source, symbols, func_name, depth);
            }
            // Recurse into nested blocks
            "compound_statement" | "if_statement" | "for_statement" | "while_statement"
            | "do_statement" | "switch_statement" | "case_statement" => {
                extract_locals_from_compound(child, source, symbols, func_name, depth + 1);
            }
            _ => {
                // Recurse into any other compound children (e.g., else clauses)
                if child.child_count() > 0 {
                    let has_compound = has_nested_declarations(child);
                    if has_compound {
                        extract_locals_from_compound(child, source, symbols, func_name, depth);
                    }
                }
            }
        }
    }
}

/// Extract a local variable declaration (inside a function body).
/// Handles plain declarations (`int x;`), initialized declarations (`int x = 5;`),
/// and comma-separated declarations (`int a, b = 3, c;`).
fn extract_local_declaration(
    node: Node,
    source: &str,
    symbols: &mut Vec<Symbol>,
    func_name: Option<&str>,
    depth: usize,
) {
    let type_text = node.child_by_field_name("type")
        .map(|t| node_text(t, source).to_string());

    // Try direct declarator field first (plain: `int x;`)
    if let Some(declarator) = node.child_by_field_name("declarator") {
        if !is_function_prototype(&declarator) {
            extract_local_var_from_declarator(declarator, source, symbols, func_name, depth, type_text.as_deref());
            return;
        }
    }

    // Walk children to find init_declarator nodes (`int x = 5;` or `int a, b;`)
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "init_declarator" {
            if let Some(decl) = child.child_by_field_name("declarator") {
                if !is_function_prototype(&decl) {
                    extract_local_var_from_declarator(decl, source, symbols, func_name, depth, type_text.as_deref());
                }
            }
        }
    }
}

fn extract_local_var_from_declarator(
    declarator: Node,
    source: &str,
    symbols: &mut Vec<Symbol>,
    func_name: Option<&str>,
    depth: usize,
    type_text: Option<&str>,
) {
    let name_node = innermost_declarator_name(declarator);
    if name_node.kind() == "identifier" || name_node.kind() == "type_identifier" {
        let name = node_text(name_node, source).to_string();
        symbols.push(Symbol {
            name,
            kind: SymbolKind::Variable,
            line: name_node.start_position().row,
            col: name_node.start_position().column,
            def_keyword: "variable".to_string(),
            doc_comment: type_text.map(|s| s.to_string()),
            signature: None,
            visibility: Visibility::Private,
            container: func_name.map(|s| s.to_string()),
            depth,
        });
    }
}

/// Extract struct/union field declarations.
fn extract_struct_fields(
    body: Node,
    source: &str,
    symbols: &mut Vec<Symbol>,
    struct_name: Option<&str>,
) {
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        if child.kind() == "field_declaration" {
            if let Some(decl) = child.child_by_field_name("declarator") {
                let name_node = innermost_declarator_name(decl);
                if name_node.kind() == "field_identifier" || name_node.kind() == "identifier" {
                    let name = node_text(name_node, source).to_string();
                    let type_text = child.child_by_field_name("type")
                        .map(|t| node_text(t, source).to_string());
                    symbols.push(Symbol {
                        name,
                        kind: SymbolKind::Variable,
                        line: name_node.start_position().row,
                        col: name_node.start_position().column,
                        def_keyword: "field".to_string(),
                        doc_comment: type_text,
                        signature: None,
                        visibility: Visibility::Unknown,
                        container: struct_name.map(|s| s.to_string()),
                        depth: 1,
                    });
                }
            }
        }
    }
}

/// Check if a node has any nested declaration or compound_statement children.
fn has_nested_declarations(node: Node) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "compound_statement" | "declaration" => return true,
            _ => {}
        }
    }
    false
}

fn is_function_prototype(node: &Node) -> bool {
    match node.kind() {
        "function_declarator" => true,
        "pointer_declarator" => {
            if let Some(decl) = node.child_by_field_name("declarator") {
                is_function_prototype(&decl)
            } else {
                false
            }
        }
        _ => false,
    }
}

fn has_storage_class_in_declaration(node: Node, source: &str, specifier: &str) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "storage_class_specifier" && node_text(child, source) == specifier {
            return true;
        }
    }
    false
}

/// Walk the entire AST and collect all identifier/type_identifier occurrences.
fn collect_occurrences(
    node: Node,
    source: &str,
    symbols: &[Symbol],
    occurrences: &mut Vec<Occurrence>,
) {
    collect_occurrences_recursive(node, source, symbols, occurrences);
}

fn collect_occurrences_recursive(
    node: Node,
    source: &str,
    symbols: &[Symbol],
    occurrences: &mut Vec<Occurrence>,
) {
    match node.kind() {
        "identifier" | "type_identifier" | "field_identifier" => {
            let start = node.start_byte();
            let end = node.end_byte();
            let len = end - start;
            if len > 0 && len <= u16::MAX as usize {
                let line = node.start_position().row;
                let col = node.start_position().column;

                // Determine role: Definition if this identifier matches a symbol at the same position
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
        // Also capture preprocessor identifiers
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_occurrences_recursive(child, source, symbols, occurrences);
    }
}

/// Get the text of a node from the source.
fn node_text<'a>(node: Node, source: &'a str) -> &'a str {
    &source[node.start_byte()..node.end_byte()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_c_parser_basic() {
        let source = r#"
#define MAX 100

struct Foo {
    int x;
};

enum Color { RED, GREEN, BLUE };

typedef unsigned int uint32;

void hello(int n) {
    int local = n + 1;
}

static int helper() {
    return 42;
}

int main() {
    return 0;
}
"#;
        let result = CParser::parse(source);

        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"MAX"), "should find #define MAX, got: {:?}", names);
        assert!(names.contains(&"Foo"), "should find struct Foo, got: {:?}", names);
        assert!(names.contains(&"Color"), "should find enum Color, got: {:?}", names);
        assert!(names.contains(&"RED"), "should find enumerator RED, got: {:?}", names);
        assert!(names.contains(&"GREEN"), "should find enumerator GREEN, got: {:?}", names);
        assert!(names.contains(&"BLUE"), "should find enumerator BLUE, got: {:?}", names);
        assert!(names.contains(&"uint32"), "should find typedef uint32, got: {:?}", names);
        assert!(names.contains(&"hello"), "should find function hello, got: {:?}", names);
        assert!(names.contains(&"helper"), "should find static function helper, got: {:?}", names);
        assert!(names.contains(&"main"), "should find function main, got: {:?}", names);

        // Local variable SHOULD now be indexed (with depth > 0)
        assert!(names.contains(&"local"), "local variables should be indexed, got: {:?}", names);
        let local_sym = result.symbols.iter().find(|s| s.name == "local").unwrap();
        assert!(local_sym.depth > 0, "local variable should have depth > 0");
        assert_eq!(local_sym.container.as_deref(), Some("hello"), "local should have container = function name");

        // Check visibility
        let helper_sym = result.symbols.iter().find(|s| s.name == "helper").unwrap();
        assert_eq!(helper_sym.visibility, Visibility::Private);

        let hello_sym = result.symbols.iter().find(|s| s.name == "hello").unwrap();
        assert_eq!(hello_sym.visibility, Visibility::Public);

        // Check occurrences exist
        assert!(!result.occurrences.is_empty(), "should have occurrences");
    }

    #[test]
    fn test_c_parser_ifdef_nested_definitions() {
        // Kernel-style code: definitions inside #ifdef / #if / #ifndef blocks
        let source = r#"
#include <linux/kernel.h>

#ifdef CONFIG_SECURITY
struct security_ops {
    int (*init)(void);
};

static int security_init(void) {
    return 0;
}
#endif

#ifndef CONFIG_PREEMPT
void preempt_disable(void) {}
#endif

#if defined(CONFIG_SMP)
typedef struct {
    int lock;
} spinlock_t;

int spin_lock(spinlock_t *lock) {
    return 0;
}

#ifdef CONFIG_DEBUG_SPINLOCK
void spin_dump(spinlock_t *lock) {}
#endif

#else
void no_smp_fallback(void) {}
#endif

#define CIA_MAX_OPS 16

enum cia_type { CIA_READ, CIA_WRITE };

void top_level_func(void) {}
"#;
        let result = CParser::parse(source);
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();

        // Top-level definitions (should still work)
        assert!(names.contains(&"CIA_MAX_OPS"), "should find top-level #define, got: {:?}", names);
        assert!(names.contains(&"cia_type"), "should find top-level enum, got: {:?}", names);
        assert!(names.contains(&"CIA_READ"), "should find enumerator, got: {:?}", names);
        assert!(names.contains(&"top_level_func"), "should find top-level function, got: {:?}", names);

        // Definitions inside #ifdef CONFIG_SECURITY
        assert!(names.contains(&"security_ops"), "should find struct inside #ifdef, got: {:?}", names);
        assert!(names.contains(&"security_init"), "should find function inside #ifdef, got: {:?}", names);

        // Definitions inside #ifndef CONFIG_PREEMPT
        assert!(names.contains(&"preempt_disable"), "should find function inside #ifndef, got: {:?}", names);

        // Definitions inside #if defined(CONFIG_SMP)
        assert!(names.contains(&"spinlock_t"), "should find typedef inside #if, got: {:?}", names);
        assert!(names.contains(&"spin_lock"), "should find function inside #if, got: {:?}", names);

        // Definitions inside nested #ifdef (inside #if)
        assert!(names.contains(&"spin_dump"), "should find function inside nested #ifdef, got: {:?}", names);

        // Definitions inside #else
        assert!(names.contains(&"no_smp_fallback"), "should find function inside #else, got: {:?}", names);
    }
}
