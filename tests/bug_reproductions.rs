//! Bug Reproduction Tests for QuickLSP
//!
//! These tests reproduce the bugs discovered during QA testing against
//! real-world repositories (flask, express, tokio, gin, redis).
//!
//! Run with:
//!   cargo test -p quicklsp --test bug_reproductions

use std::path::{Path, PathBuf};

use quicklsp::parsing::tokenizer::{self, LangFamily};
use quicklsp::workspace::Workspace;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

fn setup_workspace() -> Workspace {
    let ws = Workspace::new();
    let dir = fixtures_dir();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            let source = std::fs::read_to_string(&path).unwrap();
            ws.index_file(path, source);
        }
    }
    ws
}

// =========================================================================
// Bug #1: C/C++ function definitions not indexed
//
// Root cause: LangFamily::CLike.def_keywords() only contains
//   ["struct", "enum", "class", "union", "typedef", "namespace"]
// and is missing return-type keywords (void, int, char, etc.), so
// C function definitions are never added to the symbol table.
//
// Confirmed against redis/redis: textDocument/definition returns null
// for serverLogRaw (call at server.c:185, definition at server.c:129).
// =========================================================================

/// C struct definitions should be found (this already works).
#[test]
fn bug1_c_struct_definitions_are_indexed() {
    let ws = Workspace::new();
    ws.index_file(
        PathBuf::from("/src/server.c"),
        r#"
struct Config {
    char host[256];
    int port;
};

struct Server {
    struct Config config;
    int running;
};

enum Status {
    STATUS_ACTIVE,
    STATUS_INACTIVE,
};
"#
        .to_string(),
    );

    let config_defs = ws.find_definitions("Config");
    assert!(
        !config_defs.is_empty(),
        "struct Config should be found as a definition"
    );

    let server_defs = ws.find_definitions("Server");
    assert!(
        !server_defs.is_empty(),
        "struct Server should be found as a definition"
    );

    let status_defs = ws.find_definitions("Status");
    assert!(
        !status_defs.is_empty(),
        "enum Status should be found as a definition"
    );
}

/// C function definitions (void func, int func, etc.) should be indexed.
/// BUG: They are not, because void/int/etc. are not in def_keywords for CLike.
#[test]
#[ignore = "Bug #1: C functions not indexed — void/int/etc not in CLike def_keywords"]
fn bug1_c_function_definitions_not_indexed() {
    let ws = Workspace::new();
    ws.index_file(
        PathBuf::from("/src/server.c"),
        r#"
void server_log(int level, const char *msg) {
    printf("[%d] %s\n", level, msg);
}

int validate_port(int port) {
    return port > 0 && port < 65535;
}

static void server_run(struct Server *server) {
    server->running = 1;
}

char *sanitize_input(const char *input) {
    return NULL;
}
"#
        .to_string(),
    );

    // These all fail because void/int/static/char are not def_keywords
    let server_log = ws.find_definitions("server_log");
    assert!(
        !server_log.is_empty(),
        "BUG: void server_log() not indexed — 'void' is not a C def_keyword"
    );

    let validate_port = ws.find_definitions("validate_port");
    assert!(
        !validate_port.is_empty(),
        "BUG: int validate_port() not indexed — 'int' is not a C def_keyword"
    );

    let server_run = ws.find_definitions("server_run");
    assert!(
        !server_run.is_empty(),
        "BUG: static void server_run() not indexed — 'static' and 'void' are not C def_keywords"
    );

    let sanitize = ws.find_definitions("sanitize_input");
    assert!(
        !sanitize.is_empty(),
        "BUG: char *sanitize_input() not indexed — 'char' is not a C def_keyword"
    );
}

