# QuickLSP for VS Code

Registers [quicklsp](https://github.com/sinelaw/quicklsp) — a high-performance,
heuristic-driven universal Language Server — as the LSP for every language it
supports:

| Language       | VS Code language IDs                  | File extensions                         |
| -------------- | ------------------------------------- | --------------------------------------- |
| C              | `c`                                   | `.c`, `.h`                              |
| C++            | `cpp`                                 | `.cpp`, `.cc`, `.cxx`, `.hpp`, `.hxx`   |
| Rust           | `rust`                                | `.rs`                                   |
| Go             | `go`                                  | `.go`                                   |
| Python         | `python`                              | `.py`, `.pyi`                           |
| JavaScript     | `javascript`, `javascriptreact`       | `.js`, `.jsx`, `.mjs`, `.cjs`           |
| TypeScript     | `typescript`, `typescriptreact`       | `.ts`, `.mts`, `.tsx`                   |
| Java           | `java`                                | `.java`                                 |
| Ruby           | `ruby`                                | `.rb`                                   |

> **Experimental — not ready for production use or any serious purpose.**

## Prerequisites

You need the `quicklsp` binary available on your system. From the repo root:

```sh
cargo install --path .
```

This installs `quicklsp` into `~/.cargo/bin` (make sure that's on your `PATH`).
Alternatively, build with `cargo build --release` and point
`quicklsp.serverPath` at `target/release/quicklsp`.

## Building the extension

From `editors/vscode`:

```sh
npm install
npm run compile
```

To produce a `.vsix` package:

```sh
npm install -g @vscode/vsce   # once
npm run package
```

Install the resulting `.vsix` via `code --install-extension quicklsp-vscode-*.vsix`
or through the Extensions view (`Install from VSIX...`).

### Running from source

Open `editors/vscode` in VS Code and press `F5` to launch an Extension
Development Host with QuickLSP loaded.

## Settings

| Setting                 | Default                                | Description                                                    |
| ----------------------- | -------------------------------------- | -------------------------------------------------------------- |
| `quicklsp.serverPath`   | `quicklsp`                             | Path to the `quicklsp` executable (absolute, relative, or on `PATH`). |
| `quicklsp.serverArgs`   | `[]`                                   | Extra CLI args passed to the server.                           |
| `quicklsp.logLevel`     | `info`                                 | `RUST_LOG` level forwarded to the server process.              |
| `quicklsp.trace.server` | `off`                                  | LSP message tracing (`off` / `messages` / `verbose`).          |
| `quicklsp.languages`    | all supported languages                | VS Code language IDs for which QuickLSP should be activated.   |

## Commands

- **QuickLSP: Restart Language Server** — restart the server (e.g. after
  rebuilding the binary).
- **QuickLSP: Show Output Channel** — open the QuickLSP output channel.

## Features

Matches the capabilities advertised by the server (`src/lsp/server.rs`):

- Go to Definition
- Find References
- Document & Workspace Symbols
- Hover
- Signature Help
- Completion
