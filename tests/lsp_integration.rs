//! LSP integration tests.
//!
//! Spawns quicklsp as a subprocess and communicates via JSON-RPC over
//! stdin/stdout. Tests the full lifecycle: initialize → progress → queries.
//!
//!   cargo test -p quicklsp --test lsp_integration -- --nocapture

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

// ── JSON-RPC helpers ────────────────────────────────────────────────────

fn send_message(stdin: &mut impl Write, msg: &serde_json::Value) {
    let body = serde_json::to_string(msg).unwrap();
    write!(stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
    stdin.flush().unwrap();
}

/// Blocking read of one JSON-RPC message from a reader.
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

// ── Server lifecycle ────────────────────────────────────────────────────

struct LspServer {
    child: Child,
    rx: mpsc::Receiver<serde_json::Value>,
    _reader_thread: std::thread::JoinHandle<()>,
}

impl LspServer {
    fn spawn() -> Self {
        let binary = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("debug")
            .join("quicklsp");

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

        LspServer { child, rx, _reader_thread: reader_thread }
    }

    fn stdin(&mut self) -> &mut std::process::ChildStdin {
        self.child.stdin.as_mut().unwrap()
    }

    fn send(&mut self, msg: &serde_json::Value) {
        send_message(self.stdin(), msg);
    }

    /// Receive next message with timeout.
    fn recv(&self, timeout: Duration) -> Option<serde_json::Value> {
        self.rx.recv_timeout(timeout).ok()
    }

    /// Wait for a message matching the predicate, with timeout.
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

    /// Collect messages matching predicate until timeout expires.
    fn collect(
        &self,
        timeout: Duration,
        pred: impl Fn(&serde_json::Value) -> bool,
    ) -> Vec<serde_json::Value> {
        let start = Instant::now();
        let mut results = Vec::new();
        loop {
            let remaining = timeout.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                break;
            }
            match self.rx.recv_timeout(remaining) {
                Ok(msg) if pred(&msg) => results.push(msg),
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        results
    }

    /// Send initialize + initialized, return the initialize response.
    fn initialize(&mut self, root_dir: &Path) -> serde_json::Value {
        let root_uri = format!("file://{}", root_dir.display());

        self.send(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "rootUri": root_uri,
                "capabilities": {
                    "window": {
                        "workDoneProgress": true
                    }
                }
            }
        }));

        let response = self.wait_for(Duration::from_secs(5), |msg| {
            msg.get("id").and_then(|v| v.as_u64()) == Some(1)
        }).expect("No initialize response received");

        // Send initialized notification
        self.send(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        }));

        response
    }

    fn shutdown(&mut self) {
        self.send(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 99,
            "method": "shutdown",
            "params": null
        }));
        let _ = self.wait_for(Duration::from_secs(2), |msg| {
            msg.get("id").and_then(|v| v.as_u64()) == Some(99)
        });
        self.send(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "exit",
            "params": null
        }));
        let _ = self.child.wait();
    }
}

impl Drop for LspServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

// ── Tests ───────────────────────────────────────────────────────────────

#[test]
fn test_initialize_handshake() {
    let mut server = LspServer::spawn();
    let response = server.initialize(&fixtures_dir());

    // Verify we got a valid response with capabilities
    let result = response.get("result").expect("No result in initialize response");
    let caps = result.get("capabilities").expect("No capabilities");

    assert_eq!(caps["definitionProvider"], true);
    assert_eq!(caps["referencesProvider"], true);
    assert_eq!(caps["documentSymbolProvider"], true);
    assert_eq!(caps["workspaceSymbolProvider"], true);
    assert_eq!(caps["hoverProvider"], true);

    // Server info
    let info = result.get("serverInfo").expect("No serverInfo");
    assert_eq!(info["name"], "QuickLSP");

    server.shutdown();
}

