#!/bin/bash
# Manual LSP test script - drives neovim inside tmux to test quicklsp
# Uses :lua commands to check LSP state (tmux can't see floating windows)

set -e

SESSION="lsp-test"
RESULT_FILE="/home/user/quicklsp/lsp_test_results.md"

# Clean up
tmux kill-session -t "$SESSION" 2>/dev/null || true
pkill -f "target/release/quicklsp" 2>/dev/null || true
rm -rf ~/.cache/quicklsp/
rm -f "$RESULT_FILE"

cat > "$RESULT_FILE" << 'HEADER'
# QuickLSP Manual Test Results

Testing quicklsp on its own Rust source code via neovim in tmux.
Each test exercises an LSP feature on a specific code construct.

HEADER

# Helper functions
send() {
    tmux send-keys -t "$SESSION" "$@"
}

wait_short() { sleep 1; }
wait_med() { sleep 2; }
wait_long() { sleep 3; }

capture() {
    tmux capture-pane -t "$SESSION" -p -S -50
}

# Run a :lua command and capture the output shown in the command line area
run_lua() {
    local lua_code="$1"
    # Clear message area first
    send ":" "Enter"
    sleep 0.2
    send ":lua $lua_code" Enter
    sleep 1.5
}

# Run lua that writes results to a temp file, then read it
run_lua_to_file() {
    local lua_code="$1"
    local tmpfile="/tmp/lsp_test_output.txt"
    rm -f "$tmpfile"
    send ":lua local f = io.open('$tmpfile', 'w'); $lua_code; f:close()" Enter
    sleep 2
    if [ -f "$tmpfile" ]; then
        cat "$tmpfile"
    else
        echo "(no output file created)"
    fi
}

log_section() {
    echo "" >> "$RESULT_FILE"
    echo "## $1" >> "$RESULT_FILE"
    echo "" >> "$RESULT_FILE"
}

log_test() {
    local test_name="$1"
    local feature="$2"
    local target="$3"
    local result="$4"
    echo "### $test_name" >> "$RESULT_FILE"
    echo "- **Feature**: $feature" >> "$RESULT_FILE"
    echo "- **Target**: $target" >> "$RESULT_FILE"
    echo '```' >> "$RESULT_FILE"
    echo "$result" >> "$RESULT_FILE"
    echo '```' >> "$RESULT_FILE"
    echo "" >> "$RESULT_FILE"
}

echo "=== Starting LSP test session ==="

# Start tmux with neovim
tmux new-session -d -s "$SESSION" -x 200 -y 50
sleep 0.5
send "nvim -u /home/user/quicklsp/test_init.lua /home/user/quicklsp/tests/fixtures/sample_rust.rs" Enter
sleep 5  # Wait for LSP to initialize and index workspace

echo "--- Checking LSP connection ---"

# TEST 0: Verify LSP is attached
result=$(run_lua_to_file "local clients = vim.lsp.get_active_clients({name='quicklsp'}); f:write('clients: ' .. #clients .. '\\n'); if #clients > 0 then f:write('id: ' .. clients[1].id .. '\\n') end")
log_section "Phase 0: LSP Connection"
log_test "lsp_attached" "initialization" "quicklsp client attached" "$result"
echo "LSP check: $result"

if echo "$result" | grep -q "clients: 0"; then
    echo "LSP not attached! Trying manual start..."
    send ":lua vim.lsp.start({name='quicklsp', cmd={'/home/user/quicklsp/target/release/quicklsp'}, root_dir='/home/user/quicklsp'})" Enter
    sleep 5
    result=$(run_lua_to_file "local clients = vim.lsp.get_active_clients({name='quicklsp'}); f:write('clients: ' .. #clients .. '\\n')")
    log_test "lsp_manual_start" "initialization" "manual start attempt" "$result"
    echo "After manual start: $result"
fi

# Wait for indexing to complete
echo "Waiting for indexing..."
sleep 5

# ============================================================
log_section "Phase 1: Hover Tests on sample_rust.rs"
# ============================================================

