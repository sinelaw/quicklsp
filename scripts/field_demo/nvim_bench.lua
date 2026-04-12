-- Headless benchmark: measure cold vs warm workspace scan latency
-- as observed by a real Neovim client attached to QuickLSP.
--
-- Run three times against two repos sharing a single QUICKLSP_CACHE_DIR:
--   1. scan A cold
--   2. scan B (sibling clone, same cache) — should be warm via Layer A
--   3. rescan A (stat-fresh)
--
-- For each run, prints the time to reach ≥ `threshold` workspace/symbol
-- hits, which is a good proxy for "the scan has done real work".

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

local label = arg_nth(1)
local repo = arg_nth(2)
local file_rel = arg_nth(3)
if not (label and repo and file_rel) then
    io.stderr:write("usage: nvim -l nvim_bench.lua <label> <repo> <file_rel>\n")
    os.exit(2)
end

local file = repo .. "/" .. file_rel

local function log(msg)
    io.stdout:write("[" .. label .. "] " .. msg .. "\n")
    io.stdout:flush()
end

local t0 = vim.uv.hrtime()
vim.cmd("edit " .. vim.fn.fnameescape(file))
vim.cmd("filetype detect")
vim.cmd("doautoall FileType")

local bufnr = vim.api.nvim_get_current_buf()

-- Wait for LSP attach.
local function wait_attach(timeout_ms)
    local deadline = vim.uv.hrtime() + timeout_ms * 1e6
    while vim.uv.hrtime() < deadline do
        local cs = vim.lsp.get_clients({ bufnr = bufnr, name = "quicklsp" })
        if #cs > 0 then return cs[1] end
        vim.wait(40)
    end
end
if not wait_attach(30000) then
    log("ERROR: LSP did not attach")
    os.exit(3)
end
local t_attach = (vim.uv.hrtime() - t0) / 1e6
log(string.format("attach=%.0f ms", t_attach))

-- Wait until workspace/symbol returns >= threshold hits for "Searcher".
local THRESHOLD = 3
local deadline = vim.uv.hrtime() + 25 * 1e9
local t_ready
while vim.uv.hrtime() < deadline do
    local r = vim.lsp.buf_request_sync(bufnr, "workspace/symbol",
        { query = "Searcher" }, 1000) or {}
    local count = 0
    for _, resp in pairs(r) do
        if resp.result then count = #resp.result end
    end
    if count >= THRESHOLD then
        t_ready = (vim.uv.hrtime() - t0) / 1e6
        log(string.format("ready=%.0f ms (%d hits for 'Searcher')", t_ready, count))
        break
    end
    vim.wait(50)
end
if not t_ready then
    log("ERROR: never reached threshold")
    os.exit(4)
end

-- One quick definition + references query as a sanity check.
local params = {
    textDocument = { uri = "file://" .. file },
    position = { line = 596, character = 11 },  -- `pub struct Searcher` line
}
local t1 = vim.uv.hrtime()
local r = vim.lsp.buf_request_sync(bufnr, "textDocument/references",
    vim.tbl_extend("force", params, { context = { includeDeclaration = true } }),
    2000) or {}
local refs = 0
for _, resp in pairs(r) do
    if resp.result then refs = #resp.result end
end
log(string.format("refs=%.0f ms (%d refs)",
    (vim.uv.hrtime() - t1) / 1e6, refs))

vim.cmd("qa!")
