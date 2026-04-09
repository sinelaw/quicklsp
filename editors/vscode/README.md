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

## Installing

### From a release `.vsix` (recommended)

Every GitHub release publishes a **platform-specific** `.vsix` with the
matching `quicklsp` binary pre-bundled. No separate install step is needed —
the extension finds the server inside its own install directory.

Grab the one for your platform from
[Releases](https://github.com/sinelaw/quicklsp/releases):

| Platform                | Asset                                             |
| ----------------------- | ------------------------------------------------- |
| Linux x86_64            | `quicklsp-vscode-<ver>-linux-x64.vsix`            |
| Linux aarch64           | `quicklsp-vscode-<ver>-linux-arm64.vsix`          |
| macOS Intel             | `quicklsp-vscode-<ver>-darwin-x64.vsix`           |
| macOS Apple Silicon     | `quicklsp-vscode-<ver>-darwin-arm64.vsix`         |
| Windows x86_64          | `quicklsp-vscode-<ver>-win32-x64.vsix`            |

Then install:

```sh
code --install-extension quicklsp-vscode-<ver>-<target>.vsix
```

Once the extension is published to the VS Code Marketplace, the Marketplace
will auto-deliver the correct platform-specific build.

> **macOS note:** The bundled Darwin binary is not notarized. On first launch
> macOS Gatekeeper may block it — open *System Settings → Privacy & Security*
> and click **Allow anyway**, or set `quicklsp.serverPath` to a locally-built
> binary.

### From source (developers)

If you have a Rust toolchain and want the extension to pick up your own build:

```sh
# Build the server
cargo build --release

# Build & install the extension (without a bundled binary)
cd editors/vscode
npm install
npm run compile
npm run package
code --install-extension quicklsp-vscode.vsix
```

With no bundled `server/` directory the extension falls back to
`<repo>/target/release/quicklsp` when its install directory is inside the
repo (useful when launching the Extension Development Host via `F5`), or to
`quicklsp` on `PATH` as a last resort.

## Settings

| Setting                 | Default                                | Description                                                    |
| ----------------------- | -------------------------------------- | -------------------------------------------------------------- |
| `quicklsp.serverPath`   | `""` (auto-detect)                    | Override the server binary location. Leave empty to use the bundled binary, the repo-local `target/release/quicklsp`, or `PATH` (in that order). |
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