# Helper: position cursor and do hover request, capture result
do_hover_test() {
    local test_name="$1"
    local target="$2"
    local line="$3"
    local col="$4"
    local tmpfile="/tmp/lsp_test_output.txt"
    rm -f "$tmpfile"

    # Move cursor to exact position
    send ":call cursor($line, $col)" Enter
    sleep 0.3

    # Request hover via LSP and write result to file
    send ":lua local params = vim.lsp.util.make_position_params(); local results = vim.lsp.buf_request_sync(0, 'textDocument/hover', params, 5000); local f = io.open('$tmpfile', 'w'); if results then for _, res in pairs(results) do if res.result and res.result.contents then local c = res.result.contents; if type(c) == 'table' then f:write(c.value or vim.inspect(c)) elseif type(c) == 'string' then f:write(c) end end end else f:write('NO RESULT') end; f:close()" Enter
    sleep 3

    local result
    if [ -f "$tmpfile" ]; then
        result=$(cat "$tmpfile")
    else
        result="(no output - hover failed)"
    fi
    log_test "$test_name" "hover" "$target" "$result"
    echo "  hover $test_name: $(echo "$result" | head -1)"
}

# Hover tests on sample_rust.rs
# Line numbers based on the file content we read earlier
echo "Testing hovers on sample_rust.rs..."

# struct Config (line 13, col 8 = "Config")
do_hover_test "hover_struct_Config" "struct Config definition" 13 8

# const MAX_RETRIES (line 5, col 7 = "MAX_RETRIES")
do_hover_test "hover_const_MAX_RETRIES" "const MAX_RETRIES" 5 7

# const DEFAULT_TIMEOUT (line 8, col 7)
do_hover_test "hover_const_DEFAULT_TIMEOUT" "const DEFAULT_TIMEOUT" 8 7

# enum Status (line 20, col 6 = "Status")
do_hover_test "hover_enum_Status" "enum Status" 20 6

# trait Handler (line 29, col 7 = "Handler")
do_hover_test "hover_trait_Handler" "trait Handler" 29 7

# struct Request (line 35, col 8 = "Request")
do_hover_test "hover_struct_Request" "struct Request" 35 8

# struct Response (line 42, col 8 = "Response")
do_hover_test "hover_struct_Response" "struct Response" 42 8

# fn create_config (line 48, col 4 = "create_config")
do_hover_test "hover_fn_create_config" "fn create_config" 48 4

# fn process_request (line 59, col 4 = "process_request")
do_hover_test "hover_fn_process_request" "fn process_request" 59 4

# struct Server (line 78, col 8 = "Server")
do_hover_test "hover_struct_Server" "struct Server" 78 8

# impl method Server::new (line 84, col 8 = "new")
do_hover_test "hover_method_Server_new" "impl method Server::new" 84 8

# impl method add_handler (line 91, col 8 = "add_handler")
do_hover_test "hover_method_add_handler" "impl method Server::add_handler" 91 8

# impl method run (line 95, col 8 = "run")
do_hover_test "hover_method_Server_run" "impl method Server::run" 95 8

# type alias StatusCode (line 114, col 6 = "StatusCode")
do_hover_test "hover_type_StatusCode" "type alias StatusCode" 114 6

# type alias HandlerResult (line 115, col 6 = "HandlerResult")
do_hover_test "hover_type_HandlerResult" "type alias HandlerResult" 115 6

# fn validate_request (line 118, col 4 = "validate_request")
do_hover_test "hover_fn_validate_request" "fn validate_request" 118 4

# Unicode fn données_utilisateur (line 129, col 4)
do_hover_test "hover_unicode_fn" "fn données_utilisateur (unicode)" 129 4

# Unicode struct Über (line 133, col 8)
do_hover_test "hover_unicode_struct" "struct Über (unicode)" 133 8

# Nested fn outer (line 138, col 4)
do_hover_test "hover_nested_outer" "fn outer (containing nested fn)" 138 4

