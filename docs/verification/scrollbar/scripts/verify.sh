#!/usr/bin/env bash
set -euo pipefail
[[ $(uname -s) == Linux ]] || exit 2
root=/root/minicore-scrollbar.1ulnCV
cache=/root/minicore-tui-027-RXdxEP
export PATH=/root/.cargo/bin:/usr/lib/llvm-19/bin:$PATH
export CARGO_TARGET_DIR="$cache/target-linux"
mkdir -p "$root/logs" "$root/artifacts"
run() {
 local name=$1; shift
 local status=0
 printf 'cwd=%s\ncommand=' "$PWD" > "$root/logs/$name.log"
 printf '%q ' "$@" >> "$root/logs/$name.log"
 printf '\n' >> "$root/logs/$name.log"
 "$@" >> "$root/logs/$name.log" 2>&1 || status=$?
 printf '\nexit=%s\n' "$status" >> "$root/logs/$name.log"
 if ((status)); then tail -n 90 "$root/logs/$name.log"; exit "$status"; fi
}
cd "$root"
run format rustup run stable cargo fmt --all
run stable rustup run stable cargo test -j4 --locked --offline --all-targets
run msrv env CARGO_TARGET_DIR="$cache/target-msrv" rustup run 1.85.0 cargo test -j4 --locked --offline --all-targets
run fmt-check rustup run stable cargo fmt --all -- --check
run clippy rustup run stable cargo clippy -j4 --locked --offline --all-targets -- -D warnings
run doc env RUSTDOCFLAGS=-D\ warnings rustup run stable cargo doc -j4 --locked --offline --no-deps
run linux-build rustup run stable cargo build -j4 --locked --offline
cd "$root/agent"
run agent-build rustup run stable cargo build -j4 --locked --offline
cd "$root"
export MINICORE_AGENT_BIN="$cache/target-linux/debug/minicore-agent"
run e2e-stable rustup run stable cargo test -j4 --locked --offline --test agent_e2e -- --ignored --test-threads=1
run e2e-msrv env CARGO_TARGET_DIR="$cache/target-msrv" rustup run 1.85.0 cargo test -j4 --locked --offline --test agent_e2e -- --ignored --test-threads=1
run benchmark rustup run stable cargo test -j4 --locked --offline --lib ui::render_cache_tests::scrollbar_noop_event_benchmark -- --exact --ignored --nocapture
printf 'SCROLLBAR_LINUX_GATES_PASS\n'