/// Go-to-definition from a call site should jump to the function definition
/// in the same file. This is the exact scenario from the redis/redis test.
#[test]
#[ignore = "Bug #1: C functions not indexed — void/int/etc not in CLike def_keywords"]
fn bug1_c_goto_definition_from_call_site() {
    let ws = Workspace::new();
    let source = r#"
void serverLogRaw(int level, const char *msg) {
    printf("[%d] %s\n", level, msg);
}

void _serverLog(int level, const char *fmt) {
    char msg[1024];
    serverLogRaw(level, msg);
}
"#;
    ws.index_file(PathBuf::from("/src/server.c"), source.to_string());

    // Simulates: cursor on "serverLogRaw" at the call site in _serverLog
    let defs = ws.find_definitions("serverLogRaw");
    assert!(
        !defs.is_empty(),
        "BUG: Go-to-definition for serverLogRaw returns empty — \
         C functions are not in the definitions index"
    );

    // If definitions are found, the first one should be the actual definition
    if !defs.is_empty() {
        assert_eq!(
            defs[0].symbol.line, 1,
            "serverLogRaw should be defined on line 1"
        );
    }
}

/// The tokenizer should recognize C function definitions by extracting the
/// function name from patterns like "void funcName(" or "int funcName(".
///
/// The tokenizer marks definition-introducing keywords as DefKeyword and the
/// following identifier as Ident. For C functions, the return type (void, int)
/// should be a DefKeyword so the function name gets indexed.
#[test]
#[ignore = "Bug #1: C functions not indexed — void/int/etc not in CLike def_keywords"]
fn bug1_c_tokenizer_misses_function_names() {
    let src = r#"
void init_config(struct Config *config) {
    config->port = 8080;
}

int validate_port(int port) {
    return port > 0;
}

static void server_run(struct Server *server) {
    server->running = 1;
}
"#;
    let (scan_result, _) = tokenizer::scan_with_contexts(src, LangFamily::CLike);

    // Extract (DefKeyword, following Ident) pairs to see what definitions
    // the tokenizer recognizes
    let tokens = &scan_result.tokens;
    let mut def_names: Vec<&str> = Vec::new();
    for i in 0..tokens.len().saturating_sub(1) {
        if tokens[i].kind == tokenizer::TokenKind::DefKeyword
            && tokens[i + 1].kind == tokenizer::TokenKind::Ident
        {
            def_names.push(tokens[i + 1].text.as_str());
        }
    }

    assert!(
        def_names.contains(&"init_config"),
        "BUG: 'init_config' not found as a definition. \
         'void' is not a DefKeyword for CLike. Found definitions: {:?}",
        def_names
    );
    assert!(
        def_names.contains(&"validate_port"),
        "BUG: 'validate_port' not found as a definition. \
         'int' is not a DefKeyword for CLike. Found definitions: {:?}",
        def_names
    );
    assert!(
        def_names.contains(&"server_run"),
        "BUG: 'server_run' not found as a definition. \
         'static void' not recognized for CLike. Found definitions: {:?}",
        def_names
    );
}

/// Hover for C functions should return the function signature.
#[test]
#[ignore = "Bug #1: C functions not indexed — void/int/etc not in CLike def_keywords"]
fn bug1_c_hover_missing_for_functions() {
    let ws = Workspace::new();
    ws.index_file(
        PathBuf::from("/src/server.c"),
        r#"
/// Log a message at the given level.
void server_log(int level, const char *msg) {
    printf("[%d] %s\n", level, msg);
}
"#
        .to_string(),
    );

    let hover = ws.hover_info("server_log");
    assert!(
        hover.is_some(),
        "BUG: Hover for 'server_log' returns None — function not in definitions"
    );
}

/// C completion should include function names, not just struct/enum names.
#[test]
#[ignore = "Bug #1: C functions not indexed — void/int/etc not in CLike def_keywords"]
fn bug1_c_completion_missing_functions() {
    let ws = Workspace::new();
    ws.index_file(
        PathBuf::from("/src/server.c"),
        r#"
struct Server { int running; };
void server_init(struct Server *s) {}
void server_run(struct Server *s) {}
void server_stop(struct Server *s) {}
void server_log(int level, const char *msg) {}
"#
        .to_string(),
    );

    let results = ws.completions("server_");
    let names: Vec<&str> = results.iter().map(|r| r.symbol.name.as_str()).collect();

    // struct Server should be found (this works)
    assert!(
        ws.find_definitions("Server").len() > 0,
        "struct Server should be in definitions"
    );

    // function names should also be found (this is the bug)
    assert!(
        names.contains(&"server_init"),
        "BUG: Completion for 'server_' should include 'server_init'. Got: {:?}",
        names
    );
    assert!(
        names.contains(&"server_run"),
        "BUG: Completion for 'server_' should include 'server_run'. Got: {:?}",
        names
    );
}

