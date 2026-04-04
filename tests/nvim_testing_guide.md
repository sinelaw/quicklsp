# Testing QuickLSP with Neovim

Guide for manually testing the quicklsp LSP server using Neovim.

## Prerequisites

- Neovim 0.9+ (`nvim --version`)
- quicklsp binary built: `cargo build --release`
- Optional: [fidget.nvim](https://github.com/j-hui/fidget.nvim) for progress display

## Minimal Neovim Config

Create `~/.config/nvim/init.lua` (or use `nvim -u /path/to/init.lua`):

```lua
-- Optional: fidget.nvim for $/progress display
pcall(function() require('fidget').setup({}) end)

-- Auto-attach quicklsp to Rust buffers
vim.api.nvim_create_autocmd('FileType', {
  pattern = 'rust',
  callback = function(args)
    vim.lsp.start({
      name = 'quicklsp',
      cmd = { '/path/to/quicklsp/target/release/quicklsp' },
      root_dir = '/path/to/project',
    }, { bufnr = args.buf })
  end,
})

-- Keymaps
vim.keymap.set('n', 'gd', vim.lsp.buf.definition)
vim.keymap.set('n', 'K', vim.lsp.buf.hover)
vim.keymap.set('n', 'gr', vim.lsp.buf.references)
vim.keymap.set('n', '<leader>ds', vim.lsp.buf.document_symbol)
vim.keymap.set('n', '<leader>ws', vim.lsp.buf.workspace_symbol)
vim.keymap.set('n', '<leader>sh', vim.lsp.buf.signature_help)

-- For completion via Ctrl-X Ctrl-O
vim.api.nvim_create_autocmd('LspAttach', {
  callback = function(args)
    vim.bo[args.buf].omnifunc = 'v:lua.vim.lsp.omnifunc'
  end,
})
```

## Known Config Pitfalls

### FileType autocmd doesn't fire for the first file

Neovim detects the filetype *before* sourcing `init.lua`, so the first
file opened on the command line (`nvim src/foo.rs`) won't trigger the
`FileType` autocmd. Workarounds:

1. **Re-trigger filetype**: `:set filetype=rust` after opening
2. **Manual start**: `:lua vim.lsp.start({ name = "quicklsp", cmd = { ... }, root_dir = "..." })`
3. **Use `VimEnter` instead**: Register a `VimEnter` autocmd that checks if a Rust buffer is already open

### Buffers opened with `:e` don't auto-attach

When you `:e another_file.rs`, the LSP client may not auto-attach to the
new buffer. Fix: the `FileType` autocmd above handles this for subsequent
files, but if it doesn't fire, manually attach:

```vim
:lua vim.lsp.buf_attach_client(0, 1)
```

(where `1` is the client ID — check with `:lua print(vim.lsp.get_active_clients()[1].id)`)

### Default `K` mapping opens man pages

Neovim's default `K` mapping runs `:Man` for keyword lookup. The keymap
in the config above overrides it, but if your config doesn't load
properly, `K` will show a man page error instead of LSP hover. Use
`:lua vim.lsp.buf.hover()` directly to test.

## Testing Checklist

Open a Rust file in the quicklsp repo and verify each feature:

### 1. Server Initialization
```vim
:lua print(#vim.lsp.get_active_clients({name="quicklsp"}))
```
Should print `1`. If `0`, the client didn't start — check the config.

### 2. Hover (K or `:lua vim.lsp.buf.hover()`)
- Place cursor on `Workspace` → should show `pub struct Workspace` + doc comment
- Place cursor on `new` in `Workspace::new()` → should show `pub fn new() -> Self`
  (tests qualifier-aware ranking — should NOT show `Mutex::new` or `Arc::new`)

### 3. Go to Definition (gd)
- On `Workspace` in server.rs → should jump to `src/workspace.rs`
- On `new` in `Workspace::new()` → should jump to `Workspace`'s `new()` method
- On `scan_directory` → should jump to `workspace.rs:124`

### 4. References (gr)
- On `client` field → should show quickfix list with all usages

### 5. Document Symbols (`\ds`)
- Should list all structs/functions in the current file in a quickfix list

### 6. Workspace Symbols (`\ws`)
- Prompts for a query, e.g. `DependencyIndex` → shows location in quickfix

### 7. Completion (Ctrl-X Ctrl-O in insert mode)
- Type a partial identifier and trigger omnifunc completion
- Requires `omnifunc` to be set (handled by `LspAttach` autocmd above)

### 8. Progress Reporting
Check that `$/progress` notifications arrive during startup:
```vim
:lua local c = vim.lsp.get_active_clients({name="quicklsp"})[1]
:lua print(vim.inspect(c.messages.progress))
```
Should show a `quicklsp/indexing` entry with title, message, and percentage.
With fidget.nvim installed, this appears as a spinner in the bottom-right corner.

## Monitoring

### LSP Log
```vim
:lua print(vim.lsp.get_log_path())
```
Set `vim.lsp.set_log_level('debug')` in init.lua for verbose logging.

### Server Process
```sh
ps aux | grep quicklsp
```
During dependency indexing, expect 100% CPU for several minutes.

### Server Messages
```vim
:lua local c = vim.lsp.get_active_clients({name="quicklsp"})[1]; print(vim.inspect(c.messages))
```

## tmux-driven Testing

To test programmatically (e.g., in CI or automated scripts):

```sh
tmux new-session -d -s test -x 200 -y 50
tmux send-keys -t test "nvim src/main.rs" Enter
sleep 3
# Start LSP manually (FileType autocmd may not fire for first file)
tmux send-keys -t test ':lua vim.lsp.start({name="quicklsp", cmd={"/path/to/quicklsp"}, root_dir="/path/to/repo"})' Enter
sleep 5
# Test hover
tmux send-keys -t test ':18' Enter    # go to line
tmux send-keys -t test 'w'            # move to first word
tmux send-keys -t test ':lua vim.lsp.buf.hover()' Enter
sleep 2
tmux capture-pane -t test -e -p       # capture with ANSI escapes
```

**Note**: `tmux capture-pane` cannot capture nvim floating windows (hover
popups, fidget progress). Floating window content only exists in nvim's
internal compositing layer. Use `:lua` commands to inspect data programmatically
instead of relying on visual capture.
