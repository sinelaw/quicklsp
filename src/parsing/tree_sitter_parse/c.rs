//! Tree-sitter based C parser using SCM queries + procedural helpers.
//!
//! Uses SCM queries for main definitions (functions, structs, enums,
//! typedefs, #defines) and procedural code for:
//! - Preprocessor conditional walking (#ifdef/#if/#ifndef)
//! - Local variable extraction from function bodies
//! - Complex C declarator unwinding (function pointers, nested declarators)
//! - Function parameter extraction

use tree_sitter::Node;

use crate::parsing::symbols::{Symbol, SymbolKind};
use crate::parsing::tokenizer::Visibility;

use super::common::{
    self, find_child_by_kind, make_contained_symbol, make_symbol, node_text,
    walk_preproc_conditionals, QueryParseConfig,
};
use super::{ParseResult, TsParser};

const C_IDENT_KINDS: &[&str] = &["identifier", "type_identifier", "field_identifier"];

/// SCM query for C symbol extraction.
///
/// Handles the straightforward cases declaratively. Complex cases
/// (typedefs with function pointers, preprocessor conditionals, locals)
/// are handled in post-processing.
const C_QUERY: &str = r#"
; ── Function definitions ─────────────────────────────────────────────────
(function_definition
  declarator: (function_declarator
    declarator: (identifier) @name)) @definition.function

; ── Function definitions with pointer return ─────────────────────────────
(function_definition
  declarator: (pointer_declarator
    declarator: (function_declarator
      declarator: (identifier) @name))) @definition.function

; ── Named structs with body ──────────────────────────────────────────────
(struct_specifier
  name: (type_identifier) @name
  body: (_)) @definition.struct

; ── Named unions with body ───────────────────────────────────────────────
(declaration
  type: (union_specifier
    name: (type_identifier) @name
    body: (_))) @definition.struct

; ── Named enums ──────────────────────────────────────────────────────────
(enum_specifier
  name: (type_identifier) @name) @definition.enum

; ── Enumerators ──────────────────────────────────────────────────────────
(enumerator
  name: (identifier) @name) @definition.constant

; ── Typedefs ─────────────────────────────────────────────────────────────
(type_definition
  declarator: (type_identifier) @name) @definition.type

; ── #define ──────────────────────────────────────────────────────────────
(preproc_def
  name: (identifier) @name) @definition.constant

; ── #define function-like macros ─────────────────────────────────────────
(preproc_function_def
  name: (identifier) @name) @definition.constant

; ── Struct fields ────────────────────────────────────────────────────────
(struct_specifier
  name: (type_identifier) @container
  body: (field_declaration_list
    (field_declaration
      declarator: (field_identifier) @name) @definition.field))

; ── Struct fields with pointer declarators ───────────────────────────────
(struct_specifier
  name: (type_identifier) @container
  body: (field_declaration_list
    (field_declaration
      declarator: (pointer_declarator
        declarator: (field_identifier) @name)) @definition.field))
"#;

pub struct CParser;

impl TsParser for CParser {
    fn parse(source: &str) -> ParseResult {
        let lang: tree_sitter::Language = tree_sitter_c::LANGUAGE.into();

        // Use query-based parsing for main definitions
        let mut result = common::run_query_parse(
            source,
            &QueryParseConfig {
                language: lang,
                query_source: C_QUERY,
                identifier_kinds: C_IDENT_KINDS,
                def_keyword: c_def_keyword,
                visibility: c_visibility,
                post_process: Some(c_post_process),
            },
        );

        result
    }
}

fn c_def_keyword(_kind: SymbolKind, suffix: &str) -> &'static str {
    match suffix {
        "function" => "function",
        "struct" => "struct",
        "enum" => "enum",
        "constant" => "enum",
        "type" => "typedef",
        "field" => "field",
        "variable" => "variable",
        _ => "function",
    }
}

fn c_visibility(node: Node, source: &str) -> Visibility {
    if common::has_child_with_kind_and_text(node, "storage_class_specifier", "static", source) {
        Visibility::Private
    } else if node.kind() == "function_definition" {
        Visibility::Public
    } else {
        Visibility::Unknown
    }
}

