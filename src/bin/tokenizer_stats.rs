//! Diagnostic tool: counts how the tokenizer spends its time.
//!
//! Uses the real tokenizer with its built-in atomic counters — no duplicated
//! scanning logic.
//!
//! Usage:
//!   cargo run --bin tokenizer-stats --release -- /path/to/repo
//!   cargo run --bin tokenizer-stats --release -- /path/to/file.ts

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use quicklsp::parsing::tokenizer::{self, stats as tok_stats, LangFamily};
use tracing_subscriber::EnvFilter;

static FILES_SCANNED: AtomicU64 = AtomicU64::new(0);

fn print_stats() {
    eprintln!();
    eprintln!("=== Tokenizer Stats ===");
    eprintln!("Files scanned: {}", FILES_SCANNED.load(Relaxed));
    eprintln!("{}", tok_stats::summary());
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse().unwrap()))
        .with_writer(std::io::stderr)
        .init();

    let dir = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("Usage: tokenizer-stats <file-or-directory>");
        std::process::exit(1);
    });
    let root = PathBuf::from(&dir);

    let mut files = Vec::new();
    if root.is_file() {
        files.push(root);
    } else {
        collect_files(&root, &mut files, 0);
    }

    tok_stats::reset();

    for (idx, path) in files.iter().enumerate() {
        tracing::info!("[{}/{}] {}", idx + 1, files.len(), path.display());
        if let Ok(source) = std::fs::read_to_string(&path) {
            let lang = path
                .extension()
                .and_then(|e| e.to_str())
                .and_then(LangFamily::from_extension);
            if let Some(lang) = lang {
                FILES_SCANNED.fetch_add(1, Relaxed);
                tokenizer::scan(&source, lang);
            }
        }
        if (idx + 1) % 10 == 0 {
            tracing::info!("--- after {} files ---", idx + 1);
            print_stats();
        }
    }

    eprintln!("--- final ---");
    print_stats();
}

const SOURCE_EXTENSIONS: &[&str] = &[
    "rs", "py", "js", "ts", "tsx", "go", "c", "h", "cpp", "cc", "hpp", "java", "cs", "rb", "jsx",
    "mjs",
];

fn collect_files(dir: &Path, files: &mut Vec<PathBuf>, depth: usize) {
    if depth > 20 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.starts_with('.')
            || matches!(
                name,
                "node_modules"
                    | "target"
                    | "vendor"
                    | "build"
                    | "__pycache__"
                    | "dist"
                    | "third_party"
                    | "testdata"
            )
        {
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