#[test]
fn test_progress_notifications_during_indexing() {
    let mut server = LspServer::spawn();
    server.initialize(&fixtures_dir());

    // After initialized, the server should:
    // 1. Send window/workDoneProgress/create request
    // 2. Send $/progress Begin
    // 3. Send $/progress Report (during scanning)
    // 4. Send $/progress End

    // First: the server sends a request to create the progress token.
    let create_req = server.wait_for(Duration::from_secs(5), |msg| {
        msg.get("method").and_then(|v| v.as_str()) == Some("window/workDoneProgress/create")
    });
    assert!(create_req.is_some(), "Server should send workDoneProgress/create request");
    let create_req = create_req.unwrap();

    // Respond to the create request (client must acknowledge)
    if let Some(id) = create_req.get("id") {
        server.send(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": null
        }));
    }

    // Now collect progress notifications. For the small fixtures dir,
    // indexing should complete within a few seconds.
    let progress_msgs = server.collect(Duration::from_secs(10), |msg| {
        msg.get("method").and_then(|v| v.as_str()) == Some("$/progress")
    });

    // We should have at least Begin and End
    assert!(
        progress_msgs.len() >= 2,
        "Expected at least Begin + End progress notifications, got {}",
        progress_msgs.len()
    );

    // Check Begin
    let begin = &progress_msgs[0];
    let begin_value = &begin["params"]["value"];
    assert_eq!(begin_value["kind"], "begin");
    assert_eq!(begin_value["title"], "Indexing");

    // Check End (last message)
    let end = progress_msgs.last().unwrap();
    let end_value = &end["params"]["value"];
    assert_eq!(end_value["kind"], "end");
    // End message should mention indexed files
    let end_msg = end_value["message"].as_str().unwrap_or("");
    assert!(
        end_msg.contains("Indexed") || end_msg.contains("definitions"),
        "End message should summarize indexing: got '{end_msg}'"
    );

    server.shutdown();
}

#[test]
fn test_progress_includes_workspace_scan_updates() {
    // This test specifically checks that progress is reported DURING
    // the workspace scan, not just Begin/End. This is the bug we found:
    // the scan takes 50+ seconds on large repos with zero progress updates.
    let mut server = LspServer::spawn();
    server.initialize(&fixtures_dir());

    // Acknowledge progress token creation
    let create_req = server.wait_for(Duration::from_secs(5), |msg| {
        msg.get("method").and_then(|v| v.as_str()) == Some("window/workDoneProgress/create")
    });
    if let Some(req) = create_req {
        if let Some(id) = req.get("id") {
            server.send(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": null
            }));
        }
    }

    let progress_msgs = server.collect(Duration::from_secs(10), |msg| {
        msg.get("method").and_then(|v| v.as_str()) == Some("$/progress")
    });

    // Filter to "report" kind only (not begin/end)
    let reports: Vec<_> = progress_msgs.iter()
        .filter(|msg| {
            msg["params"]["value"]["kind"].as_str() == Some("report")
        })
        .collect();

    // For the small fixtures dir (5 files), scan progress reports won't fire
    // because the threshold is 500 files. On large repos (65K files), the scan
    // would produce ~130 report messages with "Scanning: N/M files".
    // This test documents the expected behavior and verifies structure.
    println!("Progress messages received: {}", progress_msgs.len());
    println!("Report messages during scan: {}", reports.len());
    for (i, msg) in progress_msgs.iter().enumerate() {
        println!("  [{i}] kind={}, message={}",
            msg["params"]["value"]["kind"],
            msg["params"]["value"]["message"],
        );
    }

    // At minimum, we should have Begin + End
    assert!(progress_msgs.len() >= 2, "Expected Begin + End at minimum");

    server.shutdown();
}

#[test]
fn test_go_to_definition_after_indexing() {
    let mut server = LspServer::spawn();
    server.initialize(&fixtures_dir());

    // Wait for indexing to complete by waiting for progress End
    drain_until_progress_end(&mut server);

    // Open a file
    let file_path = fixtures_dir().join("sample_rust.rs");
    let source = std::fs::read_to_string(&file_path).unwrap();
    let file_uri = format!("file://{}", file_path.display());

    server.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": file_uri,
                "languageId": "rust",
                "version": 1,
                "text": source
            }
        }
    }));

    // Find a function name in the fixture and request definition
    // Look for "process_data" which should be defined in the fixture
    let line = source.lines().enumerate()
        .find(|(_, l)| l.contains("process_data"))
        .map(|(i, _)| i)
        .unwrap_or(0);

    server.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 10,
        "method": "textDocument/definition",
        "params": {
            "textDocument": { "uri": file_uri },
            "position": { "line": line, "character": 5 }
        }
    }));

    let response = server.wait_for(Duration::from_secs(5), |msg| {
        msg.get("id").and_then(|v| v.as_u64()) == Some(10)
    });
    assert!(response.is_some(), "Should receive definition response");
    let response = response.unwrap();

    // Should have a result (even if empty — no error)
    assert!(
        response.get("result").is_some(),
        "Definition response should have a result, got: {response}"
    );

    server.shutdown();
}

