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

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

// ── JSON-RPC helpers ──────────────────────────────────────────────

fn send_message(stdin: &mut impl Write, msg: &serde_json::Value) {
    let body = serde_json::to_string(msg).unwrap();
    write!(stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
    stdin.flush().unwrap();
}

fn read_message_blocking(reader: &mut BufReader<impl Read>) -> Option<serde_json::Value> {
    let mut header = String::new();
    if reader.read_line(&mut header).ok()? == 0 {
        return None;
    }
    let content_length: usize = header
        .trim()
        .strip_prefix("Content-Length: ")?
        .parse()
        .ok()?;
    let mut blank = String::new();
    reader.read_line(&mut blank).ok()?;
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

// ── LSP server wrapper ────────────────────────────────────────────

struct LspServer {
    child: Child,
    rx: mpsc::Receiver<serde_json::Value>,
    _reader_thread: std::thread::JoinHandle<()>,
    next_id: u64,
}

impl LspServer {
    fn spawn() -> Self {
        let binary = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("debug")
            .join("quicklsp");

        // Set RUST_LOG=quicklsp=debug and use .stderr(Stdio::inherit())
        // to see server-side tracing when debugging test failures.
        let mut child = Command::new(&binary)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap_or_else(|e| panic!("Failed to spawn {}: {e}", binary.display()));

        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();

        let reader_thread = std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Some(msg) = read_message_blocking(&mut reader) {
                if tx.send(msg).is_err() {
                    break;
                }
            }
        });

        LspServer { child, rx, _reader_thread: reader_thread, next_id: 100 }
    }

    fn stdin(&mut self) -> &mut std::process::ChildStdin {
        self.child.stdin.as_mut().unwrap()
    }

    fn send(&mut self, msg: &serde_json::Value) {
        send_message(self.stdin(), msg);
    }

    fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn recv(&self, timeout: Duration) -> Option<serde_json::Value> {
        self.rx.recv_timeout(timeout).ok()
    }

    fn wait_for_id(&self, id: u64) -> serde_json::Value {
        let start = Instant::now();
        let timeout = Duration::from_secs(5);
        while start.elapsed() < timeout {
            let remaining = timeout.saturating_sub(start.elapsed());
            match self.rx.recv_timeout(remaining) {
                Ok(msg) if msg.get("id").and_then(|v| v.as_u64()) == Some(id) => return msg,
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        panic!("No response for id {id} within 5s")
    }

    fn wait_for(
        &self,
        timeout: Duration,
        pred: impl Fn(&serde_json::Value) -> bool,
    ) -> Option<serde_json::Value> {
        let start = Instant::now();
        while start.elapsed() < timeout {
            let remaining = timeout.saturating_sub(start.elapsed());
            match self.rx.recv_timeout(remaining) {
                Ok(msg) if pred(&msg) => return Some(msg),
                Ok(_) => continue,
                Err(_) => return None,
            }
        }
        None
    }

    fn initialize(&mut self, root_dir: &Path) {
        let root_uri = format!("file://{}", root_dir.display());
        self.send(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1,
            "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "rootUri": root_uri,
                "capabilities": { "window": { "workDoneProgress": true } }
            }
        }));
        let _ = self.wait_for(Duration::from_secs(5), |msg| {
            msg.get("id").and_then(|v| v.as_u64()) == Some(1)
        }).expect("No initialize response");
        self.send(&serde_json::json!({
            "jsonrpc": "2.0", "method": "initialized", "params": {}
        }));
    }

    fn shutdown(&mut self) {
        self.send(&serde_json::json!({
            "jsonrpc": "2.0", "id": 99, "method": "shutdown", "params": null
        }));
        let _ = self.wait_for(Duration::from_secs(2), |msg| {
            msg.get("id").and_then(|v| v.as_u64()) == Some(99)
        });
        self.send(&serde_json::json!({
            "jsonrpc": "2.0", "method": "exit", "params": null
        }));
        let _ = self.child.wait();
    }

    // ── LSP requests ─────────────────────────────────────────────

    fn did_open(&mut self, uri: &str, source: &str) {
        self.send(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": uri, "languageId": "c", "version": 1, "text": source
                }
            }
        }));
    }

    fn hover(&mut self, uri: &str, line: u32, col: u32) -> serde_json::Value {
        let id = self.alloc_id();
        self.send(&serde_json::json!({
            "jsonrpc": "2.0", "id": id,
            "method": "textDocument/hover",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": col }
            }
        }));
        self.wait_for_id(id)
    }

    fn goto_definition(&mut self, uri: &str, line: u32, col: u32) -> serde_json::Value {
        let id = self.alloc_id();
        self.send(&serde_json::json!({
            "jsonrpc": "2.0", "id": id,
            "method": "textDocument/definition",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": col }
            }
        }));
        self.wait_for_id(id)
    }

    fn find_references(&mut self, uri: &str, line: u32, col: u32) -> serde_json::Value {
        let id = self.alloc_id();
        self.send(&serde_json::json!({
            "jsonrpc": "2.0", "id": id,
            "method": "textDocument/references",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": col },
                "context": { "includeDeclaration": true }
            }
        }));
        self.wait_for_id(id)
    }

    fn signature_help(&mut self, uri: &str, line: u32, col: u32) -> serde_json::Value {
        let id = self.alloc_id();
        self.send(&serde_json::json!({
            "jsonrpc": "2.0", "id": id,
            "method": "textDocument/signatureHelp",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": col }
            }
        }));
        self.wait_for_id(id)
    }

    fn document_symbols(&mut self, uri: &str) -> serde_json::Value {
        let id = self.alloc_id();
        self.send(&serde_json::json!({
            "jsonrpc": "2.0", "id": id,
            "method": "textDocument/documentSymbol",
            "params": { "textDocument": { "uri": uri } }
        }));
        self.wait_for_id(id)
    }
}

