#!/usr/bin/env bash
set -euo pipefail
root=/root/minicore-tui-v03-refactor
cd "$root/tui"
for tc in 1.85.0 stable; do
  df -h /root
  rustc +"$tc" --version
  cargo +"$tc" fmt --all -- --check > "$root/e1-${tc}-fmt.log" 2>&1
  df -h /root
  cargo +"$tc" test --locked --all-targets --no-fail-fast > "$root/e1-${tc}-tests.log" 2>&1
  df -h /root
  cargo +"$tc" clippy --locked --all-targets -- -D warnings > "$root/e1-${tc}-clippy.log" 2>&1
  df -h /root
  RUSTDOCFLAGS='-D warnings' cargo +"$tc" doc --locked --no-deps > "$root/e1-${tc}-doc.log" 2>&1
  df -h /root
  MINICORE_AGENT_BIN="$root/fixedagent-target/debug/minicore-agent" cargo +"$tc" test --locked --test agent_e2e -- --ignored --test-threads=1 > "$root/e1-${tc}-e2e.log" 2>&1
  printf 'VERIFIED %s\n' "$tc"
done
df -h /root
cargo +1.85.0 test --release --locked --test performance -- --ignored --nocapture > "$root/e1-performance.log" 2>&1
cargo +1.85.0 tree --locked -d > "$root/e1-dependencies.log" 2>&1
printf 'VERIFIED performance and dependency inspection\n'
