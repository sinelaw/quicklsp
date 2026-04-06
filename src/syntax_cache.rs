//! Cached tree-sitter parse trees for editor-open files.
//!
//! Stores parse trees for open files so that LSP queries (go-to-definition,
//! hover, etc.) can inspect the AST node at the cursor position without
//! re-parsing. Trees are updated on `didOpen`/`didChange` and removed on
//! `didClose`.
//!
//! The API is language-agnostic: callers receive [`NodeInfo`] values and
//! never touch tree-sitter types directly. When tree-sitter grammars are
//! added for more languages, they plug into `parse_for_lang()` — the rest
//! of the codebase stays unchanged.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use dashmap::DashMap;
use tree_sitter::{Parser, Tree};

use crate::parsing::tokenizer::LangFamily;

/// Cached parse trees for editor-open files.
///
/// Tree-sitter `Tree` is `Send` but not `Sync`, so each tree is wrapped in
/// a `Mutex`. Contention is minimal: the lock is held only for the brief
/// tree walk during a query, and `didChange` replaces the tree atomically.
pub struct SyntaxCache {
    trees: DashMap<PathBuf, Mutex<Tree>>,
}

/// Information about an AST node at a specific position.
///
/// Language-agnostic: callers use this without depending on tree-sitter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeInfo {
    /// The tree-sitter node kind string (e.g., "identifier", "type_identifier",
    /// "field_identifier", "call_expression").
    pub kind: String,
    /// The node's text in the source.
    pub text: String,
    /// Parent node kind, if any.
    pub parent_kind: Option<String>,
    /// The field name this node occupies in its parent (e.g., "declarator",
    /// "type", "field", "function", "arguments").
    pub parent_field: Option<String>,
    /// Start line (0-indexed).
    pub line: usize,
    /// Start column (0-indexed, byte offset).
    pub col: usize,
}

/// Classified identifier context, derived from tree-sitter node info.
///
/// Used by the LSP server to choose the right lookup strategy for
/// go-to-definition and hover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentContext {
    /// Struct/union field access: `ctx->field` or `obj.field`.
    FieldAccess,
    /// Type name in a type position: `struct Foo`, parameter type, etc.
    TypeRef,
    /// Function being called: `foo(...)`.
    FunctionCall,
    /// Plain identifier (variable, parameter, etc.).
    Plain,
}

impl SyntaxCache {
    pub fn new() -> Self {
        Self {
            trees: DashMap::new(),
        }
    }

    /// Parse and cache a file's syntax tree.
    ///
    /// Called on `didOpen` and `didChange`. If the language has no tree-sitter
    /// grammar, this is a no-op and queries will return `None`.
    pub fn update(&self, path: &Path, source: &str, lang: Option<LangFamily>) {
        let tree = match Self::parse_for_lang(source, lang, path) {
            Some(t) => t,
            None => {
                // No grammar for this language — remove stale tree if any
                self.trees.remove(path);
                return;
            }
        };
        self.trees.insert(path.to_path_buf(), Mutex::new(tree));
    }

    /// Remove a file's cached tree (on `didClose`).
    pub fn remove(&self, path: &Path) {
        self.trees.remove(path);
    }

    /// Query the AST node at a specific position in a cached file.
    ///
    /// Returns `None` if the file has no cached tree or the position is
    /// outside any named node.
    pub fn node_at(&self, path: &Path, line: usize, col: usize, source: &str) -> Option<NodeInfo> {
        let entry = self.trees.get(path)?;
        let tree = entry.lock().ok()?;
        let point = tree_sitter::Point::new(line, col);
        let root = tree.root_node();
        let node = root.descendant_for_point_range(point, point)?;

        // Walk up to the closest named node (skip anonymous syntax like "(" ")")
        let named = if node.is_named() {
            node
        } else {
            let mut n = node;
            while let Some(p) = n.parent() {
                if p.is_named() {
                    n = p;
                    break;
                }
                n = p;
            }
            n
        };

        // Find parent info
        let parent = named.parent();
        let parent_kind = parent.map(|p| p.kind().to_string());
        let parent_field = parent.and_then(|p| {
            // Find which field name this node occupies in its parent
            let mut cursor = p.walk();
            for (i, child) in p.children(&mut cursor).enumerate() {
                if child.id() == named.id() {
                    return p.field_name_for_child(i as u32)
                        .map(|s| s.to_string());
                }
            }
            None
        });

        Some(NodeInfo {
            kind: named.kind().to_string(),
            text: source[named.start_byte()..named.end_byte()].to_string(),
            parent_kind,
            parent_field,
            line: named.start_position().row,
            col: named.start_position().column,
        })
    }

