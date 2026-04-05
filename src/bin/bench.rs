//! Standalone indexing benchmark for profiling with flamegraph/perf.
//!
//! Usage:
//!   cargo run --bin quicklsp-bench --release -- /path/to/repo
//!   cargo flamegraph --bin quicklsp-bench -- /path/to/repo
//!   perf record cargo run --bin quicklsp-bench --release -- /path/to/repo
//!
//! Phases (selectable via --phase):
//!   all        Run everything (default)
//!   index      Only workspace file indexing
//!   deps       Only dependency detection + indexing
//!   query      Index then run definition/reference/fuzzy queries
//!
//! The binary is intentionally minimal so flamegraphs show real bottlenecks,
//! not benchmark scaffolding.


use std::path::{Path, PathBuf};
use std::time::Instant;

use quicklsp::workspace::Workspace;
use quicklsp::DependencyIndex;

/// Read current process RSS from /proc/self/statm.
/// Returns (rss_bytes, total_vm_bytes) or None on non-Linux / error.
fn memory_rss() -> Option<(usize, usize)> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let mut fields = statm.split_whitespace();
    let vm_pages: usize = fields.next()?.parse().ok()?;
    let rss_pages: usize = fields.next()?.parse().ok()?;
    let page_size = 4096; // almost always 4K on Linux
    Some((rss_pages * page_size, vm_pages * page_size))
}

fn fmt_bytes(b: usize) -> String {
    if b < 1024 {
        format!("{b} B")
    } else if b < 1024 * 1024 {
        format!("{:.1} KB", b as f64 / 1024.0)
    } else if b < 1024 * 1024 * 1024 {
        format!("{:.1} MB", b as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", b as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

fn log_memory(label: &str) {
    if let Some((rss, vm)) = memory_rss() {
        tracing::info!("memory [{}]: rss={}, vm={}", label, fmt_bytes(rss), fmt_bytes(vm));
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let mut phase = "all";
    let mut dir: Option<&str> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--phase" => {
                i += 1;
                phase = args.get(i).map(|s| s.as_str()).unwrap_or("all");
            }
            "--help" | "-h" => {
                eprintln!("Usage: quicklsp-bench [--phase all|index|deps|query] <directory>");
                eprintln!();
                eprintln!("Phases:");
                eprintln!("  all     Full indexing + deps + queries (default)");
                eprintln!("  index   Workspace file indexing only");
                eprintln!("  deps    Dependency detection + indexing only");
                eprintln!("  query   Index then run queries (definitions, references, fuzzy)");
                std::process::exit(0);
            }
            s if !s.starts_with('-') => dir = Some(s),
            other => {
                eprintln!("Unknown flag: {other}");
                std::process::exit(1);
            }
        }
        i += 1;
    }

    let dir = dir.unwrap_or_else(|| {
        eprintln!("Usage: quicklsp-bench [--phase all|index|deps|query] <directory>");
        std::process::exit(1);
    });

    let root = PathBuf::from(dir);
    if !root.is_dir() {
        eprintln!("Not a directory: {dir}");
        std::process::exit(1);
    }

    // Initialize tracing so we can see what's happening
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("info".parse().unwrap()),
        )
        .with_writer(std::io::stderr)
        .init();

    match phase {
        "index" => run_index(&root),
        "deps" => run_deps(&root),
        "query" => run_query(&root),
        "all" => run_all(&root),
        other => {
            tracing::info!("Unknown phase: {other}");
            std::process::exit(1);
        }
    }
}

fn run_index(root: &Path) {
    log_memory("start");
    let ws = Workspace::new();
    let t = Instant::now();
    let stats = ws.scan_directory(root, None);
    let elapsed = t.elapsed();
    tracing::info!(
        "index: {} files, {} skipped, {} errors in {:.2?}",
        stats.indexed, stats.skipped, stats.errors, elapsed
    );
    tracing::info!(
        "  {} definitions, {} unique symbols",
        ws.definition_count(),
        ws.unique_symbol_count()
    );
    log_memory("after index");
}

fn run_deps(root: &Path) {
    log_memory("start");
    let dep_index = DependencyIndex::new();
    let t = Instant::now();
    dep_index.detect_and_resolve(root);
    let t_resolve = t.elapsed();
    tracing::info!("deps resolve: {:.2?}", t_resolve);

    let t = Instant::now();
    dep_index.index_pending(Some(&|done, total| {
        if done % 100 == 0 {
            tracing::info!("  deps indexed: {done}/{total}");
        }
    }));
    let t_index = t.elapsed();
    tracing::info!("deps index: {:.2?}", t_index);
    log_memory("after deps");
}

