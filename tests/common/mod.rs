//! Shared LSP integration-test harness.
//!
//! Provides `LspServer` (subprocess wrapper), cursor-position helpers,
//! a failure-collecting `TestResults` runner, and assertion helpers for
//! hover, go-to-definition, find-references, completion, document/workspace
//! symbols, and signature help.
//!
//! Both `c_integration` and `rust_integration` import this module.

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

// ── LSP server wrapper ───────────────────────────────────────────

pub struct LspServer {
    child: Child,
    rx: mpsc::Receiver<serde_json::Value>,
    _reader_thread: std::thread::JoinHandle<()>,
    _cache_dir: tempfile::TempDir,
    next_id: u64,
}

impl LspServer {
    pub fn spawn() -> Self {
        let binary = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("debug")
            .join("quicklsp");

        let cache_dir = tempfile::tempdir().expect("failed to create temp cache dir");

        let mut child = Command::new(&binary)
            .env("XDG_CACHE_HOME", cache_dir.path())
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

        LspServer {
            child,
            rx,
            _reader_thread: reader_thread,
            _cache_dir: cache_dir,
            next_id: 100,
        }
    }

    fn stdin(&mut self) -> &mut std::process::ChildStdin {
        self.child.stdin.as_mut().unwrap()
    }

    pub fn send(&mut self, msg: &serde_json::Value) {
        send_message(self.stdin(), msg);
    }

    pub fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn recv(&self, timeout: Duration) -> Option<serde_json::Value> {
        self.rx.recv_timeout(timeout).ok()
    }

