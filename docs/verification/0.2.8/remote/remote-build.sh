#!/usr/bin/env bash
set -euo pipefail
export PATH=/root/.cargo/bin:/usr/lib/llvm-19/bin:/usr/local/bin:$PATH
root=/root/minicore-release-028-033.Fm9vbA
cache=/root/minicore-tui-027-RXdxEP
mkdir -p "$root/logs" "$root/artifacts"
for project in minicore-agent minicore-tui; do
  cd "$root/$project"
  export CARGO_TARGET_DIR="$cache/target-linux"
  cargo fmt --all -- --check > "$root/logs/$project-fmt.log" 2>&1
  cargo test --locked --offline --all-targets > "$root/logs/$project-stable.log" 2>&1
  cargo clippy --locked --offline --all-targets -- -D warnings > "$root/logs/$project-clippy.log" 2>&1
  RUSTDOCFLAGS='-D warnings' cargo doc --locked --offline --no-deps > "$root/logs/$project-doc.log" 2>&1
  CARGO_TARGET_DIR="$cache/target-msrv" rustup run 1.85.0 cargo test --locked --offline --all-targets > "$root/logs/$project-msrv.log" 2>&1
done
cd "$root/minicore-tui"
CARGO_TARGET_DIR="$cache/target-linux" cargo build --locked --offline --manifest-path "$root/minicore-agent/Cargo.toml" > "$root/logs/agent-linux-build.log" 2>&1
MINICORE_AGENT_BIN="$cache/target-linux/debug/minicore-agent" CARGO_TARGET_DIR="$cache/target-linux" cargo test --locked --offline --test agent_e2e -- --ignored --test-threads=1 > "$root/logs/e2e.log" 2>&1
printf 'LINUX_CHECKS_PASS\n'
export CARGO_TARGET_DIR="$cache/target-macos-llvm"
export CARGO_TARGET_X86_64_APPLE_DARWIN_LINKER="$root/minicore-tui/scripts/macos-linker.sh"
export SDKROOT="$cache/MacOSX.sdk" MACOSX_DEPLOYMENT_TARGET=11.0
export CC_x86_64_apple_darwin=clang AR_x86_64_apple_darwin=llvm-ar
export CFLAGS_x86_64_apple_darwin="-target x86_64-apple-macosx11.0 -isysroot $SDKROOT"
# Agent has no target linker flags of its own; apply the same target settings.
export RUSTFLAGS='-C link-arg=-Wl,-headerpad,0x4000 -C link-arg=-mmacosx-version-min=11.0'
for project in minicore-agent minicore-tui; do
  cd "$root/$project"
  rustup run 1.98.0 cargo build --locked --offline --target x86_64-apple-darwin > "$root/logs/$project-macos-debug.log" 2>&1
  cp "$CARGO_TARGET_DIR/x86_64-apple-darwin/debug/$project" "$root/artifacts/$project-debug"
  rustup run 1.98.0 cargo build --locked --offline --release --target x86_64-apple-darwin > "$root/logs/$project-macos-release.log" 2>&1
  cp "$CARGO_TARGET_DIR/x86_64-apple-darwin/release/$project" "$root/artifacts/$project-release"
  dsymutil "$root/artifacts/$project-debug" -o "$root/artifacts/$project-debug.dSYM" > "$root/logs/$project-dsym.log" 2>&1
done
cd "$root/minicore-tui"
rustup run 1.98.0 cargo build --locked --offline --target x86_64-apple-darwin --test terminal_restore --message-format=json > "$root/logs/tty-build.jsonl" 2> "$root/logs/tty-build.log"
python3 -c 'import json,pathlib,shutil; r=pathlib.Path("/root/minicore-release-028-033.Fm9vbA"); frames=[json.loads(l) for l in (r/"logs/tty-build.jsonl").read_text().splitlines() if l.startswith("{")]; shutil.copy2(next(f["executable"] for f in frames if f.get("reason")=="compiler-artifact" and f.get("target",{}).get("name")=="terminal_restore" and f.get("executable")),r/"artifacts/terminal-restore")'
sha256sum "$root"/artifacts/*-debug "$root"/artifacts/*-release "$root/artifacts/terminal-restore" > "$root/logs/artifact-hashes.txt"
printf 'MACOS_BUILDS_PASS\n'
