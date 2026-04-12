#!/usr/bin/env bash
# Run the nvim LSP bench: cold scan A, warm scan B (sibling clone),
# rescan A. All three share a single QUICKLSP_CACHE_DIR so that
# Layer A is warm from step 2 onward.
set -euo pipefail

REPO_A="${REPO_A:-/tmp/quicklsp-field-test/ripgrep}"
REPO_B="${REPO_B:-/tmp/quicklsp-field-test/ripgrep-clone}"
FILE_REL="${FILE_REL:-crates/searcher/src/searcher/mod.rs}"
CACHE="${QUICKLSP_CACHE_DIR:-/tmp/qlsp-nvim-bench}"
QLSP_BIN="${QUICKLSP_BIN:-/tmp/quicklsp-wrap.sh}"
INIT="$(dirname "$0")/nvim_config/init.lua"
BENCH="$(dirname "$0")/nvim_bench.lua"

if [ ! -x "$QLSP_BIN" ]; then
    QLSP_BIN="/home/user/quicklsp/target/release/quicklsp"
fi

export QUICKLSP_BIN="$QLSP_BIN"
export QUICKLSP_CACHE_DIR="$CACHE"

rm -rf "$CACHE"
echo "Fresh QUICKLSP_CACHE_DIR: $CACHE"
echo

# If the sibling clone doesn't exist, make one from REPO_A.
if [ ! -d "$REPO_B" ]; then
    cp -r "$REPO_A" "$REPO_B"
fi

run() {
    local label="$1" repo="$2"
    timeout 60 nvim --headless -u "$INIT" -l "$BENCH" \
        "$label" "$repo" "$FILE_REL" 2>/dev/null
}

run "cold  (empty cache)   " "$REPO_A"
echo
run "warm  (Layer A hot)   " "$REPO_B"
echo
run "rescan(stat-fresh)    " "$REPO_A"