# Nested fn inner (line 139, col 8)
do_hover_test "hover_nested_inner" "nested fn inner" 139 8

# Module function sanitize_input (line 105, col 12)
do_hover_test "hover_mod_sanitize_input" "utils::sanitize_input" 105 12

# Module function validate_port (line 109, col 12)
do_hover_test "hover_mod_validate_port" "utils::validate_port" 109 12

# static GLOBAL_COUNTER (line 146, col 8)
do_hover_test "hover_static_GLOBAL_COUNTER" "static GLOBAL_COUNTER" 146 8

# const FINAL_STATUS (line 145, col 7)
do_hover_test "hover_const_FINAL_STATUS" "const FINAL_STATUS" 145 7

# Hover on "Config" usage in fn process_request params (line 59, col 26)
do_hover_test "hover_Config_usage" "Config used as parameter type" 59 26

# Hover on "Request" usage (line 59, col 36)
do_hover_test "hover_Request_usage" "Request used as parameter type" 59 36

# Hover on "Status" usage (line 61, col 9)
do_hover_test "hover_Status_usage" "Status::Active usage in fn body" 61 9

# Hover on "Handler" in trait bound (line 80, col 31)
do_hover_test "hover_Handler_trait_usage" "Handler in Box<dyn Handler>" 80 31

# Hover on MAX_RETRIES usage (line 97, col 22)
do_hover_test "hover_MAX_RETRIES_usage" "MAX_RETRIES usage in loop" 97 22

# Hover inside string literal (line 67, col 10 = inside "Handled...")
do_hover_test "hover_inside_string" "word inside string literal" 67 10

# Hover on format! macro name (line 66, col 16)
do_hover_test "hover_format_macro" "format! macro" 66 16

# ============================================================
log_section "Phase 2: Go-to-Definition Tests on sample_rust.rs"
# ============================================================

do_goto_def_test() {
    local test_name="$1"
    local target="$2"
    local line="$3"
    local col="$4"
    local tmpfile="/tmp/lsp_test_output.txt"
    rm -f "$tmpfile"

    # First go back to original file
    send ":e /home/user/quicklsp/tests/fixtures/sample_rust.rs" Enter
    sleep 1

    send ":call cursor($line, $col)" Enter
    sleep 0.3

    send ":lua local params = vim.lsp.util.make_position_params(); local results = vim.lsp.buf_request_sync(0, 'textDocument/definition', params, 5000); local f = io.open('$tmpfile', 'w'); if results then for _, res in pairs(results) do if res.result then local r = res.result; if r.uri then f:write('file: ' .. r.uri .. '\\nline: ' .. (r.range and r.range.start.line or '?') .. '\\n') elseif r[1] then f:write('file: ' .. (r[1].uri or r[1].targetUri or '?') .. '\\nline: ' .. ((r[1].range and r[1].range.start.line) or '?') .. '\\n') else f:write(vim.inspect(r)) end end end else f:write('NO RESULT') end; f:close()" Enter
    sleep 3

    local result
    if [ -f "$tmpfile" ]; then
        result=$(cat "$tmpfile")
    else
        result="(no output - goto def failed)"
    fi
    log_test "$test_name" "definition" "$target" "$result"
    echo "  gotodef $test_name: $(echo "$result" | head -2 | tr '\n' ' ')"
}

echo "Testing go-to-definition on sample_rust.rs..."

# Config usage in process_request (line 59, col 26) → should go to line 13
do_goto_def_test "gotodef_Config_param" "Config in fn param type" 59 26

# Request usage (line 59, col 36) → should go to line 35
do_goto_def_test "gotodef_Request_param" "Request in fn param type" 59 36

# Response in return type (line 59, col 49) → should go to line 42
do_goto_def_test "gotodef_Response_return" "Response in return type" 59 49

# Status::Active (line 61, col 9) → should go to line 20
do_goto_def_test "gotodef_Status_usage" "Status::Active in fn body" 61 9

# MAX_RETRIES usage (line 97, col 22) → should go to line 5
do_goto_def_test "gotodef_MAX_RETRIES" "MAX_RETRIES in loop range" 97 22