impl Drop for LspServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ── Fixture helpers ───────────────────────────────────────────────

fn project_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("c_project")
}

fn drain_until_progress_end(server: &mut LspServer) {
    let start = Instant::now();
    let timeout = Duration::from_secs(30);
    while start.elapsed() < timeout {
        let remaining = timeout.saturating_sub(start.elapsed());
        if let Some(msg) = server.recv(remaining) {
            if msg.get("method").and_then(|v| v.as_str())
                == Some("window/workDoneProgress/create")
            {
                if let Some(id) = msg.get("id") {
                    server.send(&serde_json::json!({
                        "jsonrpc": "2.0", "id": id.clone(), "result": null
                    }));
                }
            }
            if msg.get("method").and_then(|v| v.as_str()) == Some("$/progress")
                && msg["params"]["value"]["kind"].as_str() == Some("end")
            {
                return;
            }
        }
    }
    panic!("Timed out waiting for indexing to complete");
}

/// Find the (line, col) where `@mark TAG` appears, then locate `token`
/// on the same line and return that position. Panics if not found.
fn mark(source: &str, tag: &str, token: &str) -> (u32, u32) {
    let marker = format!("@mark {tag}");
    for (i, line) in source.lines().enumerate() {
        if line.contains(&marker) {
            let col = line.find(token).unwrap_or_else(|| {
                panic!("Token '{token}' not found on line with @mark {tag}:\n  {line}")
            });
            return (i as u32, col as u32);
        }
    }
    panic!("Marker '@mark {tag}' not found in source")
}

// ── Assertion helpers ─────────────────────────────────────────────

fn assert_hover_contains(resp: &serde_json::Value, substring: &str, ctx: &str) {
    assert!(resp.get("error").is_none(), "{ctx}: hover error: {resp}");
    let result = &resp["result"];
    assert!(!result.is_null(), "{ctx}: hover returned null");
    let content = result["contents"]["value"].as_str().unwrap_or("");
    assert!(
        content.contains(substring),
        "{ctx}: hover should contain '{substring}', got:\n{content}"
    );
}

fn assert_hover_not_null(resp: &serde_json::Value, ctx: &str) {
    assert!(resp.get("error").is_none(), "{ctx}: hover error: {resp}");
    assert!(!resp["result"].is_null(), "{ctx}: hover returned null");
}

fn assert_definition_found(resp: &serde_json::Value, ctx: &str) {
    assert!(resp.get("error").is_none(), "{ctx}: definition error: {resp}");
    let result = &resp["result"];
    assert!(!result.is_null(), "{ctx}: definition returned null");
    if result.is_array() {
        assert!(!result.as_array().unwrap().is_empty(), "{ctx}: definition empty array");
    }
}

fn assert_references_ge(resp: &serde_json::Value, min: usize, ctx: &str) {
    assert!(resp.get("error").is_none(), "{ctx}: references error: {resp}");
    let empty = vec![];
    let refs = resp["result"].as_array().unwrap_or(&empty);
    assert!(
        refs.len() >= min,
        "{ctx}: expected >= {min} refs, got {}",
        refs.len()
    );
}

