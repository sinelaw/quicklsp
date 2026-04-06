//! Rust integration test — full LSP round-trip through the server binary.
//!
//! Spawns quicklsp, opens the `sample_rust.rs` fixture, and sends hover,
//! go-to-definition, find-references, completion, document/workspace
//! symbols, and signature-help requests covering every major Rust syntax
//! element: structs, enums, traits, functions, impl methods, consts,
//! statics, type aliases, modules, nested functions, unicode identifiers.
//!
//! Cursor positions are anchored to `@mark TAG` comments in the fixture.
//!
//!   cargo test -p quicklsp --test rust_integration -- --nocapture

mod common;

use common::*;
use std::time::Duration;

#[test]
fn test_rust_full_lsp() {
    // ── Setup ────────────────────────────────────────────────────
    let dir = fixtures_dir();
    let mut s = LspServer::spawn();
    s.initialize(&dir);
    drain_until_progress_end(&mut s);

    let (uri, src) = open_fixture(&mut s, &dir, "sample_rust.rs", "rust");

    // Give the server a moment to process didOpen
    std::thread::sleep(Duration::from_millis(200));

    let mut t = TestResults::new();

    // ═════════════════════════════════════════════════════════════
    //  1. HOVER — definitions (23 tests from manual report Phase 1)
    // ═════════════════════════════════════════════════════════════

    // ── 1.1 struct definition ────────────────────────────────────
    {
        let (l, c) = mark(&src, "Config_DEF", "Config");
        check_hover_contains(&mut t, &s.hover(&uri, l, c), "Config", "hover@Config_DEF");
    }

    // ── 1.2 const: MAX_RETRIES ──────────────────────────────────
    {
        let (l, c) = mark(&src, "MAX_RETRIES_DEF", "MAX_RETRIES");
        check_hover_contains(
            &mut t,
            &s.hover(&uri, l, c),
            "MAX_RETRIES",
            "hover@MAX_RETRIES_DEF",
        );
    }

    // ── 1.3 const: DEFAULT_TIMEOUT ──────────────────────────────
    {
        let (l, c) = mark(&src, "DEFAULT_TIMEOUT_DEF", "DEFAULT_TIMEOUT");
        check_hover_contains(
            &mut t,
            &s.hover(&uri, l, c),
            "DEFAULT_TIMEOUT",
            "hover@DEFAULT_TIMEOUT_DEF",
        );
    }

    // ── 1.4 enum ─────────────────────────────────────────────────
    {
        let (l, c) = mark(&src, "Status_DEF", "Status");
        check_hover_contains(&mut t, &s.hover(&uri, l, c), "Status", "hover@Status_DEF");
    }

    // ── 1.5 trait ────────────────────────────────────────────────
    {
        let (l, c) = mark(&src, "Handler_DEF", "Handler");
        check_hover_contains(
            &mut t,
            &s.hover(&uri, l, c),
            "Handler",
            "hover@Handler_DEF",
        );
    }

    // ── 1.6 struct: Request ─────────────────────────────────────
    {
        let (l, c) = mark(&src, "Request_DEF", "Request");
        check_hover_contains(
            &mut t,
            &s.hover(&uri, l, c),
            "Request",
            "hover@Request_DEF",
        );
    }

    // ── 1.7 struct: Response ────────────────────────────────────
    {
        let (l, c) = mark(&src, "Response_DEF", "Response");
        check_hover_contains(
            &mut t,
            &s.hover(&uri, l, c),
            "Response",
            "hover@Response_DEF",
        );
    }

    // ── 1.8 fn: create_config ───────────────────────────────────
    {
        let (l, c) = mark(&src, "create_config_DEF", "create_config");
        check_hover_contains(
            &mut t,
            &s.hover(&uri, l, c),
            "create_config",
            "hover@create_config_DEF",
        );
    }

    // ── 1.9 fn: process_request (full signature with params) ────
    {
        let (l, c) = mark(&src, "process_request_DEF", "process_request");
        let resp = s.hover(&uri, l, c);
        check_hover_contains(&mut t, &resp, "process_request", "hover@process_request_DEF");
    }

    // ── 1.10 struct: Server ─────────────────────────────────────
    {
        let (l, c) = mark(&src, "Server_DEF", "Server");
        check_hover_contains(&mut t, &s.hover(&uri, l, c), "Server", "hover@Server_DEF");
    }

    // ── 1.11 impl method: Server::new ───────────────────────────
    {
        let (l, c) = mark(&src, "Server_new_DEF", "new");
        check_hover_contains(&mut t, &s.hover(&uri, l, c), "new", "hover@Server_new_DEF");
    }

    // ── 1.12 impl method: Server::add_handler ───────────────────
    {
        let (l, c) = mark(&src, "Server_add_handler_DEF", "add_handler");
        check_hover_contains(
            &mut t,
            &s.hover(&uri, l, c),
            "add_handler",
            "hover@Server_add_handler_DEF",
        );
    }

    // ── 1.13 impl method: Server::run ───────────────────────────
    {
        let (l, c) = mark(&src, "Server_run_DEF", "run");
        check_hover_contains(&mut t, &s.hover(&uri, l, c), "run", "hover@Server_run_DEF");
    }

    // ── 1.14 type alias: StatusCode ─────────────────────────────
    {
        let (l, c) = mark(&src, "StatusCode_DEF", "StatusCode");
        check_hover_contains(
            &mut t,
            &s.hover(&uri, l, c),
            "StatusCode",
            "hover@StatusCode_DEF",
        );
    }

    // ── 1.15 type alias: HandlerResult ──────────────────────────
    {
        let (l, c) = mark(&src, "HandlerResult_DEF", "HandlerResult");
        check_hover_contains(
            &mut t,
            &s.hover(&uri, l, c),
            "HandlerResult",
            "hover@HandlerResult_DEF",
        );
    }

    // ── 1.16 fn: validate_request ───────────────────────────────
    {
        let (l, c) = mark(&src, "validate_request_DEF", "validate_request");
        check_hover_contains(
            &mut t,
            &s.hover(&uri, l, c),
            "validate_request",
            "hover@validate_request_DEF",
        );
    }

    // ── 1.17 unicode fn: données_utilisateur ────────────────────
    {
        let (l, c) = mark(&src, "unicode_fn_DEF", "données_utilisateur");
        check_hover_contains(
            &mut t,
            &s.hover(&uri, l, c),
            "données_utilisateur",
            "hover@unicode_fn_DEF",
        );
    }

    // ── 1.18 unicode struct: Über ───────────────────────────────
    {
        let (l, c) = mark(&src, "unicode_struct_DEF", "Über");
        check_hover_contains(
            &mut t,
            &s.hover(&uri, l, c),
            "Über",
            "hover@unicode_struct_DEF",
        );
    }

    // ── 1.19 nested fn: outer ───────────────────────────────────
    {
        let (l, c) = mark(&src, "outer_DEF", "outer");
        check_hover_contains(&mut t, &s.hover(&uri, l, c), "outer", "hover@outer_DEF");
    }

    // ── 1.20 nested fn: inner ───────────────────────────────────
    {
        let (l, c) = mark(&src, "inner_DEF", "inner");
        check_hover_contains(&mut t, &s.hover(&uri, l, c), "inner", "hover@inner_DEF");
    }

    // ── 1.21 module fn: sanitize_input ──────────────────────────
    {
        let (l, c) = mark(&src, "sanitize_input_DEF", "sanitize_input");
        check_hover_contains(
            &mut t,
            &s.hover(&uri, l, c),
            "sanitize_input",
            "hover@sanitize_input_DEF",
        );
    }

    // ── 1.22 module fn: validate_port ───────────────────────────
    {
        let (l, c) = mark(&src, "validate_port_DEF", "validate_port");
        check_hover_contains(
            &mut t,
            &s.hover(&uri, l, c),
            "validate_port",
            "hover@validate_port_DEF",
        );
    }

    // ── 1.23 static: GLOBAL_COUNTER ─────────────────────────────
    {
        let (l, c) = mark(&src, "GLOBAL_COUNTER_DEF", "GLOBAL_COUNTER");
        check_hover_contains(
            &mut t,
            &s.hover(&uri, l, c),
            "GLOBAL_COUNTER",
            "hover@GLOBAL_COUNTER_DEF",
        );
    }

    // ── 1.24 const: FINAL_STATUS ────────────────────────────────
    {
        let (l, c) = mark(&src, "FINAL_STATUS_DEF", "FINAL_STATUS");
        check_hover_contains(
            &mut t,
            &s.hover(&uri, l, c),
            "FINAL_STATUS",
            "hover@FINAL_STATUS_DEF",
        );
    }

    // ═════════════════════════════════════════════════════════════
    //  2. HOVER — usages (symbol references in expressions)
    // ═════════════════════════════════════════════════════════════

    // ── 2.1 Status::Active usage ────────────────────────────────
    {
        let (l, c) = mark(&src, "USE_Status_Active", "Status");
        check_hover_contains(
            &mut t,
            &s.hover(&uri, l, c),
            "Status",
            "hover@USE_Status_Active",
        );
    }

    // ── 2.2 Handler in dyn bound ────────────────────────────────
    {
        let (l, c) = mark(&src, "USE_Handler_dyn", "Handler");
        check_hover_contains(
            &mut t,
            &s.hover(&uri, l, c),
            "Handler",
            "hover@USE_Handler_dyn",
        );
    }

    // ── 2.3 MAX_RETRIES usage ───────────────────────────────────
    {
        let (l, c) = mark(&src, "USE_MAX_RETRIES", "MAX_RETRIES");
        check_hover_contains(
            &mut t,
            &s.hover(&uri, l, c),
            "MAX_RETRIES",
            "hover@USE_MAX_RETRIES",
        );
    }

    // ── 2.4 Inside string literal — should return null ──────────
    {
        let (l, c) = mark(&src, "INSIDE_STRING", "inside");
        check_hover_no_error(&mut t, &s.hover(&uri, l, c), "hover@INSIDE_STRING");
    }

    // ── 2.5 println! macro — not tracked, should not error ─────
    {
        let (l, c) = mark(&src, "USE_println", "println");
        check_hover_no_error(&mut t, &s.hover(&uri, l, c), "hover@USE_println");
    }

    // ── 2.6 mod: utils ──────────────────────────────────────────
    {
        let (l, c) = mark(&src, "utils_DEF", "utils");
        check_hover_contains(&mut t, &s.hover(&uri, l, c), "utils", "hover@utils_DEF");
    }

    // ═════════════════════════════════════════════════════════════
    //  3. GO TO DEFINITION (14 tests from manual report Phase 2)
    // ═════════════════════════════════════════════════════════════

    // ── 3.1 Status::Active → enum Status ────────────────────────
    {
        let (l, c) = mark(&src, "USE_Status_Active", "Status");
        check_definition_found(&mut t, &s.goto_definition(&uri, l, c), "def@Status_from_Active");
        check_definition_target(
            &mut t,
            &s.goto_definition(&uri, l, c),
            "sample_rust.rs",
            "def@Status_from_Active_target",
        );
    }

    // ── 3.2 MAX_RETRIES usage → const ───────────────────────────
    {
        let (l, c) = mark(&src, "USE_MAX_RETRIES", "MAX_RETRIES");
        check_definition_found(
            &mut t,
            &s.goto_definition(&uri, l, c),
            "def@MAX_RETRIES_usage",
        );
    }

    // ── 3.3 DEFAULT_TIMEOUT usage → const ───────────────────────
    {
        let (l, c) = mark(&src, "USE_DEFAULT_TIMEOUT", "DEFAULT_TIMEOUT");
        check_definition_found(
            &mut t,
            &s.goto_definition(&uri, l, c),
            "def@DEFAULT_TIMEOUT_usage",
        );
    }

    // ── 3.4 Handler in dyn bound → trait ────────────────────────
    {
        let (l, c) = mark(&src, "USE_Handler_dyn", "Handler");
        check_definition_found(
            &mut t,
            &s.goto_definition(&uri, l, c),
            "def@Handler_dyn_bound",
        );
    }

    // ── 3.5 validate_request call → fn def ──────────────────────
    {
        let (l, c) = mark(&src, "CALL_validate_request", "validate_request");
        check_definition_found(
            &mut t,
            &s.goto_definition(&uri, l, c),
            "def@validate_request_call",
        );
        check_definition_target(
            &mut t,
            &s.goto_definition(&uri, l, c),
            "sample_rust.rs",
            "def@validate_request_call_target",
        );
    }

    // ── 3.6 inner() call → nested fn def ────────────────────────
    {
        let (l, c) = mark(&src, "CALL_inner", "inner");
        check_definition_found(
            &mut t,
            &s.goto_definition(&uri, l, c),
            "def@inner_call",
        );
    }

    // ── 3.7 create_config call → fn def ─────────────────────────
    {
        let (l, c) = mark(&src, "CALL_create_config", "create_config");
        check_definition_found(
            &mut t,
            &s.goto_definition(&uri, l, c),
            "def@create_config_call",
        );
        check_definition_target(
            &mut t,
            &s.goto_definition(&uri, l, c),
            "sample_rust.rs",
            "def@create_config_call_target",
        );
    }

    // ── 3.8 process_request call → fn def ───────────────────────
    {
        let (l, c) = mark(&src, "CALL_process_request", "process_request");
        check_definition_found(
            &mut t,
            &s.goto_definition(&uri, l, c),
            "def@process_request_call",
        );
        check_definition_target(
            &mut t,
            &s.goto_definition(&uri, l, c),
            "sample_rust.rs",
            "def@process_request_call_target",
        );
    }

    // ═════════════════════════════════════════════════════════════
    //  4. FIND REFERENCES (11 tests from manual report Phase 3)
    // ═════════════════════════════════════════════════════════════

    // ── 4.1 Config — used in multiple places ────────────────────
    {
        let (l, c) = mark(&src, "Config_DEF", "Config");
        check_references_ge(&mut t, &s.find_references(&uri, l, c), 3, "refs@Config");
    }

    // ── 4.2 Status — used in process_request ────────────────────
    {
        let (l, c) = mark(&src, "Status_DEF", "Status");
        check_references_ge(&mut t, &s.find_references(&uri, l, c), 2, "refs@Status");
    }

    // ── 4.3 Handler — used in struct field + fn param ───────────
    {
        let (l, c) = mark(&src, "Handler_DEF", "Handler");
        check_references_ge(&mut t, &s.find_references(&uri, l, c), 2, "refs@Handler");
    }

    // ── 4.4 Request — used in trait, fn params ──────────────────
    {
        let (l, c) = mark(&src, "Request_DEF", "Request");
        check_references_ge(&mut t, &s.find_references(&uri, l, c), 3, "refs@Request");
    }

    // ── 4.5 Response — used in trait, fn return types ───────────
    {
        let (l, c) = mark(&src, "Response_DEF", "Response");
        check_references_ge(&mut t, &s.find_references(&uri, l, c), 3, "refs@Response");
    }

    // ── 4.6 MAX_RETRIES — used in Server::run ──────────────────
    {
        let (l, c) = mark(&src, "MAX_RETRIES_DEF", "MAX_RETRIES");
        check_references_ge(&mut t, &s.find_references(&uri, l, c), 2, "refs@MAX_RETRIES");
    }

    // ── 4.7 Server — used in impl block, new(), etc ─────────────
    {
        let (l, c) = mark(&src, "Server_DEF", "Server");
        check_references_ge(&mut t, &s.find_references(&uri, l, c), 2, "refs@Server");
    }

    // ── 4.8 process_request — used at call sites ────────────────
    {
        let (l, c) = mark(&src, "process_request_DEF", "process_request");
        check_references_ge(
            &mut t,
            &s.find_references(&uri, l, c),
            2,
            "refs@process_request",
        );
    }

    // ── 4.9 validate_request — used at call site ────────────────
    {
        let (l, c) = mark(&src, "validate_request_DEF", "validate_request");
        check_references_ge(
            &mut t,
            &s.find_references(&uri, l, c),
            2,
            "refs@validate_request",
        );
    }

    // ── 4.10 Refs include same file ─────────────────────────────
    {
        let (l, c) = mark(&src, "Config_DEF", "Config");
        check_references_include_file(
            &mut t,
            &s.find_references(&uri, l, c),
            "sample_rust.rs",
            "refs@Config_self_file",
        );
    }

    // ── 4.11 create_config references ───────────────────────────
    {
        let (l, c) = mark(&src, "create_config_DEF", "create_config");
        check_references_ge(
            &mut t,
            &s.find_references(&uri, l, c),
            2,
            "refs@create_config",
        );
    }

    // ═════════════════════════════════════════════════════════════
    //  5. COMPLETION (20 tests from manual report Phase 4)
    // ═════════════════════════════════════════════════════════════

    // Completion at the "process_request" definition line.
    // The prefix "process_" at col 12 should suggest process_request.
    {
        let (l, _) = mark(&src, "process_request_DEF", "process_request");
        // Position cursor at col 12 = after "fn process_" → prefix "process_"
        let resp = s.completion(&uri, l, 12);
        check_completion_contains(&mut t, &resp, "process_request", "completion@process_");
    }

    // ── 5.1 Prefix "Conf" → Config ─────────────────────────────
    {
        let (l, c) = mark(&src, "Config_DEF", "Config");
        let resp = s.completion(&uri, l, c + 4); // "Conf"
        check_completion_contains(&mut t, &resp, "Config", "completion@Conf");
    }

    // ── 5.2 Prefix "Stat" → Status, StatusCode ─────────────────
    {
        let (l, c) = mark(&src, "Status_DEF", "Status");
        let resp = s.completion(&uri, l, c + 4); // "Stat"
        check_completion_contains(&mut t, &resp, "Status", "completion@Stat");
    }

    // ── 5.3 Prefix "Hand" → Handler, HandlerResult ─────────────
    {
        let (l, c) = mark(&src, "Handler_DEF", "Handler");
        let resp = s.completion(&uri, l, c + 4); // "Hand"
        check_completion_contains(&mut t, &resp, "Handler", "completion@Hand");
    }

    // ── 5.4 Prefix "Serv" → Server ─────────────────────────────
    {
        let (l, c) = mark(&src, "Server_DEF", "Server");
        let resp = s.completion(&uri, l, c + 4); // "Serv"
        check_completion_contains(&mut t, &resp, "Server", "completion@Serv");
    }

    // ── 5.5 Prefix "Req" → Request ─────────────────────────────
    {
        let (l, c) = mark(&src, "Request_DEF", "Request");
        let resp = s.completion(&uri, l, c + 3); // "Req"
        check_completion_contains(&mut t, &resp, "Request", "completion@Req");
    }

    // ── 5.6 Prefix "Resp" → Response ───────────────────────────
    {
        let (l, c) = mark(&src, "Response_DEF", "Response");
        let resp = s.completion(&uri, l, c + 4); // "Resp"
        check_completion_contains(&mut t, &resp, "Response", "completion@Resp");
    }

    // ── 5.7 Prefix "create" → create_config ────────────────────
    {
        let (l, c) = mark(&src, "create_config_DEF", "create_config");
        let resp = s.completion(&uri, l, c + 6); // "create"
        check_completion_contains(&mut t, &resp, "create_config", "completion@create");
    }

    // ── 5.8 Prefix "valid" → validate_request, validate_port ───
    {
        let (l, c) = mark(&src, "validate_request_DEF", "validate_request");
        let resp = s.completion(&uri, l, c + 5); // "valid"
        check_completion_contains(&mut t, &resp, "validate_request", "completion@valid");
    }

    // ── 5.9 Prefix "MAX" → MAX_RETRIES ─────────────────────────
    {
        let (l, c) = mark(&src, "MAX_RETRIES_DEF", "MAX_RETRIES");
        let resp = s.completion(&uri, l, c + 3); // "MAX"
        check_completion_contains(&mut t, &resp, "MAX_RETRIES", "completion@MAX");
    }

    // ── 5.10 Prefix "DEFAULT" → DEFAULT_TIMEOUT ────────────────
    {
        let (l, c) = mark(&src, "DEFAULT_TIMEOUT_DEF", "DEFAULT_TIMEOUT");
        let resp = s.completion(&uri, l, c + 7); // "DEFAULT"
        check_completion_contains(&mut t, &resp, "DEFAULT_TIMEOUT", "completion@DEFAULT");
    }

    // ── 5.11 Prefix "out" → outer ──────────────────────────────
    {
        let (l, c) = mark(&src, "outer_DEF", "outer");
        let resp = s.completion(&uri, l, c + 3); // "out"
        check_completion_contains(&mut t, &resp, "outer", "completion@out");
    }

    // ── 5.12 Prefix "inn" → inner ──────────────────────────────
    {
        let (l, c) = mark(&src, "inner_DEF", "inner");
        let resp = s.completion(&uri, l, c + 3); // "inn"
        check_completion_contains(&mut t, &resp, "inner", "completion@inn");
    }

    // ── 5.13 Prefix "san" → sanitize_input ─────────────────────
    {
        let (l, c) = mark(&src, "sanitize_input_DEF", "sanitize_input");
        let resp = s.completion(&uri, l, c + 3); // "san"
        check_completion_contains(&mut t, &resp, "sanitize_input", "completion@san");
    }

    // ── 5.14 Any completion returns non-empty ───────────────────
    {
        let (l, c) = mark(&src, "Server_DEF", "Server");
        let resp = s.completion(&uri, l, c + 2); // "Se"
        check_completion_non_empty(&mut t, &resp, "completion@Se_non_empty");
    }

    // ═════════════════════════════════════════════════════════════
    //  6. DOCUMENT SYMBOLS (7 tests from manual report Phase 5)
    // ═════════════════════════════════════════════════════════════

    {
        let resp = s.document_symbols(&uri);

        // Should contain all top-level symbols
        for name in &[
            "Config",
            "Status",
            "Handler",
            "Request",
            "Response",
            "Server",
            "create_config",
            "process_request",
            "validate_request",
            "outer",
            "MAX_RETRIES",
            "DEFAULT_TIMEOUT",
            "FINAL_STATUS",
            "GLOBAL_COUNTER",
            "StatusCode",
            "HandlerResult",
            "utils",
        ] {
            check_symbols_contain(&mut t, &resp, name, &format!("docSymbol@{name}"));
        }

        // Unicode symbols
        check_symbols_contain(&mut t, &resp, "données_utilisateur", "docSymbol@unicode_fn");
        check_symbols_contain(&mut t, &resp, "Über", "docSymbol@unicode_struct");

        // Impl methods
        check_symbols_contain(&mut t, &resp, "new", "docSymbol@Server_new");
        check_symbols_contain(&mut t, &resp, "add_handler", "docSymbol@Server_add_handler");
        check_symbols_contain(&mut t, &resp, "run", "docSymbol@Server_run");

        // Total count sanity
        check_symbols_count_ge(&mut t, &resp, 20, "docSymbol@count");

        // Should not be polluted with local variables
        check_symbols_exclude_locals(&mut t, &resp, "docSymbol@sample_rust.rs_no_locals");
    }

    // ═════════════════════════════════════════════════════════════
    //  7. WORKSPACE SYMBOLS (7 tests from manual report Phase 6)
    // ═════════════════════════════════════════════════════════════

    // ── 7.1 "Config" query ──────────────────────────────────────
    {
        let resp = s.workspace_symbols("Config");
        check_symbols_contain(&mut t, &resp, "Config", "wsSymbol@Config");
    }

    // ── 7.2 "Server" query ──────────────────────────────────────
    {
        let resp = s.workspace_symbols("Server");
        check_symbols_contain(&mut t, &resp, "Server", "wsSymbol@Server");
    }

    // ── 7.3 "Status" query ──────────────────────────────────────
    {
        let resp = s.workspace_symbols("Status");
        check_symbols_contain(&mut t, &resp, "Status", "wsSymbol@Status");
    }

    // ── 7.4 "Handler" query ─────────────────────────────────────
    {
        let resp = s.workspace_symbols("Handler");
        check_symbols_contain(&mut t, &resp, "Handler", "wsSymbol@Handler");
    }

    // ── 7.5 "process_request" query ─────────────────────────────
    {
        let resp = s.workspace_symbols("process_request");
        check_symbols_contain(&mut t, &resp, "process_request", "wsSymbol@process_request");
    }

    // ── 7.6 "validate_request" query ──────────────────────────────
    {
        let resp = s.workspace_symbols("validate_request");
        check_symbols_contain(&mut t, &resp, "validate_request", "wsSymbol@validate_request");
    }

    // ── 7.7 "StatusCode" query ──────────────────────────────────
    {
        let resp = s.workspace_symbols("StatusCode");
        check_symbols_contain(&mut t, &resp, "StatusCode", "wsSymbol@StatusCode");
    }

    // ═════════════════════════════════════════════════════════════
    //  8. SIGNATURE HELP (4 tests from manual report Phase 7)
    // ═════════════════════════════════════════════════════════════

    // ── 8.1 inner() call site ───────────────────────────────────
    {
        let (l, c) = mark(&src, "CALL_inner", "inner");
        let c_inside = c + "inner(".len() as u32;
        check_sighelp_found(
            &mut t,
            &s.signature_help(&uri, l, c_inside),
            "sighelp@inner",
        );
    }

    // ── 8.2 create_config() call site ───────────────────────────
    {
        let (l, c) = mark(&src, "CALL_create_config", "create_config");
        let c_inside = c + "create_config(".len() as u32;
        check_sighelp_found(
            &mut t,
            &s.signature_help(&uri, l, c_inside),
            "sighelp@create_config",
        );
    }

    // ── 8.3 process_request() call site ─────────────────────────
    {
        let (l, c) = mark(&src, "CALL_process_request", "process_request");
        let c_inside = c + "process_request(".len() as u32;
        check_sighelp_found(
            &mut t,
            &s.signature_help(&uri, l, c_inside),
            "sighelp@process_request",
        );
    }

    // ── 8.4 validate_request() call site ────────────────────────
    {
        let (l, c) = mark(&src, "CALL_validate_request", "validate_request");
        let c_inside = c + "validate_request(".len() as u32;
        check_sighelp_found(
            &mut t,
            &s.signature_help(&uri, l, c_inside),
            "sighelp@validate_request",
        );
    }

    // ═════════════════════════════════════════════════════════════
    //  9. HOVER — doc comments verify rich content
    // ═════════════════════════════════════════════════════════════

    // ── 9.1 Config hover includes doc comment about "connection parameters"
    {
        let (l, c) = mark(&src, "Config_DEF", "Config");
        check_hover_contains(
            &mut t,
            &s.hover(&uri, l, c),
            "connection parameters",
            "hover@Config_doc_content",
        );
    }

    // ── 9.2 Handler hover includes doc about "Implementors"
    {
        let (l, c) = mark(&src, "Handler_DEF", "Handler");
        check_hover_contains(
            &mut t,
            &s.hover(&uri, l, c),
            "Implementors",
            "hover@Handler_doc_content",
        );
    }

    // ── 9.3 process_request hover includes doc about "Routes"
    {
        let (l, c) = mark(&src, "process_request_DEF", "process_request");
        check_hover_contains(
            &mut t,
            &s.hover(&uri, l, c),
            "Routes",
            "hover@process_request_doc_content",
        );
    }

    // ── Cleanup ──────────────────────────────────────────────────
    s.shutdown();
    t.finish();
}
