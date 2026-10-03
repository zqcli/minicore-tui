#!/usr/bin/env bash
# Final quality gates; all actual statuses and source identities are enforced.
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
root=/root/minicore-tui-closeout-20261003
cap=${1:?capture}; tc=${2:?toolchain}
src=$root/$cap; out=$root/logs/$cap/full-$tc
mkdir -p "$out"
export RUSTUP_TOOLCHAIN=$tc CARGO_TARGET_DIR="$root/targets/$cap-$tc"
unset RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER
cd "$src" || exit 2
: > "$out/status.txt"
failed=0
run() { local label=$1; shift; timeout 2400 "$@" > "$out/$label.log" 2>&1; local rc=$?; printf '%s=%s\n' "$label" "$rc" | tee -a "$out/status.txt"; [ "$rc" -eq 0 ] || failed=1; }
run source_before sha256sum --check source.sha256
[ "$failed" -eq 0 ] || exit 2
sha256sum source.sha256 > "$out/source-manifest.sha256"
find . -type f -not -name source.sha256 -not -path './.git/*' -print0 | xargs -0 sha256sum | LC_ALL=C sort -k 2 > "$out/tree-before.sha256"
rustc -Vv > "$out/toolchain.log"
run tests cargo test --locked --offline --all-targets --no-fail-fast
run fmt cargo fmt --all --check
run clippy cargo clippy --locked --offline --all-targets -- -D warnings
run doc env RUSTDOCFLAGS='-D warnings' cargo doc --locked --offline --no-deps
run metadata cargo metadata --locked --offline --no-deps --format-version=1
export MINICORE_AGENT_BIN="$root/targets/fixed-agent-$tc/debug/minicore-agent"
case $tc in stable) expected=854361e303cbafe6bb39492441590087eacd1bee74eabb15ad69e7596607561f ;; 1.85.0) expected=5b003dc09308577c03bbbab4ee85aca3a3c4ffa3d3cd8ee8b6b3dd152a3fc341 ;; *) exit 2 ;; esac
printf '%s  %s\n' "$expected" "$MINICORE_AGENT_BIN" > "$out/agent-expected.sha256"
run agent_identity_before sha256sum --check "$out/agent-expected.sha256"
run agent_version "$MINICORE_AGENT_BIN" --version
if sha256sum --check "$out/agent-expected.sha256" >/dev/null 2>&1; then
  run e2e setsid timeout --kill-after=10s 300 cargo test --locked --offline --test agent_e2e -- --ignored --test-threads=1 --nocapture
  if ! grep -q 'test result: ok. 35 passed; 0 failed' "$out/e2e.log"; then printf 'e2e_count=1\n' >> "$out/status.txt"; failed=1; else printf 'e2e_count=0\n' >> "$out/status.txt"; fi
else printf 'e2e=BLOCKED_AGENT_IDENTITY\n' >> "$out/status.txt"; failed=1; fi
run agent_identity_after sha256sum --check "$out/agent-expected.sha256"
run source_after sha256sum --check source.sha256
find . -type f -not -name source.sha256 -not -path './.git/*' -print0 | xargs -0 sha256sum | LC_ALL=C sort -k 2 > "$out/tree-after.sha256"
run tree_immutable cmp "$out/tree-before.sha256" "$out/tree-after.sha256"
printf 'final_status=%s\n' "$failed" >> "$out/status.txt"
printf 'finished %s exit %s\n' "$(date -u +%FT%TZ)" "$failed" > "$out/completed.txt"
exit "$failed"
