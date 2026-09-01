#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/.." && pwd)
cd "$repo_root"

if ! command -v cargo-zigbuild >/dev/null 2>&1; then
    printf '%s\n' 'error: cargo-zigbuild is required' >&2
    exit 1
fi
if ! command -v zig >/dev/null 2>&1; then
    printf '%s\n' 'error: zig is required' >&2
    exit 1
fi

readonly target=x86_64-apple-darwin
MACOSX_DEPLOYMENT_TARGET=11.0
export MACOSX_DEPLOYMENT_TARGET

# Keep the build reproducible from the checked-in target configuration.
unset \
    RUSTFLAGS \
    CARGO_ENCODED_RUSTFLAGS \
    CARGO_BUILD_RUSTFLAGS \
    CARGO_TARGET_X86_64_APPLE_DARWIN_RUSTFLAGS \
    CARGO_TARGET_X86_64_APPLE_DARWIN_LINKER \
    CARGO_BUILD_TARGET \
    CARGO_TARGET_DIR \
    CARGO_BUILD_TARGET_DIR

# cargo-zigbuild 0.20.x emits an invalid Zig 0.13 target when a Darwin
# version suffix is used. The wrapper keeps the valid target spelling while
# cargo-zigbuild still supplies its macOS SDK dependency paths.
link_dir=$(mktemp -d "${TMPDIR:-/tmp}/minicore-tui-link.XXXXXX")
trap 'rm -rf "$link_dir"' EXIT HUP INT TERM
linker="$link_dir/darwin-linker"
cargo_zigbuild=$(command -v cargo-zigbuild)
printf '%s\n' '#!/bin/sh' "exec \"$cargo_zigbuild\" zig cc -- -g -fno-sanitize=all -target x86_64-macos.11.0 \"\$@\"" >"$linker"
chmod 755 "$linker"
export CARGO_TARGET_X86_64_APPLE_DARWIN_LINKER="$linker"

cargo +1.85.0 zigbuild --release --locked --target "$target"
test -x target/x86_64-apple-darwin/release/minicore-tui
artifact=target/$target/release/minicore-tui
printf 'artifact=%s\n' "$artifact"
printf 'artifact_absolute=%s/%s\n' "$repo_root" "$artifact"