/// Using the full sample_c.c fixture, verify the end-to-end workflow.
#[test]
#[ignore = "Bug #1: C functions not indexed — void/int/etc not in CLike def_keywords"]
fn bug1_c_fixture_end_to_end() {
    let ws = Workspace::new();
    let fixture = fixtures_dir().join("sample_c.c");
    let source = std::fs::read_to_string(&fixture).unwrap();
    ws.index_file(fixture, source);

    // Structs/enums should work (existing behavior)
    assert!(
        !ws.find_definitions("Config").is_empty(),
        "struct Config should be found"
    );
    assert!(
        !ws.find_definitions("Status").is_empty(),
        "enum Status should be found"
    );
    assert!(
        !ws.find_definitions("Server").is_empty(),
        "struct Server should be found"
    );

    // Functions should also work (the bug)
    let functions = [
        "init_config",
        "validate_port",
        "process_request",
        "server_init",
        "server_run",
        "server_stop",
        "server_log",
        "sanitize_input",
        "main",
    ];
    for func_name in &functions {
        let defs = ws.find_definitions(func_name);
        assert!(
            !defs.is_empty(),
            "BUG: C function '{}' not found in definitions",
            func_name
        );
    }
}

// =========================================================================
// Bug #2: Progress notification reports "0 packages, 0 definitions"
//
// Root cause: src/lsp/server.rs:406-410 calls dep_index.package_count()
// and dep_index.definition_count(), which only count EXTERNAL dependency
// packages — not the workspace definitions that power all LSP features.
//
// This is a cosmetic/misleading bug, but we can verify that the workspace
// definition count is non-zero after indexing, proving the progress message
// undercounts.
// =========================================================================

/// After indexing fixtures, the workspace should report non-zero definition
/// and symbol counts, proving that the "0 definitions" progress message
/// is wrong.
#[test]
fn bug2_workspace_has_definitions_but_dep_index_reports_zero() {
    let ws = setup_workspace();

    let def_count = ws.definition_count();
    let sym_count = ws.unique_symbol_count();

    assert!(
        def_count > 0,
        "Workspace has {} definitions, but progress reports '0 definitions'",
        def_count,
    );
    assert!(
        sym_count > 0,
        "Workspace has {} unique symbols, but progress reports '0 definitions'",
        sym_count,
    );

    // The dep_index reports 0 because we haven't indexed any external packages
    let dep_index = quicklsp::deps::DependencyIndex::new();
    assert_eq!(
        dep_index.package_count(),
        0,
        "Dep index package count is 0 (no external packages indexed)"
    );
    assert_eq!(
        dep_index.definition_count(),
        0,
        "Dep index definition count is 0 (no external packages indexed)"
    );

    // This proves the progress message format string should include
    // workspace definition counts, not just dep_index counts.
    println!(
        "BUG: Progress says 'Indexed {} packages, {} definitions' \
         but workspace actually has {} definitions and {} symbols",
        dep_index.package_count(),
        dep_index.definition_count(),
        def_count,
        sym_count,
    );
}

/// Index a real-ish directory and verify that workspace counts are accurate.
#[test]
fn bug2_scan_directory_populates_definitions_not_counted_in_progress() {
    let ws = Workspace::new();
    let fixture_dir = fixtures_dir();
    ws.scan_directory(&fixture_dir);

    let def_count = ws.definition_count();
    let file_count = ws.file_count();

    assert!(
        file_count >= 3,
        "Should have indexed at least 3 fixture files, got {}",
        file_count
    );
    assert!(
        def_count > 20,
        "Should have >20 definitions from fixtures, got {}. \
         Progress message would incorrectly report 0.",
        def_count
    );
}

