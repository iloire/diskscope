#!/usr/bin/env bash
# Times diskscope against the usual suspects on one tree.
#
#   scripts/bench.sh ~/code [runs]
#
# Run it twice: the first pass warms the metadata cache and measures the disk,
# the second measures the program. Both numbers are worth knowing, and only the
# warm one is comparable between tools.
set -euo pipefail

target="${1:?usage: bench.sh <path> [runs]}"
runs="${2:-3}"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cargo build --release --manifest-path "$here/Cargo.toml" -p diskscope-core --examples

echo "== diskscope: bulk vs portable walker =="
"$here/target/release/examples/bench" "$target" "$runs"

time_it() {
  local label="$1"; shift
  if ! command -v "$1" >/dev/null 2>&1; then
    printf '  %-22s not installed\n' "$label"
    return
  fi
  local best=99999
  for _ in $(seq "$runs"); do
    local start end
    start=$(python3 -c 'import time; print(time.time())')
    "$@" >/dev/null 2>&1 || true
    end=$(python3 -c 'import time; print(time.time())')
    best=$(python3 -c "print(min($best, $end - $start))")
  done
  printf '  %-22s %7.3fs\n' "$label" "$best"
}

echo
echo "== baselines (same tree, warm cache) =="
time_it "du -sk" du -sk "$target"
time_it "dust -d0" dust -d0 "$target"
time_it "find | wc -l" find "$target"