fn run_query(root: &Path) {
    // Index first
    let ws = Workspace::new();
    let stats = ws.scan_directory(root, None);
    tracing::info!("indexed {} files for query phase", stats.indexed);

    let names = ws.sample_symbol_names(500);
    tracing::info!("querying {} symbols", names.len());

    // Definition lookups
    let t = Instant::now();
    let mut total_defs = 0usize;
    for name in &names {
        total_defs += ws.find_definitions(name).len();
    }
    tracing::info!("definitions: {} found in {:.2?}", total_defs, t.elapsed());

    // Reference searches (expensive, do fewer)
    let t = Instant::now();
    let mut total_refs = 0usize;
    for name in names.iter().take(50) {
        total_refs += ws.find_references(name).len();
    }
    tracing::info!("references: {} found in {:.2?}", total_refs, t.elapsed());

    // Fuzzy search with typos
    let t = Instant::now();
    let mut total_fuzzy = 0usize;
    for name in names.iter().take(200) {
        if name.len() < 4 {
            continue;
        }
        let mut chars: Vec<char> = name.chars().collect();
        chars.swap(1, 2);
        let typo: String = chars.into_iter().collect();
        total_fuzzy += ws.search_symbols(&typo).len();
    }
    tracing::info!("fuzzy: {} results in {:.2?}", total_fuzzy, t.elapsed());
}

fn run_all(root: &Path) {
    log_memory("start");

    // Index once, reuse for queries
    let ws = Workspace::new();
    let t = Instant::now();
    let stats = ws.scan_directory(root, None);
    tracing::info!(
        "index: {} files, {} skipped, {} errors in {:.2?}",
        stats.indexed, stats.skipped, stats.errors, t.elapsed()
    );
    tracing::info!(
        "  {} definitions, {} unique symbols",
        ws.definition_count(),
        ws.unique_symbol_count()
    );
    log_memory("after index");

    let dep_index = DependencyIndex::new();
    let t = Instant::now();
    dep_index.detect_and_resolve(root);
    tracing::info!("deps resolve: {:.2?}", t.elapsed());

    let t = Instant::now();
    dep_index.index_pending(Some(&|done, total| {
        if done % 100 == 0 {
            tracing::info!("  deps indexed: {done}/{total}");
        }
    }));
    tracing::info!("deps index: {:.2?}", t.elapsed());
    tracing::info!(
        "  deps: {} definitions",
        dep_index.definition_count()
    );
    log_memory("after deps");

    let names = ws.sample_symbol_names(500);

    let t = Instant::now();
    for name in &names {
        ws.find_definitions(name);
    }
    tracing::info!("definitions: {:.2?}", t.elapsed());

    let t = Instant::now();
    for name in names.iter().take(50) {
        ws.find_references(name);
    }
    tracing::info!("references: {:.2?}", t.elapsed());

    // Fuzzy queries against local workspace
    let t = Instant::now();
    let mut fuzzy_count = 0;
    for name in names.iter().take(200) {
        if name.len() >= 4 {
            let mut chars: Vec<char> = name.chars().collect();
            chars.swap(1, 2);
            let typo: String = chars.into_iter().collect();
            ws.search_symbols(&typo);
            fuzzy_count += 1;
        }
    }
    tracing::info!("fuzzy (local, {} queries): {:.2?}", fuzzy_count, t.elapsed());

    // Fuzzy queries against dependency index
    let t = Instant::now();
    let mut dep_fuzzy_count = 0;
    for name in names.iter().take(200) {
        if name.len() >= 4 {
            let mut chars: Vec<char> = name.chars().collect();
            chars.swap(1, 2);
            let typo: String = chars.into_iter().collect();
            dep_index.completions(&typo);
            dep_fuzzy_count += 1;
        }
    }
    tracing::info!("fuzzy (deps, {} queries): {:.2?}", dep_fuzzy_count, t.elapsed());

    log_memory("end");

    // Memory breakdown
    tracing::info!("--- memory breakdown ---");
    let breakdown = ws.memory_breakdown();
    let mut total_measured = 0usize;
    for (name, bytes) in &breakdown {
        if name.starts_with("(count)") {
            tracing::info!("  {}: {}", name, bytes);
        } else {
            tracing::info!("  {}: {}", name, fmt_bytes(*bytes));
            total_measured += bytes;
        }
    }
    tracing::info!("  TOTAL measured: {}", fmt_bytes(total_measured));
}
