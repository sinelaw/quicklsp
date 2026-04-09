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
    ws.scan_directory(&fixture_dir, None);

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
    for name in &[
        "Config",
        "Status",
        "HandlerFunc",
        "Request",
        "Response",
        "Server",
    ] {
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

// =========================================================================
// Bugs found during vim testing on the Linux kernel (2026-04-05)
//
// go-to-definition (gd) jumps to wrong locations for C code.
// The tokenizer doesn't recognize C function definitions, #define
// constants, or enum values as definition sites, so find_definitions
// returns wrong or empty results.
//
// find_references (gr) works correctly via the word index.
// =========================================================================

/// C function definitions (void f, int f, static void f, etc.) should be
/// found by find_definitions. Currently the tokenizer only has struct/enum/
/// typedef/union as CLike def_keywords, so functions are never indexed.
#[test]
fn c_function_go_to_definition() {
    let ws = Workspace::new();
    let fixture = fixtures_dir().join("sample_c.c");
    let source = std::fs::read_to_string(&fixture).unwrap();
    ws.index_file(fixture, source);

    // These are all C function definitions in sample_c.c
    for name in &[
        "init_config",
        "validate_port",
        "process_request",
        "server_init",
        "server_run",
        "server_stop",
        "server_log",
        "sanitize_input",
        "main",
    ] {
        let defs = ws.find_definitions(name);
        assert!(
            !defs.is_empty(),
            "C function '{}' should be found by find_definitions, got 0 results",
            name
        );
    }
}

/// C #define constants should be found by find_definitions.
/// Currently #define is not a def_keyword so they're not indexed.
#[test]
fn c_define_go_to_definition() {
    let ws = Workspace::new();
    let fixture = fixtures_dir().join("sample_c.c");
    let source = std::fs::read_to_string(&fixture).unwrap();
    ws.index_file(fixture, source);

    for name in &["MAX_RETRIES", "DEFAULT_TIMEOUT"] {
        let defs = ws.find_definitions(name);
        assert!(
            !defs.is_empty(),
            "C #define '{}' should be found by find_definitions, got 0 results",
            name
        );
    }
}

/// C enum values should be found by find_definitions.
/// The enum keyword is in def_keywords, but individual enum VALUES
/// (STATUS_ACTIVE, etc.) are not indexed — only the enum name (Status) is.
#[test]
fn c_enum_values_go_to_definition() {
    let ws = Workspace::new();
    let fixture = fixtures_dir().join("sample_c.c");
    let source = std::fs::read_to_string(&fixture).unwrap();
    ws.index_file(fixture, source);

    for name in &["STATUS_ACTIVE", "STATUS_INACTIVE", "STATUS_ERROR"] {
        let defs = ws.find_definitions(name);
        assert!(
            !defs.is_empty(),
            "C enum value '{}' should be found by find_definitions, got 0 results",
            name
        );
    }
}

/// C typedef names should be found by find_definitions.
/// Tree-sitter correctly identifies the alias name in typedefs.
#[test]
fn c_typedef_go_to_definition() {
    let ws = Workspace::new();
    let fixture = fixtures_dir().join("sample_c.c");
    let source = std::fs::read_to_string(&fixture).unwrap();
    ws.index_file(fixture, source);

    for name in &["StatusCode", "ServerConfig"] {
        let defs = ws.find_definitions(name);
        assert!(
            !defs.is_empty(),
            "C typedef '{}' should be found by find_definitions, got 0 results",
            name
        );
    }
}

/// C struct definitions should be found by find_definitions.
/// struct IS in def_keywords, so these should work.
#[test]
fn c_struct_go_to_definition() {
    let ws = Workspace::new();
    let fixture = fixtures_dir().join("sample_c.c");
    let source = std::fs::read_to_string(&fixture).unwrap();
    ws.index_file(fixture, source);

    for name in &["Config", "Server"] {
        let defs = ws.find_definitions(name);
        assert!(
            !defs.is_empty(),
            "C struct '{}' should be found by find_definitions, got 0 results",
            name
        );
    }
}

/// find_references should work for all C identifiers via the word index,
/// even when find_definitions fails. This was confirmed working in vim.
#[test]
fn c_find_references_works_for_all_identifiers() {
    let ws = Workspace::new();
    let fixture = fixtures_dir().join("sample_c.c");
    let source = std::fs::read_to_string(&fixture).unwrap();
    ws.index_file(fixture.clone(), source);

    // Scan directory to build word index (find_references needs it)
    // For a single file, use the fallback text search instead
    let refs = ws.find_references("Config");
    assert!(
        refs.len() >= 5,
        "Config should appear many times in sample_c.c, got {} refs",
        refs.len()
    );

    let refs = ws.find_references("server_init");
    assert!(
        refs.len() >= 2,
        "server_init should appear at definition + call site, got {} refs",
        refs.len()
    );

    let refs = ws.find_references("MAX_RETRIES");
    assert!(
        refs.len() >= 2,
        "MAX_RETRIES should appear at #define + usage, got {} refs",
        refs.len()
    );
}

// =========================================================================
// Kernel-style C: definitions inside #ifdef / #if / #ifndef / #else blocks
//
// Root cause (fixed): collect_definitions() only walked root.children(),
// missing definitions nested inside preproc_ifdef/preproc_if/etc nodes.
// =========================================================================

/// Definitions inside #ifdef CONFIG_* blocks should be indexed.
#[test]
fn kernel_ifdef_guarded_definitions_indexed() {
    let ws = Workspace::new();
    let fixture = fixtures_dir().join("sample_kernel.c");
    let source = std::fs::read_to_string(&fixture).unwrap();
    ws.index_file(fixture, source);

    // Top-level definitions (always worked)
    for name in &[
        "CIA_VERSION",
        "CIA_MAX_OPS",
        "CIA_MIN",
        "cia_op_type",
        "CIA_READ",
        "CIA_WRITE",
        "CIA_EXEC",
        "cia_context",
        "cia_ctx_t",
        "cia_init",
        "cia_execute",
        "cia_main",
    ] {
        let defs = ws.find_definitions(name);
        assert!(
            !defs.is_empty(),
            "Top-level symbol '{}' should be indexed",
            name
        );
    }

    // Definitions inside #ifdef CONFIG_CIA_SECURITY
    for name in &[
        "cia_security_ops",
        "cia_security_check",
        "cia_security_audit",
    ] {
        let defs = ws.find_definitions(name);
        assert!(
            !defs.is_empty(),
            "'{}' inside #ifdef CONFIG_CIA_SECURITY should be indexed",
            name
        );
    }

    // Definitions inside #ifndef CONFIG_CIA_MINIMAL
    let defs = ws.find_definitions("cia_full_init");
    assert!(
        !defs.is_empty(),
        "cia_full_init inside #ifndef should be indexed"
    );

    // Definitions inside #if defined(CONFIG_SMP)
    for name in &["cia_spinlock_t", "cia_spin_lock"] {
        let defs = ws.find_definitions(name);
        assert!(!defs.is_empty(), "'{}' inside #if should be indexed", name);
    }

    // Definitions inside nested #ifdef (inside #if)
    let defs = ws.find_definitions("cia_spin_dump");
    assert!(
        !defs.is_empty(),
        "cia_spin_dump inside nested #ifdef should be indexed"
    );

    // Definitions inside #else
    let defs = ws.find_definitions("cia_nosmp_fallback");
    assert!(
        !defs.is_empty(),
        "cia_nosmp_fallback inside #else should be indexed"
    );
}

// =========================================================================
// Local variable go-to-definition / find-references / hover
//
// quicklsp does not index local variables (by design — they'd bloat the
// symbol table). However, find_references works via word-boundary text
// search, so usages of local variables are found in the current file.
// go-to-definition and hover return nothing for locals since they're not
// in the definitions map.
// =========================================================================

/// find_references should find local variable usages within the same file.
#[test]
fn local_variable_find_references() {
    let ws = Workspace::new();
    let fixture = fixtures_dir().join("sample_kernel.c");
    let source = std::fs::read_to_string(&fixture).unwrap();
    ws.index_file(fixture.clone(), source);

    // "status" is a local variable in cia_execute, used multiple times
    let refs = ws.find_references("status");
    assert!(
        refs.len() >= 2,
        "Local variable 'status' should have references via word search, got {} refs",
        refs.len()
    );

    // "prev_count" is a local variable in cia_execute
    let refs = ws.find_references("prev_count");
    assert!(
        refs.len() >= 1,
        "Local variable 'prev_count' should have at least 1 reference, got {} refs",
        refs.len()
    );

    // "result" is a local in multiple functions
    let refs = ws.find_references("result");
    assert!(
        refs.len() >= 2,
        "Local variable 'result' should have references, got {} refs",
        refs.len()
    );
}

/// go-to-definition for local variables: not in global definitions map,
/// but found via find_local_definitions() in the same file.
#[test]
fn local_variable_go_to_definition() {
    let ws = Workspace::new();
    let fixture = fixtures_dir().join("sample_kernel.c");
    let source = std::fs::read_to_string(&fixture).unwrap();
    ws.index_file(fixture.clone(), source);

    // Local variables should NOT be in the global definitions map
    for name in &["status", "prev_count"] {
        let defs = ws.find_definitions(name);
        assert!(
            defs.is_empty(),
            "Local variable '{}' should NOT be in global definitions (got {} defs)",
            name,
            defs.len()
        );
    }

    // But they SHOULD be found via find_local_definitions()
    let local_defs = ws.find_local_definitions("status", &fixture);
    assert!(
        !local_defs.is_empty(),
        "Local variable 'status' should be found via find_local_definitions"
    );
    assert_eq!(
        local_defs[0].symbol.kind,
        quicklsp::parsing::symbols::SymbolKind::Variable
    );
    assert!(
        local_defs[0].symbol.depth > 0,
        "Local should have depth > 0"
    );
    assert_eq!(
        local_defs[0].symbol.container.as_deref(),
        Some("cia_execute"),
        "Local 'status' should have container = 'cia_execute'"
    );

    let local_defs = ws.find_local_definitions("prev_count", &fixture);
    assert!(
        !local_defs.is_empty(),
        "Local variable 'prev_count' should be found via find_local_definitions"
    );
}

/// Hover for local variables shows type information.
#[test]
fn local_variable_hover_shows_type() {
    let ws = Workspace::new();
    let fixture = fixtures_dir().join("sample_kernel.c");
    let source = std::fs::read_to_string(&fixture).unwrap();
    ws.index_file(fixture.clone(), source);

    // Local variables have their type stored in doc_comment by the C parser
    let local_defs = ws.find_local_definitions("status", &fixture);
    assert!(!local_defs.is_empty(), "Should find local 'status'");
    assert_eq!(
        local_defs[0].symbol.doc_comment.as_deref(),
        Some("int"),
        "Local 'status' should have type 'int' in doc_comment"
    );
}

/// Function parameters should be found via find_local_definitions.
#[test]
fn function_parameter_go_to_definition() {
    let ws = Workspace::new();
    let fixture = fixtures_dir().join("sample_kernel.c");
    let source = std::fs::read_to_string(&fixture).unwrap();
    ws.index_file(fixture.clone(), source);

    // 'ctx' is a parameter of cia_init, cia_execute, etc.
    let local_defs = ws.find_local_definitions("ctx", &fixture);
    assert!(
        !local_defs.is_empty(),
        "Parameter 'ctx' should be found via find_local_definitions"
    );
    assert_eq!(local_defs[0].symbol.def_keyword, "parameter");
    assert!(local_defs[0].symbol.depth > 0);

    // 'op' is a parameter of cia_execute
    let local_defs = ws.find_local_definitions("op", &fixture);
    assert!(
        !local_defs.is_empty(),
        "Parameter 'op' should be found via find_local_definitions"
    );
}

/// Struct fields should be found via find_local_definitions.
#[test]
fn struct_field_go_to_definition() {
    let ws = Workspace::new();
    let fixture = fixtures_dir().join("sample_kernel.c");
    let source = std::fs::read_to_string(&fixture).unwrap();
    ws.index_file(fixture.clone(), source);

    // 'op_count' is a field of struct cia_context
    let local_defs = ws.find_local_definitions("op_count", &fixture);
    assert!(
        !local_defs.is_empty(),
        "Struct field 'op_count' should be found via find_local_definitions"
    );
    assert_eq!(local_defs[0].symbol.def_keyword, "field");
    assert_eq!(
        local_defs[0].symbol.container.as_deref(),
        Some("cia_context"),
        "Field 'op_count' should have container = 'cia_context'"
    );

    // 'last_op' is a field of struct cia_context
    let local_defs = ws.find_local_definitions("last_op", &fixture);
    assert!(
        !local_defs.is_empty(),
        "Struct field 'last_op' should be found via find_local_definitions"
    );

    // 'name' is a field of struct cia_context
    let local_defs = ws.find_local_definitions("name", &fixture);
    assert!(
        !local_defs.is_empty(),
        "Struct field 'name' should be found via find_local_definitions"
    );
}

// =========================================================================
// printk: symbol defined in external headers (not in workspace)
//
// quicklsp doesn't follow #include directives or index header files
// outside the workspace. Symbols like printk, defined in <linux/printk.h>,
// won't have definitions. find_references still works via word search.
// =========================================================================

/// printk is not defined in the workspace, so go-to-definition returns empty.
#[test]
fn external_symbol_printk_no_definition() {
    let ws = Workspace::new();
    // Use a source that references printk
    ws.index_file(
        PathBuf::from("/src/core_cia.c"),
        r#"
#include <linux/printk.h>

void cia_log(int level, const char *msg) {
    printk(KERN_INFO "CIA: %s\n", msg);
}

void cia_warn(const char *msg) {
    printk(KERN_WARNING "CIA warning: %s\n", msg);
}
"#
        .to_string(),
    );

    // printk is not defined in this file or workspace
    let defs = ws.find_definitions("printk");
    assert!(
        defs.is_empty(),
        "printk is defined in <linux/printk.h>, not in workspace — \
         find_definitions should return empty, got {} defs",
        defs.len()
    );

    // But find_references should find all usages in the file
    let refs = ws.find_references("printk");
    assert!(
        refs.len() >= 2,
        "printk should appear at least twice via word search, got {} refs",
        refs.len()
    );

    // hover returns None since there's no definition
    let hover = ws.hover_info("printk");
    assert!(
        hover.is_none(),
        "Hover for printk should return None — not defined in workspace"
    );
}

// =========================================================================
// Cursor position: LSP features should work even when the cursor is in
// the middle of a symbol, not just at the first character.
//
// word_at_position() expands outward from the cursor position to find
// the full identifier word, so this should already work.
// =========================================================================

/// word_at_position should extract the full identifier regardless of cursor column.
#[test]
fn word_at_position_mid_symbol() {
    use quicklsp::lsp::server::QuickLspServer;

    let source = "void cia_security_check(struct cia_context *ctx) {\n    return;\n}\n";

    // Cursor on 'c' of 'cia_security_check' (col=5, first char)
    let word = QuickLspServer::word_at_position(source, 0, 5);
    assert_eq!(
        word.as_deref(),
        Some("cia_security_check"),
        "Cursor at start of symbol should extract full word"
    );

    // Cursor on 's' of 'security' (col=9, middle of symbol)
    let word = QuickLspServer::word_at_position(source, 0, 9);
    assert_eq!(
        word.as_deref(),
        Some("cia_security_check"),
        "Cursor in middle of symbol should extract full word"
    );

    // Cursor on 'k' of 'check' (col=22, near end)
    let word = QuickLspServer::word_at_position(source, 0, 22);
    assert_eq!(
        word.as_deref(),
        Some("cia_security_check"),
        "Cursor near end of symbol should extract full word"
    );

    // Cursor on 'c' of 'cia_context' (col=31)
    let word = QuickLspServer::word_at_position(source, 0, 35);
    assert_eq!(
        word.as_deref(),
        Some("cia_context"),
        "Cursor on struct type name should extract full word"
    );

    // Cursor on 't' of 'ctx' (middle of param name)
    let word = QuickLspServer::word_at_position(source, 0, 44);
    assert_eq!(
        word.as_deref(),
        Some("ctx"),
        "Cursor on parameter name should extract full word"
    );
}

/// go-to-definition should work with cursor in the middle of a symbol name.
#[test]
fn go_to_definition_mid_cursor() {
    let ws = Workspace::new();
    ws.index_file(
        PathBuf::from("/src/test.c"),
        r#"
void cia_security_check(void) {}

void caller(void) {
    cia_security_check();
}
"#
        .to_string(),
    );

    // The word extraction is tested above; here we verify the full pipeline:
    // find_definitions should find it regardless of which character we'd cursor on
    let defs = ws.find_definitions("cia_security_check");
    assert!(
        !defs.is_empty(),
        "find_definitions for 'cia_security_check' should succeed"
    );
    assert_eq!(defs[0].symbol.line, 1, "Definition should be on line 1");
}

/// find_references should work with cursor anywhere in the symbol.
#[test]
fn find_references_mid_cursor() {
    let ws = Workspace::new();
    ws.index_file(
        PathBuf::from("/src/test.c"),
        r#"
void cia_security_check(void) {}

void caller(void) {
    cia_security_check();
}
"#
        .to_string(),
    );

    let refs = ws.find_references("cia_security_check");
    assert!(
        refs.len() >= 2,
        "Should find at least 2 references (def + call), got {}",
        refs.len()
    );
}

/// Scope-aware local variable resolution: when the same name is declared in
/// nested blocks (shadowing), find_local_definition_at should pick the
/// innermost declaration that precedes the cursor.
#[test]
fn local_variable_scope_shadowing() {
    let ws = Workspace::new();
    let source = r#"void foo(void) {
    int x = 1;
    if (1) {
        int x = 2;
        x;
    }
    x;
}
"#;
    let path = PathBuf::from("/src/shadow.c");
    ws.index_file(path.clone(), source.to_string());

    // Both 'x' declarations should exist as locals
    let all_x = ws.find_local_definitions("x", &path);
    assert_eq!(
        all_x.len(),
        2,
        "Should find 2 declarations of 'x', got {}",
        all_x.len()
    );

    // Cursor on line 4 (inside if block, after inner `int x = 2;` on line 3):
    // should resolve to the inner x (line 3, depth 2)
    let inner = ws.find_local_definition_at("x", &path, 4);
    assert!(inner.is_some(), "Should find inner x at cursor line 4");
    let inner = inner.unwrap();
    assert_eq!(
        inner.symbol.line, 3,
        "Should pick inner x on line 3, got line {}",
        inner.symbol.line
    );

    // Cursor on line 6 (after the if block closed):
    // should resolve to the outer x (line 1, depth 1)
    let outer = ws.find_local_definition_at("x", &path, 6);
    assert!(outer.is_some(), "Should find outer x at cursor line 6");
    let outer = outer.unwrap();
    assert_eq!(
        outer.symbol.line, 1,
        "Should pick outer x on line 1, got line {}",
        outer.symbol.line
    );
}