// =========================================================================
// Bug #3: Completion uses Levenshtein distance, not prefix matching
//
// Root cause: completions() delegates to search_symbols() -> fuzzy.resolve()
// which uses bounded Levenshtein distance (MAX_EDIT_DISTANCE=2). The
// length-difference pre-filter rejects any symbol whose length differs
// by more than 2 from the query.
//
// This means typing "Hand" (4 chars) cannot match "HandlerFunc" (11 chars)
// because abs(4 - 11) = 7 > 2. Completion only works when the user types
// nearly the full name (within ±2 characters).
// =========================================================================

/// Short prefixes should match longer symbol names.
/// BUG: They don't because Levenshtein rejects length differences > 2.
#[test]
fn bug3_completion_short_prefix_should_match_long_name() {
    let ws = Workspace::new();
    ws.index_file(
        PathBuf::from("/src/main.rs"),
        r#"
fn process_request() {}
fn process_data() {}
fn process_input() {}
struct ProcessManager {}
"#
        .to_string(),
    );

    // "proc" (4 chars) should match "process_request" (15 chars),
    // "process_data" (12 chars), "process_input" (13 chars), "ProcessManager" (14 chars)
    let results = ws.completions("proc");
    let names: Vec<&str> = results.iter().map(|r| r.symbol.name.as_str()).collect();

    assert!(
        !names.is_empty(),
        "BUG: 'proc' matches no completions. Levenshtein rejects len diff > 2. Got: {:?}",
        names
    );
    assert!(
        names.iter().any(|n| n.starts_with("process")),
        "BUG: 'proc' should match symbols starting with 'process'. Got: {:?}",
        names
    );
}

/// Typing a 4-char prefix for an 11-char symbol should produce completions.
/// This is the exact scenario from the evaluate_completions test:
///   'Hand' -> []  (expected: Handler, HandlerResult)
#[test]
fn bug3_completion_hand_should_match_handler() {
    let ws = setup_workspace();

    let results = ws.completions("Hand");
    let names: Vec<&str> = results.iter().map(|r| r.symbol.name.as_str()).collect();

    assert!(
        names.iter().any(|n| n.starts_with("Handler")),
        "BUG: 'Hand' should match 'Handler' or 'HandlerResult' but got: {:?}\n\
         Root cause: abs(len('Hand')=4, len('Handler')=7) = 3 > MAX_EDIT_DISTANCE=2",
        names
    );
}

/// Typing 4-char prefix "crea" should match "create_config" etc.
/// This is another case from evaluate_completions: 'crea' -> []
#[test]
fn bug3_completion_crea_should_match_create() {
    let ws = setup_workspace();

    let results = ws.completions("crea");
    let names: Vec<&str> = results.iter().map(|r| r.symbol.name.as_str()).collect();

    assert!(
        names.iter().any(|n| n.starts_with("create")),
        "BUG: 'crea' should match 'create_config' or 'createConfig' but got: {:?}\n\
         Root cause: abs(len('crea')=4, len('create_config')=13) = 9 > MAX_EDIT_DISTANCE=2",
        names
    );
}

/// Typing "MAX" should match "MAX_RETRIES" (length diff = 8).
#[test]
fn bug3_completion_max_should_match_max_retries() {
    let ws = setup_workspace();

    let results = ws.completions("MAX");
    let names: Vec<&str> = results.iter().map(|r| r.symbol.name.as_str()).collect();

    assert!(
        names.iter().any(|n| n.starts_with("MAX")),
        "BUG: 'MAX' should match 'MAX_RETRIES' but got: {:?}\n\
         Root cause: abs(len('MAX')=3, len('MAX_RETRIES')=11) = 8 > MAX_EDIT_DISTANCE=2",
        names
    );
}