/// Post-process: handle typedef function pointers, local variables,
/// function parameters, and file-scope declarations.
fn c_post_process(root: Node, source: &str, symbols: &mut Vec<Symbol>) {
    // Handle typedef function pointers (complex declarators)
    extract_typedef_function_ptrs(root, source, symbols);
    // Handle typedef struct/union fields (anonymous structs)
    extract_typedef_inner_defs(root, source, symbols);
    // Handle file-scope variable declarations
    extract_file_scope_vars(root, source, symbols);
    // Handle function parameters and local variables
    extract_params_and_locals(root, source, symbols);
}

/// Extract typedef function pointers like `typedef void (*Handler)(int)`.
/// The query matches `(type_definition declarator: (type_identifier) @name)`
/// but function pointer typedefs have the name nested deeper.
fn extract_typedef_function_ptrs(node: Node, source: &str, symbols: &mut Vec<Symbol>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "type_definition" => {
                if let Some(decl) = child.child_by_field_name("declarator") {
                    // If the declarator is not a simple type_identifier, the query
                    // may not have matched it. Handle function pointer typedefs.
                    if decl.kind() != "type_identifier" {
                        let name_node = innermost_declarator_name(decl);
                        let name = node_text(name_node, source);
                        let line = name_node.start_position().row;
                        let col = name_node.start_position().column;
                        // Check if already extracted by query
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

/// Extract inner definitions from typedef structs/enums (anonymous).
fn extract_typedef_inner_defs(node: Node, source: &str, symbols: &mut Vec<Symbol>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "type_definition" => {
                if let Some(type_node) = child.child_by_field_name("type") {
                    match type_node.kind() {
                        "enum_specifier" => {
                            // Enumerators inside typedef enum might already be captured
                            // by the query, but let's make sure
                            if let Some(body) = type_node.child_by_field_name("body") {
                                let mut body_cursor = body.walk();
                                for bc in body.children(&mut body_cursor) {
                                    if bc.kind() == "enumerator" {
                                        if let Some(name_node) = bc.child_by_field_name("name") {
                                            let line = name_node.start_position().row;
                                            let col = name_node.start_position().column;
                                            if !symbols
                                                .iter()
                                                .any(|s| s.line == line && s.col == col)
                                            {
                                                symbols.push(make_symbol(
                                                    node_text(name_node, source).to_string(),
                                                    SymbolKind::Constant,
                                                    line,
                                                    col,
                                                    "enum",
                                                    Visibility::Unknown,
                                                ));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        "struct_specifier" | "union_specifier" => {
                            if let Some(body) = type_node.child_by_field_name("body") {
                                let typedef_name = child
                                    .child_by_field_name("declarator")
                                    .map(|d| node_text(innermost_declarator_name(d), source));
                                extract_struct_fields(body, source, symbols, typedef_name);
                            }
                        }
                        _ => {}
                    }
                }
            }
            "preproc_ifdef" | "preproc_if" | "preproc_elif" | "preproc_else" | "preproc_ifndef" => {
                extract_typedef_inner_defs(child, source, symbols);
            }
            _ => {}
        }
    }
}

/// Extract file-scope variable declarations (not function prototypes).
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

/// Extract function parameters and local variables.
fn extract_params_and_locals(node: Node, source: &str, symbols: &mut Vec<Symbol>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "function_definition" => {
                let func_name = child.child_by_field_name("declarator").and_then(|d| {
                    let n = innermost_declarator_name(d);
                    if n.kind() == "identifier" {
                        Some(node_text(n, source).to_string())
                    } else {
                        None
                    }
                });
                // Extract parameters
                if let Some(ref fname) = func_name {
                    extract_function_params(child, source, symbols, fname);
                }
                // Extract locals from body
                if let Some(body) = child.child_by_field_name("body") {
                    let body_end = body.end_position().row;
                    extract_locals_from_compound(
                        body,
                        source,
                        symbols,
                        func_name.as_deref(),
                        1,
                        body_end,
                    );
                }
            }
            "preproc_ifdef" | "preproc_if" | "preproc_elif" | "preproc_else" | "preproc_ifndef" => {
                extract_params_and_locals(child, source, symbols);
            }
            _ => {}
        }
    }
}

/// Drill into nested declarators to find the actual identifier node.
fn innermost_declarator_name(mut node: Node) -> Node {
    loop {
        match node.kind() {
            "function_declarator"
            | "array_declarator"
            | "parenthesized_declarator"
            | "pointer_declarator"
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
                            | "pointer_declarator"
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
            _ => break,
        }
    }
    node
}

/// Extract function parameters.
fn extract_function_params(
    func_node: Node,
    source: &str,
    symbols: &mut Vec<Symbol>,
    func_name: &str,
) {
    let declarator = match func_node.child_by_field_name("declarator") {
        Some(d) => d,
        None => return,
    };
    let func_decl = match find_function_declarator(declarator) {
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
                        let type_text = child
                            .child_by_field_name("type")
                            .map(|t| node_text(t, source).to_string());
                        symbols.push(make_contained_symbol(
                            name,
                            SymbolKind::Variable,
                            name_node.start_position().row,
                            name_node.start_position().column,
                            "parameter",
                            Visibility::Private,
                            Some(func_name),
                            1,
                            func_node.end_position().row.checked_sub(0),
                            type_text,
                        ));
                    }
                }
            }
        }
    }
}

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

/// Extract local variables from compound statements.
fn extract_locals_from_compound(
    node: Node,
    source: &str,
    symbols: &mut Vec<Symbol>,
    func_name: Option<&str>,
    depth: usize,
    scope_end: usize,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "declaration" => {
                extract_local_declaration(child, source, symbols, func_name, depth, scope_end);
            }
            "compound_statement" | "if_statement" | "for_statement" | "while_statement"
            | "do_statement" | "switch_statement" | "case_statement" => {
                let inner_end = child.end_position().row;
                extract_locals_from_compound(
                    child,
                    source,
                    symbols,
                    func_name,
                    depth + 1,
                    inner_end,
                );
            }
            _ => {
                if child.child_count() > 0 && has_nested_declarations(child) {
                    extract_locals_from_compound(
                        child, source, symbols, func_name, depth, scope_end,
                    );
                }
            }
        }
    }
}