# DEFAULT_TIMEOUT usage (line 98, col 27) → should go to line 8
do_goto_def_test "gotodef_DEFAULT_TIMEOUT" "DEFAULT_TIMEOUT in fn body" 98 27

# Handler in Box<dyn Handler> (line 80, col 31) → should go to line 29
do_goto_def_test "gotodef_Handler_trait" "Handler in dyn trait bound" 80 31

# Config in impl (line 84, col 18 — Config param) → should go to line 13
do_goto_def_test "gotodef_Config_in_impl" "Config in Server::new param" 84 18

# validate_request call (if referenced elsewhere) — test from line 118 itself
do_goto_def_test "gotodef_validate_request" "validate_request fn definition" 118 4

# inner() call (line 142, col 5) → should go to nested fn inner at line 139
do_goto_def_test "gotodef_inner_call" "inner() call in outer()" 142 5

# ============================================================
log_section "Phase 3: Find References Tests on sample_rust.rs"
# ============================================================

do_refs_test() {
    local test_name="$1"
    local target="$2"
    local line="$3"
    local col="$4"
    local tmpfile="/tmp/lsp_test_output.txt"
    rm -f "$tmpfile"

    send ":e /home/user/quicklsp/tests/fixtures/sample_rust.rs" Enter
    sleep 1

    send ":call cursor($line, $col)" Enter
    sleep 0.3

    send ":lua local params = vim.lsp.util.make_position_params(); params.context = {includeDeclaration = true}; local results = vim.lsp.buf_request_sync(0, 'textDocument/references', params, 10000); local f = io.open('$tmpfile', 'w'); if results then for _, res in pairs(results) do if res.result then f:write('count: ' .. #res.result .. '\\n'); for i, loc in ipairs(res.result) do local uri = loc.uri or loc.targetUri or '?'; local ln = loc.range and loc.range.start.line or '?'; f:write(i .. ': ' .. uri:match('[^/]+$') .. ':' .. tostring(ln) .. '\\n'); if i >= 20 then f:write('... (truncated)\\n'); break end end end end else f:write('NO RESULT') end; f:close()" Enter
    sleep 5

    local result
    if [ -f "$tmpfile" ]; then
        result=$(cat "$tmpfile")
    else
        result="(no output - refs failed)"
    fi
    log_test "$test_name" "references" "$target" "$result"
    echo "  refs $test_name: $(echo "$result" | head -1)"
}

echo "Testing find-references on sample_rust.rs..."

# References for Config (line 13, col 8)
do_refs_test "refs_Config" "struct Config" 13 8

# References for Status (line 20, col 6)
do_refs_test "refs_Status" "enum Status" 20 6

# References for Handler (line 29, col 7)
do_refs_test "refs_Handler" "trait Handler" 29 7

# References for Request (line 35, col 8)
do_refs_test "refs_Request" "struct Request" 35 8

# References for Response (line 42, col 8)
do_refs_test "refs_Response" "struct Response" 42 8

# References for MAX_RETRIES (line 5, col 7)
do_refs_test "refs_MAX_RETRIES" "const MAX_RETRIES" 5 7

# References for Server (line 78, col 8)
do_refs_test "refs_Server" "struct Server" 78 8

# References for process_request (line 59, col 4)
do_refs_test "refs_process_request" "fn process_request" 59 4

# ============================================================
log_section "Phase 4: Completion Tests on sample_rust.rs"
# ============================================================