/// Scope-aware resolution for for-loop variables.
#[test]
fn local_variable_scope_for_loop() {
    let ws = Workspace::new();
    let source = r#"void bar(void) {
    for (int i = 0; i < 10; i++) {
        i;
    }
    for (int i = 0; i < 5; i++) {
        i;
    }
}
"#;
    let path = PathBuf::from("/src/forloop.c");
    ws.index_file(path.clone(), source.to_string());

    // Cursor on line 2 (inside first for loop): should find the first i (line 1)
    let first = ws.find_local_definition_at("i", &path, 2);
    assert!(first.is_some(), "Should find i at cursor line 2");
    assert_eq!(
        first.unwrap().symbol.line,
        1,
        "Should pick first loop's i on line 1"
    );

    // Cursor on line 5 (inside second for loop): should find the second i (line 4)
    let second = ws.find_local_definition_at("i", &path, 5);
    assert!(second.is_some(), "Should find i at cursor line 5");
    assert_eq!(
        second.unwrap().symbol.line,
        4,
        "Should pick second loop's i on line 4"
    );
}

// =========================================================================
// Bug #4: Rust local variables (`let` bindings) and function parameters are
// never indexed, so go-to-definition on a local returns nothing (or jumps
// to an unrelated global symbol that happens to share the same name).
//
// Root cause: the Rust tree-sitter query in
// `src/parsing/tree_sitter_parse/rust.rs` captures only item-level
// definitions (fn, struct, enum, trait, const, static, mod, macros,
// struct fields, enum variants, impl methods). It has no capture for
// `let_declaration` (`identifier_pattern` inside `let` bindings) or for
// function `parameter`/`self_parameter` nodes. Because of this, locals
// never reach the per-file symbol list, so `find_local_definitions` /
// `find_local_definition_at` always return empty for Rust.
//
// Observable symptom (tested against quicklsp's own source tree via
// neovim):
//   * cursor on a unique local like `let id_table = ...` —
//     `textDocument/definition` returns `null` (looks like "nothing
//     happens" in the editor).
//   * cursor on a common local like `let path = ...` — the server falls
//     through to the global lookup and jumps to an unrelated `path`
//     symbol in another file.
// =========================================================================

