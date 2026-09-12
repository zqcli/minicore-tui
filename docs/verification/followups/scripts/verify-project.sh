#!/usr/bin/env bash
# Parent-owned remote-only verification; never run on the workstation.
set -euo pipefail
[[ "$(uname -s)" == Linux ]] || { printf 'remote Linux only\n' >&2; exit 2; }
project=${1:?project}
stage=${2:?stage}
[[ "$project" == minicore-agent || "$project" == minicore-tui ]] || exit 2
[[ "$stage" =~ ^[a-z0-9-]+$ ]] || exit 2
root=/root/minicore-followups.6Dpa6e
export PATH=/root/.cargo/bin:$PATH
export CARGO_TARGET_DIR=/root/minicore-tui-027-RXdxEP/target-linux
cd "$root/source/$project"
run() {
  local label=$1
  shift
  local log="$root/logs/parent-$stage-$label.log"
  printf 'cwd=%s\ncommand=' "$PWD" > "$log"
  printf '%q ' "$@" >> "$log"
  printf '\n' >> "$log"
  local result=0
  "$@" >> "$log" 2>&1 || result=$?
  printf '\nexit=%s\n' "$result" >> "$log"
  if [[ "$result" != 0 ]]; then
    tail -n 65 "$log"
    exit "$result"
  fi
}
run format rustup run stable cargo fmt --all
run stable rustup run stable cargo test -j4 --locked --offline --all-targets
run msrv env CARGO_TARGET_DIR=/root/minicore-tui-027-RXdxEP/target-msrv rustup run 1.85.0 cargo test -j4 --locked --offline --all-targets
run fmt-check rustup run stable cargo fmt --all -- --check
run clippy rustup run stable cargo clippy -j4 --locked --offline --all-targets -- -D warnings
run doc env RUSTDOCFLAGS=-D\ warnings rustup run stable cargo doc -j4 --locked --offline --no-deps
run build rustup run stable cargo build -j4 --locked --offline
if [[ "$project" == minicore-tui ]]; then
  run e2e env MINICORE_AGENT_BIN="$CARGO_TARGET_DIR/debug/minicore-agent" rustup run stable cargo test -j4 --locked --offline --test agent_e2e -- --ignored --test-threads=1
fi
printf 'PARENT_REMOTE_VERIFIED project=%s stage=%s\n' "$project" "$stage"
