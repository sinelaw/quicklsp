-- Headless Neovim demo that drives QuickLSP against a ripgrep file.
--
-- Runs the same operations a human would: opens a real source file,
-- waits for the server to attach, asks for the definition of a symbol,
-- asks for references, and does a workspace-symbol search. Prints
-- timing and result counts to stdout; exits when done.
--
-- Usage:
--     nvim --headless -u scripts/field_demo/nvim_config/init.lua \
--          -l scripts/field_demo/nvim_demo.lua -- /tmp/ripgrep

-- ── Argument parsing ──────────────────────────────────────────────────

-- Neovim's `-l` mode puts script args in `_G.arg`. With some invocations
-- a literal `--` separator remains as the first element; skip it.
local args = _G.arg or {}
local repo
for i = 1, #args do
    if args[i] ~= "--" and args[i] ~= "" then
        repo = args[i]
        break
    end
end
if not repo then
    io.stderr:write("usage: nvim -l nvim_demo.lua <repo-path>\n")
    os.exit(2)
end

local function log(msg)
    io.stdout:write("[demo] " .. msg .. "\n")
    io.stdout:flush()
end

local file = repo .. "/crates/searcher/src/searcher/mod.rs"

-- ── Open file (this fires autocommands → LSP attach) ───────────────────

local t0 = vim.uv.hrtime()
vim.cmd("edit " .. vim.fn.fnameescape(file))
-- Force filetype detection (headless mode does not always run it).
vim.cmd("filetype detect")
-- Kick lazy.nvim into loading deferred plugins so nvim-lspconfig's
-- FileType autocmd actually fires.
vim.cmd("doautoall FileType")

local loaded_name = vim.api.nvim_buf_get_name(0)
local loaded_ft = vim.bo.filetype
log("opened: " .. loaded_name .. " ft=" .. loaded_ft)

-- ── Wait for the server to attach ──────────────────────────────────────

local function wait_for_lsp(bufnr, timeout_ms)
    local deadline = vim.uv.hrtime() + timeout_ms * 1e6
    while vim.uv.hrtime() < deadline do
        local clients = vim.lsp.get_clients({ bufnr = bufnr, name = "quicklsp" })
        if #clients > 0 then
            return clients[1]
        end
        vim.wait(50)
    end
    return nil
end

local bufnr = vim.api.nvim_get_current_buf()
local client = wait_for_lsp(bufnr, 30000)
if not client then
    log("ERROR: quicklsp did not attach within 30s")
    os.exit(3)
end
local t_attach_ms = (vim.uv.hrtime() - t0) / 1e6
log(string.format("LSP attached in %.0f ms", t_attach_ms))

-- ── Wait for the first successful workspace/symbol response ───────────

-- Poll until workspace/symbol returns at least `min_hits` — this means
-- the background workspace scan has caught up (rather than just the
-- currently-opened buffer having been indexed via didOpen).
local function wait_for_workspace_ready(query, min_hits, timeout_ms)
    local deadline = vim.uv.hrtime() + timeout_ms * 1e6
    local last_count = 0
    while vim.uv.hrtime() < deadline do
        local results, err = vim.lsp.buf_request_sync(
            bufnr, "workspace/symbol", { query = query }, 1000
        )
        local count = 0
        if not err and results then
            for _, r in pairs(results) do
                if r.result then count = #r.result end
            end
        end
        if count ~= last_count then
            log(string.format("  scan progress: %d hits at %.0f ms",
                count, (vim.uv.hrtime() - t0) / 1e6))
            last_count = count
        end
        if count >= min_hits then
            return count, (vim.uv.hrtime() - t0) / 1e6
        end
        vim.wait(80)
    end
    return last_count, nil
end

local syms_count, t_first = wait_for_workspace_ready("Searcher", 3, 25000)
if not t_first then
    log(string.format("ERROR: workspace never reached %d hits (stuck at %d)",
        3, syms_count or 0))
    os.exit(4)
end
log(string.format(
    "workspace scan matured: %.0f ms total  (%d hits for 'Searcher')",
    t_first, syms_count
))

-- Now fetch the matured results for display.
local results = vim.lsp.buf_request_sync(
    bufnr, "workspace/symbol", { query = "Searcher" }, 1000) or {}
local syms = {}
for _, r in pairs(results) do
    if r.result then syms = r.result; break end
end
for i = 1, math.min(5, #syms) do
    local s = syms[i]
    local loc = s.location or {}
    local uri = (loc.uri or ""):gsub("^file://", "")
    local line = (loc.range and loc.range.start and loc.range.start.line or 0) + 1
    log(string.format("  - %s at %s:%d", s.name or "?", uri, line))
end

-- ── textDocument/definition on a Searcher call site ───────────────────

-- Find a line calling SearcherBuilder::new(); point the cursor at it.
local target_line, target_col
for lnum = 1, vim.api.nvim_buf_line_count(bufnr) do
    local line = vim.api.nvim_buf_get_lines(bufnr, lnum - 1, lnum, false)[1] or ""
    if line:find("SearcherBuilder::new") and lnum > 20 then
        target_line = lnum - 1
        target_col = (line:find("SearcherBuilder") or 1) + 3 - 1
        break
    end
end
if target_line then
    vim.api.nvim_win_set_cursor(0, { target_line + 1, target_col })
    local td = { uri = "file://" .. file,
                 -- textDocument/definition takes a TextDocumentIdentifier
               }
    local params = {
        textDocument = { uri = "file://" .. file },
        position = { line = target_line, character = target_col },
    }
    local t1 = vim.uv.hrtime()
    local r, err = vim.lsp.buf_request_sync(bufnr, "textDocument/definition", params, 2000)
    local dt = (vim.uv.hrtime() - t1) / 1e6
    local def = nil
    if r then
        for _, resp in pairs(r) do
            if resp.result then
                def = type(resp.result) == "table" and (resp.result[1] or resp.result) or nil
                break
            end
        end
    end
    if def and def.range then
        local uri = (def.uri or ""):gsub("^file://", "")
        log(string.format("textDocument/definition: %.0f ms → %s:%d",
            dt, uri, def.range.start.line + 1))
    else
        log("textDocument/definition: no result (err=" .. tostring(err) .. ")")
    end
end

-- ── textDocument/references on the struct Searcher ────────────────────

local sline, scol
for lnum = 1, vim.api.nvim_buf_line_count(bufnr) do
    local line = vim.api.nvim_buf_get_lines(bufnr, lnum - 1, lnum, false)[1] or ""
    if line:match("^pub struct Searcher ") then
        sline = lnum - 1
        scol = line:find("Searcher") - 1
        break
    end
end
if sline then
    local params = {
        textDocument = { uri = "file://" .. file },
        position = { line = sline, character = scol },
        context = { includeDeclaration = true },
    }
    local t1 = vim.uv.hrtime()
    local r = vim.lsp.buf_request_sync(bufnr, "textDocument/references", params, 2000)
    local dt = (vim.uv.hrtime() - t1) / 1e6
    local refs = {}
    if r then
        for _, resp in pairs(r) do
            if resp.result then refs = resp.result; break end
        end
    end
    log(string.format("textDocument/references: %.0f ms → %d refs",
        dt, #refs))
    for i = 1, math.min(3, #refs) do
        local rref = refs[i]
        local uri = (rref.uri or ""):gsub("^file://", ""):gsub("^" .. repo .. "/", "")
        log(string.format("  - %s:%d", uri, (rref.range.start.line or 0) + 1))
    end
end

log("done")
vim.cmd("qa!")