/// Rust `let` bindings must be extracted as local symbols so that
/// go-to-definition on a local variable resolves to the `let` line.
#[test]
fn bug4_rust_let_binding_indexed_as_local() {
    let ws = Workspace::new();
    // Line 0: `pub fn foo(count: usize) -> usize {`
    // Line 1: `    let id_table = count + 1;`
    // Line 2: `    id_table`
    // Line 3: `}`
    let source = "pub fn foo(count: usize) -> usize {\n\
                  \x20   let id_table = count + 1;\n\
                  \x20   id_table\n\
                  }\n";
    let path = PathBuf::from("/src/sample.rs");
    ws.index_file(path.clone(), source.to_string());

    // `id_table` is a unique identifier, so the only way for the server
    // to find it is through the per-file local index.
    let locals = ws.find_local_definitions("id_table", &path);
    assert!(
        !locals.is_empty(),
        "BUG #4: Rust `let id_table = ...` is not indexed — \
         the tree-sitter Rust query has no capture for `let_declaration`"
    );

    // Cursor on line 2 (the use site `id_table`): scope-aware lookup
    // must return the let binding on line 1.
    let def = ws.find_local_definition_at("id_table", &path, 2);
    assert!(
        def.is_some(),
        "BUG #4: find_local_definition_at returned None for `id_table` — \
         Rust locals never reach the symbol table"
    );
    assert_eq!(
        def.unwrap().symbol.line,
        1,
        "Should resolve to the `let id_table = ...` line"
    );
}