do_completion_test() {
    local test_name="$1"
    local target="$2"
    local line="$3"
    local col="$4"
    local prefix="$5"
    local tmpfile="/tmp/lsp_test_output.txt"
    rm -f "$tmpfile"

    send ":e /home/user/quicklsp/tests/fixtures/sample_rust.rs" Enter
    sleep 1

    send ":call cursor($line, $col)" Enter
    sleep 0.3

    # Use make_position_params at current location but override the position for completion
    send ":lua local params = {textDocument = vim.lsp.util.make_text_document_params(), position = {line = $((line - 1)), character = $col}}; local results = vim.lsp.buf_request_sync(0, 'textDocument/completion', params, 5000); local f = io.open('$tmpfile', 'w'); if results then for _, res in pairs(results) do if res.result then local items = res.result.items or res.result; f:write('count: ' .. #items .. '\\n'); for i, item in ipairs(items) do f:write(i .. ': ' .. (item.label or '?') .. ' (' .. (item.detail or '') .. ')\\n'); if i >= 15 then f:write('... (truncated)\\n'); break end end end end else f:write('NO RESULT') end; f:close()" Enter
    sleep 3

    local result
    if [ -f "$tmpfile" ]; then
        result=$(cat "$tmpfile")
    else
        result="(no output - completion failed)"
    fi
    log_test "$test_name" "completion" "$target" "$result"
    echo "  completion $test_name: $(echo "$result" | head -1)"
}

echo "Testing completion..."

# Completion for "Conf" prefix (somewhere in function body, line 65)
do_completion_test "completion_Conf" "prefix 'Conf' → Config?" 65 10 "Conf"

# Completion for "Sta" prefix → Status, StatusCode
do_completion_test "completion_Sta" "prefix 'Sta' → Status/StatusCode?" 65 10 "Sta"

# Completion for "Hand" prefix → Handler, HandlerResult
do_completion_test "completion_Hand" "prefix 'Hand' → Handler/HandlerResult?" 65 10 "Hand"

# Completion for "Ser" prefix → Server
do_completion_test "completion_Ser" "prefix 'Ser' → Server?" 65 10 "Ser"

# Completion for "val" prefix → validate_request, validate_port
do_completion_test "completion_val" "prefix 'val' → validate_*?" 65 10 "val"

# Completion for "create" prefix → create_config
do_completion_test "completion_create" "prefix 'create' → create_config?" 65 10 "create"

# Completion for "process" prefix → process_request
do_completion_test "completion_proc" "prefix 'process' → process_request?" 65 10 "process"

# ============================================================
log_section "Phase 5: Document Symbols Tests"
# ============================================================

do_doc_symbols_test() {
    local test_name="$1"
    local file="$2"
    local tmpfile="/tmp/lsp_test_output.txt"
    rm -f "$tmpfile"

    send ":e $file" Enter
    sleep 2

    send ":lua local params = {textDocument = vim.lsp.util.make_text_document_params()}; local results = vim.lsp.buf_request_sync(0, 'textDocument/documentSymbol', params, 5000); local f = io.open('$tmpfile', 'w'); if results then for _, res in pairs(results) do if res.result then f:write('count: ' .. #res.result .. '\\n'); for i, sym in ipairs(res.result) do local kind_names = {1='File',2='Module',3='Namespace',5='Class',6='Method',8='Constructor',10='Enum',11='Interface',12='Function',13='Variable',14='Constant',15='String',22='Struct',23='Event',26='TypeParam'}; f:write(i .. ': ' .. (sym.name or '?') .. ' (kind=' .. (kind_names[sym.kind] or sym.kind) .. ', line=' .. (sym.range and sym.range.start.line or sym.location and sym.location.range.start.line or '?') .. ')\\n') end end end else f:write('NO RESULT') end; f:close()" Enter
    sleep 3

    local result
    if [ -f "$tmpfile" ]; then
        result=$(cat "$tmpfile")
    else
        result="(no output - doc symbols failed)"
    fi
    log_test "$test_name" "documentSymbol" "$file" "$result"
    echo "  docSymbols $test_name: $(echo "$result" | head -1)"
}

echo "Testing document symbols..."

do_doc_symbols_test "docsym_sample_rust" "/home/user/quicklsp/tests/fixtures/sample_rust.rs"
do_doc_symbols_test "docsym_workspace" "/home/user/quicklsp/src/workspace.rs"
do_doc_symbols_test "docsym_server" "/home/user/quicklsp/src/lsp/server.rs"
do_doc_symbols_test "docsym_tokenizer" "/home/user/quicklsp/src/parsing/tokenizer.rs"
do_doc_symbols_test "docsym_symbols" "/home/user/quicklsp/src/parsing/symbols.rs"
do_doc_symbols_test "docsym_main" "/home/user/quicklsp/src/main.rs"

