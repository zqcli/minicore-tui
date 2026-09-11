#!/usr/bin/env bash
set -euo pipefail
export PATH=/root/.cargo/bin:/usr/lib/llvm-19/bin:$PATH
root=/root/minicore-session-panels.H0sLzv/native-final
cache=/root/minicore-tui-027-RXdxEP
export CARGO_TARGET_DIR="$cache/target-linux"
for project in minicore-agent minicore-tui; do
  cd "$root/source/$project"
  rustup run stable cargo test -j4 --locked --offline --all-targets > "$root/logs/$project-stable.log" 2>&1
  CARGO_TARGET_DIR="$cache/target-msrv" rustup run 1.85.0 cargo test -j4 --locked --offline --all-targets > "$root/logs/$project-msrv.log" 2>&1
  rustup run stable cargo fmt --all -- --check > "$root/logs/$project-fmt.log" 2>&1
  rustup run stable cargo clippy -j4 --locked --offline --all-targets -- -D warnings > "$root/logs/$project-clippy.log" 2>&1
  RUSTDOCFLAGS='-D warnings' rustup run stable cargo doc -j4 --locked --offline --no-deps > "$root/logs/$project-doc.log" 2>&1
done
cd "$root/source/minicore-agent"
rustup run stable cargo build -j4 --locked --offline > "$root/logs/agent-linux-build.log" 2>&1
cd "$root/source/minicore-tui"
MINICORE_AGENT_BIN="$cache/target-linux/debug/minicore-agent" rustup run stable cargo test -j4 --locked --offline --test agent_e2e -- --ignored --test-threads=1 > "$root/logs/e2e.log" 2>&1
printf 'PARENT_NATIVE_FINAL_LINUX_PASS\n'
export CARGO_TARGET_DIR="$cache/target-macos-llvm"
export CARGO_TARGET_X86_64_APPLE_DARWIN_LINKER="$root/source/minicore-tui/scripts/macos-linker.sh"
export SDKROOT="$cache/MacOSX.sdk" MACOSX_DEPLOYMENT_TARGET=11.0
export CC_x86_64_apple_darwin=clang AR_x86_64_apple_darwin=llvm-ar
export CFLAGS_x86_64_apple_darwin="-target x86_64-apple-macosx11.0 -isysroot $SDKROOT"
export RUSTFLAGS='-C link-arg=-Wl,-headerpad,0x4000 -C link-arg=-mmacosx-version-min=11.0'
for project in minicore-agent minicore-tui; do
  cd "$root/source/$project"
  for profile in debug release; do
    flags=()
    if [[ "$profile" == release ]]; then flags+=(--release); fi
    rustup run 1.98.0 cargo build -j4 --locked --offline --target x86_64-apple-darwin "${flags[@]}" > "$root/logs/$project-macos-$profile.log" 2>&1
    cp "$CARGO_TARGET_DIR/x86_64-apple-darwin/$profile/$project" "$root/artifacts/$project-$profile"
  done
  dsymutil "$root/artifacts/$project-debug" -o "$root/artifacts/$project-debug.dSYM" > "$root/logs/$project-dsym.log" 2>&1
done
cd "$root/source/minicore-tui"
rustup run 1.98.0 cargo build -j4 --locked --offline --target x86_64-apple-darwin --test terminal_restore --message-format=json > "$root/logs/tty-build.jsonl" 2> "$root/logs/tty-build.log"
python3 -c 'import json,pathlib,shutil; r=pathlib.Path("/root/minicore-session-panels.H0sLzv/native-final"); frames=[json.loads(l) for l in (r/"logs/tty-build.jsonl").read_text().splitlines() if l.startswith("{")]; shutil.copy2(next(f["executable"] for f in frames if f.get("reason")=="compiler-artifact" and f.get("target",{}).get("name")=="terminal_restore" and f.get("executable")),r/"artifacts/terminal-restore")'
sha256sum "$root/artifacts/"*-debug "$root/artifacts/"*-release "$root/artifacts/terminal-restore" > "$root/logs/hashes.txt"
printf 'PARENT_NATIVE_FINAL_MACOS_PASS\n'
