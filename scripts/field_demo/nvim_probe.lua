-- Probe the LSP server's capabilities + behavior on a real file, then
-- exercise each common user operation and report quirks relative to
-- mainstream servers like rust-analyzer.

local args = _G.arg or {}
local function arg_nth(n)
    local i = 0
    for _, a in ipairs(args) do
        if a ~= "--" and a ~= "" then
            i = i + 1
            if i == n then return a end
        end
    end
end

local repo = arg_nth(1) or "/tmp/quicklsp-field-test/ripgrep"
local file = repo .. "/crates/searcher/src/searcher/mod.rs"

local function log(s) io.stdout:write(s .. "\n"); io.stdout:flush() end

local t0 = vim.uv.hrtime()
vim.cmd("edit " .. vim.fn.fnameescape(file))
vim.cmd("filetype detect")
vim.cmd("doautoall FileType")

local bufnr = vim.api.nvim_get_current_buf()

-- Wait for LSP to be usable (workspace scan matured).
local function wait_ready(timeout_ms)
    local deadline = vim.uv.hrtime() + timeout_ms * 1e6
    while vim.uv.hrtime() < deadline do
        local cs = vim.lsp.get_clients({ bufnr = bufnr, name = "quicklsp" })
        if #cs > 0 then
            local r = vim.lsp.buf_request_sync(bufnr, "workspace/symbol",
                { query = "Searcher" }, 1000) or {}
            for _, resp in pairs(r) do
                if resp.result and #resp.result >= 3 then
                    return cs[1]
                end
            end
        end
        vim.wait(80)
    end
    return nil
end

local client = wait_ready(30000)
if not client then log("FAIL: LSP never matured"); os.exit(1) end

log("## Advertised capabilities")
log("  (only methods we test are listed)")
local caps = client.server_capabilities or {}
local function has(cap) return caps[cap] and "YES" or "—" end
log(string.format("  hover ............... %s", has("hoverProvider")))
log(string.format("  definition .......... %s", has("definitionProvider")))
log(string.format("  references .......... %s", has("referencesProvider")))
log(string.format("  documentSymbol ...... %s", has("documentSymbolProvider")))
log(string.format("  workspaceSymbol ..... %s", has("workspaceSymbolProvider")))
log(string.format("  completion .......... %s", has("completionProvider")))
log(string.format("  signatureHelp ....... %s", has("signatureHelpProvider")))
log(string.format("  rename .............. %s",
    caps.renameProvider and "YES" or "NO"))
log(string.format("  codeAction .......... %s",
    caps.codeActionProvider and "YES" or "NO"))
log(string.format("  formatting .......... %s",
    caps.documentFormattingProvider and "YES" or "NO"))
log(string.format("  semanticTokens ...... %s",
    caps.semanticTokensProvider and "YES" or "NO"))
log(string.format("  inlayHint ........... %s",
    caps.inlayHintProvider and "YES" or "NO"))
log(string.format("  typeDefinition ...... %s",
    caps.typeDefinitionProvider and "YES" or "NO"))
log(string.format("  implementation ...... %s",
    caps.implementationProvider and "YES" or "NO"))
log(string.format("  callHierarchy ....... %s",
    caps.callHierarchyProvider and "YES" or "NO"))
log(string.format("  publishDiagnostics .. (server-push; not a static capability)"))
log("")

-- ── Live-exercise each provider ───────────────────────────────────────

local function req(method, params, timeout)
    local r = vim.lsp.buf_request_sync(bufnr, method, params, timeout or 2000) or {}
    for _, resp in pairs(r) do
        if resp.error then return nil, resp.error end
        if resp.result ~= nil then return resp.result, nil end
    end
    return nil, nil
end

-- Find the "pub struct Searcher" line.
local searcher_line, searcher_col
for lnum = 1, vim.api.nvim_buf_line_count(bufnr) do
    local line = vim.api.nvim_buf_get_lines(bufnr, lnum - 1, lnum, false)[1] or ""
    if line:match("^pub struct Searcher ") then
        searcher_line = lnum - 1
        searcher_col = line:find("Searcher") - 1
        break
    end
end

local td_ident = { uri = "file://" .. file }
local pos = { line = searcher_line, character = searcher_col }

log("## Live-exercise each operation on `Searcher`")

-- hover
local t1 = vim.uv.hrtime()
local hover, err = req("textDocument/hover", { textDocument = td_ident, position = pos })
local dt = (vim.uv.hrtime() - t1) / 1e6
if err then
    log(string.format("  hover ........ ERR (%s) [%.0f ms]", err.message or "?", dt))
