-- Recommended-UX Neovim config wired to QuickLSP.
--
-- Features users expect out of the box:
--   - Automatic LSP attach on .rs/.c/.cpp/.go/.py/.js/.ts/.java/.rb files
--   - Inline diagnostics (virtual text + signs)
--   - Auto-completion via nvim-cmp with snippet support (LuaSnip)
--   - Familiar keymaps: gd / gD / gr / gi / K / <leader>ws / <leader>rn
--   - Telescope picker for workspace symbols (if installed) or builtin picker
--   - Treesitter syntax highlighting (nice defaults)
--
-- Launched from the field-demo scripts with:
--     nvim -u scripts/field_demo/nvim_config/init.lua ...
-- so user settings are untouched.

-- ── Shared bootstrap ───────────────────────────────────────────────────

local dir = vim.fn.stdpath("data") .. "/lazy/lazy.nvim"
if not vim.loop.fs_stat(dir) then
    vim.fn.system({
        "git", "clone", "--filter=blob:none",
        "https://github.com/folke/lazy.nvim.git",
        "--branch=stable", dir,
    })
end
vim.opt.rtp:prepend(dir)

-- Sane editor defaults
vim.o.number = true
vim.o.relativenumber = false
vim.o.termguicolors = true
vim.o.updatetime = 300
vim.o.signcolumn = "yes"
vim.g.mapleader = " "

-- Path to the locally-built QuickLSP binary (overridable via env).
local QUICKLSP_BIN = vim.env.QUICKLSP_BIN
    or (vim.env.HOME or "/root") .. "/quicklsp/target/release/quicklsp"
local QUICKLSP_FT = {
    "rust", "c", "cpp", "go", "python", "javascript", "typescript", "java", "ruby",
}
-- Verbose LSP client logging — helpful when wiring up a custom server.
vim.lsp.set_log_level(vim.env.QUICKLSP_LSP_LOG_LEVEL or "warn")

-- ── Plugins ────────────────────────────────────────────────────────────

require("lazy").setup({
    { "neovim/nvim-lspconfig" },
    { "nvim-treesitter/nvim-treesitter", build = ":TSUpdate" },

    -- Completion stack
    {
        "hrsh7th/nvim-cmp",
        dependencies = {
            "hrsh7th/cmp-nvim-lsp",
            "hrsh7th/cmp-buffer",
            "hrsh7th/cmp-path",
            "L3MON4D3/LuaSnip",
            "saadparwaiz1/cmp_luasnip",
        },
    },

    -- Fuzzy picker — only loaded on Nvim 0.11+, older versions fall back
    -- to the built-in vim.lsp.buf.workspace_symbol keymap below.
    {
        "nvim-telescope/telescope.nvim",
        cond = function() return vim.fn.has("nvim-0.11") == 1 end,
        dependencies = { "nvim-lua/plenary.nvim" },
    },

    -- Minimal colorscheme so diagnostics etc. show up visibly.
    { "folke/tokyonight.nvim", priority = 1000, config = function()
        pcall(vim.cmd.colorscheme, "tokyonight-night")
    end },
}, {
    install = { missing = true },
    change_detection = { enabled = false },
})

-- ── Completion setup ───────────────────────────────────────────────────

local cmp = require("cmp")
local luasnip = require("luasnip")
cmp.setup({
    snippet = {
        expand = function(args) luasnip.lsp_expand(args.body) end,
    },
    mapping = cmp.mapping.preset.insert({
        ["<C-Space>"] = cmp.mapping.complete(),
        ["<CR>"] = cmp.mapping.confirm({ select = true }),
        ["<Tab>"] = cmp.mapping.select_next_item(),
        ["<S-Tab>"] = cmp.mapping.select_prev_item(),
    }),
    sources = cmp.config.sources({
        { name = "nvim_lsp" },
        { name = "luasnip" },
        { name = "buffer" },
        { name = "path" },
    }),
})

-- ── Diagnostics look & feel ────────────────────────────────────────────

vim.diagnostic.config({
    virtual_text = { prefix = "●" },
    signs = true,
    update_in_insert = false,
    severity_sort = true,
    float = { border = "rounded", source = "if_many" },
})
vim.lsp.handlers["textDocument/hover"] =
    vim.lsp.with(vim.lsp.handlers.hover, { border = "rounded" })
vim.lsp.handlers["textDocument/signatureHelp"] =
    vim.lsp.with(vim.lsp.handlers.signature_help, { border = "rounded" })

-- ── on_attach: the keymaps a user expects ──────────────────────────────

local function on_attach(_, bufnr)
    local function map(lhs, rhs, desc)
        vim.keymap.set("n", lhs, rhs, { buffer = bufnr, silent = true, desc = desc })
    end
    map("gd", vim.lsp.buf.definition, "Go to definition")
    map("gD", vim.lsp.buf.declaration, "Go to declaration")
    map("gr", vim.lsp.buf.references, "List references")
    map("gi", vim.lsp.buf.implementation, "List implementations")
    map("K", vim.lsp.buf.hover, "Hover doc")
    map("<leader>rn", vim.lsp.buf.rename, "Rename symbol")
    map("<leader>ca", vim.lsp.buf.code_action, "Code action")
    map("<leader>ws", function()
        require("telescope.builtin").lsp_dynamic_workspace_symbols()
    end, "Workspace symbol search")
    map("<leader>ds", require("telescope.builtin").lsp_document_symbols, "Document symbols")
    map("[d", vim.diagnostic.goto_prev, "Prev diagnostic")
    map("]d", vim.diagnostic.goto_next, "Next diagnostic")
end

-- ── Register QuickLSP as a server ──────────────────────────────────────

-- Use lspconfig's configs registry to define a new custom server.
local configs = require("lspconfig.configs")
local lspconfig = require("lspconfig")

if not configs.quicklsp then
    configs.quicklsp = {
        default_config = {
            cmd = { QUICKLSP_BIN },
            filetypes = QUICKLSP_FT,
            root_dir = function(fname)
                -- Prefer the outermost .git / workspace root so the LSP
                -- indexes the whole repo, not just the nearest crate.
                local git_root = lspconfig.util.root_pattern(".git")(fname)
                if git_root then return git_root end
                return lspconfig.util.root_pattern(
                    "Cargo.toml", "go.mod", "package.json",
                    "pyproject.toml", "setup.py"
                )(fname) or vim.fn.getcwd()
            end,
            -- Only start when opening a file inside a recognised project
            -- root — prevents the server from booting with the shell's cwd.
            single_file_support = false,
            settings = {},
        },
        docs = {
            description = "QuickLSP — heuristic universal LSP server",
        },
    }
end

local caps = require("cmp_nvim_lsp").default_capabilities()
lspconfig.quicklsp.setup({
    capabilities = caps,
    on_attach = on_attach,
})

-- ── Lightweight status message so the user knows LSP is attaching ──────

vim.api.nvim_create_autocmd("LspAttach", {
    callback = function(args)
        local client = vim.lsp.get_client_by_id(args.data.client_id)
        if client then
            vim.schedule(function()
                vim.notify("LSP attached: " .. client.name, vim.log.levels.INFO)
            end)
        end
    end,
})