    /// Classify the identifier context at a position.
    ///
    /// This is the primary API for the LSP server: it returns a high-level
    /// classification that determines the lookup strategy.
    pub fn ident_context_at(
        &self,
        path: &Path,
        line: usize,
        col: usize,
        source: &str,
    ) -> IdentContext {
        let info = match self.node_at(path, line, col, source) {
            Some(i) => i,
            None => return IdentContext::Plain,
        };

        Self::classify_node(&info)
    }

    /// Classify a node into an identifier context.
    ///
    /// Separated from `ident_context_at` for testability.
    pub fn classify_node(info: &NodeInfo) -> IdentContext {
        // tree-sitter-c uses distinct node kinds for different identifier roles:
        // - "field_identifier"  → struct member access
        // - "type_identifier"   → type names in type positions
        // - "identifier"        → everything else
        //
        // Other tree-sitter grammars use similar conventions.
        match info.kind.as_str() {
            "field_identifier" => IdentContext::FieldAccess,
            "type_identifier" => IdentContext::TypeRef,
            "identifier" => {
                // Check parent context for more specific classification
                match info.parent_kind.as_deref() {
                    Some("call_expression") if info.parent_field.as_deref() == Some("function") => {
                        IdentContext::FunctionCall
                    }
                    _ => IdentContext::Plain,
                }
            }
            _ => IdentContext::Plain,
        }
    }

