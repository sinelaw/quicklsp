//! C integration test — full LSP round-trip through the server binary.
//!
//! Spawns quicklsp, opens three C fixture files (types.h, server.h, main.c),
//! and sends hover, go-to-definition, find-references, and signature-help
//! requests at dozens of cursor positions covering every major C syntax
//! element.
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

    // Give the server a moment to process didOpen notifications
    std::thread::sleep(Duration::from_millis(200));

    let mut t = TestResults::new();

    // ── 1. Document Symbols ──────────────────────────────────────
    {
        let resp = s.document_symbols(&mu);
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
                format!("docSymbol missing '{expect}'"),
            );
        }
    }

    // ── 2. Hover: struct definition ──────────────────────────────
    {
        let (l, c) = mark(&types_h, "Address_DEF", "Address");
        check_hover_contains(&mut t, &s.hover(&tu, l, c), "Address", "hover@Address_DEF");
    }

    // ── 3. Hover: enum value ─────────────────────────────────────
    {
        let (l, c) = mark(&types_h, "LOG_ERROR_DEF", "LOG_ERROR");
        check_hover_contains(
            &mut t,
            &s.hover(&tu, l, c),
            "LOG_ERROR",
            "hover@LOG_ERROR_DEF",
        );
    }

    // ── 4. Hover: typedef name ───────────────────────────────────
    {
        let (l, c) = mark(&types_h, "Buffer_DEF", "Buffer");
        check_hover_contains(&mut t, &s.hover(&tu, l, c), "Buffer", "hover@Buffer_DEF");
    }

    // ── 5. Hover: #define macro ──────────────────────────────────
    {
        let (l, c) = mark(&types_h, "MAX_CONNECTIONS_DEF", "MAX_CONNECTIONS");
        check_hover_contains(
            &mut t,
            &s.hover(&tu, l, c),
            "MAX_CONNECTIONS",
            "hover@MAX_CONNECTIONS_DEF",
        );
    }

    // ── 6. Hover: function definition ────────────────────────────
    {
        let (l, c) = mark(&main_c, "method_to_string_DEF", "method_to_string");
        check_hover_contains(
            &mut t,
            &s.hover(&mu, l, c),
            "method_to_string",
            "hover@method_to_string_DEF",
        );
    }

    // ── 7. Hover: static inline function ─────────────────────────
    {
        let (l, c) = mark(&server_h, "validate_port_DEF", "validate_port");
        check_hover_contains(
            &mut t,
            &s.hover(&su, l, c),
            "validate_port",
            "hover@validate_port_DEF",
        );
    }

    // ── 8. Hover: function pointer typedef ───────────────────────
    {
        let (l, c) = mark(&types_h, "RequestHandler_DEF", "RequestHandler");
        check_hover_contains(
            &mut t,
            &s.hover(&tu, l, c),
            "RequestHandler",
            "hover@RequestHandler_DEF",
        );
    }

    // ── 9. Hover: function call site ─────────────────────────────
    {
        let (l, c) = mark(&main_c, "CALL_buffer_init_in_request", "buffer_init");
        check_hover_contains(
            &mut t,
            &s.hover(&mu, l, c),
            "buffer_init",
            "hover@CALL_buffer_init",
        );
    }

    // ── 10. Hover: struct field access (arrow) — must not error
    {
        let (l, c) = mark(&main_c, "ACCESS_bytes_sent", "bytes_sent");
        check_hover_no_error(&mut t, &s.hover(&mu, l, c), "hover@ACCESS_bytes_sent");
    }

    // ── 11. Hover: local variable — must not error
    {
        let (l, c) = mark(&main_c, "backoff_ms_local_var", "backoff_ms");
        check_hover_no_error(&mut t, &s.hover(&mu, l, c), "hover@backoff_ms_local_var");
    }

    // ── 12. Hover: enum value in switch/case ─────────────────────
    {
        let (l, c) = mark(&main_c, "USE_HTTP_GET_switch", "HTTP_GET");
        check_hover_contains(
            &mut t,
            &s.hover(&mu, l, c),
            "HTTP_GET",
            "hover@USE_HTTP_GET_switch",
        );
    }

    // ── 13. Hover: function pointer parameter type ───────────────
    {
        let (l, c) = mark(&main_c, "USE_RequestHandler_param", "RequestHandler");
        check_hover_contains(
            &mut t,
            &s.hover(&mu, l, c),
            "RequestHandler",
            "hover@USE_RequestHandler_param",
        );
    }

    // ── 14. Hover: VERSION_STRING macro usage ────────────────────
    {
        let (l, c) = mark(&main_c, "USE_VERSION_STRING_in_main", "VERSION_STRING");
        check_hover_contains(
            &mut t,
            &s.hover(&mu, l, c),
            "VERSION_STRING",
            "hover@USE_VERSION_STRING_in_main",
        );
    }

    // ── 15. Goto-def: Buffer typedef from main.c ─────────────────
    {
        let (l, c) = mark(&main_c, "buffer_init_IMPL", "Buffer");
        check_definition_found(
            &mut t,
            &s.goto_definition(&mu, l, c),
            "def@Buffer_from_buffer_init",
        );
    }

    // ── 16. Goto-def: MAX_CONNECTIONS macro ──────────────────────
    {
        let (l, c) = mark(&main_c, "USE_MAX_CONNECTIONS", "MAX_CONNECTIONS");
        check_definition_found(&mut t, &s.goto_definition(&mu, l, c), "def@MAX_CONNECTIONS");
    }

    // ── 17. Goto-def: struct ServerConfig from main.c ────────────
    {
        let (l, c) = mark(&main_c, "USE_ServerConfig", "ServerConfig");
        check_definition_found(&mut t, &s.goto_definition(&mu, l, c), "def@ServerConfig");
    }

    // ── 18. Goto-def: handle_request passed as function pointer ──
    {
        let (l, c) = mark(&main_c, "PASS_handle_request_as_fnptr", "handle_request");
        check_definition_found(
            &mut t,
            &s.goto_definition(&mu, l, c),
            "def@handle_request_fnptr",
        );
    }

    // ── 19. Goto-def: LOG_INFO enum value ────────────────────────
    {
        let (l, c) = mark(&main_c, "CALL_server_log_info", "LOG_INFO");
        check_definition_found(&mut t, &s.goto_definition(&mu, l, c), "def@LOG_INFO");
    }

    // ── 20. Goto-def: Connection typedef ─────────────────────────
    {
        let (l, c) = mark(&main_c, "connection_init_IMPL", "Connection");
        check_definition_found(&mut t, &s.goto_definition(&mu, l, c), "def@Connection");
    }

    // ── 21. Goto-def: CONN_ESTABLISHED enum ──────────────────────
    {
        let (l, c) = mark(&main_c, "USE_CONN_ESTABLISHED_in_if", "CONN_ESTABLISHED");
        check_definition_found(
            &mut t,
            &s.goto_definition(&mu, l, c),
            "def@CONN_ESTABLISHED",
        );
    }

    // ── 22. Goto-def: address_format function ────────────────────
    {
        let (l, c) = mark(&main_c, "CALL_address_format", "address_format");
        check_definition_found(&mut t, &s.goto_definition(&mu, l, c), "def@address_format");
    }

    // ── 23. Goto-def: validate_port (static inline in server.h) ──
    {
        let (l, c) = mark(&main_c, "CALL_validate_port", "validate_port");
        check_definition_found(&mut t, &s.goto_definition(&mu, l, c), "def@validate_port");
    }

    // ── 24. Goto-def: HTTP_OK macro ──────────────────────────────
    {
        let (l, c) = mark(&main_c, "USE_HTTP_OK", "HTTP_OK");
        check_definition_found(&mut t, &s.goto_definition(&mu, l, c), "def@HTTP_OK");
    }

    // ── 25. Goto-def: MIN macro ──────────────────────────────────
    {
        let (l, c) = mark(&main_c, "USE_MIN_macro", "MIN");
        check_definition_found(&mut t, &s.goto_definition(&mu, l, c), "def@MIN_macro");
    }

    // ── 26. Goto-def: MAX macro ──────────────────────────────────
    {
        let (l, c) = mark(&main_c, "USE_MAX_macro", "MAX");
        check_definition_found(&mut t, &s.goto_definition(&mu, l, c), "def@MAX_macro");
    }

    // ── 27. Goto-def: StatusCode typedef ─────────────────────────
    {
        let (l, c) = mark(&main_c, "USE_StatusCode", "StatusCode");
        check_definition_found(&mut t, &s.goto_definition(&mu, l, c), "def@StatusCode");
    }

    // ── 28. Goto-def: RequestHandler from Server struct field ────
    {
        let (l, c) = mark(&server_h, "Server_handler_field", "RequestHandler");
        check_definition_found(
            &mut t,
            &s.goto_definition(&su, l, c),
            "def@RequestHandler_from_Server",
        );
    }

    // ── 29. Find-refs: buffer_init ───────────────────────────────
    {
        let (l, c) = mark(&main_c, "buffer_init_IMPL", "buffer_init");
        check_references_ge(&mut t, &s.find_references(&mu, l, c), 3, "refs@buffer_init");
    }

    // ── 30. Find-refs: Connection ────────────────────────────────
    {
        let (l, c) = mark(&types_h, "Connection_DEF", "Connection");
        check_references_ge(&mut t, &s.find_references(&tu, l, c), 4, "refs@Connection");
    }

    // ── 31. Find-refs: MAX_HEADERS ───────────────────────────────
    {
        let (l, c) = mark(&types_h, "MAX_HEADERS_DEF", "MAX_HEADERS");
        check_references_ge(&mut t, &s.find_references(&tu, l, c), 2, "refs@MAX_HEADERS");
    }

    // ── 32. Find-refs: server_log ────────────────────────────────
    {
        let (l, c) = mark(&main_c, "server_log_IMPL", "server_log");
        check_references_ge(&mut t, &s.find_references(&mu, l, c), 5, "refs@server_log");
    }

    // ── 33. Find-refs: CONN_ESTABLISHED ──────────────────────────
    {
        let (l, c) = mark(&types_h, "CONN_ESTABLISHED_DEF", "CONN_ESTABLISHED");
        check_references_ge(
            &mut t,
            &s.find_references(&tu, l, c),
            2,
            "refs@CONN_ESTABLISHED",
        );
    }

    // ── 34. Find-refs: Request ───────────────────────────────────
    {
        let (l, c) = mark(&types_h, "Request_DEF", "Request");
        check_references_ge(&mut t, &s.find_references(&tu, l, c), 5, "refs@Request");
    }

    // ── 35. Find-refs: handle_request ────────────────────────────
    {
        let (l, c) = mark(&main_c, "handle_request_DEF", "handle_request");
        check_references_ge(
            &mut t,
            &s.find_references(&mu, l, c),
            2,
            "refs@handle_request",
        );
    }

    // ── 36. Find-refs: HTTP_OK ───────────────────────────────────
    {
        let (l, c) = mark(&types_h, "HTTP_OK_DEF", "HTTP_OK");
        check_references_ge(&mut t, &s.find_references(&tu, l, c), 3, "refs@HTTP_OK");
    }

    // ── 37. Signature-help: buffer_append() ──────────────────────
    {
        let (l, c) = mark(&main_c, "CALL_buffer_append_in_set_body", "buffer_append");
        let c_inside = c + "buffer_append(".len() as u32;
        check_sighelp_found(
            &mut t,
            &s.signature_help(&mu, l, c_inside),
            "sighelp@buffer_append",
        );
    }

    // ── 38. Signature-help: address_format() ─────────────────────
    {
        let (l, c) = mark(&main_c, "CALL_address_format_in_main", "address_format");
        let c_inside = c + "address_format(".len() as u32;
        check_sighelp_found(
            &mut t,
            &s.signature_help(&mu, l, c_inside),
            "sighelp@address_format",
        );
    }

    // ── 39. Signature-help: server_log() ─────────────────────────
    {
        let (l, c) = mark(&main_c, "CALL_server_log_error", "server_log");
        let c_inside = c + "server_log(".len() as u32;
        check_sighelp_found(
            &mut t,
            &s.signature_help(&mu, l, c_inside),
            "sighelp@server_log",
        );
    }

    // ── 40. Signature-help: server_create() ──────────────────────
    {
        let (l, c) = mark(&main_c, "CALL_server_create", "server_create");
        let c_inside = c + "server_create(".len() as u32;
        check_sighelp_found(
            &mut t,
            &s.signature_help(&mu, l, c_inside),
            "sighelp@server_create",
        );
    }

    // ── 41. Signature-help: connection_init() ────────────────────
    {
        let (l, c) = mark(&main_c, "CALL_connection_init", "connection_init");
        let c_inside = c + "connection_init(".len() as u32;
        check_sighelp_found(
            &mut t,
            &s.signature_help(&mu, l, c_inside),
            "sighelp@connection_init",
        );
    }

    // ── 42. Signature-help: address_parse() ──────────────────────
    {
        let (l, c) = mark(&main_c, "CALL_address_parse", "address_parse");
        let c_inside = c + "address_parse(".len() as u32;
        check_sighelp_found(
            &mut t,
            &s.signature_help(&mu, l, c_inside),
            "sighelp@address_parse",
        );
    }

    // ═════════════════════════════════════════════════════════════
    //  BUG-REPRODUCING TESTS (found via manual nvim testing)
    // ═════════════════════════════════════════════════════════════

    // ── 43. BUG: Find-refs for buffer_init from main.c should
    //    include references IN main.c (calls at lines 123, 147),
    //    not just the server.h declaration.
    {
        let (l, c) = mark(&main_c, "buffer_init_IMPL", "buffer_init");
        let resp = s.find_references(&mu, l, c);
        check_references_include_file(
            &mut t,
            &resp,
            "main.c",
            "refs@buffer_init: must include main.c self-references (calls within same file)",
        );
    }

    // ── 44. BUG: Find-refs for Buffer from types.h should include
    //    main.c usages (Buffer appears in function params throughout
    //    main.c), not just types.h + server.h.
    {
        let (l, c) = mark(&types_h, "Buffer_DEF", "Buffer");
        let resp = s.find_references(&tu, l, c);
        check_references_include_file(
            &mut t,
            &resp,
            "main.c",
            "refs@Buffer: must include main.c usages (cross-file from header to .c)",
        );
    }

    // ── 45. BUG: Find-refs for LogLevel from types.h should
    //    include main.c usages (LogLevel is used at multiple
    //    locations in main.c).
    {
        let (l, c) = mark(&types_h, "LogLevel_DEF", "LogLevel");
        let resp = s.find_references(&tu, l, c);
        check_references_include_file(
            &mut t,
            &resp,
            "main.c",
            "refs@LogLevel: must include main.c usages (cross-file from header to .c)",
        );
    }

    // ── 46. BUG: Find-refs for Connection from types.h should
    //    include main.c usages (Connection is used in calloc,
    //    loop vars, etc. in main.c).
    {
        let (l, c) = mark(&types_h, "Connection_DEF", "Connection");
        let resp = s.find_references(&tu, l, c);
        check_references_include_file(
            &mut t,
            &resp,
            "main.c",
            "refs@Connection: must include main.c usages (cross-file from header to .c)",
        );
    }

    // ── 47. BUG: Find-refs for CONN_ESTABLISHED from types.h
    //    should include main.c usages (used in if-conditions,
    //    assignments). Currently returns only 1 ref (the definition).
    {
        let (l, c) = mark(&types_h, "CONN_ESTABLISHED_DEF", "CONN_ESTABLISHED");
        let resp = s.find_references(&tu, l, c);
        check_references_include_file(
            &mut t,
            &resp,
            "main.c",
            "refs@CONN_ESTABLISHED: must include main.c usages (cross-file from header to .c)",
        );
    }

    // ── 48. BUG: Find-refs for server_log from main.c should
    //    include server.h declaration. Currently only finds
    //    main.c references but misses the server.h declaration.
    {
        let (l, c) = mark(&main_c, "server_log_IMPL", "server_log");
        let resp = s.find_references(&mu, l, c);
        check_references_include_file(
            &mut t,
            &resp,
            "server.h",
            "refs@server_log: must include server.h declaration (cross-file from .c to header)",
        );
    }

    // ── 49. BUG: Find-refs for MAX_HEADERS from types.h should
    //    include main.c usages (used in request_add_header).
    {
        let (l, c) = mark(&types_h, "MAX_HEADERS_DEF", "MAX_HEADERS");
        let resp = s.find_references(&tu, l, c);
        check_references_include_file(
            &mut t,
            &resp,
            "main.c",
            "refs@MAX_HEADERS: must include main.c usages (cross-file from header to .c)",
        );
    }

    // ── 50. BUG: Hover on typedef struct should show clean type
    //    signature, not `} Buffer` (the closing brace of the
    //    struct body leaks into the hover content).
    {
        let (l, c) = mark(&types_h, "Buffer_DEF", "Buffer");
        let resp = s.hover(&tu, l, c);
        check_hover_not_contains(
            &mut t,
            &resp,
            "} ",
            "hover@Buffer_DEF: should not show closing brace '} Buffer' — display typedef cleanly",
        );
    }

    // ── 51. BUG: Hover on typedef enum should show clean type
    //    name, not `} LogLevel`.
    {
        let (l, c) = mark(&types_h, "LogLevel_DEF", "LogLevel");
        let resp = s.hover(&tu, l, c);
        check_hover_not_contains(&mut t, &resp, "} ",
            "hover@LogLevel_DEF: should not show closing brace '} LogLevel' — display typedef cleanly");
    }

    // ── 52. BUG: Hover on Connection typedef shows `} Connection`
    //    instead of a clean typedef presentation.
    {
        let (l, c) = mark(&types_h, "Connection_DEF", "Connection");
        let resp = s.hover(&tu, l, c);
        check_hover_not_contains(&mut t, &resp, "} ",
            "hover@Connection_DEF: should not show closing brace '} Connection' — display typedef cleanly");
    }

    // ── 53. BUG: Document symbols for main.c includes local
    //    variables (i, buf, h, etc.) polluting the symbol list.
    //    An LSP should return functions, types, and globals —
    //    not every local variable.
    {
        let resp = s.document_symbols(&mu);
        check_symbols_exclude_locals(
            &mut t,
            &resp,
            "docSymbols@main.c: too many local variables in symbol list",
        );
    }

    // ── 54. BUG: Goto-def on buffer_init from its call site in
    //    main.c should navigate to the implementation in main.c
    //    (or at least the declaration in server.h), not to
    //    validate_headers in server.h (wrong target).
    {
        let (l, c) = mark(&main_c, "CALL_buffer_init_in_request", "buffer_init");
        let resp = s.goto_definition(&mu, l, c);
        check_definition_target(
            &mut t,
            &resp,
            "main.c",
            "def@buffer_init_call: goto-def from call site should go to main.c implementation",
        );
    }

    // ── 55. BUG: Find-refs for HTTP_OK from types.h should
    //    include main.c usages (HTTP_OK is used in handle_request
    //    and response_init calls in main.c).
    {
        let (l, c) = mark(&types_h, "HTTP_OK_DEF", "HTTP_OK");
        let resp = s.find_references(&tu, l, c);
        check_references_include_file(
            &mut t,
            &resp,
            "main.c",
            "refs@HTTP_OK: must include main.c usages (cross-file from header to .c)",
        );
    }

    // ── Cleanup ──────────────────────────────────────────────────
    s.shutdown();
    t.finish();
}