    pub fn wait_for_id(&self, id: u64) -> serde_json::Value {
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

    pub fn wait_for(
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

    pub fn initialize(&mut self, root_dir: &Path) {
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
        let _ = self
            .wait_for(Duration::from_secs(5), |msg| {
                msg.get("id").and_then(|v| v.as_u64()) == Some(1)
            })
            .expect("No initialize response");
        self.send(&serde_json::json!({
            "jsonrpc": "2.0", "method": "initialized", "params": {}
        }));
    }

    pub fn shutdown(&mut self) {
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

    pub fn did_open(&mut self, uri: &str, lang: &str, source: &str) {
        self.send(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": uri, "languageId": lang, "version": 1, "text": source
                }
            }
        }));
    }

    pub fn did_change(&mut self, uri: &str, version: i32, text: &str) {
        self.send(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [{ "text": text }]
            }
        }));
    }

    pub fn hover(&mut self, uri: &str, line: u32, col: u32) -> serde_json::Value {
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

    pub fn goto_definition(&mut self, uri: &str, line: u32, col: u32) -> serde_json::Value {
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

    pub fn find_references(&mut self, uri: &str, line: u32, col: u32) -> serde_json::Value {
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

    pub fn completion(&mut self, uri: &str, line: u32, col: u32) -> serde_json::Value {
        let id = self.alloc_id();
        self.send(&serde_json::json!({
            "jsonrpc": "2.0", "id": id,
            "method": "textDocument/completion",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": col }
            }
        }));
        self.wait_for_id(id)
    }

    pub fn signature_help(&mut self, uri: &str, line: u32, col: u32) -> serde_json::Value {
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

    pub fn document_symbols(&mut self, uri: &str) -> serde_json::Value {
        let id = self.alloc_id();
        self.send(&serde_json::json!({
            "jsonrpc": "2.0", "id": id,
            "method": "textDocument/documentSymbol",
            "params": { "textDocument": { "uri": uri } }
        }));
        self.wait_for_id(id)
    }

    pub fn workspace_symbols(&mut self, query: &str) -> serde_json::Value {
        let id = self.alloc_id();
        self.send(&serde_json::json!({
            "jsonrpc": "2.0", "id": id,
            "method": "workspace/symbol",
            "params": { "query": query }
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

// ── Common helpers ───────────────────────────────────────────────

pub fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

/// Drain messages until we see a progress End, or timeout.
/// Also responds to workDoneProgress/create requests.
pub fn drain_until_progress_end(server: &mut LspServer) {
    let start = Instant::now();
    let timeout = Duration::from_secs(30);
    while start.elapsed() < timeout {
        let remaining = timeout.saturating_sub(start.elapsed());
        if let Some(msg) = server.recv(remaining) {
            if msg
                .get("method")
                .and_then(|v: &serde_json::Value| v.as_str())
                == Some("window/workDoneProgress/create")
            {
                if let Some(id) = msg.get("id") {
                    server.send(&serde_json::json!({
                        "jsonrpc": "2.0", "id": id.clone(), "result": null
                    }));
                }
            }
            if msg
                .get("method")
                .and_then(|v: &serde_json::Value| v.as_str())
                == Some("$/progress")
            {
                if msg["params"]["value"]["kind"].as_str() == Some("end") {
                    return;
                }
            }
        }
    }
    panic!("Timed out waiting for indexing to complete");
}

/// Find the (line, col) where `@mark TAG` appears, then locate `token`
/// on the same line and return that position. Panics if not found.
pub fn mark(source: &str, tag: &str, token: &str) -> (u32, u32) {
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

/// Open a fixture file via textDocument/didOpen. Returns (uri, source).
pub fn open_fixture(server: &mut LspServer, dir: &Path, filename: &str, lang: &str) -> (String, String) {
    let file_path = dir.join(filename);
    let source = std::fs::read_to_string(&file_path).unwrap();
    let file_uri = format!("file://{}", file_path.display());
    server.did_open(&file_uri, lang, &source);
    (file_uri, source)
}

// ── Failure collector ────────────────────────────────────────────

pub struct TestResults {
    pub failures: Vec<String>,
    pub pass_count: usize,
}

impl TestResults {
    pub fn new() -> Self {
        TestResults {
            failures: Vec::new(),
            pass_count: 0,
        }
    }

    pub fn check(&mut self, ok: bool, msg: String) {
        if ok {
            self.pass_count += 1;
        } else {
            eprintln!("  FAIL: {msg}");
            self.failures.push(msg);
        }
    }

    pub fn finish(self) {
        let total = self.pass_count + self.failures.len();
        eprintln!("\n{} / {} checks passed.", self.pass_count, total);
        if !self.failures.is_empty() {
            eprintln!("\n{} FAILURES:", self.failures.len());
            for (i, f) in self.failures.iter().enumerate() {
                eprintln!("  {}. {f}", i + 1);
            }
            panic!(
                "{} of {} checks failed — see details above",
                self.failures.len(),
                total
            );
        }
    }
}

// ── Assertion helpers ────────────────────────────────────────────

pub fn check_hover_contains(t: &mut TestResults, resp: &serde_json::Value, substring: &str, ctx: &str) {
    if resp.get("error").is_some() {
        t.check(false, format!("{ctx}: hover error: {resp}"));
        return;
    }
    let result = &resp["result"];
    if result.is_null() {
        t.check(false, format!("{ctx}: hover returned null"));
        return;
    }
    let content = result["contents"]["value"].as_str().unwrap_or("");
    t.check(
        content.contains(substring),
        format!("{ctx}: hover should contain '{substring}', got:\n  {content}"),
    );
}

pub fn check_hover_not_contains(t: &mut TestResults, resp: &serde_json::Value, bad: &str, ctx: &str) {
    if resp.get("error").is_some() || resp["result"].is_null() {
        t.check(false, format!("{ctx}: hover returned error/null"));
        return;
    }
    let content = resp["result"]["contents"]["value"].as_str().unwrap_or("");
    t.check(
        !content.contains(bad),
        format!("{ctx}: hover should NOT contain '{bad}', got:\n  {content}"),
    );
}

pub fn check_hover_no_error(t: &mut TestResults, resp: &serde_json::Value, ctx: &str) {
    t.check(
        resp.get("error").is_none(),
        format!("{ctx}: hover returned error: {resp}"),
    );
}

pub fn check_hover_null(t: &mut TestResults, resp: &serde_json::Value, ctx: &str) {
    if resp.get("error").is_some() {
        t.check(false, format!("{ctx}: hover returned error: {resp}"));
        return;
    }
    t.check(
        resp["result"].is_null(),
        format!("{ctx}: hover should return null, got: {}", resp["result"]),
    );
}

pub fn check_definition_found(t: &mut TestResults, resp: &serde_json::Value, ctx: &str) {
    if resp.get("error").is_some() {
        t.check(false, format!("{ctx}: definition error: {resp}"));
        return;
    }
    let result = &resp["result"];
    if result.is_null() {
        t.check(false, format!("{ctx}: definition returned null"));
        return;
    }
    if result.is_array() {
        t.check(
            !result.as_array().unwrap().is_empty(),
            format!("{ctx}: definition empty array"),
        );
    } else {
        t.check(true, String::new());
    }
}

pub fn check_definition_target(
    t: &mut TestResults,
    resp: &serde_json::Value,
    expected_file: &str,
    ctx: &str,
) {
    if resp.get("error").is_some() || resp["result"].is_null() {
        t.check(false, format!("{ctx}: definition error or null"));
        return;
    }
    let result = &resp["result"];
    let locations: Vec<&serde_json::Value> = if result.is_array() {
        result.as_array().unwrap().iter().collect()
    } else {
        vec![result]
    };
    let found = locations.iter().any(|loc| {
        let uri = loc["uri"]
            .as_str()
            .or_else(|| loc["targetUri"].as_str())
            .unwrap_or("");
        uri.ends_with(expected_file)
    });
    let uris: Vec<&str> = locations
        .iter()
        .filter_map(|loc| loc["uri"].as_str().or_else(|| loc["targetUri"].as_str()))
        .collect();
    t.check(
        found,
        format!("{ctx}: expected definition in '{expected_file}', got: {uris:?}"),
    );
}

pub fn check_references_ge(t: &mut TestResults, resp: &serde_json::Value, min: usize, ctx: &str) {
    if resp.get("error").is_some() {
        t.check(false, format!("{ctx}: references error: {resp}"));
        return;
    }
    let empty = vec![];
    let refs = resp["result"].as_array().unwrap_or(&empty);
    t.check(
        refs.len() >= min,
        format!("{ctx}: expected >= {min} refs, got {}", refs.len()),
    );
}

pub fn check_references_include_file(
    t: &mut TestResults,
    resp: &serde_json::Value,
    filename: &str,
    ctx: &str,
) {
    if resp.get("error").is_some() {
        t.check(false, format!("{ctx}: references error: {resp}"));
        return;
    }
    let empty = vec![];
    let refs = resp["result"].as_array().unwrap_or(&empty);
    let found = refs
        .iter()
        .any(|r| r["uri"].as_str().map_or(false, |u| u.ends_with(filename)));
    let files: Vec<&str> = refs
        .iter()
        .filter_map(|r| r["uri"].as_str())
        .map(|u| u.rsplit('/').next().unwrap_or(u))
        .collect();
    t.check(
        found,
        format!(
            "{ctx}: expected refs to include '{filename}', got {} refs in files: {files:?}",
            refs.len()
        ),
    );
}

pub fn check_completion_contains(
    t: &mut TestResults,
    resp: &serde_json::Value,
    expected_label: &str,
    ctx: &str,
) {
    if resp.get("error").is_some() {
        t.check(false, format!("{ctx}: completion error: {resp}"));
        return;
    }
    let result = &resp["result"];
    // Completion can be an array or a CompletionList { items: [...] }
    let items = result
        .as_array()
        .or_else(|| result["items"].as_array());
    match items {
        None => {
            t.check(false, format!("{ctx}: completion result has no items"));
        }
        Some(arr) => {
            let labels: Vec<&str> = arr.iter().filter_map(|i| i["label"].as_str()).collect();
            t.check(
                labels.iter().any(|l| l.contains(expected_label)),
                format!("{ctx}: completion should contain '{expected_label}', got: {labels:?}"),
            );
        }
    }
}

pub fn check_completion_non_empty(
    t: &mut TestResults,
    resp: &serde_json::Value,
    ctx: &str,
) {
    if resp.get("error").is_some() {
        t.check(false, format!("{ctx}: completion error: {resp}"));
        return;
    }
    let result = &resp["result"];
    let items = result
        .as_array()
        .or_else(|| result["items"].as_array());
    match items {
        None => t.check(false, format!("{ctx}: completion result has no items")),
        Some(arr) => t.check(!arr.is_empty(), format!("{ctx}: completion returned empty list")),
    }
}

pub fn check_sighelp_found(t: &mut TestResults, resp: &serde_json::Value, ctx: &str) {
    if resp.get("error").is_some() {
        t.check(false, format!("{ctx}: sighelp error: {resp}"));
        return;
    }
    let result = &resp["result"];
    if result.is_null() {
        t.check(false, format!("{ctx}: sighelp null"));
        return;
    }
    let sigs = result["signatures"].as_array();
    t.check(
        sigs.map_or(false, |s| !s.is_empty()),
        format!("{ctx}: sighelp no signatures"),
    );
}

pub fn check_symbols_contain(
    t: &mut TestResults,
    resp: &serde_json::Value,
    expected_name: &str,
    ctx: &str,
) {
    let syms = match resp["result"].as_array() {
        Some(s) => s,
        None => {
            t.check(false, format!("{ctx}: no symbols array"));
            return;
        }
    };
    let names: Vec<&str> = syms.iter().filter_map(|s| s["name"].as_str()).collect();
    t.check(
        names.contains(&expected_name),
        format!("{ctx}: should find '{expected_name}', got: {names:?}"),
    );
}

pub fn check_symbols_count_ge(
    t: &mut TestResults,
    resp: &serde_json::Value,
    min: usize,
    ctx: &str,
) {
    let syms = match resp["result"].as_array() {
        Some(s) => s,
        None => {
            t.check(false, format!("{ctx}: no symbols array"));
            return;
        }
    };
    t.check(
        syms.len() >= min,
        format!("{ctx}: expected >= {min} symbols, got {}", syms.len()),
    );
}

pub fn check_symbols_exclude_locals(t: &mut TestResults, resp: &serde_json::Value, ctx: &str) {
    let syms = match resp["result"].as_array() {
        Some(s) => s,
        None => {
            t.check(false, format!("{ctx}: no symbols array"));
            return;
        }
    };
    let local_names: Vec<&str> = syms
        .iter()
        .filter(|s| s["kind"].as_u64() == Some(13))
        .filter_map(|s| s["name"].as_str())
        .collect();
    let total = syms.len();
    let locals = local_names.len();
    t.check(
        locals <= total / 2,
        format!("{ctx}: {locals} of {total} symbols are local variables — too many locals pollute the symbol list"),
    );
}