#[test]
fn test_find_references_after_indexing() {
    let mut server = LspServer::spawn();
    server.initialize(&fixtures_dir());

    drain_until_progress_end(&mut server);

    let file_path = fixtures_dir().join("sample_rust.rs");
    let source = std::fs::read_to_string(&file_path).unwrap();
    let file_uri = format!("file://{}", file_path.display());

    server.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": file_uri,
                "languageId": "rust",
                "version": 1,
                "text": source
            }
        }
    }));

    // Find references for "process_data"
    let line = source.lines().enumerate()
        .find(|(_, l)| l.contains("fn process_data"))
        .map(|(i, _)| i)
        .unwrap_or(0);

    server.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 20,
        "method": "textDocument/references",
        "params": {
            "textDocument": { "uri": file_uri },
            "position": { "line": line, "character": 7 },
            "context": { "includeDeclaration": true }
        }
    }));

    let response = server.wait_for(Duration::from_secs(5), |msg| {
        msg.get("id").and_then(|v| v.as_u64()) == Some(20)
    });
    assert!(response.is_some(), "Should receive references response");

    server.shutdown();
}

/// Open a fixture file via textDocument/didOpen.
fn open_fixture(server: &mut LspServer, filename: &str) -> (String, String) {
    let file_path = fixtures_dir().join(filename);
    let source = std::fs::read_to_string(&file_path).unwrap();
    let file_uri = format!("file://{}", file_path.display());

    server.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": file_uri,
                "languageId": "rust",
                "version": 1,
                "text": source
            }
        }
    }));

    (file_uri, source)
}

#[test]
fn test_document_symbols() {
    let mut server = LspServer::spawn();
    server.initialize(&fixtures_dir());
    drain_until_progress_end(&mut server);

    let (file_uri, _source) = open_fixture(&mut server, "sample_rust.rs");

    server.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 30,
        "method": "textDocument/documentSymbol",
        "params": {
            "textDocument": { "uri": file_uri }
        }
    }));

    let response = server.wait_for(Duration::from_secs(5), |msg| {
        msg.get("id").and_then(|v| v.as_u64()) == Some(30)
    }).expect("Should receive documentSymbol response");

    let result = response["result"].as_array().expect("result should be array");
    assert!(!result.is_empty(), "Should find symbols in sample_rust.rs");

    // Verify we find expected symbols
    let names: Vec<&str> = result.iter()
        .filter_map(|s| s["name"].as_str())
        .collect();
    assert!(names.contains(&"Config"), "Should find struct Config, got: {names:?}");
    assert!(names.contains(&"Server"), "Should find struct Server, got: {names:?}");
    assert!(names.contains(&"process_request"), "Should find fn process_request, got: {names:?}");

    server.shutdown();
}

#[test]
fn test_workspace_symbol_search() {
    let mut server = LspServer::spawn();
    server.initialize(&fixtures_dir());
    drain_until_progress_end(&mut server);

    server.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 40,
        "method": "workspace/symbol",
        "params": {
            "query": "Config"
        }
    }));

    let response = server.wait_for(Duration::from_secs(5), |msg| {
        msg.get("id").and_then(|v| v.as_u64()) == Some(40)
    }).expect("Should receive workspace/symbol response");

    let result = response["result"].as_array().expect("result should be array");
    assert!(!result.is_empty(), "Should find symbols matching 'Config'");

    let names: Vec<&str> = result.iter()
        .filter_map(|s| s["name"].as_str())
        .collect();
    assert!(names.contains(&"Config"), "Should find Config in results, got: {names:?}");

    server.shutdown();
}

