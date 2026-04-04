//! Diagnostic tool: counts how the tokenizer spends its time.
//!
//! Usage: cargo run --bin tokenizer-stats --release -- /path/to/repo

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use tracing_subscriber::EnvFilter;

/// Guard that prints stats on drop (covers normal exit, panic, etc.)
struct StatsGuard;
impl Drop for StatsGuard {
    fn drop(&mut self) {
        print_stats();
    }
}

// Global counters — cheap to increment, no locking
static FILES_SCANNED: AtomicU64 = AtomicU64::new(0);
static TOTAL_BYTES: AtomicU64 = AtomicU64::new(0);
static ASCII_BYTES: AtomicU64 = AtomicU64::new(0);
static NON_ASCII_BYTES: AtomicU64 = AtomicU64::new(0);
static IDENT_CALLS: AtomicU64 = AtomicU64::new(0);
static IDENT_ASCII_ONLY: AtomicU64 = AtomicU64::new(0);
static IDENT_HIT_UNICODE: AtomicU64 = AtomicU64::new(0);
static IDENT_TOTAL_BYTES: AtomicU64 = AtomicU64::new(0);
static IDENT_UNICODE_CHARS: AtomicU64 = AtomicU64::new(0);
static SKIPPED_BYTES_IN_STRINGS: AtomicU64 = AtomicU64::new(0);
static SKIPPED_BYTES_IN_COMMENTS: AtomicU64 = AtomicU64::new(0);
static NON_ASCII_OUTSIDE_SKIP: AtomicU64 = AtomicU64::new(0);

fn print_stats() {
    let total = TOTAL_BYTES.load(Relaxed);
    let ascii = ASCII_BYTES.load(Relaxed);
    let non_ascii = NON_ASCII_BYTES.load(Relaxed);
    let ident_calls = IDENT_CALLS.load(Relaxed);
    let ident_ascii = IDENT_ASCII_ONLY.load(Relaxed);
    let ident_unicode = IDENT_HIT_UNICODE.load(Relaxed);
    let ident_bytes = IDENT_TOTAL_BYTES.load(Relaxed);
    let unicode_chars = IDENT_UNICODE_CHARS.load(Relaxed);
    let str_bytes = SKIPPED_BYTES_IN_STRINGS.load(Relaxed);
    let comment_bytes = SKIPPED_BYTES_IN_COMMENTS.load(Relaxed);
    let non_ascii_outside = NON_ASCII_OUTSIDE_SKIP.load(Relaxed);

    eprintln!();
    eprintln!("=== Tokenizer Stats ===");
    eprintln!();
    eprintln!("Files scanned:     {}", FILES_SCANNED.load(Relaxed));
    eprintln!("Total bytes:       {} ({:.1} MB)", total, total as f64 / 1_048_576.0);
    eprintln!("  ASCII bytes:     {} ({:.1}%)", ascii, pct(ascii, total));
    eprintln!("  Non-ASCII bytes: {} ({:.1}%)", non_ascii, pct(non_ascii, total));
    eprintln!();
    eprintln!("Skip regions:");
    eprintln!("  In strings:      {} bytes ({:.1}%)", str_bytes, pct(str_bytes, total));
    eprintln!("  In comments:     {} bytes ({:.1}%)", comment_bytes, pct(comment_bytes, total));
    eprintln!();
    eprintln!("Non-ASCII outside strings/comments: {} bytes", non_ascii_outside);
    eprintln!();
    eprintln!("Identifier consumption:");
    eprintln!("  Total calls:     {}", ident_calls);
    eprintln!("  Pure ASCII:      {} ({:.1}%)", ident_ascii, pct(ident_ascii, ident_calls));
    eprintln!("  Hit Unicode:     {} ({:.1}%)", ident_unicode, pct(ident_unicode, ident_calls));
    eprintln!("  Total ident bytes: {}", ident_bytes);
    eprintln!("  Unicode chars iterated: {}", unicode_chars);
    if ident_unicode > 0 {
        eprintln!("  Avg unicode chars/hit: {:.1}", unicode_chars as f64 / ident_unicode as f64);
    }
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse().unwrap()))
        .with_writer(std::io::stderr)
        .init();

    let _guard = StatsGuard;

    let dir = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("Usage: tokenizer-stats <directory>");
        std::process::exit(1);
    });
    let root = PathBuf::from(&dir);

    let mut files = Vec::new();
    if root.is_file() {
        files.push(root);
    } else {
        collect_files(&root, &mut files, 0);
    }

    for (idx, path) in files.iter().enumerate() {
        tracing::info!("[{}/{}] {}", idx + 1, files.len(), path.display());
        if let Ok(source) = std::fs::read_to_string(path) {
            FILES_SCANNED.fetch_add(1, Relaxed);
            analyze_file(&source, path);
        }
        // Print intermediate stats every 500 files
        if (idx + 1) % 10 == 0 {
            tracing::info!("--- after {} files ---", idx + 1);
            print_stats();
        }
    }

    eprintln!("--- final ---");
    print_stats();
}

