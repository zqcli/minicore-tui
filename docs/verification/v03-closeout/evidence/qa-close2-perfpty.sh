#!/usr/bin/env bash
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
root=/root/minicore-tui-closeout-20261003
cap=${1:?capture}; tc=${2:?toolchain}
src=$root/$cap; out=$root/logs/$cap/perfpty-$tc
mkdir -p "$out"
export RUSTUP_TOOLCHAIN=$tc CARGO_TARGET_DIR="$root/targets/$cap-$tc"
unset RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER
cd "$src" || exit 2
failed=0; : > "$out/status.txt"
run() { local label=$1; shift; timeout 2400 "$@" > "$out/$label.log" 2>&1; local rc=$?; printf '%s=%s\n' "$label" "$rc" | tee -a "$out/status.txt"; [ "$rc" -eq 0 ] || failed=1; }
run source_before sha256sum --check source.sha256
[ "$failed" -eq 0 ] || exit 2
sha256sum source.sha256 > "$out/source-manifest.sha256"
find . -type f -not -name source.sha256 -print0 | xargs -0 sha256sum | LC_ALL=C sort -k 2 > "$out/tree-before.sha256"
rustc -Vv > "$out/toolchain.log"
run release_build cargo build --release --locked --offline
[ "$failed" -eq 0 ] || exit 1
TUI_BIN=$CARGO_TARGET_DIR/release/minicore-tui
sha256sum "$TUI_BIN" > "$out/tui-release-bin.sha256" || exit 2
run performance cargo test --release --locked --offline --test performance -- --ignored --nocapture
if ! grep -q 'test result: ok. 9 passed; 0 failed' "$out/performance.log"; then printf 'performance_count=1\n' >> "$out/status.txt"; failed=1; else printf 'performance_count=0\n' >> "$out/status.txt"; fi
artifact_path() {
  local want=$1; shift
  cargo test --locked --offline --no-run --message-format=json "$@" > "$out/artifact-$want.jsonl" 2> "$out/artifact-$want.stderr"
  local rc=$?
  printf 'artifact_%s=%s\n' "$want" "$rc" >> "$out/status.txt"
  [ "$rc" -eq 0 ] || return "$rc"
  python3 - "$out/artifact-$want.jsonl" "$want" <<'PY'
import sys, json, pathlib
path,want=sys.argv[1:]
bins=[]
for line in open(path):
    m=json.loads(line)
    if m.get('reason')=='compiler-artifact' and m.get('executable') and m.get('target',{}).get('name')==want:
        bins.append(m['executable'])
assert len(bins)==1 and pathlib.Path(bins[0]).is_file(), bins
print(bins[0])
PY
}
TERMINAL_BIN=$(artifact_path terminal_restore --test terminal_restore) || exit 2
FAKE_BIN=$(artifact_path agent_process --test agent_process) || exit 2
MAIN_BIN=$(artifact_path minicore-tui --bin minicore-tui) || exit 2
printf 'terminal_bin=%s\nfake_agent_bin=%s\nmain_test_bin=%s\n' "$TERMINAL_BIN" "$FAKE_BIN" "$MAIN_BIN" >> "$out/status.txt"
sha256sum "$TERMINAL_BIN" "$FAKE_BIN" "$MAIN_BIN" > "$out/test-bins.sha256" || exit 2
run pty timeout 900 python3 scripts/pty_terminal_validation.py --terminal-test-bin "$TERMINAL_BIN" --tui-bin "$TUI_BIN" --fake-agent-bin "$FAKE_BIN" --main-test-bin "$MAIN_BIN" --output "$out/pty-report.json"
run pty_report python3 -c 'import json,sys; r=json.load(open(sys.argv[1])); print(json.dumps(r,ensure_ascii=False,indent=2)); assert r.get("status")=="PASS",r.get("status")' "$out/pty-report.json"
run source_after sha256sum --check source.sha256
find . -type f -not -name source.sha256 -print0 | xargs -0 sha256sum | LC_ALL=C sort -k 2 > "$out/tree-after.sha256"
run tree_immutable cmp "$out/tree-before.sha256" "$out/tree-after.sha256"
printf 'final_status=%s\n' "$failed" >> "$out/status.txt"
printf 'finished %s exit %s\n' "$(date -u +%FT%TZ)" "$failed" > "$out/completed.txt"
exit "$failed"