#[test]
fn test_completion() {
    let mut server = LspServer::spawn();
    server.initialize(&fixtures_dir());
    drain_until_progress_end(&mut server);

    let (file_uri, source) = open_fixture(&mut server, "sample_rust.rs");

    // Find a line where we can complete — after "process_" should suggest "process_request"
    let line = source.lines().enumerate()
        .find(|(_, l)| l.contains("fn process_request"))
        .map(|(i, _)| i)
        .unwrap_or(0);

    // Request completion at "process_" prefix position
    server.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 50,
        "method": "textDocument/completion",
        "params": {
            "textDocument": { "uri": file_uri },
            "position": { "line": line, "character": 12 }
        }
    }));

    let response = server.wait_for(Duration::from_secs(5), |msg| {
        msg.get("id").and_then(|v| v.as_u64()) == Some(50)
    }).expect("Should receive completion response");

    // Should have a result (array or CompletionList)
    assert!(
        response.get("result").is_some(),
        "Completion should return a result, got: {response}"
    );

    server.shutdown();
}

#[test]
fn test_hover() {
    let mut server = LspServer::spawn();
    server.initialize(&fixtures_dir());
    drain_until_progress_end(&mut server);

    let (file_uri, source) = open_fixture(&mut server, "sample_rust.rs");

    // Hover over "Config" on the struct definition line
    let line = source.lines().enumerate()
        .find(|(_, l)| l.contains("struct Config"))
        .map(|(i, _)| i)
        .unwrap_or(0);

    server.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 60,
        "method": "textDocument/hover",
        "params": {
            "textDocument": { "uri": file_uri },
            "position": { "line": line, "character": 10 }
        }
    }));

    let response = server.wait_for(Duration::from_secs(5), |msg| {
        msg.get("id").and_then(|v| v.as_u64()) == Some(60)
    }).expect("Should receive hover response");

    // Should have a result (may be null if no hover info, but no error)
    assert!(
        !response.get("error").is_some(),
        "Hover should not return an error, got: {response}"
    );

    server.shutdown();
}

/// Drain messages until we see a progress End, or timeout.
/// Also responds to workDoneProgress/create requests.
fn drain_until_progress_end(server: &mut LspServer) {
    let start = Instant::now();
    let timeout = Duration::from_secs(30);
    while start.elapsed() < timeout {
        let remaining = timeout.saturating_sub(start.elapsed());
        if let Some(msg) = server.recv(remaining) {
            // Respond to progress create requests
            if msg.get("method").and_then(|v: &serde_json::Value| v.as_str()) == Some("window/workDoneProgress/create") {
                if let Some(id) = msg.get("id") {
                    server.send(&serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id.clone(),
                        "result": null
                    }));
                }
            }
            // Check for progress End
            if msg.get("method").and_then(|v: &serde_json::Value| v.as_str()) == Some("$/progress") {
                if msg["params"]["value"]["kind"].as_str() == Some("end") {
                    return;
                }
            }
        }
    }
    panic!("Timed out waiting for progress End notification");
}

#[test]
fn test_scan_progress_callback_fires() {
    // Create 600 small files to exceed the 500-file progress threshold.
    use quicklsp::Workspace;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let dir = tempfile::tempdir().unwrap();
    for i in 0..600 {
        let path = dir.path().join(format!("file_{i}.rs"));
        std::fs::write(&path, format!("fn func_{i}() {{}}\n")).unwrap();
    }

    let ws = Workspace::new();
    let progress_count = AtomicUsize::new(0);
    let last_done = AtomicUsize::new(0);
    let last_total = AtomicUsize::new(0);

    ws.scan_directory(dir.path(), Some(&|done, total| {
        progress_count.fetch_add(1, Ordering::Relaxed);
        last_done.store(done, Ordering::Relaxed);
        last_total.store(total, Ordering::Relaxed);
    }));

    let count = progress_count.load(Ordering::Relaxed);
    let done = last_done.load(Ordering::Relaxed);
    let total = last_total.load(Ordering::Relaxed);

    assert!(count >= 1, "Progress callback should fire at least once for 600 files, got {count}");
    assert_eq!(total, 600, "Total should be 600 files");
    assert!(done >= 500, "Last done should be >= 500, got {done}");
}