# ============================================================
log_section "Phase 6: Workspace Symbols Tests"
# ============================================================

do_workspace_symbols_test() {
    local test_name="$1"
    local query="$2"
    local tmpfile="/tmp/lsp_test_output.txt"
    rm -f "$tmpfile"

    send ":lua local results = vim.lsp.buf_request_sync(0, 'workspace/symbol', {query = '$query'}, 5000); local f = io.open('$tmpfile', 'w'); if results then for _, res in pairs(results) do if res.result then f:write('count: ' .. #res.result .. '\\n'); for i, sym in ipairs(res.result) do f:write(i .. ': ' .. (sym.name or '?') .. ' in ' .. (sym.location and sym.location.uri and sym.location.uri:match('[^/]+$') or '?') .. '\\n'); if i >= 15 then f:write('... (truncated)\\n'); break end end end end else f:write('NO RESULT') end; f:close()" Enter
    sleep 3

    local result
    if [ -f "$tmpfile" ]; then
        result=$(cat "$tmpfile")
    else
        result="(no output - workspace symbols failed)"
    fi
    log_test "$test_name" "workspaceSymbol" "query: $query" "$result"
    echo "  wsSymbols $test_name: $(echo "$result" | head -1)"
}

echo "Testing workspace symbols..."

do_workspace_symbols_test "wssym_Workspace" "Workspace"
do_workspace_symbols_test "wssym_Server" "Server"
do_workspace_symbols_test "wssym_LangFamily" "LangFamily"
do_workspace_symbols_test "wssym_Symbol" "Symbol"
do_workspace_symbols_test "wssym_DeletionIndex" "DeletionIndex"
do_workspace_symbols_test "wssym_Config" "Config"
do_workspace_symbols_test "wssym_PosEncoding" "PosEncoding"

# ============================================================
log_section "Phase 7: Signature Help Tests"
# ============================================================

do_sig_help_test() {
    local test_name="$1"
    local target="$2"
    local line="$3"
    local col="$4"
    local tmpfile="/tmp/lsp_test_output.txt"
    rm -f "$tmpfile"

    send ":e /home/user/quicklsp/tests/fixtures/sample_rust.rs" Enter
    sleep 1
    send ":call cursor($line, $col)" Enter
    sleep 0.3

    send ":lua local params = vim.lsp.util.make_position_params(); local results = vim.lsp.buf_request_sync(0, 'textDocument/signatureHelp', params, 5000); local f = io.open('$tmpfile', 'w'); if results then for _, res in pairs(results) do if res.result and res.result.signatures then for i, sig in ipairs(res.result.signatures) do f:write('sig ' .. i .. ': ' .. (sig.label or '?') .. '\\n'); if sig.documentation then f:write('doc: ' .. (type(sig.documentation) == 'table' and sig.documentation.value or tostring(sig.documentation)) .. '\\n') end; if sig.parameters then for j, p in ipairs(sig.parameters) do f:write('  param ' .. j .. ': ' .. (p.label or vim.inspect(p)) .. '\\n') end end end else f:write('no signatures') end end else f:write('NO RESULT') end; f:close()" Enter
    sleep 3

    local result
    if [ -f "$tmpfile" ]; then
        result=$(cat "$tmpfile")
    else
        result="(no output - sig help failed)"
    fi
    log_test "$test_name" "signatureHelp" "$target" "$result"
    echo "  sigHelp $test_name: $(echo "$result" | head -1)"
}

echo "Testing signature help..."

# Inside create_config call - well, it takes no params. Let's test on process_request
# line 68: config.host - after "config."
do_sig_help_test "sighelp_process_request_call" "inside process_request body" 68 36