fn extract_local_declaration(
    node: Node,
    source: &str,
    symbols: &mut Vec<Symbol>,
    func_name: Option<&str>,
    depth: usize,
    scope_end: usize,
) {
    let type_text = node
        .child_by_field_name("type")
        .map(|t| node_text(t, source).to_string());

    if let Some(declarator) = node.child_by_field_name("declarator") {
        if !is_function_prototype(&declarator) {
            extract_local_var(
                declarator,
                source,
                symbols,
                func_name,
                depth,
                type_text.as_deref(),
                scope_end,
            );
            return;
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "init_declarator" {
            if let Some(decl) = child.child_by_field_name("declarator") {
                if !is_function_prototype(&decl) {
                    extract_local_var(
                        decl,
                        source,
                        symbols,
                        func_name,
                        depth,
                        type_text.as_deref(),
                        scope_end,
                    );
                }
            }
        }
    }
}

fn extract_local_var(
    declarator: Node,
    source: &str,
    symbols: &mut Vec<Symbol>,
    func_name: Option<&str>,
    depth: usize,
    type_text: Option<&str>,
    scope_end: usize,
) {
    let name_node = innermost_declarator_name(declarator);
    if name_node.kind() == "identifier" || name_node.kind() == "type_identifier" {
        let name = node_text(name_node, source).to_string();
        symbols.push(make_contained_symbol(
            name,
            SymbolKind::Variable,
            name_node.start_position().row,
            name_node.start_position().column,
            "variable",
            Visibility::Private,
            func_name,
            depth,
            Some(scope_end),
            type_text.map(|s| s.to_string()),
        ));
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
                    let line = name_node.start_position().row;
                    let col = name_node.start_position().column;
                    if !symbols.iter().any(|s| s.line == line && s.col == col) {
                        let type_text = child
                            .child_by_field_name("type")
                            .map(|t| node_text(t, source).to_string());
                        symbols.push(make_contained_symbol(
                            name,
                            SymbolKind::Variable,
                            line,
                            col,
                            "field",
                            Visibility::Unknown,
                            struct_name,
                            1,
                            None,
                            type_text,
                        ));
                    }
                }
            }
        }
    }
}

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
        assert!(
            names.contains(&"MAX"),
            "should find #define MAX, got: {:?}",
            names
        );
        assert!(
            names.contains(&"Foo"),
            "should find struct Foo, got: {:?}",
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
            names.contains(&"GREEN"),
            "should find enumerator GREEN, got: {:?}",
            names
        );
        assert!(
            names.contains(&"BLUE"),
            "should find enumerator BLUE, got: {:?}",
            names
        );
        assert!(
            names.contains(&"uint32"),
            "should find typedef uint32, got: {:?}",
            names
        );
        assert!(
            names.contains(&"hello"),
            "should find function hello, got: {:?}",
            names
        );
        assert!(
            names.contains(&"helper"),
            "should find static function helper, got: {:?}",
            names
        );
        assert!(
            names.contains(&"main"),
            "should find function main, got: {:?}",
            names
        );

        // Local variable SHOULD be indexed (with depth > 0)
        assert!(
            names.contains(&"local"),
            "local variables should be indexed, got: {:?}",
            names
        );
        let local_sym = result.symbols.iter().find(|s| s.name == "local").unwrap();
        assert!(local_sym.depth > 0, "local variable should have depth > 0");
        assert_eq!(
            local_sym.container.as_deref(),
            Some("hello"),
            "local should have container = function name"
        );

        // Check visibility
        let helper_sym = result.symbols.iter().find(|s| s.name == "helper").unwrap();
        assert_eq!(helper_sym.visibility, Visibility::Private);

        let hello_sym = result.symbols.iter().find(|s| s.name == "hello").unwrap();
        assert_eq!(hello_sym.visibility, Visibility::Public);

        // Check occurrences exist
        assert!(!result.occurrences.is_empty(), "should have occurrences");
    }

    #[test]
    fn test_c_parser_typedef_enum_and_fnptr() {
        let source = r#"
typedef enum {
    LOG_DEBUG,
    LOG_INFO,
    LOG_ERROR
} LogLevel;

typedef void (*RequestHandler)(int a, int b);
typedef int (*Validator)(const char *s);

typedef struct {
    int fd;
    int state;
} Connection;
"#;
        let result = CParser::parse(source);
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();

        // typedef enum members
        assert!(
            names.contains(&"LOG_DEBUG"),
            "should find LOG_DEBUG, got: {:?}",
            names
        );
        assert!(
            names.contains(&"LOG_INFO"),
            "should find LOG_INFO, got: {:?}",
            names
        );
        assert!(
            names.contains(&"LOG_ERROR"),
            "should find LOG_ERROR, got: {:?}",
            names
        );
        assert!(
            names.contains(&"LogLevel"),
            "should find typedef LogLevel, got: {:?}",
            names
        );

        // Function pointer typedefs
        assert!(
            names.contains(&"RequestHandler"),
            "should find RequestHandler, got: {:?}",
            names
        );
        assert!(
            names.contains(&"Validator"),
            "should find Validator, got: {:?}",
            names
        );

        // Typedef struct
        assert!(
            names.contains(&"Connection"),
            "should find Connection, got: {:?}",
            names
        );

        // Struct fields inside typedef struct
        assert!(
            names.contains(&"fd"),
            "should find field fd, got: {:?}",
            names
        );
        assert!(
            names.contains(&"state"),
            "should find field state, got: {:?}",
            names
        );
    }

    #[test]
    fn test_c_parser_ifdef_nested_definitions() {
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

        assert!(
            names.contains(&"CIA_MAX_OPS"),
            "should find top-level #define, got: {:?}",
            names
        );
        assert!(
            names.contains(&"cia_type"),
            "should find top-level enum, got: {:?}",
            names
        );
        assert!(
            names.contains(&"CIA_READ"),
            "should find enumerator, got: {:?}",
            names
        );
        assert!(
            names.contains(&"top_level_func"),
            "should find top-level function, got: {:?}",
            names
        );

        assert!(
            names.contains(&"security_ops"),
            "should find struct inside #ifdef, got: {:?}",
            names
        );
        assert!(
            names.contains(&"security_init"),
            "should find function inside #ifdef, got: {:?}",
            names
        );

        assert!(
            names.contains(&"preempt_disable"),
            "should find function inside #ifndef, got: {:?}",
            names
        );

        assert!(
            names.contains(&"spinlock_t"),
            "should find typedef inside #if, got: {:?}",
            names
        );
        assert!(
            names.contains(&"spin_lock"),
            "should find function inside #if, got: {:?}",
            names
        );

        assert!(
            names.contains(&"spin_dump"),
            "should find function inside nested #ifdef, got: {:?}",
            names
        );

        assert!(
            names.contains(&"no_smp_fallback"),
            "should find function inside #else, got: {:?}",
            names
        );
    }
}