fn pct(num: u64, denom: u64) -> f64 {
    if denom == 0 { 0.0 } else { num as f64 / denom as f64 * 100.0 }
}

fn analyze_file(source: &str, _path: &Path) {
    let bytes = source.as_bytes();
    let len = bytes.len();
    TOTAL_BYTES.fetch_add(len as u64, Relaxed);

    let ascii_count = bytes.iter().filter(|b| b.is_ascii()).count();
    ASCII_BYTES.fetch_add(ascii_count as u64, Relaxed);
    NON_ASCII_BYTES.fetch_add((len - ascii_count) as u64, Relaxed);

    // Simulate the tokenizer's main loop to count what happens
    let mut i = 0;
    while i < len {
        let b = bytes[i];

        if b == b'\n' || b.is_ascii_whitespace() {
            i += 1;
            continue;
        }

        // Comments
        if b == b'#' || (b == b'/' && i + 1 < len && bytes[i + 1] == b'/') {
            let start = i;
            while i < len && bytes[i] != b'\n' {
                i += 1;
            }
            SKIPPED_BYTES_IN_COMMENTS.fetch_add((i - start) as u64, Relaxed);
            continue;
        }
        if b == b'/' && i + 1 < len && bytes[i + 1] == b'*' {
            let start = i;
            i += 2;
            while i + 1 < len && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            if i + 1 < len { i += 2; }
            SKIPPED_BYTES_IN_COMMENTS.fetch_add((i - start) as u64, Relaxed);
            continue;
        }

        // Strings
        if b == b'"' || b == b'\'' || b == b'`' {
            let start = i;
            let quote = b;
            i += 1;
            while i < len && bytes[i] != quote {
                if bytes[i] == b'\\' { i += 1; }
                i += 1;
            }
            if i < len { i += 1; }
            SKIPPED_BYTES_IN_STRINGS.fetch_add((i - start) as u64, Relaxed);
            continue;
        }

        // Identifier
        if b.is_ascii_alphabetic() || b == b'_' || b >= 0xC0 {
            let start = i;
            IDENT_CALLS.fetch_add(1, Relaxed);

            // ASCII fast path
            let mut hit_unicode = false;
            while i < len && bytes[i] < 0x80 {
                if bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' {
                    i += 1;
                } else {
                    break;
                }
            }

            // Unicode path
            if i < len && bytes[i] >= 0x80 {
                hit_unicode = true;
                let mut uc = 0u64;
                for ch in source[i..].chars() {
                    if ch == '_' || ch.is_alphanumeric() {
                        i += ch.len_utf8();
                        uc += 1;
                    } else {
                        break;
                    }
                }
                IDENT_UNICODE_CHARS.fetch_add(uc, Relaxed);
            }

            let ident_len = i - start;
            IDENT_TOTAL_BYTES.fetch_add(ident_len as u64, Relaxed);
            if hit_unicode {
                IDENT_HIT_UNICODE.fetch_add(1, Relaxed);
            } else {
                IDENT_ASCII_ONLY.fetch_add(1, Relaxed);
            }
            continue;
        }

        // Non-ASCII outside skip regions
        if b >= 0x80 {
            NON_ASCII_OUTSIDE_SKIP.fetch_add(1, Relaxed);
            // advance by UTF-8 char length
            match b {
                0xC0..=0xDF => i += 2,
                0xE0..=0xEF => i += 3,
                0xF0..=0xFF => i += 4,
                _ => i += 1,
            }
            continue;
        }

        i += 1;
    }
}

const SOURCE_EXTENSIONS: &[&str] = &[
    "rs", "py", "js", "ts", "tsx", "go", "c", "h", "cpp", "cc", "hpp", "java", "cs", "rb", "jsx",
    "mjs",
];

fn collect_files(dir: &Path, files: &mut Vec<PathBuf>, depth: usize) {
    if depth > 20 { return; }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.starts_with('.') || matches!(name, "node_modules" | "target" | "vendor" | "build" | "__pycache__" | "dist" | "third_party" | "testdata") {
            continue;
        }
        if path.is_dir() {
            collect_files(&path, files, depth + 1);
        } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if SOURCE_EXTENSIONS.contains(&ext) {
                files.push(path);
            }
        }
    }
}
