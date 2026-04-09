//! C integration test — full LSP round-trip through the server binary.
//!
//! Spawns quicklsp, opens three C fixture files (types.h, server.h, main.c),
//! and sends hover, go-to-definition, find-references, and signature-help
//! requests at dozens of cursor positions covering every major C syntax
//! element.
//!
//! Every check runs **twice**: once against the original (vanilla) files
//! and once after a `textDocument/didChange` re-index, to ensure features
//! work both on initial scan and after buffer updates.
//!
//! Cursor positions are anchored to `@mark TAG` comments in the fixture
//! files — each test references a specific marker, then offsets to the
//! token of interest on that line. This makes positions self-documenting
//! and resilient to edits elsewhere in the file.
//!
//!   cargo test -p quicklsp --test c_integration -- --nocapture

mod common;

use common::*;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn project_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("c_project")
}

/// Run all LSP feature checks against the currently-open buffers.
/// `phase` is "vanilla" or "didChange" — used as a prefix in check labels.
fn run_all_checks(
    s: &mut LspServer,
    t: &mut TestResults,
    tu: &str,
    su: &str,
    mu: &str,
    types_h: &str,
    server_h: &str,
    main_c: &str,
    phase: &str,
) {
    // ── 1. Document Symbols ──────────────────────────────────────
    {
        let resp = s.document_symbols(mu);
        let syms = resp["result"].as_array().expect("symbols array");
        let names: Vec<&str> = syms.iter().filter_map(|s| s["name"].as_str()).collect();
        for expect in &[
            "handle_request",
            "process_connections",
            "run_loop",
            "server_run",
            "server_log",
            "buffer_init",
            "buffer_append",
            "address_parse",
            "connection_init",
            "request_init",
            "response_init",
            "main",
            "method_to_string",
            "hash_string",
            "process_batch",
        ] {
            t.check(
                names.contains(expect),
                format!("{phase}:docSymbol missing '{expect}'"),
            );
        }
    }

    // ── 2. Hover: struct definition ──────────────────────────────
    {
        let (l, c) = mark(types_h, "Address_DEF", "Address");
        check_hover_contains(
            t,
            &s.hover(tu, l, c),
            "Address",
            &format!("{phase}:hover@Address_DEF"),
        );
    }

    // ── 3. Hover: enum value ─────────────────────────────────────
    {
        let (l, c) = mark(types_h, "LOG_ERROR_DEF", "LOG_ERROR");
        check_hover_contains(
            t,
            &s.hover(tu, l, c),
            "LOG_ERROR",
            &format!("{phase}:hover@LOG_ERROR_DEF"),
        );
    }

    // ── 4. Hover: typedef name ───────────────────────────────────
    {
        let (l, c) = mark(types_h, "Buffer_DEF", "Buffer");
        check_hover_contains(
            t,
            &s.hover(tu, l, c),
            "Buffer",
            &format!("{phase}:hover@Buffer_DEF"),
        );
    }

    // ── 5. Hover: #define macro ──────────────────────────────────
    {
        let (l, c) = mark(types_h, "MAX_CONNECTIONS_DEF", "MAX_CONNECTIONS");
        check_hover_contains(
            t,
            &s.hover(tu, l, c),
            "MAX_CONNECTIONS",
            &format!("{phase}:hover@MAX_CONNECTIONS_DEF"),
        );
    }

    // ── 6. Hover: function definition ────────────────────────────
    {
        let (l, c) = mark(main_c, "method_to_string_DEF", "method_to_string");
        check_hover_contains(
            t,
            &s.hover(mu, l, c),
            "method_to_string",
            &format!("{phase}:hover@method_to_string_DEF"),
        );
    }

    // ── 7. Hover: static inline function ─────────────────────────
    {
        let (l, c) = mark(server_h, "validate_port_DEF", "validate_port");
        check_hover_contains(
            t,
            &s.hover(su, l, c),
            "validate_port",
            &format!("{phase}:hover@validate_port_DEF"),
        );
    }

    // ── 8. Hover: function pointer typedef ───────────────────────
    {
        let (l, c) = mark(types_h, "RequestHandler_DEF", "RequestHandler");
        check_hover_contains(
            t,
            &s.hover(tu, l, c),
            "RequestHandler",
            &format!("{phase}:hover@RequestHandler_DEF"),
        );
    }

    // ── 9. Hover: function call site ─────────────────────────────
    {
        let (l, c) = mark(main_c, "CALL_buffer_init_in_request", "buffer_init");
        check_hover_contains(
            t,
            &s.hover(mu, l, c),
            "buffer_init",
            &format!("{phase}:hover@CALL_buffer_init"),
        );
    }

    // ── 10. Hover: struct field access (arrow) — must not error
    {
        let (l, c) = mark(main_c, "ACCESS_bytes_sent", "bytes_sent");
        check_hover_no_error(
            t,
            &s.hover(mu, l, c),
            &format!("{phase}:hover@ACCESS_bytes_sent"),
        );
    }

    // ── 11. Hover: local variable — must not error
    {
        let (l, c) = mark(main_c, "backoff_ms_local_var", "backoff_ms");
        check_hover_no_error(
            t,
            &s.hover(mu, l, c),
            &format!("{phase}:hover@backoff_ms_local_var"),
        );
    }

    // ── 12. Hover: enum value in switch/case ─────────────────────
    {
        let (l, c) = mark(main_c, "USE_HTTP_GET_switch", "HTTP_GET");
        check_hover_contains(
            t,
            &s.hover(mu, l, c),
            "HTTP_GET",
            &format!("{phase}:hover@USE_HTTP_GET_switch"),
        );
    }

    // ── 13. Hover: function pointer parameter type ───────────────
    {
        let (l, c) = mark(main_c, "USE_RequestHandler_param", "RequestHandler");
        check_hover_contains(
            t,
            &s.hover(mu, l, c),
            "RequestHandler",
            &format!("{phase}:hover@USE_RequestHandler_param"),
        );
    }

    // ── 14. Hover: VERSION_STRING macro usage ────────────────────
    {
        let (l, c) = mark(main_c, "USE_VERSION_STRING_in_main", "VERSION_STRING");
        check_hover_contains(
            t,
            &s.hover(mu, l, c),
            "VERSION_STRING",
            &format!("{phase}:hover@USE_VERSION_STRING_in_main"),
        );
    }

    // ── 15. Goto-def: Buffer typedef from main.c ─────────────────
    {
        let (l, c) = mark(main_c, "buffer_init_IMPL", "Buffer");
        check_definition_found(
            t,
            &s.goto_definition(mu, l, c),
            &format!("{phase}:def@Buffer_from_buffer_init"),
        );
    }

    // ── 16. Goto-def: MAX_CONNECTIONS macro ──────────────────────
    {
        let (l, c) = mark(main_c, "USE_MAX_CONNECTIONS", "MAX_CONNECTIONS");
        check_definition_found(
            t,
            &s.goto_definition(mu, l, c),
            &format!("{phase}:def@MAX_CONNECTIONS"),
        );
    }

    // ── 17. Goto-def: struct ServerConfig from main.c ────────────
    {
        let (l, c) = mark(main_c, "USE_ServerConfig", "ServerConfig");
        check_definition_found(
            t,
            &s.goto_definition(mu, l, c),
            &format!("{phase}:def@ServerConfig"),
        );
    }

    // ── 18. Goto-def: handle_request passed as function pointer ──
    {
        let (l, c) = mark(main_c, "PASS_handle_request_as_fnptr", "handle_request");
        check_definition_found(
            t,
            &s.goto_definition(mu, l, c),
            &format!("{phase}:def@handle_request_fnptr"),
        );
    }

    // ── 19. Goto-def: LOG_INFO enum value ────────────────────────
    {
        let (l, c) = mark(main_c, "CALL_server_log_info", "LOG_INFO");
        check_definition_found(
            t,
            &s.goto_definition(mu, l, c),
            &format!("{phase}:def@LOG_INFO"),
        );
    }

    // ── 20. Goto-def: Connection typedef ─────────────────────────
    {
        let (l, c) = mark(main_c, "connection_init_IMPL", "Connection");
        check_definition_found(
            t,
            &s.goto_definition(mu, l, c),
            &format!("{phase}:def@Connection"),
        );
    }

    // ── 21. Goto-def: CONN_ESTABLISHED enum ──────────────────────
    {
        let (l, c) = mark(main_c, "USE_CONN_ESTABLISHED_in_if", "CONN_ESTABLISHED");
        check_definition_found(
            t,
            &s.goto_definition(mu, l, c),
            &format!("{phase}:def@CONN_ESTABLISHED"),
        );
    }

    // ── 22. Goto-def: address_format function ────────────────────
    {
        let (l, c) = mark(main_c, "CALL_address_format", "address_format");
        check_definition_found(
            t,
            &s.goto_definition(mu, l, c),
            &format!("{phase}:def@address_format"),
        );
    }

    // ── 23. Goto-def: validate_port (static inline in server.h) ──
    {
        let (l, c) = mark(main_c, "CALL_validate_port", "validate_port");
        check_definition_found(
            t,
            &s.goto_definition(mu, l, c),
            &format!("{phase}:def@validate_port"),
        );
    }

    // ── 24. Goto-def: HTTP_OK macro ──────────────────────────────
    {
        let (l, c) = mark(main_c, "USE_HTTP_OK", "HTTP_OK");
        check_definition_found(
            t,
            &s.goto_definition(mu, l, c),
            &format!("{phase}:def@HTTP_OK"),
        );
    }

    // ── 25. Goto-def: MIN macro ──────────────────────────────────
    {
        let (l, c) = mark(main_c, "USE_MIN_macro", "MIN");
        check_definition_found(
            t,
            &s.goto_definition(mu, l, c),
            &format!("{phase}:def@MIN_macro"),
        );
    }

    // ── 26. Goto-def: MAX macro ──────────────────────────────────
    {
        let (l, c) = mark(main_c, "USE_MAX_macro", "MAX");
        check_definition_found(
            t,
            &s.goto_definition(mu, l, c),
            &format!("{phase}:def@MAX_macro"),
        );
    }

    // ── 27. Goto-def: StatusCode typedef ─────────────────────────
    {
        let (l, c) = mark(main_c, "USE_StatusCode", "StatusCode");
        check_definition_found(
            t,
            &s.goto_definition(mu, l, c),
            &format!("{phase}:def@StatusCode"),
        );
    }

    // ── 28. Goto-def: RequestHandler from Server struct field ────
    {
        let (l, c) = mark(server_h, "Server_handler_field", "RequestHandler");
        check_definition_found(
            t,
            &s.goto_definition(su, l, c),
            &format!("{phase}:def@RequestHandler_from_Server"),
        );
    }

    // ── 29. Find-refs: buffer_init ───────────────────────────────
    {
        let (l, c) = mark(main_c, "buffer_init_IMPL", "buffer_init");
        check_references_ge(
            t,
            &s.find_references(mu, l, c),
            3,
            &format!("{phase}:refs@buffer_init"),
        );
    }

    // ── 30. Find-refs: Connection ────────────────────────────────
    {
        let (l, c) = mark(types_h, "Connection_DEF", "Connection");
        check_references_ge(
            t,
            &s.find_references(tu, l, c),
            4,
            &format!("{phase}:refs@Connection"),
        );
    }

    // ── 31. Find-refs: MAX_HEADERS ───────────────────────────────
    {
        let (l, c) = mark(types_h, "MAX_HEADERS_DEF", "MAX_HEADERS");
        check_references_ge(
            t,
            &s.find_references(tu, l, c),
            2,
            &format!("{phase}:refs@MAX_HEADERS"),
        );
    }

    // ── 32. Find-refs: server_log ────────────────────────────────
    {
        let (l, c) = mark(main_c, "server_log_IMPL", "server_log");
        check_references_ge(
            t,
            &s.find_references(mu, l, c),
            5,
            &format!("{phase}:refs@server_log"),
        );
    }

    // ── 33. Find-refs: CONN_ESTABLISHED ──────────────────────────
    {
        let (l, c) = mark(types_h, "CONN_ESTABLISHED_DEF", "CONN_ESTABLISHED");
        check_references_ge(
            t,
            &s.find_references(tu, l, c),
            2,
            &format!("{phase}:refs@CONN_ESTABLISHED"),
        );
    }

    // ── 34. Find-refs: Request ───────────────────────────────────
    {
        let (l, c) = mark(types_h, "Request_DEF", "Request");
        check_references_ge(
            t,
            &s.find_references(tu, l, c),
            5,
            &format!("{phase}:refs@Request"),
        );
    }

    // ── 35. Find-refs: handle_request ────────────────────────────
    {
        let (l, c) = mark(main_c, "handle_request_DEF", "handle_request");
        check_references_ge(
            t,
            &s.find_references(mu, l, c),
            2,
            &format!("{phase}:refs@handle_request"),
        );
    }

    // ── 36. Find-refs: HTTP_OK ───────────────────────────────────
    {
        let (l, c) = mark(types_h, "HTTP_OK_DEF", "HTTP_OK");
        check_references_ge(
            t,
            &s.find_references(tu, l, c),
            3,
            &format!("{phase}:refs@HTTP_OK"),
        );
    }

    // ── 37. Signature-help: buffer_append() ──────────────────────
    {
        let (l, c) = mark(main_c, "CALL_buffer_append_in_set_body", "buffer_append");
        let c_inside = c + "buffer_append(".len() as u32;
        check_sighelp_found(
            t,
            &s.signature_help(mu, l, c_inside),
            &format!("{phase}:sighelp@buffer_append"),
        );
    }

    // ── 38. Signature-help: address_format() ─────────────────────
    {
        let (l, c) = mark(main_c, "CALL_address_format_in_main", "address_format");
        let c_inside = c + "address_format(".len() as u32;
        check_sighelp_found(
            t,
            &s.signature_help(mu, l, c_inside),
            &format!("{phase}:sighelp@address_format"),
        );
    }

    // ── 39. Signature-help: server_log() ─────────────────────────
    {
        let (l, c) = mark(main_c, "CALL_server_log_error", "server_log");
        let c_inside = c + "server_log(".len() as u32;
        check_sighelp_found(
            t,
            &s.signature_help(mu, l, c_inside),
            &format!("{phase}:sighelp@server_log"),
        );
    }

    // ── 40. Signature-help: server_create() ──────────────────────
    {
        let (l, c) = mark(main_c, "CALL_server_create", "server_create");
        let c_inside = c + "server_create(".len() as u32;
        check_sighelp_found(
            t,
            &s.signature_help(mu, l, c_inside),
            &format!("{phase}:sighelp@server_create"),
        );
    }

    // ── 41. Signature-help: connection_init() ────────────────────
    {
        let (l, c) = mark(main_c, "CALL_connection_init", "connection_init");
        let c_inside = c + "connection_init(".len() as u32;
        check_sighelp_found(
            t,
            &s.signature_help(mu, l, c_inside),
            &format!("{phase}:sighelp@connection_init"),
        );
    }

    // ── 42. Signature-help: address_parse() ──────────────────────
    {
        let (l, c) = mark(main_c, "CALL_address_parse", "address_parse");
        let c_inside = c + "address_parse(".len() as u32;
        check_sighelp_found(
            t,
            &s.signature_help(mu, l, c_inside),
            &format!("{phase}:sighelp@address_parse"),
        );
    }

    // ═════════════════════════════════════════════════════════════
    //  BUG-REPRODUCING TESTS (found via manual nvim testing)
    // ═════════════════════════════════════════════════════════════

    // ── 43. Find-refs for buffer_init includes main.c self-refs
    {
        let (l, c) = mark(main_c, "buffer_init_IMPL", "buffer_init");
        check_references_include_file(
            t,
            &s.find_references(mu, l, c),
            "main.c",
            &format!("{phase}:refs@buffer_init_self_file"),
        );
    }

    // ── 44. Find-refs for Buffer includes main.c usages
    {
        let (l, c) = mark(types_h, "Buffer_DEF", "Buffer");
        check_references_include_file(
            t,
            &s.find_references(tu, l, c),
            "main.c",
            &format!("{phase}:refs@Buffer_cross_file"),
        );
    }

    // ── 45. Find-refs for LogLevel includes main.c usages
    {
        let (l, c) = mark(types_h, "LogLevel_DEF", "LogLevel");
        check_references_include_file(
            t,
            &s.find_references(tu, l, c),
            "main.c",
            &format!("{phase}:refs@LogLevel_cross_file"),
        );
    }

    // ── 46. Find-refs for Connection includes main.c usages
    {
        let (l, c) = mark(types_h, "Connection_DEF", "Connection");
        check_references_include_file(
            t,
            &s.find_references(tu, l, c),
            "main.c",
            &format!("{phase}:refs@Connection_cross_file"),
        );
    }

    // ── 47. Find-refs for CONN_ESTABLISHED includes main.c usages
    {
        let (l, c) = mark(types_h, "CONN_ESTABLISHED_DEF", "CONN_ESTABLISHED");
        check_references_include_file(
            t,
            &s.find_references(tu, l, c),
            "main.c",
            &format!("{phase}:refs@CONN_ESTABLISHED_cross_file"),
        );
    }

    // ── 48. Find-refs for server_log includes server.h declaration
    {
        let (l, c) = mark(main_c, "server_log_IMPL", "server_log");
        check_references_include_file(
            t,
            &s.find_references(mu, l, c),
            "server.h",
            &format!("{phase}:refs@server_log_cross_file"),
        );
    }

    // ── 49. Find-refs for MAX_HEADERS includes main.c usages
    {
        let (l, c) = mark(types_h, "MAX_HEADERS_DEF", "MAX_HEADERS");
        check_references_include_file(
            t,
            &s.find_references(tu, l, c),
            "main.c",
            &format!("{phase}:refs@MAX_HEADERS_cross_file"),
        );
    }

    // ── 50. Hover on Buffer typedef — clean, no closing brace
    {
        let (l, c) = mark(types_h, "Buffer_DEF", "Buffer");
        check_hover_not_contains(
            t,
            &s.hover(tu, l, c),
            "} ",
            &format!("{phase}:hover@Buffer_DEF_clean"),
        );
    }

    // ── 51. Hover on LogLevel typedef — clean
    {
        let (l, c) = mark(types_h, "LogLevel_DEF", "LogLevel");
        check_hover_not_contains(
            t,
            &s.hover(tu, l, c),
            "} ",
            &format!("{phase}:hover@LogLevel_DEF_clean"),
        );
    }

    // ── 52. Hover on Connection typedef — clean
    {
        let (l, c) = mark(types_h, "Connection_DEF", "Connection");
        check_hover_not_contains(
            t,
            &s.hover(tu, l, c),
            "} ",
            &format!("{phase}:hover@Connection_DEF_clean"),
        );
    }

    // ── 53. Document symbols exclude local variables
    {
        let resp = s.document_symbols(mu);
        check_symbols_exclude_locals(t, &resp, &format!("{phase}:docSymbols@main.c_no_locals"));
    }

    // ── 54. Goto-def buffer_init from call site → main.c
    {
        let (l, c) = mark(main_c, "CALL_buffer_init_in_request", "buffer_init");
        check_definition_target(
            t,
            &s.goto_definition(mu, l, c),
            "main.c",
            &format!("{phase}:def@buffer_init_call_target"),
        );
    }

    // ── 55. Find-refs for HTTP_OK includes main.c usages
    {
        let (l, c) = mark(types_h, "HTTP_OK_DEF", "HTTP_OK");
        check_references_include_file(
            t,
            &s.find_references(tu, l, c),
            "main.c",
            &format!("{phase}:refs@HTTP_OK_cross_file"),
        );
    }
}