/// Rust function parameters must be extracted as local symbols so that
/// go-to-definition on a parameter use resolves to the parameter binding.
#[test]
fn bug4_rust_fn_parameter_indexed_as_local() {
    let ws = Workspace::new();
    // Line 0: `pub fn foo(unique_param: usize) -> usize {`
    // Line 1: `    unique_param + 1`
    // Line 2: `}`
    let source = "pub fn foo(unique_param: usize) -> usize {\n\
                  \x20   unique_param + 1\n\
                  }\n";
    let path = PathBuf::from("/src/sample.rs");
    ws.index_file(path.clone(), source.to_string());

    let locals = ws.find_local_definitions("unique_param", &path);
    assert!(
        !locals.is_empty(),
        "BUG #4: Rust fn parameter `unique_param` is not indexed — \
         the tree-sitter Rust query has no capture for function parameters"
    );

    let def = ws.find_local_definition_at("unique_param", &path, 1);
    assert!(
        def.is_some(),
        "BUG #4: find_local_definition_at returned None for fn parameter \
         `unique_param`"
    );
    assert_eq!(
        def.unwrap().symbol.line,
        0,
        "Should resolve to the signature line of `foo`"
    );
}

/// End-to-end reproduction of what the user observes in the editor:
/// `textDocument/definition` on the use of a local `let` binding should
/// return the let line. Today it returns empty because locals are not
/// indexed and `find_definitions` (global) also finds nothing.
#[test]
fn bug4_rust_goto_definition_on_let_binding() {
    let ws = Workspace::new();
    let source = "pub fn foo(count: usize) -> usize {\n\
                  \x20   let id_table = count + 1;\n\
                  \x20   id_table\n\
                  }\n";
    let path = PathBuf::from("/src/sample.rs");
    ws.index_file(path.clone(), source.to_string());

    // Global definitions must be empty (it's a local), and the local
    // lookup must succeed. Today BOTH are empty, which is why the editor
    // sees "nothing happens".
    assert!(
        ws.find_definitions("id_table").is_empty(),
        "Precondition: `id_table` should not be a global symbol"
    );
    let local = ws.find_local_definition_at("id_table", &path, 2);
    assert!(
        local.is_some(),
        "BUG #4: Go-to-definition on the use of a Rust `let` binding \
         returns nothing — Rust locals are not extracted by the parser"
    );
}

