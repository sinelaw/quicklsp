use quicklsp::{DependencyIndex, Workspace};
use std::path::{Path, PathBuf};

fn main() {
    // Simulate what the LSP server does
    let ws = Workspace::new();
    let dep = DependencyIndex::new();

    // Index our workspace (like scan_directory does)
    let root = Path::new("/home/user/quicklsp");
    ws.scan_directory(root, None);

    // Index deps
    dep.detect_and_resolve(root);
    dep.index_pending(None);

    // Now simulate goto_definition for "ServerCapabilities"
    let name = "ServerCapabilities";
    let ws_defs = ws.find_definitions(name);
    println!(
        "workspace.find_definitions({name}): {} results",
        ws_defs.len()
    );
    for d in &ws_defs {
        println!(
            "  {} line {} in {}",
            d.symbol.def_keyword,
            d.symbol.line,
            d.file.display()
        );
    }

    if ws_defs.is_empty() {
        let dep_defs = dep.find_definitions(name);
        println!(
            "dep_index.find_definitions({name}): {} results",
            dep_defs.len()
        );
        for d in &dep_defs {
            println!(
                "  {} line {} in {}",
                d.symbol.def_keyword,
                d.symbol.line,
                d.file.display()
            );
        }
    }

    // Also check Client
    let name = "Client";
    let ws_defs = ws.find_definitions(name);
    println!(
        "\nworkspace.find_definitions({name}): {} results",
        ws_defs.len()
    );
    if ws_defs.is_empty() {
        let dep_defs = dep.find_definitions(name);
        println!(
            "dep_index.find_definitions({name}): {} results",
            dep_defs.len()
        );
    }
}