# Inside Server::new() call - line 85, col 15 (inside the parens)
do_sig_help_test "sighelp_server_new" "inside Server::new(config)" 85 15

# Inside add_handler call - line 92
do_sig_help_test "sighelp_add_handler" "inside add_handler call" 92 15

# ============================================================
log_section "Phase 8: Cross-file Tests (workspace.rs, server.rs, tokenizer.rs)"
# ============================================================

echo "Testing hover on workspace.rs symbols..."
send ":e /home/user/quicklsp/src/workspace.rs" Enter
sleep 2

# Hover on SymbolLocation (line 35)
do_hover_cross() {
    local test_name="$1"
    local target="$2"
    local file="$3"
    local line="$4"
    local col="$5"
    local tmpfile="/tmp/lsp_test_output.txt"
    rm -f "$tmpfile"

    send ":e $file" Enter
    sleep 1
    send ":call cursor($line, $col)" Enter
    sleep 0.3

    send ":lua local params = vim.lsp.util.make_position_params(); local results = vim.lsp.buf_request_sync(0, 'textDocument/hover', params, 5000); local f = io.open('$tmpfile', 'w'); if results then for _, res in pairs(results) do if res.result and res.result.contents then local c = res.result.contents; if type(c) == 'table' then f:write(c.value or vim.inspect(c)) elseif type(c) == 'string' then f:write(c) end end end else f:write('NO RESULT') end; f:close()" Enter
    sleep 3

    local result
    if [ -f "$tmpfile" ]; then
        result=$(cat "$tmpfile")
    else
        result="(no output - hover failed)"
    fi
    log_test "$test_name" "hover" "$target" "$result"
    echo "  hover $test_name: $(echo "$result" | head -1)"
}

do_hover_cross "hover_SymbolLocation" "pub struct SymbolLocation" "/home/user/quicklsp/src/workspace.rs" 35 16
do_hover_cross "hover_Reference" "pub struct Reference" "/home/user/quicklsp/src/workspace.rs" 42 16
do_hover_cross "hover_FileId" "struct FileId" "/home/user/quicklsp/src/workspace.rs" 53 8
do_hover_cross "hover_SymbolRef" "struct SymbolRef" "/home/user/quicklsp/src/workspace.rs" 58 8
do_hover_cross "hover_FileEntry" "struct FileEntry" "/home/user/quicklsp/src/workspace.rs" 64 8
do_hover_cross "hover_LogWriteMsg" "struct LogWriteMsg" "/home/user/quicklsp/src/workspace.rs" 73 8

echo "Testing hover on server.rs symbols..."
do_hover_cross "hover_PosEncoding" "enum PosEncoding" "/home/user/quicklsp/src/lsp/server.rs" 22 6
do_hover_cross "hover_byte_col_to_encoding" "fn byte_col_to_encoding" "/home/user/quicklsp/src/lsp/server.rs" 31 4
do_hover_cross "hover_encoding_col_to_byte" "fn encoding_col_to_byte" "/home/user/quicklsp/src/lsp/server.rs" 52 4

echo "Testing hover on tokenizer.rs symbols..."
do_hover_cross "hover_stats_mod" "pub mod stats" "/home/user/quicklsp/src/parsing/tokenizer.rs" 21 9
do_hover_cross "hover_Counters" "struct Counters" "/home/user/quicklsp/src/parsing/tokenizer.rs" 27 12
do_hover_cross "hover_flush_fn" "pub fn flush" "/home/user/quicklsp/src/parsing/tokenizer.rs" 58 12

echo "Testing go-to-def cross-file..."

