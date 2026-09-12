#!/usr/bin/env bash
set -euo pipefail
[[ "$(uname -s)" == Linux ]] || exit 2
root=/root/minicore-followups.6Dpa6e/reload-refresh
cache=/root/minicore-tui-027-RXdxEP
export PATH=/root/.cargo/bin:/usr/lib/llvm-19/bin:$PATH
export CARGO_TARGET_DIR="$cache/target-linux"
run() {
  local label=$1
  shift
  local log="$root/logs/$label.log" result=0
  printf 'cwd=%s\ncommand=' "$PWD" > "$log"
  printf '%q ' "$@" >> "$log"
  printf '\n' >> "$log"
  "$@" >> "$log" 2>&1 || result=$?
  printf '\nexit=%s\n' "$result" >> "$log"
  if [[ "$result" != 0 ]]; then tail -n 65 "$log"; exit "$result"; fi
}
run stable-version rustup run stable rustc --version
run msrv-version rustup run 1.85.0 rustc --version
run delivery-version rustup run 1.98.0 rustc --version
run clang-version clang --version
run linker-version ld.lld --version
cd "$root/source/minicore-agent"
run agent-linux-build rustup run stable cargo build -j4 --locked --offline
cd "$root/source/minicore-tui"
run tui-stable rustup run stable cargo test -j4 --locked --offline --all-targets
run tui-msrv env CARGO_TARGET_DIR="$cache/target-msrv" rustup run 1.85.0 cargo test -j4 --locked --offline --all-targets
run tui-fmt rustup run stable cargo fmt --all -- --check
run tui-clippy rustup run stable cargo clippy -j4 --locked --offline --all-targets -- -D warnings
run tui-doc env RUSTDOCFLAGS=-D\ warnings rustup run stable cargo doc -j4 --locked --offline --no-deps
run tui-linux-build rustup run stable cargo build -j4 --locked --offline
export MINICORE_AGENT_BIN="$cache/target-linux/debug/minicore-agent"
run e2e-stable rustup run stable cargo test -j4 --locked --offline --test agent_e2e -- --ignored --test-threads=1
run e2e-msrv env CARGO_TARGET_DIR="$cache/target-msrv" rustup run 1.85.0 cargo test -j4 --locked --offline --test agent_e2e -- --ignored --test-threads=1
printf 'PARENT_RELOAD_REFRESH_LINUX_PASS\n'
export CARGO_TARGET_DIR="$cache/target-macos-llvm"
export CARGO_TARGET_X86_64_APPLE_DARWIN_LINKER="$root/source/minicore-tui/scripts/macos-linker.sh"
export SDKROOT="$cache/MacOSX.sdk" MACOSX_DEPLOYMENT_TARGET=11.0
export CC_x86_64_apple_darwin=clang AR_x86_64_apple_darwin=llvm-ar
export CFLAGS_x86_64_apple_darwin="-target x86_64-apple-macosx11.0 -isysroot $SDKROOT"
export RUSTFLAGS='-C link-arg=-Wl,-headerpad,0x4000 -C link-arg=-mmacosx-version-min=11.0'
for profile in debug release; do
  flags=()
  if [[ "$profile" == release ]]; then flags+=(--release); fi
  run "tui-macos-$profile" rustup run 1.98.0 cargo build -j4 --locked --offline --target x86_64-apple-darwin "${flags[@]}"
  cp "$CARGO_TARGET_DIR/x86_64-apple-darwin/$profile/minicore-tui" "$root/artifacts/minicore-tui-$profile"
done
run tui-dsym dsymutil "$root/artifacts/minicore-tui-debug" -o "$root/artifacts/minicore-tui-debug.dSYM"
rustup run 1.98.0 cargo build -j4 --locked --offline --target x86_64-apple-darwin --test terminal_restore --message-format=json > "$root/logs/tty-build.jsonl" 2> "$root/logs/tty-build.log"
python3 -c 'import json,pathlib,shutil; r=pathlib.Path("/root/minicore-followups.6Dpa6e/reload-refresh"); frames=[json.loads(l) for l in (r/"logs/tty-build.jsonl").read_text().splitlines() if l.startswith("{")]; shutil.copy2(next(f["executable"] for f in frames if f.get("reason")=="compiler-artifact" and f.get("target",{}).get("name")=="terminal_restore" and f.get("executable")),r/"artifacts/terminal-restore")'
(cd "$root" && sha256sum source/*.tar artifacts/*-debug artifacts/*-release artifacts/terminal-restore) > "$root/hashes.txt"
printf 'PARENT_RELOAD_REFRESH_MACOS_PASS\n'
