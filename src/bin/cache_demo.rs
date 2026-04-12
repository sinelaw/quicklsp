//! End-user-style driver that exercises the cache v3 objectives on a real
//! repository and prints the resulting counter snapshots.
//!
//! Usage:
//!   quicklsp-cache-demo <scenario> <path> [extra-path]
//!
//! Scenarios:
//!   cold <repo>                         cold scan (baseline)
//!   rescan <repo>                       cold then re-scan same tree
//!   clone <repo-a> <repo-b>             scan A, then scan B (identical content)
//!   subsume-parent <parent> <child>     scan child, then scan parent
//!   subsume-child <parent> <child>      scan parent, then scan child
//!   edit <repo>                         scan, mutate 5% of files, re-scan

use std::path::{Path, PathBuf};
use std::time::Instant;

use quicklsp::Workspace;

fn fmt_bytes(b: u64) -> String {
    if b < 1024 {
        format!("{b} B")
    } else if b < 1024 * 1024 {
        format!("{:.1} KiB", b as f64 / 1024.0)
    } else {
        format!("{:.2} MiB", b as f64 / (1024.0 * 1024.0))
    }
}

fn report(label: &str, ws: &Workspace, elapsed_ms: u128) {
    let s = ws.metrics().snapshot();
    println!("── {label} ────────────────────────────────");
    println!("  wall_ms           : {elapsed_ms}");
    println!("  files_stat_called : {}", s.files_stat_called);
    println!("  files_bytes_read  : {}", fmt_bytes(s.files_bytes_read));
    println!("  files_blake3_hashed: {}", s.files_blake3_hashed);
    println!("  files_parsed      : {}", s.files_parsed);
    println!("  layer_a_hits      : {}", s.layer_a_hits);
    println!("  layer_a_writes    : {}", s.layer_a_writes);
    println!("  manifest_rows_copied: {}", s.manifest_rows_copied);
    let stat = s.files_stat_called as f64;
    if stat > 0.0 {
        let reparse_ratio = (s.files_parsed as f64) / stat;
        let hash_savings = 1.0 - (s.files_blake3_hashed as f64) / stat;
        let hit_rate = if s.layer_a_hits + s.layer_a_writes > 0 {
            (s.layer_a_hits as f64) / ((s.layer_a_hits + s.layer_a_writes) as f64)
        } else {
            0.0
        };
        println!(
            "  reparse_ratio     : {reparse_ratio:.3}   hash_savings: {hash_savings:.3}   layer_a_hit_rate: {hit_rate:.3}"
        );
    }
    println!("  files indexed     : {}", ws.file_count());
    println!("  unique symbols    : {}", ws.unique_symbol_count());
}

fn scan(label: &str, ws: &Workspace, root: &Path) {
    ws.metrics().reset();
    let t = Instant::now();
    let stats = ws.scan_directory(root, None);
    let ms = t.elapsed().as_millis();
    report(label, ws, ms);
    println!(
        "  ScanStats: indexed={} skipped={} errors={}",
        stats.indexed, stats.skipped, stats.errors
    );
    println!();
}

fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let ft = entry.file_type().unwrap();
        let to = dst.join(entry.file_name());
        if ft.is_dir() {
            copy_tree(&entry.path(), &to);
        } else if ft.is_file() {
            std::fs::copy(entry.path(), to).unwrap();
        }
    }
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: {} <scenario> <path> [extra-path]", args[0]);
        std::process::exit(2);
    }
    let scenario = args[1].clone();
    let p1 = PathBuf::from(&args[2]);

    match scenario.as_str() {
        "cold" => {
            let ws = Workspace::new();
            scan("cold scan", &ws, &p1);
        }
        "rescan" => {
            let ws = Workspace::new();
            scan("[1] cold scan", &ws, &p1);
            scan("[2] rescan (stat-fresh)", &ws, &p1);
        }
        "clone" => {
            let p2 = PathBuf::from(&args[3]);
            let ws_a = Workspace::new();
            scan("[1] cold scan of A", &ws_a, &p1);
            let ws_b = Workspace::new();
            scan("[2] cold scan of B (should reuse Layer A)", &ws_b, &p2);
        }
        "subsume-parent" => {
            // args[2] = parent, args[3] = child
            let child = PathBuf::from(&args[3]);
            let ws_c = Workspace::new();
            scan("[1] scan child subtree", &ws_c, &child);
            drop(ws_c);
            let ws_p = Workspace::new();
            scan("[2] scan parent (expect subsumption)", &ws_p, &p1);
        }
        "subsume-child" => {
            let child = PathBuf::from(&args[3]);
            let ws_p = Workspace::new();
            scan("[1] scan parent", &ws_p, &p1);
            drop(ws_p);
            let ws_c = Workspace::new();
            scan("[2] scan child (expect subsumption)", &ws_c, &child);
        }
        "edit" => {
            let ws = Workspace::new();
            scan("[1] cold scan", &ws, &p1);
            // Append a unique comment line to ~5% of the .rs files.
            let mut to_edit: Vec<PathBuf> = Vec::new();
            collect_rs(&p1, &mut to_edit);
            let k = (to_edit.len() / 20).max(1);
            for p in to_edit.iter().take(k) {
                let s = std::fs::read_to_string(p).unwrap();
                std::fs::write(p, format!("{s}\n// quicklsp-cache-demo edit\n")).unwrap();
                // Force a future mtime.
                let t = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
                set_mtime(p, t);
            }
            println!("(edited {k} files)\n");
            scan("[2] rescan after edits", &ws, &p1);
        }
        "copy-tree" => {
            // Utility: copy args[2] to args[3].
            let p2 = PathBuf::from(&args[3]);
            copy_tree(&p1, &p2);
            println!("copied {} -> {}", p1.display(), p2.display());
        }
        other => {
            eprintln!("unknown scenario: {other}");
            std::process::exit(2);
        }
    }
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            let skip = p
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n == ".git" || n == "target")
                .unwrap_or(false);
            if skip {
                continue;
            }
            collect_rs(&p, out);
        } else if p.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(p);
        }
    }
}

#[cfg(unix)]
fn set_mtime(path: &Path, t: std::time::SystemTime) {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = CString::new(path.as_os_str().as_bytes()).unwrap();
    let d = t.duration_since(std::time::UNIX_EPOCH).unwrap();
    let tv = libc::timespec {
        tv_sec: d.as_secs() as libc::time_t,
        tv_nsec: d.subsec_nanos() as _,
    };
    let times = [tv, tv];
    unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), times.as_ptr(), 0) };
}
