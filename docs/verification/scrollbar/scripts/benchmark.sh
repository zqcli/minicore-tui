#!/usr/bin/env bash
set -euo pipefail
root=/root/minicore-scrollbar.1ulnCV
export PATH=/root/.cargo/bin:$PATH CARGO_TARGET_DIR=/root/minicore-tui-027-RXdxEP/target-linux
cd "$root"
rustup run stable cargo test -j4 --locked --offline --lib --no-run --message-format=json > "$root/logs/benchmark-candidate-build.jsonl"
exe=$(python3 -c 'import sys,json; print(next(f["executable"] for l in sys.stdin if (f:=json.loads(l)).get("executable") and f.get("target",{}).get("name")=="minicore_tui"))' < "$root/logs/benchmark-candidate-build.jsonl")
cp "$exe" "$root/artifacts/candidate-benchmark"
cd "$root/baseline"
rustup run stable cargo test -j4 --locked --offline --lib --no-run --message-format=json > "$root/logs/benchmark-baseline-build.jsonl"
exe=$(python3 -c 'import sys,json; print(next(f["executable"] for l in sys.stdin if (f:=json.loads(l)).get("executable") and f.get("target",{}).get("name")=="minicore_tui"))' < "$root/logs/benchmark-baseline-build.jsonl")
cp "$exe" "$root/artifacts/baseline-benchmark"
for run in 1 2 3; do
 for version in baseline candidate; do
  cd "$root"; [[ $version != baseline ]] || cd "$root/baseline"
  "$root/artifacts/$version-benchmark" ui::render_cache_tests::scrollbar_noop_event_benchmark --exact --ignored --nocapture > "$root/logs/benchmark-$version-$run.log"
 done
done
rg 'scrollbar_noop events=' "$root"/logs/benchmark-{baseline,candidate}-*.log