elseif hover and hover.contents then
    local body = type(hover.contents) == "table"
        and (hover.contents.value or vim.inspect(hover.contents))
        or tostring(hover.contents)
    body = body:gsub("\n", " ⏎ "):sub(1, 120)
    log(string.format("  hover ........ %.0f ms  [%s]", dt, body))
else
    log(string.format("  hover ........ %.0f ms  (empty)", dt))
end

-- definition
t1 = vim.uv.hrtime()
local def, derr = req("textDocument/definition", { textDocument = td_ident, position = pos })
dt = (vim.uv.hrtime() - t1) / 1e6
if derr then
    log(string.format("  definition ... ERR (%s) [%.0f ms]", derr.message or "?", dt))
else
    local count = type(def) == "table"
        and (def.uri and 1 or #def) or 0
    log(string.format("  definition ... %.0f ms  (%d result%s)",
        dt, count, count == 1 and "" or "s"))
end

-- references
t1 = vim.uv.hrtime()
local refs, rerr = req("textDocument/references",
    { textDocument = td_ident, position = pos, context = { includeDeclaration = true } })
dt = (vim.uv.hrtime() - t1) / 1e6
if rerr then
    log(string.format("  references ... ERR [%.0f ms]", dt))
else
    log(string.format("  references ... %.0f ms  (%d refs)", dt, refs and #refs or 0))
end

-- workspace/symbol
t1 = vim.uv.hrtime()
local syms = req("workspace/symbol", { query = "Builder" })
dt = (vim.uv.hrtime() - t1) / 1e6
log(string.format("  workspace/sym  %.0f ms  ('Builder': %d hits)",
    dt, syms and #syms or 0))

-- documentSymbol
t1 = vim.uv.hrtime()
local ds = req("textDocument/documentSymbol", { textDocument = td_ident })
dt = (vim.uv.hrtime() - t1) / 1e6
log(string.format("  docSymbol .... %.0f ms  (%d symbols)", dt, ds and #ds or 0))

-- rename (expected to fail)
t1 = vim.uv.hrtime()
local _r, rnerr = req("textDocument/rename",
    { textDocument = td_ident, position = pos, newName = "Searcher2" })
dt = (vim.uv.hrtime() - t1) / 1e6
if rnerr then
    log(string.format("  rename ....... NOT SUPPORTED (%s) [%.0f ms]",
        (rnerr.message or "?"):sub(1, 60), dt))
elseif _r and (_r.changes or _r.documentChanges) then
    log(string.format("  rename ....... %.0f ms  (edits returned)", dt))
else
    log(string.format("  rename ....... %.0f ms  (empty — probably unsupported)", dt))
end

-- codeAction (expected to fail)
t1 = vim.uv.hrtime()
local _ca, caerr = req("textDocument/codeAction",
    { textDocument = td_ident, range = { start = pos, ["end"] = pos },
      context = { diagnostics = {} } })
dt = (vim.uv.hrtime() - t1) / 1e6
log(string.format("  codeAction ... %s [%.0f ms]",
    caerr and ("ERR: " .. (caerr.message or "")) or
    (_ca and (#_ca .. " actions") or "empty"), dt))

-- Diagnostics (count across all buffers known to the client)
local diag_count = 0
for _, d in pairs(vim.diagnostic.get()) do
    diag_count = diag_count + 1
end
log(string.format("  diagnostics .. %d published by server", diag_count))

log("")
log("## Precision probe: do `references` pick up non-code mentions?")
-- Look for the word "Searcher" in comments or docstrings, count how many
-- of the returned refs point at lines whose raw content matches.
local comment_hits = 0
local string_hits = 0
if refs then
    for _, r in ipairs(refs) do
        local uri = (r.uri or ""):gsub("^file://", "")
        local line = r.range.start.line
        local ok, lines = pcall(vim.fn.readfile, uri)
        if ok and lines and lines[line + 1] then
            local text = lines[line + 1]
            local stripped = text:match("^%s*(.-)%s*$") or ""
            if stripped:match("^//") or stripped:match("^/%*")
               or stripped:match("^%s*%*") then
                comment_hits = comment_hits + 1
            end
        end
    end
end
log(string.format("  refs in comments: %d / %d  (heuristic identifier-match)",
    comment_hits, refs and #refs or 0))

vim.cmd("qa!")