// ═══════════════════════════════════════════════════════════════════
//                          THE TEST
// ═══════════════════════════════════════════════════════════════════

#[test]
fn test_c_project_full_lsp() {
    // ── Setup ────────────────────────────────────────────────────
    let dir = project_dir();
    let mut s = LspServer::spawn();
    s.initialize(&dir);
    drain_until_progress_end(&mut s);

    let types_h = std::fs::read_to_string(dir.join("types.h")).unwrap();
    let server_h = std::fs::read_to_string(dir.join("server.h")).unwrap();
    let main_c = std::fs::read_to_string(dir.join("main.c")).unwrap();

    let tu = format!("file://{}", dir.join("types.h").display());
    let su = format!("file://{}", dir.join("server.h").display());
    let mu = format!("file://{}", dir.join("main.c").display());

    s.did_open(&tu, "c", &types_h);
    s.did_open(&su, "c", &server_h);
    s.did_open(&mu, "c", &main_c);

    std::thread::sleep(Duration::from_millis(200));

    let mut t = TestResults::new();

    // ── Phase 1: vanilla (initial file open) ────────────────────
    eprintln!("\n=== Phase 1: vanilla ===");
    run_all_checks(
        &mut s, &mut t, &tu, &su, &mu, &types_h, &server_h, &main_c, "vanilla",
    );

    // ── Phase 2: after didChange re-index ───────────────────────
    //
    // Send trivial modifications to all three files to trigger
    // re-indexing, then run every check again.
    eprintln!("\n=== Phase 2: didChange ===");
    let types_h_mod = format!("{}\n/* didChange trigger */\n", types_h);
    let server_h_mod = format!("{}\n/* didChange trigger */\n", server_h);
    let main_c_mod = format!("{}\n/* didChange trigger */\n", main_c);

    s.did_change(&tu, 2, &types_h_mod);
    s.did_change(&su, 2, &server_h_mod);
    s.did_change(&mu, 2, &main_c_mod);
    std::thread::sleep(Duration::from_millis(200));

    run_all_checks(
        &mut s,
        &mut t,
        &tu,
        &su,
        &mu,
        &types_h_mod,
        &server_h_mod,
        &main_c_mod,
        "didChange",
    );

    // ── Cleanup ──────────────────────────────────────────────────
    s.shutdown();
    t.finish();
}