    /// Parse source with the appropriate tree-sitter grammar.
    ///
    /// Tries extension-based lookup first (more precise: distinguishes C vs C++,
    /// TS vs JS), then falls back to language family.
    fn parse_for_lang(source: &str, lang: Option<LangFamily>, path: &Path) -> Option<Tree> {
        let language = path
            .extension()
            .and_then(|e| e.to_str())
            .and_then(crate::parsing::tree_sitter_parse::language_for_extension)
            .or_else(|| {
                lang.and_then(crate::parsing::tree_sitter_parse::language_for_family)
            })?;
        let mut parser = Parser::new();
        parser.set_language(&language).ok()?;
        parser.parse(source, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_c_field_access() {
        let cache = SyntaxCache::new();
        let source = "void f(struct Ctx *ctx) { ctx->op_count; }";
        let path = PathBuf::from("/test.c");
        cache.update(&path, source, Some(LangFamily::CLike));

        // "op_count" in "ctx->op_count" — should be FieldAccess
        // ctx->op_count: c=0,t=1,x=2,-=3,>=4,o=5
        // Find the column of "op_count"
        let col = source.find("op_count").unwrap();
        let ctx = cache.ident_context_at(&path, 0, col, source);
        assert_eq!(ctx, IdentContext::FieldAccess, "ctx->op_count should be FieldAccess");
    }

    #[test]
    fn test_c_type_ref() {
        let cache = SyntaxCache::new();
        let source = "struct Foo { int x; };\nvoid f(struct Foo *p) {}";
        let path = PathBuf::from("/test.c");
        cache.update(&path, source, Some(LangFamily::CLike));

        // "Foo" in "struct Foo *p" on line 1
        let line1 = source.lines().nth(1).unwrap();
        let col = line1.find("Foo").unwrap();
        let ctx = cache.ident_context_at(&path, 1, col, source);
        assert_eq!(ctx, IdentContext::TypeRef, "struct Foo in parameter should be TypeRef");
    }

    #[test]
    fn test_c_function_call() {
        let cache = SyntaxCache::new();
        let source = "void foo(void) {}\nvoid bar(void) { foo(); }";
        let path = PathBuf::from("/test.c");
        cache.update(&path, source, Some(LangFamily::CLike));

        // "foo" in "foo()" on line 1
        let line1 = source.lines().nth(1).unwrap();
        let col = line1.find("foo").unwrap();
        let ctx = cache.ident_context_at(&path, 1, col, source);
        assert_eq!(ctx, IdentContext::FunctionCall, "foo() should be FunctionCall");
    }

    #[test]
    fn test_c_plain_identifier() {
        let cache = SyntaxCache::new();
        let source = "void f(int x) { x; }";
        let path = PathBuf::from("/test.c");
        cache.update(&path, source, Some(LangFamily::CLike));

        // "x" in the expression statement — plain identifier
        let col = source.rfind("x").unwrap();
        let ctx = cache.ident_context_at(&path, 0, col, source);
        assert_eq!(ctx, IdentContext::Plain, "bare x should be Plain");
    }

    #[test]
    fn test_c_dot_field_access() {
        let cache = SyntaxCache::new();
        let source = "struct S { int val; };\nvoid f(struct S s) { s.val; }";
        let path = PathBuf::from("/test.c");
        cache.update(&path, source, Some(LangFamily::CLike));

        let line1 = source.lines().nth(1).unwrap();
        let col = line1.find("val").unwrap();
        let ctx = cache.ident_context_at(&path, 1, col, source);
        assert_eq!(ctx, IdentContext::FieldAccess, "s.val should be FieldAccess");
    }

    #[test]
    fn test_no_tree_returns_plain() {
        let cache = SyntaxCache::new();
        let path = PathBuf::from("/test.py");
        // Python has no tree-sitter grammar registered
        cache.update(&path, "def foo(): pass", Some(LangFamily::Python));

        let ctx = cache.ident_context_at(&path, 0, 4, "def foo(): pass");
        assert_eq!(ctx, IdentContext::Plain, "No tree → Plain");
    }

    #[test]
    fn test_node_at_position() {
        let cache = SyntaxCache::new();
        let source = "struct Ctx { int count; };\nvoid f(struct Ctx *c) { c->count; }";
        let path = PathBuf::from("/test.c");
        cache.update(&path, source, Some(LangFamily::CLike));

        // "count" in "c->count" on line 1
        let line1 = source.lines().nth(1).unwrap();
        let col = line1.rfind("count").unwrap();
        let info = cache.node_at(&path, 1, col, source);
        assert!(info.is_some());
        let info = info.unwrap();
        assert_eq!(info.kind, "field_identifier");
        assert_eq!(info.text, "count");
        assert_eq!(info.parent_kind.as_deref(), Some("field_expression"));
    }

    #[test]
    fn test_update_replaces_tree() {
        let cache = SyntaxCache::new();
        let path = PathBuf::from("/test.c");

        cache.update(&path, "int x;", Some(LangFamily::CLike));
        let info1 = cache.node_at(&path, 0, 4, "int x;");
        assert!(info1.is_some());
        assert_eq!(info1.unwrap().text, "x");

        // Update with new source
        cache.update(&path, "int y;", Some(LangFamily::CLike));
        let info2 = cache.node_at(&path, 0, 4, "int y;");
        assert!(info2.is_some());
        assert_eq!(info2.unwrap().text, "y");
    }

    #[test]
    fn test_remove_clears_tree() {
        let cache = SyntaxCache::new();
        let path = PathBuf::from("/test.c");
        cache.update(&path, "int x;", Some(LangFamily::CLike));
        assert!(cache.node_at(&path, 0, 4, "int x;").is_some());

        cache.remove(&path);
        assert!(cache.node_at(&path, 0, 4, "int x;").is_none());
    }

    #[test]
    fn test_classify_node_field_identifier() {
        let info = NodeInfo {
            kind: "field_identifier".to_string(),
            text: "count".to_string(),
            parent_kind: Some("field_expression".to_string()),
            parent_field: Some("field".to_string()),
            line: 0,
            col: 0,
        };
        assert_eq!(SyntaxCache::classify_node(&info), IdentContext::FieldAccess);
    }

    #[test]
    fn test_classify_node_type_identifier() {
        let info = NodeInfo {
            kind: "type_identifier".to_string(),
            text: "Foo".to_string(),
            parent_kind: Some("struct_specifier".to_string()),
            parent_field: Some("name".to_string()),
            line: 0,
            col: 0,
        };
        assert_eq!(SyntaxCache::classify_node(&info), IdentContext::TypeRef);
    }

    #[test]
    fn test_classify_node_function_call() {
        let info = NodeInfo {
            kind: "identifier".to_string(),
            text: "printf".to_string(),
            parent_kind: Some("call_expression".to_string()),
            parent_field: Some("function".to_string()),
            line: 0,
            col: 0,
        };
        assert_eq!(SyntaxCache::classify_node(&info), IdentContext::FunctionCall);
    }
}