/// Completions should work as prefix matching across all languages.
/// Tests realistic developer typing patterns.
#[test]
fn bug3_completion_realistic_typing_patterns() {
    let ws = Workspace::new();

    // Rust
    ws.index_file(
        PathBuf::from("/src/main.rs"),
        "fn handle_connection() {}\nstruct HttpServer {}\n".to_string(),
    );
    // Python
    ws.index_file(
        PathBuf::from("/src/app.py"),
        "def handle_request():\n    pass\nclass HttpClient:\n    pass\n".to_string(),
    );
    // Go
    ws.index_file(
        PathBuf::from("/src/main.go"),
        "func HandleMessage() {}\ntype HttpHandler struct {}\n".to_string(),
    );
    // TypeScript
    ws.index_file(
        PathBuf::from("/src/app.ts"),
        "function handleEvent() {}\nclass HttpService {}\n".to_string(),
    );

    // A user typing "hand" expects to see all handle* functions
    let results = ws.completions("hand");
    let names: Vec<&str> = results.iter().map(|r| r.symbol.name.as_str()).collect();
    assert!(
        names.len() >= 2,
        "BUG: Typing 'hand' should match handle_connection, handle_request, \
         HandleMessage, handleEvent, etc. Got: {:?}",
        names
    );

    // A user typing "Http" expects to see all Http* types
    let results = ws.completions("Http");
    let names: Vec<&str> = results.iter().map(|r| r.symbol.name.as_str()).collect();
    assert!(
        names.len() >= 2,
        "BUG: Typing 'Http' should match HttpServer, HttpClient, \
         HttpHandler, HttpService, etc. Got: {:?}",
        names
    );
}

/// Completions with nearly-complete names should still work (this is the
/// happy path that already passes, for comparison with the failing cases).
#[test]
fn bug3_completion_near_complete_names_already_work() {
    let ws = setup_workspace();

    // "Confi" -> "Config" (diff=1, within MAX_EDIT_DISTANCE=2) — passes
    let results = ws.completions("Confi");
    assert!(
        results.iter().any(|r| r.symbol.name == "Config"),
        "Near-complete 'Confi' should match 'Config' (this works, as baseline)"
    );

    // "Serve" -> "Server" (diff=1, within MAX_EDIT_DISTANCE=2) — passes
    let results = ws.completions("Serve");
    assert!(
        results.iter().any(|r| r.symbol.name == "Server"),
        "Near-complete 'Serve' should match 'Server' (this works, as baseline)"
    );

    // "Statu" -> "Status" (diff=1, within MAX_EDIT_DISTANCE=2) — passes
    let results = ws.completions("Statu");
    assert!(
        results.iter().any(|r| r.symbol.name == "Status"),
        "Near-complete 'Statu' should match 'Status' (this works, as baseline)"
    );
}

// =========================================================================
// Bug #1 + #3 combined: C fixture end-to-end with Go fixture
//
// Using the new fixture files to exercise the full workspace flow.
// =========================================================================

/// Go fixture should have all definitions indexed (func, type, var, const).
#[test]
fn go_fixture_definitions_all_indexed() {
    let ws = Workspace::new();
    let fixture = fixtures_dir().join("sample_go.go");
    let source = std::fs::read_to_string(&fixture).unwrap();
    ws.index_file(fixture, source);

    // Types
    for name in &["Config", "Status", "HandlerFunc", "Request", "Response", "Server"] {
        assert!(
            !ws.find_definitions(name).is_empty(),
            "Go type '{}' should be in definitions",
            name
        );
    }

    // Functions
    for name in &[
        "NewConfig",
        "ValidatePort",
        "ProcessRequest",
        "NewServer",
        "AddHandler",
        "Run",
        "SanitizeInput",
        "main",
    ] {
        assert!(
            !ws.find_definitions(name).is_empty(),
            "Go function '{}' should be in definitions",
            name
        );
    }

    // Constants/variables
    for name in &["MaxRetries", "DefaultTimeout", "globalCounter"] {
        assert!(
            !ws.find_definitions(name).is_empty(),
            "Go const/var '{}' should be in definitions",
            name
        );
    }
}

/// Cross-language definition test using all 5 fixture files.
#[test]
fn cross_language_shared_concepts() {
    let ws = setup_workspace();

    // "Config" should be defined in multiple languages
    let config_defs = ws.find_definitions("Config");
    assert!(
        config_defs.len() >= 3,
        "Config should be defined in at least 3 languages, got {} definitions",
        config_defs.len()
    );

    // "Server" should be defined in multiple languages
    let server_defs = ws.find_definitions("Server");
    assert!(
        server_defs.len() >= 3,
        "Server should be defined in at least 3 languages, got {} definitions",
        server_defs.len()
    );
}