/// When a local variable shadows a global symbol, go-to-definition on
/// the local must prefer the local — otherwise the editor silently jumps
/// to the wrong file. This is the `path` case observed against the real
/// quicklsp workspace: `let path = &id_table[...]` in `resolve_refs`
/// currently jumps to an unrelated `path` in a test fixture.
#[test]
fn bug4_rust_local_shadows_global() {
    let ws = Workspace::new();
    // Global `path` defined in one file.
    ws.index_file(
        PathBuf::from("/src/other.rs"),
        "pub const path: &str = \"/tmp\";\n".to_string(),
    );
    // Local `path` defined via `let` in another file.
    let caller_path = PathBuf::from("/src/caller.rs");
    let caller_src = "pub fn run() -> usize {\n\
                      \x20   let path = 1usize;\n\
                      \x20   path + 1\n\
                      }\n";
    ws.index_file(caller_path.clone(), caller_src.to_string());

    // The local lookup must find the `let path = 1;` on line 1.
    let local = ws.find_local_definition_at("path", &caller_path, 2);
    assert!(
        local.is_some(),
        "BUG #4: local `let path = ...` not indexed, so the editor falls \
         through to the global `pub const path` in other.rs"
    );
    assert_eq!(
        local.unwrap().file,
        caller_path,
        "Local `path` should resolve to caller.rs, not other.rs"
    );
}
