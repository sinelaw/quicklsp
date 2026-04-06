//! Rust integration test — full LSP round-trip through the server binary.
//!
//! Spawns quicklsp, opens the `sample_rust.rs` fixture, and sends hover,
//! go-to-definition, find-references, completion, document/workspace
//! symbols, and signature-help requests covering every major Rust syntax
//! element: structs, enums, traits, functions, impl methods, consts,
//! statics, type aliases, modules, nested functions, unicode identifiers.
//!
//! Every check runs **twice**: once against the original (vanilla) file
//! and once after a `textDocument/didChange` re-index, to ensure features
//! work both on initial scan and after buffer updates.
//!
//! Cursor positions are anchored to `@mark TAG` comments in the fixture.
//!
//!   cargo test -p quicklsp --test rust_integration -- --nocapture

mod common;

use common::*;
use std::time::Duration;

/// Run all LSP feature checks against the currently-open buffer.
/// `phase` is "vanilla" or "didChange" — used as a prefix in check labels.
fn run_all_checks(s: &mut LspServer, t: &mut TestResults, uri: &str, src: &str, phase: &str) {
    // ═════════════════════════════════════════════════════════════
    //  1. HOVER — definitions
    // ═════════════════════════════════════════════════════════════

    {
        let (l, c) = mark(src, "Config_DEF", "Config");
        check_hover_contains(t, &s.hover(uri, l, c), "Config", &format!("{phase}:hover@Config_DEF"));
    }
    {
        let (l, c) = mark(src, "MAX_RETRIES_DEF", "MAX_RETRIES");
        check_hover_contains(t, &s.hover(uri, l, c), "MAX_RETRIES", &format!("{phase}:hover@MAX_RETRIES_DEF"));
    }
    {
        let (l, c) = mark(src, "DEFAULT_TIMEOUT_DEF", "DEFAULT_TIMEOUT");
        check_hover_contains(t, &s.hover(uri, l, c), "DEFAULT_TIMEOUT", &format!("{phase}:hover@DEFAULT_TIMEOUT_DEF"));
    }
    {
        let (l, c) = mark(src, "Status_DEF", "Status");
        check_hover_contains(t, &s.hover(uri, l, c), "Status", &format!("{phase}:hover@Status_DEF"));
    }
    {
        let (l, c) = mark(src, "Handler_DEF", "Handler");
        check_hover_contains(t, &s.hover(uri, l, c), "Handler", &format!("{phase}:hover@Handler_DEF"));
    }
    {
        let (l, c) = mark(src, "Request_DEF", "Request");
        check_hover_contains(t, &s.hover(uri, l, c), "Request", &format!("{phase}:hover@Request_DEF"));
    }
    {
        let (l, c) = mark(src, "Response_DEF", "Response");
        check_hover_contains(t, &s.hover(uri, l, c), "Response", &format!("{phase}:hover@Response_DEF"));
    }
    {
        let (l, c) = mark(src, "create_config_DEF", "create_config");
        check_hover_contains(t, &s.hover(uri, l, c), "create_config", &format!("{phase}:hover@create_config_DEF"));
    }
    {
        let (l, c) = mark(src, "process_request_DEF", "process_request");
        check_hover_contains(t, &s.hover(uri, l, c), "process_request", &format!("{phase}:hover@process_request_DEF"));
    }
    {
        let (l, c) = mark(src, "Server_DEF", "Server");
        check_hover_contains(t, &s.hover(uri, l, c), "Server", &format!("{phase}:hover@Server_DEF"));
    }
    {
        let (l, c) = mark(src, "Server_new_DEF", "new");
        check_hover_contains(t, &s.hover(uri, l, c), "new", &format!("{phase}:hover@Server_new_DEF"));
    }
    {
        let (l, c) = mark(src, "Server_add_handler_DEF", "add_handler");
        check_hover_contains(t, &s.hover(uri, l, c), "add_handler", &format!("{phase}:hover@Server_add_handler_DEF"));
    }
    {
        let (l, c) = mark(src, "Server_run_DEF", "run");
        check_hover_contains(t, &s.hover(uri, l, c), "run", &format!("{phase}:hover@Server_run_DEF"));
    }
    {
        let (l, c) = mark(src, "StatusCode_DEF", "StatusCode");
        check_hover_contains(t, &s.hover(uri, l, c), "StatusCode", &format!("{phase}:hover@StatusCode_DEF"));
    }
    {
        let (l, c) = mark(src, "HandlerResult_DEF", "HandlerResult");
        check_hover_contains(t, &s.hover(uri, l, c), "HandlerResult", &format!("{phase}:hover@HandlerResult_DEF"));
    }
    {
        let (l, c) = mark(src, "validate_request_DEF", "validate_request");
        check_hover_contains(t, &s.hover(uri, l, c), "validate_request", &format!("{phase}:hover@validate_request_DEF"));
    }
    {
        let (l, c) = mark(src, "unicode_fn_DEF", "données_utilisateur");
        check_hover_contains(t, &s.hover(uri, l, c), "données_utilisateur", &format!("{phase}:hover@unicode_fn_DEF"));
    }
    {
        let (l, c) = mark(src, "unicode_struct_DEF", "Über");
        check_hover_contains(t, &s.hover(uri, l, c), "Über", &format!("{phase}:hover@unicode_struct_DEF"));
    }
    {
        let (l, c) = mark(src, "outer_DEF", "outer");
        check_hover_contains(t, &s.hover(uri, l, c), "outer", &format!("{phase}:hover@outer_DEF"));
    }
    {
        let (l, c) = mark(src, "inner_DEF", "inner");
        check_hover_contains(t, &s.hover(uri, l, c), "inner", &format!("{phase}:hover@inner_DEF"));
    }
    {
        let (l, c) = mark(src, "sanitize_input_DEF", "sanitize_input");
        check_hover_contains(t, &s.hover(uri, l, c), "sanitize_input", &format!("{phase}:hover@sanitize_input_DEF"));
    }
    {
        let (l, c) = mark(src, "validate_port_DEF", "validate_port");
        check_hover_contains(t, &s.hover(uri, l, c), "validate_port", &format!("{phase}:hover@validate_port_DEF"));
    }
    {
        let (l, c) = mark(src, "GLOBAL_COUNTER_DEF", "GLOBAL_COUNTER");
        check_hover_contains(t, &s.hover(uri, l, c), "GLOBAL_COUNTER", &format!("{phase}:hover@GLOBAL_COUNTER_DEF"));
    }
    {
        let (l, c) = mark(src, "FINAL_STATUS_DEF", "FINAL_STATUS");
        check_hover_contains(t, &s.hover(uri, l, c), "FINAL_STATUS", &format!("{phase}:hover@FINAL_STATUS_DEF"));
    }

    // ═════════════════════════════════════════════════════════════
    //  2. HOVER — usages (symbol references in expressions)
    // ═════════════════════════════════════════════════════════════

    {
        let (l, c) = mark(src, "USE_Status_Active", "Status");
        check_hover_contains(t, &s.hover(uri, l, c), "Status", &format!("{phase}:hover@USE_Status_Active"));
    }
    {
        let (l, c) = mark(src, "USE_Handler_dyn", "Handler");
        check_hover_contains(t, &s.hover(uri, l, c), "Handler", &format!("{phase}:hover@USE_Handler_dyn"));
    }
    {
        let (l, c) = mark(src, "USE_MAX_RETRIES", "MAX_RETRIES");
        check_hover_contains(t, &s.hover(uri, l, c), "MAX_RETRIES", &format!("{phase}:hover@USE_MAX_RETRIES"));
    }
    {
        let (l, c) = mark(src, "INSIDE_STRING", "inside");
        check_hover_no_error(t, &s.hover(uri, l, c), &format!("{phase}:hover@INSIDE_STRING"));
    }
    {
        let (l, c) = mark(src, "USE_println", "println");
        check_hover_no_error(t, &s.hover(uri, l, c), &format!("{phase}:hover@USE_println"));
    }
    {
        let (l, c) = mark(src, "utils_DEF", "utils");
        check_hover_contains(t, &s.hover(uri, l, c), "utils", &format!("{phase}:hover@utils_DEF"));
    }

    // Hover doc comment content
    {
        let (l, c) = mark(src, "Config_DEF", "Config");
        check_hover_contains(t, &s.hover(uri, l, c), "connection parameters", &format!("{phase}:hover@Config_doc"));
    }
    {
        let (l, c) = mark(src, "Handler_DEF", "Handler");
        check_hover_contains(t, &s.hover(uri, l, c), "Implementors", &format!("{phase}:hover@Handler_doc"));
    }
    {
        let (l, c) = mark(src, "process_request_DEF", "process_request");
        check_hover_contains(t, &s.hover(uri, l, c), "Routes", &format!("{phase}:hover@process_request_doc"));
    }

    // ═════════════════════════════════════════════════════════════
    //  3. GO TO DEFINITION
    // ═════════════════════════════════════════════════════════════

    {
        let (l, c) = mark(src, "USE_Status_Active", "Status");
        check_definition_found(t, &s.goto_definition(uri, l, c), &format!("{phase}:def@Status_from_Active"));
        check_definition_target(t, &s.goto_definition(uri, l, c), "sample_rust.rs", &format!("{phase}:def@Status_target"));
    }
    {
        let (l, c) = mark(src, "USE_MAX_RETRIES", "MAX_RETRIES");
        check_definition_found(t, &s.goto_definition(uri, l, c), &format!("{phase}:def@MAX_RETRIES_usage"));
    }
    {
        let (l, c) = mark(src, "USE_DEFAULT_TIMEOUT", "DEFAULT_TIMEOUT");
        check_definition_found(t, &s.goto_definition(uri, l, c), &format!("{phase}:def@DEFAULT_TIMEOUT_usage"));
    }
    {
        let (l, c) = mark(src, "USE_Handler_dyn", "Handler");
        check_definition_found(t, &s.goto_definition(uri, l, c), &format!("{phase}:def@Handler_dyn_bound"));
    }
    {
        let (l, c) = mark(src, "CALL_validate_request", "validate_request");
        check_definition_found(t, &s.goto_definition(uri, l, c), &format!("{phase}:def@validate_request_call"));
        check_definition_target(t, &s.goto_definition(uri, l, c), "sample_rust.rs", &format!("{phase}:def@validate_request_target"));
    }
    {
        let (l, c) = mark(src, "CALL_inner", "inner");
        check_definition_found(t, &s.goto_definition(uri, l, c), &format!("{phase}:def@inner_call"));
    }
    {
        let (l, c) = mark(src, "CALL_create_config", "create_config");
        check_definition_found(t, &s.goto_definition(uri, l, c), &format!("{phase}:def@create_config_call"));
        check_definition_target(t, &s.goto_definition(uri, l, c), "sample_rust.rs", &format!("{phase}:def@create_config_target"));
    }
    {
        let (l, c) = mark(src, "CALL_process_request", "process_request");
        check_definition_found(t, &s.goto_definition(uri, l, c), &format!("{phase}:def@process_request_call"));
        check_definition_target(t, &s.goto_definition(uri, l, c), "sample_rust.rs", &format!("{phase}:def@process_request_target"));
    }

    // ═════════════════════════════════════════════════════════════
    //  4. FIND REFERENCES
    // ═════════════════════════════════════════════════════════════

    {
        let (l, c) = mark(src, "Config_DEF", "Config");
        check_references_ge(t, &s.find_references(uri, l, c), 3, &format!("{phase}:refs@Config"));
        check_references_include_file(t, &s.find_references(uri, l, c), "sample_rust.rs", &format!("{phase}:refs@Config_self"));
    }
    {
        let (l, c) = mark(src, "Status_DEF", "Status");
        check_references_ge(t, &s.find_references(uri, l, c), 2, &format!("{phase}:refs@Status"));
    }
    {
        let (l, c) = mark(src, "Handler_DEF", "Handler");
        check_references_ge(t, &s.find_references(uri, l, c), 2, &format!("{phase}:refs@Handler"));
    }
    {
        let (l, c) = mark(src, "Request_DEF", "Request");
        check_references_ge(t, &s.find_references(uri, l, c), 3, &format!("{phase}:refs@Request"));
    }
    {
        let (l, c) = mark(src, "Response_DEF", "Response");
        check_references_ge(t, &s.find_references(uri, l, c), 3, &format!("{phase}:refs@Response"));
    }
    {
        let (l, c) = mark(src, "MAX_RETRIES_DEF", "MAX_RETRIES");
        check_references_ge(t, &s.find_references(uri, l, c), 2, &format!("{phase}:refs@MAX_RETRIES"));
    }
    {
        let (l, c) = mark(src, "Server_DEF", "Server");
        check_references_ge(t, &s.find_references(uri, l, c), 2, &format!("{phase}:refs@Server"));
    }
    {
        let (l, c) = mark(src, "process_request_DEF", "process_request");
        check_references_ge(t, &s.find_references(uri, l, c), 2, &format!("{phase}:refs@process_request"));
    }
    {
        let (l, c) = mark(src, "validate_request_DEF", "validate_request");
        check_references_ge(t, &s.find_references(uri, l, c), 2, &format!("{phase}:refs@validate_request"));
    }
    {
        let (l, c) = mark(src, "create_config_DEF", "create_config");
        check_references_ge(t, &s.find_references(uri, l, c), 2, &format!("{phase}:refs@create_config"));
    }

    // ═════════════════════════════════════════════════════════════
    //  5. COMPLETION
    // ═════════════════════════════════════════════════════════════

    {
        let (l, _) = mark(src, "process_request_DEF", "process_request");
        check_completion_contains(t, &s.completion(uri, l, 12), "process_request", &format!("{phase}:completion@process_"));
    }
    {
        let (l, c) = mark(src, "Config_DEF", "Config");
        check_completion_contains(t, &s.completion(uri, l, c + 4), "Config", &format!("{phase}:completion@Conf"));
    }
    {
        let (l, c) = mark(src, "Status_DEF", "Status");
        check_completion_contains(t, &s.completion(uri, l, c + 4), "Status", &format!("{phase}:completion@Stat"));
    }
    {
        let (l, c) = mark(src, "Handler_DEF", "Handler");
        check_completion_contains(t, &s.completion(uri, l, c + 4), "Handler", &format!("{phase}:completion@Hand"));
    }
    {
        let (l, c) = mark(src, "Server_DEF", "Server");
        check_completion_contains(t, &s.completion(uri, l, c + 4), "Server", &format!("{phase}:completion@Serv"));
    }
    {
        let (l, c) = mark(src, "Request_DEF", "Request");
        check_completion_contains(t, &s.completion(uri, l, c + 3), "Request", &format!("{phase}:completion@Req"));
    }
    {
        let (l, c) = mark(src, "Response_DEF", "Response");
        check_completion_contains(t, &s.completion(uri, l, c + 4), "Response", &format!("{phase}:completion@Resp"));
    }
    {
        let (l, c) = mark(src, "create_config_DEF", "create_config");
        check_completion_contains(t, &s.completion(uri, l, c + 6), "create_config", &format!("{phase}:completion@create"));
    }
    {
        let (l, c) = mark(src, "validate_request_DEF", "validate_request");
        check_completion_contains(t, &s.completion(uri, l, c + 5), "validate_request", &format!("{phase}:completion@valid"));
    }
    {
        let (l, c) = mark(src, "MAX_RETRIES_DEF", "MAX_RETRIES");
        check_completion_contains(t, &s.completion(uri, l, c + 3), "MAX_RETRIES", &format!("{phase}:completion@MAX"));
    }
    {
        let (l, c) = mark(src, "DEFAULT_TIMEOUT_DEF", "DEFAULT_TIMEOUT");
        check_completion_contains(t, &s.completion(uri, l, c + 7), "DEFAULT_TIMEOUT", &format!("{phase}:completion@DEFAULT"));
    }
    {
        let (l, c) = mark(src, "outer_DEF", "outer");
        check_completion_contains(t, &s.completion(uri, l, c + 3), "outer", &format!("{phase}:completion@out"));
    }
    {
        let (l, c) = mark(src, "inner_DEF", "inner");
        check_completion_contains(t, &s.completion(uri, l, c + 3), "inner", &format!("{phase}:completion@inn"));
    }
    {
        let (l, c) = mark(src, "sanitize_input_DEF", "sanitize_input");
        check_completion_contains(t, &s.completion(uri, l, c + 3), "sanitize_input", &format!("{phase}:completion@san"));
    }
    {
        let (l, c) = mark(src, "Server_DEF", "Server");
        check_completion_non_empty(t, &s.completion(uri, l, c + 2), &format!("{phase}:completion@Se_non_empty"));
    }

    // ═════════════════════════════════════════════════════════════
    //  6. DOCUMENT SYMBOLS
    // ═════════════════════════════════════════════════════════════

    {
        let resp = s.document_symbols(uri);

        for name in &[
            "Config", "Status", "Handler", "Request", "Response", "Server",
            "create_config", "process_request", "validate_request", "outer",
            "MAX_RETRIES", "DEFAULT_TIMEOUT", "FINAL_STATUS", "GLOBAL_COUNTER",
            "StatusCode", "HandlerResult", "utils",
        ] {
            check_symbols_contain(t, &resp, name, &format!("{phase}:docSymbol@{name}"));
        }

        // Unicode symbols
        check_symbols_contain(t, &resp, "données_utilisateur", &format!("{phase}:docSymbol@unicode_fn"));
        check_symbols_contain(t, &resp, "Über", &format!("{phase}:docSymbol@unicode_struct"));

        // Impl methods
        check_symbols_contain(t, &resp, "new", &format!("{phase}:docSymbol@Server_new"));
        check_symbols_contain(t, &resp, "add_handler", &format!("{phase}:docSymbol@Server_add_handler"));
        check_symbols_contain(t, &resp, "run", &format!("{phase}:docSymbol@Server_run"));

        // Total count sanity
        check_symbols_count_ge(t, &resp, 20, &format!("{phase}:docSymbol@count"));

        // Should not be polluted with local variables
        check_symbols_exclude_locals(t, &resp, &format!("{phase}:docSymbol@no_locals"));
    }

    // ═════════════════════════════════════════════════════════════
    //  7. WORKSPACE SYMBOLS
    // ═════════════════════════════════════════════════════════════

    {
        let resp = s.workspace_symbols("Config");
        check_symbols_contain(t, &resp, "Config", &format!("{phase}:wsSymbol@Config"));
    }
    {
        let resp = s.workspace_symbols("Server");
        check_symbols_contain(t, &resp, "Server", &format!("{phase}:wsSymbol@Server"));
    }
    {
        let resp = s.workspace_symbols("Status");
        check_symbols_contain(t, &resp, "Status", &format!("{phase}:wsSymbol@Status"));
    }
    {
        let resp = s.workspace_symbols("Handler");
        check_symbols_contain(t, &resp, "Handler", &format!("{phase}:wsSymbol@Handler"));
    }
    {
        let resp = s.workspace_symbols("process_request");
        check_symbols_contain(t, &resp, "process_request", &format!("{phase}:wsSymbol@process_request"));
    }
    {
        let resp = s.workspace_symbols("validate_request");
        check_symbols_contain(t, &resp, "validate_request", &format!("{phase}:wsSymbol@validate_request"));
    }
    {
        let resp = s.workspace_symbols("StatusCode");
        check_symbols_contain(t, &resp, "StatusCode", &format!("{phase}:wsSymbol@StatusCode"));
    }

    // ═════════════════════════════════════════════════════════════
    //  8. SIGNATURE HELP
    // ═════════════════════════════════════════════════════════════

    {
        let (l, c) = mark(src, "CALL_inner", "inner");
        check_sighelp_found(t, &s.signature_help(uri, l, c + "inner(".len() as u32), &format!("{phase}:sighelp@inner"));
    }
    {
        let (l, c) = mark(src, "CALL_create_config", "create_config");
        check_sighelp_found(t, &s.signature_help(uri, l, c + "create_config(".len() as u32), &format!("{phase}:sighelp@create_config"));
    }
    {
        let (l, c) = mark(src, "CALL_process_request", "process_request");
        check_sighelp_found(t, &s.signature_help(uri, l, c + "process_request(".len() as u32), &format!("{phase}:sighelp@process_request"));
    }
    {
        let (l, c) = mark(src, "CALL_validate_request", "validate_request");
        check_sighelp_found(t, &s.signature_help(uri, l, c + "validate_request(".len() as u32), &format!("{phase}:sighelp@validate_request"));
    }

    // ═════════════════════════════════════════════════════════════
    //  9. BUG-REPRODUCING TESTS
    // ═════════════════════════════════════════════════════════════

    // BUG 1: Cross-language name collision — Config should resolve
    // to Rust struct, not C definition from sample_c.c / c_project.
    {
        let (l, c) = mark(src, "USE_Config_param", "Config");
        check_hover_contains(t, &s.hover(uri, l, c), "struct Config",
            &format!("{phase}:BUG1_hover@Config_param"));
        check_definition_target(t, &s.goto_definition(uri, l, c), "sample_rust.rs",
            &format!("{phase}:BUG1_def@Config_param"));
    }
    {
        let (l, c) = mark(src, "Server_new_DEF", "Config");
        check_definition_target(t, &s.goto_definition(uri, l, c), "sample_rust.rs",
            &format!("{phase}:BUG1_def@Config_in_Server_new"));
    }

    // BUG 2: Unicode prefix completion via didChange with fresh prefix.
    // Send modified buffer with "don" typed on a new line — should
    // match "données_utilisateur".
    {
        let modified = format!("{}\nfn _bug2_test() {{\n    don\n}}\n", src);
        let bug_line = modified.lines().count() as u32 - 2;
        s.did_change(uri, 100, &modified);
        std::thread::sleep(Duration::from_millis(100));

        check_completion_contains(t, &s.completion(uri, bug_line, 7),
            "données_utilisateur", &format!("{phase}:BUG2_completion@don"));

        // Restore
        s.did_change(uri, 101, src);
        std::thread::sleep(Duration::from_millis(100));
    }
    {
        let modified = format!("{}\nfn _bug2_test2() {{\n    Üb\n}}\n", src);
        let bug_line = modified.lines().count() as u32 - 2;
        // "    Üb" — col 7 is past the end (4 spaces + Ü(2 bytes) + b(1 byte))
        s.did_change(uri, 102, &modified);
        std::thread::sleep(Duration::from_millis(100));

        check_completion_contains(t, &s.completion(uri, bug_line, 7),
            "Über", &format!("{phase}:BUG2_completion@Üb"));

        // Restore
        s.did_change(uri, 103, src);
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn test_rust_full_lsp() {
    // ── Setup ────────────────────────────────────────────────────
    let dir = fixtures_dir();
    let mut s = LspServer::spawn();
    s.initialize(&dir);
    drain_until_progress_end(&mut s);

    let (uri, src) = open_fixture(&mut s, &dir, "sample_rust.rs", "rust");
    std::thread::sleep(Duration::from_millis(200));

    let mut t = TestResults::new();

    // ── Phase 1: vanilla (initial file open) ────────────────────
    eprintln!("\n=== Phase 1: vanilla ===");
    run_all_checks(&mut s, &mut t, &uri, &src, "vanilla");

    // ── Phase 2: after didChange re-index ───────────────────────
    //
    // Send a trivial modification (append a comment) to trigger
    // a full re-index, then run every check again.
    eprintln!("\n=== Phase 2: didChange ===");
    let modified_src = format!("{}\n// didChange trigger comment\n", src);
    s.did_change(&uri, 50, &modified_src);
    std::thread::sleep(Duration::from_millis(200));
    run_all_checks(&mut s, &mut t, &uri, &modified_src, "didChange");

    // ── Cleanup ──────────────────────────────────────────────────
    s.shutdown();
    t.finish();
}
