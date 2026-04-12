-- Additional probes for completion, signatureHelp, and workspace
-- symbol substring matching.

local args = _G.arg or {}
local function nth(n)
    local i = 0
    for _, a in ipairs(args) do
        if a ~= "--" and a ~= "" then
            i = i + 1
            if i == n then return a end
        end
    end
end

local repo = nth(1) or "/tmp/quicklsp-field-test/ripgrep"
local file = repo .. "/crates/searcher/src/searcher/mod.rs"
local function log(s) io.stdout:write(s .. "\n"); io.stdout:flush() end

vim.cmd("edit " .. vim.fn.fnameescape(file))
vim.cmd("filetype detect")
vim.cmd("doautoall FileType")

local bufnr = vim.api.nvim_get_current_buf()
local function wait_ready(timeout_ms)
    local deadline = vim.uv.hrtime() + timeout_ms * 1e6
    while vim.uv.hrtime() < deadline do
        local cs = vim.lsp.get_clients({ bufnr = bufnr, name = "quicklsp" })
        if #cs > 0 then
            local r = vim.lsp.buf_request_sync(bufnr, "workspace/symbol",
                { query = "Searcher" }, 1000) or {}
            for _, resp in pairs(r) do
                if resp.result and #resp.result >= 3 then return true end
            end
        end
        vim.wait(80)
    end
    return false
end
if not wait_ready(30000) then log("FAIL"); os.exit(1) end

local function req(method, params)
    local r = vim.lsp.buf_request_sync(bufnr, method, params, 2000) or {}
    for _, resp in pairs(r) do
        if resp.result ~= nil or resp.error then return resp.result, resp.error end
    end
    return nil, nil
end

log("## workspace/symbol substring behavior")
for _, q in ipairs({ "Builder", "Searcher", "sinks", "new" }) do
    local r = req("workspace/symbol", { query = q })
    log(string.format("  query=%-10q → %d hits", q, r and #r or 0))
end

log("")
log("## textDocument/completion — try multiple prefixes")
local td_ident = { uri = "file://" .. file }
-- Write `Sear` into a scratch line and ask for completions there.
-- Find a blank line we can temporarily overwrite.
local last_line = vim.api.nvim_buf_line_count(bufnr)
vim.api.nvim_buf_set_lines(bufnr, last_line, last_line, false, { "Sear" })
local r, err = req("textDocument/completion",
    { textDocument = td_ident,
      position = { line = last_line, character = 4 },
      context = { triggerKind = 1 } })
if err then
    log("  ERR: " .. (err.message or "?"))
else
    local items = (r and r.items) and r.items or (type(r) == "table" and r or {})
    log(string.format("  'Sear' → %d item(s)", #items))
    for i = 1, math.min(5, #items) do
        log(string.format("    - %s", items[i].label or "?"))
    end
end

log("")
log("## textDocument/signatureHelp — probe at a call site")
-- Find a call site like `func(` and place cursor between `(` and `)`
local sig_line, sig_col
for lnum = 1, vim.api.nvim_buf_line_count(bufnr) do
    local line = vim.api.nvim_buf_get_lines(bufnr, lnum - 1, lnum, false)[1] or ""
    local i = line:find("SearcherBuilder::new%(")
    if i then
        sig_line = lnum - 1
        sig_col = i + #"SearcherBuilder::new(" - 1
        break
    end
end
if sig_line then
    local rr, eerr = req("textDocument/signatureHelp",
        { textDocument = td_ident,
          position = { line = sig_line, character = sig_col },
          context = { triggerKind = 2, triggerCharacter = "(" } })
    if eerr then
        log("  ERR: " .. (eerr.message or "?"))
    elseif rr and rr.signatures and #rr.signatures > 0 then
        log(string.format("  signatures: %d", #rr.signatures))
        for i, sig in ipairs(rr.signatures) do
            log(string.format("    %d. %s", i, sig.label or ""))
        end
    else
        log("  no signatures")
    end
else
    log("  (no call site found)")
end

vim.cmd("qa!")