do_goto_def_cross() {
    local test_name="$1"
    local target="$2"
    local file="$3"
    local line="$4"
    local col="$5"
    local tmpfile="/tmp/lsp_test_output.txt"
    rm -f "$tmpfile"

    send ":e $file" Enter
    sleep 1
    send ":call cursor($line, $col)" Enter
    sleep 0.3

    send ":lua local params = vim.lsp.util.make_position_params(); local results = vim.lsp.buf_request_sync(0, 'textDocument/definition', params, 5000); local f = io.open('$tmpfile', 'w'); if results then for _, res in pairs(results) do if res.result then local r = res.result; if r.uri then f:write('file: ' .. r.uri:match('[^/]+$') .. '\\nline: ' .. (r.range and r.range.start.line or '?') .. '\\n') elseif r[1] then f:write('file: ' .. ((r[1].uri or r[1].targetUri or '?'):match('[^/]+$') or '?') .. '\\nline: ' .. ((r[1].range and r[1].range.start.line) or '?') .. '\\n') else f:write(vim.inspect(r)) end end end else f:write('NO RESULT') end; f:close()" Enter
    sleep 3

    local result
    if [ -f "$tmpfile" ]; then
        result=$(cat "$tmpfile")
    else
        result="(no output - goto def failed)"
    fi
    log_test "$test_name" "definition" "$target" "$result"
    echo "  gotodef $test_name: $(echo "$result" | head -2 | tr '\n' ' ')"
}

# From server.rs, go to Workspace definition (line 18, col ~47)
do_goto_def_cross "gotodef_Workspace_from_server" "Workspace import in server.rs" "/home/user/quicklsp/src/lsp/server.rs" 18 47
# From server.rs, go to SymbolLocation
do_goto_def_cross "gotodef_SymbolLocation_from_server" "SymbolLocation import in server.rs" "/home/user/quicklsp/src/lsp/server.rs" 18 30
# From workspace.rs, go to Symbol import (line 25)
do_goto_def_cross "gotodef_Symbol_from_workspace" "Symbol import in workspace.rs" "/home/user/quicklsp/src/workspace.rs" 25 37
# From workspace.rs, go to LangFamily (line 26)
do_goto_def_cross "gotodef_LangFamily_from_workspace" "LangFamily import in workspace.rs" "/home/user/quicklsp/src/workspace.rs" 26 42

echo "Testing find-references cross-file..."

do_refs_cross() {
    local test_name="$1"
    local target="$2"
    local file="$3"
    local line="$4"
    local col="$5"
    local tmpfile="/tmp/lsp_test_output.txt"
    rm -f "$tmpfile"

    send ":e $file" Enter
    sleep 1
    send ":call cursor($line, $col)" Enter
    sleep 0.3

    send ":lua local params = vim.lsp.util.make_position_params(); params.context = {includeDeclaration = true}; local results = vim.lsp.buf_request_sync(0, 'textDocument/references', params, 10000); local f = io.open('$tmpfile', 'w'); if results then for _, res in pairs(results) do if res.result then f:write('count: ' .. #res.result .. '\\n'); for i, loc in ipairs(res.result) do local uri = loc.uri or '?'; f:write(i .. ': ' .. uri:match('[^/]+$') .. ':' .. tostring(loc.range and loc.range.start.line or '?') .. '\\n'); if i >= 20 then f:write('... (truncated)\\n'); break end end end end else f:write('NO RESULT') end; f:close()" Enter
    sleep 5

    local result
    if [ -f "$tmpfile" ]; then
        result=$(cat "$tmpfile")
    else
        result="(no output - refs failed)"
    fi
    log_test "$test_name" "references" "$target" "$result"
    echo "  refs $test_name: $(echo "$result" | head -1)"
}

# References for Workspace across the project
do_refs_cross "refs_Workspace" "Workspace struct" "/home/user/quicklsp/src/workspace.rs" 82 16
# References for Symbol across the project
do_refs_cross "refs_Symbol" "Symbol struct" "/home/user/quicklsp/src/parsing/symbols.rs" 1 1
# References for LangFamily
do_refs_cross "refs_LangFamily" "LangFamily enum" "/home/user/quicklsp/src/parsing/tokenizer.rs" 1 1

echo ""
echo "=== All tests completed ==="
echo "Results written to $RESULT_FILE"

# Close neovim and tmux
send ":qa!" Enter
sleep 1
tmux kill-session -t "$SESSION" 2>/dev/null || true

echo "=== Test session ended ==="
