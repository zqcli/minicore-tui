#!/usr/bin/env bash
set -euo pipefail
export PATH=/root/.cargo/bin:/root/.local/bin:/usr/local/bin:$PATH
root=/root/minicore-tui-027-RXdxEP
export CARGO_TARGET_DIR="$root/target-linux"
cd "$root/minicore-tui"
cargo fmt --all -- --check > "$root/logs/fmt.log" 2>&1
cargo test --locked --offline --all-targets > "$root/logs/stable.log" 2>&1
cargo clippy --locked --offline --all-targets -- -D warnings > "$root/logs/clippy.log" 2>&1
RUSTDOCFLAGS='-D warnings' cargo doc --locked --offline --no-deps > "$root/logs/doc.log" 2>&1
cargo build --manifest-path "$root/minicore-agent/Cargo.toml" --locked --offline > "$root/logs/agent-build.log" 2>&1
MINICORE_AGENT_BIN="$CARGO_TARGET_DIR/debug/minicore-agent" cargo test --locked --offline --test agent_e2e -- --ignored --test-threads=1 > "$root/logs/e2e.log" 2>&1
CARGO_TARGET_DIR="$root/target-msrv" rustup run 1.85.0 cargo test --locked --offline --all-targets > "$root/logs/msrv.log" 2>&1
printf 'ALL_LINUX_CHECKS_PASS\n'
