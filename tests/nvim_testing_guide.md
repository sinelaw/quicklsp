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

## Progress Reporting

QuickLSP sends `$/progress` notifications during indexing. To see them
visually, install [fidget.nvim](https://github.com/j-hui/fidget.nvim):

```lua
-- Add to runtimepath and setup
vim.opt.rtp:prepend('/path/to/fidget.nvim')
require('fidget').setup({})
```

Without fidget.nvim, vanilla neovim has no built-in display for progress
notifications. You can inspect them programmatically:

```vim
:lua local c = vim.lsp.get_clients({name="quicklsp"})[1]
:lua for token, data in pairs(c.progress) do print(token, vim.inspect(data)) end
```

### Cold start on large repos

On the Linux kernel (~65K files), the initial scan takes ~50 seconds.
With fidget.nvim, you'll see:

```
Scanning: 28500/64857 files (43%) Indexing
                                  quicklsp ⠴
```

Clear the cache to force a cold start: `rm -rf ~/.cache/quicklsp/`

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
During indexing, expect high CPU usage (multi-core parallel scan).
After indexing completes, CPU should drop to ~0%.

### Server Messages
```vim
:lua local c = vim.lsp.get_clients({name="quicklsp"})[1]; print(vim.inspect(c.messages))
```

## tmux-driven Testing

To test programmatically (e.g., in CI or automated scripts):

```sh
tmux new-session -d -s test -x 200 -y 50
tmux send-keys -t test "nvim -u /path/to/test-init.lua src/main.rs" Enter
sleep 3
```

### Effective patterns for tmux testing

**Always use `-u` with a minimal config** to avoid interference from the
user's real config. Include fidget.nvim in the test config for progress
visibility.

**Use VimEnter autocmd** for the first file, since FileType fires before
`init.lua` is sourced for the initial buffer:

```lua
vim.api.nvim_create_autocmd('VimEnter', {
  callback = function()
    if vim.bo.filetype == 'c' then
      vim.lsp.start({ name = 'quicklsp', cmd = { ... }, root_dir = '...' })
    end
  end,
})
```

**Check LSP state via `:lua`**, not by visual inspection of floating
windows. `tmux capture-pane` cannot see nvim floating windows (hover
popups, fidget spinners) — they exist only in nvim's internal compositing:

```sh
# Check client is attached
tmux send-keys -t test ':lua print(#vim.lsp.get_clients({name="quicklsp"}))' Enter

# Check progress notifications are arriving
tmux send-keys -t test ':lua local c = vim.lsp.get_clients({name="quicklsp"})[1]; for _, d in pairs(c.progress) do print(vim.inspect(d)) end' Enter

# Test completion — type text then trigger omnifunc
tmux send-keys -t test 'o' 'mutex_' 'C-x C-o'
sleep 2
tmux capture-pane -t test -p | tail -15  # completion menu IS visible in capture

# Test definition jump
tmux send-keys -t test '/sched_init' Enter 'gd'
sleep 2
tmux capture-pane -t test -p | tail -5  # shows the destination file
```

**Kill old quicklsp processes** before re-testing, and **clear the cache**
for cold start testing:

```sh
pkill -f "target/release/quicklsp"
rm -rf ~/.cache/quicklsp/
```

## Automated Integration Tests

The `tests/lsp_integration.rs` file provides automated coverage of the
full LSP lifecycle without needing neovim. Run with:

```sh
cargo test --test lsp_integration -- --nocapture
```

These tests spawn quicklsp as a subprocess, communicate via JSON-RPC, and
verify: initialization, progress notifications, go-to-definition,
references, document symbols, workspace symbols, completion, and hover.
