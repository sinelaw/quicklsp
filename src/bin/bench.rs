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
            tracing_subscriber::EnvFilter::from_default_env(),
        )
        .with_writer(std::io::stderr)
        .init();

    match phase {
        "index" => run_index(&root),
        "deps" => run_deps(&root),
        "query" => run_query(&root),
        "all" => run_all(&root),
        other => {
            eprintln!("Unknown phase: {other}");
            std::process::exit(1);
        }
    }
}

fn run_index(root: &Path) {
    let ws = Workspace::new();
    let t = Instant::now();
    let stats = ws.scan_directory(root);
    let elapsed = t.elapsed();
    eprintln!(
        "index: {} files, {} skipped, {} errors in {:.2?}",
        stats.indexed, stats.skipped, stats.errors, elapsed
    );
    eprintln!(
        "  {} definitions, {} unique symbols",
        ws.definition_count(),
        ws.unique_symbol_count()
    );
}

fn run_deps(root: &Path) {
    let dep_index = DependencyIndex::new();
    let t = Instant::now();
    dep_index.detect_and_resolve(root);
    let t_resolve = t.elapsed();
    eprintln!("deps resolve: {:.2?}", t_resolve);

    let t = Instant::now();
    dep_index.index_pending(Some(&|done, total| {
        if done % 100 == 0 {
            eprintln!("  deps indexed: {done}/{total}");
        }
    }));
    let t_index = t.elapsed();
    eprintln!("deps index: {:.2?}", t_index);
}

fn run_query(root: &Path) {
    // Index first
    let ws = Workspace::new();
    let stats = ws.scan_directory(root);
    eprintln!("indexed {} files for query phase", stats.indexed);

    // Collect symbol names to query
    let names = collect_symbol_names(&ws, root, 500);
    eprintln!("querying {} symbols", names.len());

    // Definition lookups
    let t = Instant::now();
    let mut total_defs = 0usize;
    for name in &names {
        total_defs += ws.find_definitions(name).len();
    }
    eprintln!("definitions: {} found in {:.2?}", total_defs, t.elapsed());

    // Reference searches (expensive, do fewer)
    let t = Instant::now();
    let mut total_refs = 0usize;
    for name in names.iter().take(50) {
        total_refs += ws.find_references(name).len();
    }
    eprintln!("references: {} found in {:.2?}", total_refs, t.elapsed());

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
    eprintln!("fuzzy: {} results in {:.2?}", total_fuzzy, t.elapsed());
}

fn run_all(root: &Path) {
    run_index(root);
    run_deps(root);

    // Re-index for queries (keeps profiling clean per-phase)
    let ws = Workspace::new();
    ws.scan_directory(root);
    let names = collect_symbol_names(&ws, root, 500);

    let t = Instant::now();
    for name in &names {
        ws.find_definitions(name);
    }
    eprintln!("definitions: {:.2?}", t.elapsed());

    let t = Instant::now();
    for name in names.iter().take(50) {
        ws.find_references(name);
    }
    eprintln!("references: {:.2?}", t.elapsed());

    let t = Instant::now();
    for name in names.iter().take(200) {
        if name.len() >= 4 {
            let mut chars: Vec<char> = name.chars().collect();
            chars.swap(1, 2);
            let typo: String = chars.into_iter().collect();
            ws.search_symbols(&typo);
        }
    }
    eprintln!("fuzzy: {:.2?}", t.elapsed());
}

fn collect_symbol_names(ws: &Workspace, root: &Path, max: usize) -> Vec<String> {
    let mut names = Vec::new();
    collect_names_recursive(ws, root, &mut names, max);
    names
}

fn collect_names_recursive(ws: &Workspace, dir: &Path, names: &mut Vec<String>, max: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if names.len() >= max {
            return;
        }
        let path = entry.path();
        if path.is_dir() {
            collect_names_recursive(ws, &path, names, max);
        } else if path.is_file() {
            for s in ws.file_symbols(&path) {
                if !names.contains(&s.name) {
                    names.push(s.name);
                }
                if names.len() >= max {
                    return;
                }
            }
        }
    }
}