fn assert_sighelp_found(resp: &serde_json::Value, ctx: &str) {
    assert!(resp.get("error").is_none(), "{ctx}: sighelp error: {resp}");
    let result = &resp["result"];
    assert!(!result.is_null(), "{ctx}: sighelp null");
    let sigs = result["signatures"].as_array();
    assert!(
        sigs.map_or(false, |s| !s.is_empty()),
        "{ctx}: sighelp no signatures: {resp}"
    );
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

    let types_h  = std::fs::read_to_string(dir.join("types.h")).unwrap();
    let server_h = std::fs::read_to_string(dir.join("server.h")).unwrap();
    let main_c   = std::fs::read_to_string(dir.join("main.c")).unwrap();

    let tu = format!("file://{}", dir.join("types.h").display());
    let su = format!("file://{}", dir.join("server.h").display());
    let mu = format!("file://{}", dir.join("main.c").display());

    s.did_open(&tu, &types_h);
    s.did_open(&su, &server_h);
    s.did_open(&mu, &main_c);

    // Give the server a moment to process didOpen notifications
    std::thread::sleep(Duration::from_millis(200));

    // ── 1. Document Symbols ──────────────────────────────────────
    {
        let resp = s.document_symbols(&mu);
        let syms = resp["result"].as_array().expect("symbols array");
        let names: Vec<&str> = syms.iter().filter_map(|s| s["name"].as_str()).collect();
        for expect in &[
            "handle_request", "process_connections", "run_loop",
            "server_run", "server_log", "buffer_init", "buffer_append",
            "address_parse", "connection_init", "request_init",
            "response_init", "main", "method_to_string", "hash_string",
            "process_batch",
        ] {
            assert!(names.contains(expect), "docSymbol missing '{expect}': {names:?}");
        }
    }

    // ── 2. Hover: struct definition ──────────────────────────────
    {
        let (l, c) = mark(&types_h, "Address_DEF", "Address");
        assert_hover_contains(&s.hover(&tu, l, c), "Address", "hover@Address_DEF");
    }

    // ── 3. Hover: enum value ─────────────────────────────────────
    {
        let (l, c) = mark(&types_h, "LOG_ERROR_DEF", "LOG_ERROR");
        assert_hover_contains(&s.hover(&tu, l, c), "LOG_ERROR", "hover@LOG_ERROR_DEF");
    }

    // ── 4. Hover: typedef name ───────────────────────────────────
    {
        let (l, c) = mark(&types_h, "Buffer_DEF", "Buffer");
        assert_hover_contains(&s.hover(&tu, l, c), "Buffer", "hover@Buffer_DEF");
    }

    // ── 5. Hover: #define macro ──────────────────────────────────
    {
        let (l, c) = mark(&types_h, "MAX_CONNECTIONS_DEF", "MAX_CONNECTIONS");
        assert_hover_contains(&s.hover(&tu, l, c), "MAX_CONNECTIONS", "hover@MAX_CONNECTIONS_DEF");
    }

    // ── 6. Hover: function definition ────────────────────────────
    {
        let (l, c) = mark(&main_c, "method_to_string_DEF", "method_to_string");
        assert_hover_contains(&s.hover(&mu, l, c), "method_to_string", "hover@method_to_string_DEF");
    }

    // ── 7. Hover: static inline function ─────────────────────────
    {
        let (l, c) = mark(&server_h, "validate_port_DEF", "validate_port");
        assert_hover_contains(&s.hover(&su, l, c), "validate_port", "hover@validate_port_DEF");
    }

    // ── 8. Hover: function pointer typedef ───────────────────────
    {
        let (l, c) = mark(&types_h, "RequestHandler_DEF", "RequestHandler");
        assert_hover_contains(&s.hover(&tu, l, c), "RequestHandler", "hover@RequestHandler_DEF");
    }

    // ── 9. Hover: function call site ─────────────────────────────
    {
        let (l, c) = mark(&main_c, "CALL_buffer_init_in_request", "buffer_init");
        assert_hover_contains(&s.hover(&mu, l, c), "buffer_init", "hover@CALL_buffer_init");
    }

    // ── 10. Hover: struct field access (arrow) — may return null for
    //    cross-file field access (no type inference), but must not error
    {
        let (l, c) = mark(&main_c, "ACCESS_bytes_sent", "bytes_sent");
        let resp = s.hover(&mu, l, c);
        assert!(resp.get("error").is_none(), "hover@ACCESS_bytes_sent: should not error");
    }

    // ── 11. Hover: local variable in nested scope — may return null
    //    for local variables (depth > 0), but must not error
    {
        let (l, c) = mark(&main_c, "backoff_ms_local_var", "backoff_ms");
        let resp = s.hover(&mu, l, c);
        assert!(resp.get("error").is_none(), "hover@backoff_ms_local_var: should not error");
    }

    // ── 12. Hover: enum value in switch/case ─────────────────────
    {
        let (l, c) = mark(&main_c, "USE_HTTP_GET_switch", "HTTP_GET");
        assert_hover_contains(&s.hover(&mu, l, c), "HTTP_GET", "hover@USE_HTTP_GET_switch");
    }

    // ── 13. Hover: function pointer parameter type ───────────────
    {
        let (l, c) = mark(&main_c, "USE_RequestHandler_param", "RequestHandler");
        assert_hover_contains(&s.hover(&mu, l, c), "RequestHandler", "hover@USE_RequestHandler_param");
    }

    // ── 14. Hover: VERSION_STRING macro usage ────────────────────
    {
        let (l, c) = mark(&main_c, "USE_VERSION_STRING_in_main", "VERSION_STRING");
        assert_hover_contains(&s.hover(&mu, l, c), "VERSION_STRING", "hover@USE_VERSION_STRING_in_main");
    }

    // ── 15. Goto-def: Buffer typedef from main.c ─────────────────
    {
        let (l, c) = mark(&main_c, "buffer_init_IMPL", "Buffer");
        assert_definition_found(&s.goto_definition(&mu, l, c), "def@Buffer_from_buffer_init");
    }

    // ── 16. Goto-def: MAX_CONNECTIONS macro ──────────────────────
    {
        let (l, c) = mark(&main_c, "USE_MAX_CONNECTIONS", "MAX_CONNECTIONS");
        assert_definition_found(&s.goto_definition(&mu, l, c), "def@MAX_CONNECTIONS");
    }

    // ── 17. Goto-def: struct ServerConfig from main.c ────────────
    {
        let (l, c) = mark(&main_c, "USE_ServerConfig", "ServerConfig");
        assert_definition_found(&s.goto_definition(&mu, l, c), "def@ServerConfig");
    }

    // ── 18. Goto-def: handle_request passed as function pointer ──
    {
        let (l, c) = mark(&main_c, "PASS_handle_request_as_fnptr", "handle_request");
        assert_definition_found(&s.goto_definition(&mu, l, c), "def@handle_request_fnptr");
    }

    // ── 19. Goto-def: LOG_INFO enum value ────────────────────────
    {
        let (l, c) = mark(&main_c, "CALL_server_log_info", "LOG_INFO");
        assert_definition_found(&s.goto_definition(&mu, l, c), "def@LOG_INFO");
    }

    // ── 20. Goto-def: Connection typedef ─────────────────────────
    {
        let (l, c) = mark(&main_c, "connection_init_IMPL", "Connection");
        assert_definition_found(&s.goto_definition(&mu, l, c), "def@Connection");
    }

    // ── 21. Goto-def: CONN_ESTABLISHED enum ──────────────────────
    {
        let (l, c) = mark(&main_c, "USE_CONN_ESTABLISHED_in_if", "CONN_ESTABLISHED");
        assert_definition_found(&s.goto_definition(&mu, l, c), "def@CONN_ESTABLISHED");
    }

    // ── 22. Goto-def: address_format function ────────────────────
    {
        let (l, c) = mark(&main_c, "CALL_address_format", "address_format");
        assert_definition_found(&s.goto_definition(&mu, l, c), "def@address_format");
    }

    // ── 23. Goto-def: validate_port (static inline in server.h) ──
    {
        let (l, c) = mark(&main_c, "CALL_validate_port", "validate_port");
        assert_definition_found(&s.goto_definition(&mu, l, c), "def@validate_port");
    }

    // ── 24. Goto-def: HTTP_OK macro ──────────────────────────────
    {
        let (l, c) = mark(&main_c, "USE_HTTP_OK", "HTTP_OK");
        assert_definition_found(&s.goto_definition(&mu, l, c), "def@HTTP_OK");
    }

    // ── 25. Goto-def: MIN macro ──────────────────────────────────
    {
        let (l, c) = mark(&main_c, "USE_MIN_macro", "MIN");
        assert_definition_found(&s.goto_definition(&mu, l, c), "def@MIN_macro");
    }

    // ── 26. Goto-def: MAX macro ──────────────────────────────────
    {
        let (l, c) = mark(&main_c, "USE_MAX_macro", "MAX");
        assert_definition_found(&s.goto_definition(&mu, l, c), "def@MAX_macro");
    }

    // ── 27. Goto-def: StatusCode typedef ─────────────────────────
    {
        let (l, c) = mark(&main_c, "USE_StatusCode", "StatusCode");
        assert_definition_found(&s.goto_definition(&mu, l, c), "def@StatusCode");
    }

    // ── 28. Goto-def: RequestHandler from Server struct field ────
    {
        let (l, c) = mark(&server_h, "Server_handler_field", "RequestHandler");
        assert_definition_found(&s.goto_definition(&su, l, c), "def@RequestHandler_from_Server");
    }

    // ── 29. Find-refs: buffer_init ───────────────────────────────
    {
        let (l, c) = mark(&main_c, "buffer_init_IMPL", "buffer_init");
        assert_references_ge(&s.find_references(&mu, l, c), 3, "refs@buffer_init");
    }

    // ── 30. Find-refs: Connection ────────────────────────────────
    {
        let (l, c) = mark(&types_h, "Connection_DEF", "Connection");
        assert_references_ge(&s.find_references(&tu, l, c), 4, "refs@Connection");
    }

    // ── 31. Find-refs: MAX_HEADERS ───────────────────────────────
    {
        let (l, c) = mark(&types_h, "MAX_HEADERS_DEF", "MAX_HEADERS");
        assert_references_ge(&s.find_references(&tu, l, c), 2, "refs@MAX_HEADERS");
    }

    // ── 32. Find-refs: server_log ────────────────────────────────
    {
        let (l, c) = mark(&main_c, "server_log_IMPL", "server_log");
        assert_references_ge(&s.find_references(&mu, l, c), 5, "refs@server_log");
    }

    // ── 33. Find-refs: CONN_ESTABLISHED ──────────────────────────
    {
        let (l, c) = mark(&types_h, "CONN_ESTABLISHED_DEF", "CONN_ESTABLISHED");
        assert_references_ge(&s.find_references(&tu, l, c), 2, "refs@CONN_ESTABLISHED");
    }

    // ── 34. Find-refs: Request ───────────────────────────────────
    {
        let (l, c) = mark(&types_h, "Request_DEF", "Request");
        assert_references_ge(&s.find_references(&tu, l, c), 5, "refs@Request");
    }

    // ── 35. Find-refs: handle_request ────────────────────────────
    {
        let (l, c) = mark(&main_c, "handle_request_DEF", "handle_request");
        assert_references_ge(&s.find_references(&mu, l, c), 2, "refs@handle_request");
    }

    // ── 36. Find-refs: HTTP_OK ───────────────────────────────────
    {
        let (l, c) = mark(&types_h, "HTTP_OK_DEF", "HTTP_OK");
        assert_references_ge(&s.find_references(&tu, l, c), 3, "refs@HTTP_OK");
    }

    // ── 37. Signature-help: buffer_append() ──────────────────────
    {
        let (l, c) = mark(&main_c, "CALL_buffer_append_in_set_body", "buffer_append");
        // Position cursor inside the parens (after the function name)
        let c_inside = c + "buffer_append(".len() as u32;
        assert_sighelp_found(&s.signature_help(&mu, l, c_inside), "sighelp@buffer_append");
    }

    // ── 38. Signature-help: address_format() ─────────────────────
    {
        let (l, c) = mark(&main_c, "CALL_address_format_in_main", "address_format");
        let c_inside = c + "address_format(".len() as u32;
        assert_sighelp_found(&s.signature_help(&mu, l, c_inside), "sighelp@address_format");
    }

    // ── 39. Signature-help: server_log() ─────────────────────────
    {
        let (l, c) = mark(&main_c, "CALL_server_log_error", "server_log");
        let c_inside = c + "server_log(".len() as u32;
        assert_sighelp_found(&s.signature_help(&mu, l, c_inside), "sighelp@server_log");
    }

    // ── 40. Signature-help: server_create() ──────────────────────
    {
        let (l, c) = mark(&main_c, "CALL_server_create", "server_create");
        let c_inside = c + "server_create(".len() as u32;
        assert_sighelp_found(&s.signature_help(&mu, l, c_inside), "sighelp@server_create");
    }

    // ── 41. Signature-help: connection_init() ────────────────────
    {
        let (l, c) = mark(&main_c, "CALL_connection_init", "connection_init");
        let c_inside = c + "connection_init(".len() as u32;
        assert_sighelp_found(&s.signature_help(&mu, l, c_inside), "sighelp@connection_init");
    }

    // ── 42. Signature-help: address_parse() ──────────────────────
    {
        let (l, c) = mark(&main_c, "CALL_address_parse", "address_parse");
        let c_inside = c + "address_parse(".len() as u32;
        assert_sighelp_found(&s.signature_help(&mu, l, c_inside), "sighelp@address_parse");
    }

    // ── Cleanup ──────────────────────────────────────────────────
    s.shutdown();
    eprintln!("All 42 C integration checks passed!");
}
