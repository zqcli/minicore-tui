#!/usr/bin/env bash
set -euo pipefail
root=/root/minicore-tui-v03-refactor
cd "$root/tui"
check_disk() {
  df -h /root
  available=$(df -Pk /root | awk 'NR==2 {print $4}')
  if (( available < 12 * 1024 * 1024 )); then
    # Only this TUI's exact disposable incremental cache, never sources/Store.
    rm -rf /root/minicore-tui-v03-refactor/tui/target/debug/incremental
    df -h /root
  fi
}
for tc in 1.85.0 stable; do
  check_disk
  rustc +"$tc" --version
  cargo +"$tc" fmt --all -- --check > "$root/e3-${tc}-fmt.log" 2>&1
  cargo +"$tc" test --locked --all-targets --no-fail-fast > "$root/e3-${tc}-tests.log" 2>&1
  check_disk
  cargo +"$tc" clippy --locked --all-targets -- -D warnings > "$root/e3-${tc}-clippy.log" 2>&1
  check_disk
  RUSTDOCFLAGS='-D warnings' cargo +"$tc" doc --locked --no-deps > "$root/e3-${tc}-doc.log" 2>&1
  check_disk
  MINICORE_AGENT_BIN="$root/fixedagent-target/debug/minicore-agent" cargo +"$tc" test --locked --test agent_e2e -- --ignored --test-threads=1 > "$root/e3-${tc}-e2e.log" 2>&1
  printf 'VERIFIED %s\n' "$tc"
done
check_disk
cargo +1.85.0 test --release --locked --test performance -- --ignored --nocapture > "$root/e3-performance.log" 2>&1
cargo +1.85.0 tree --locked -d > "$root/e3-dependencies.log" 2>&1
printf 'VERIFIED performance and dependency inspection\n'
