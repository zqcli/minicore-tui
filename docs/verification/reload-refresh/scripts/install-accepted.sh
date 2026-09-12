#!/usr/bin/env bash
set -euo pipefail
root=/tmp/minicore-followups.AdCugX/reload-refresh
repo=/Users/zzq/Develops/minicore-tui
backup="$repo/target/preserved-before-reload-refresh-AdCugX"
suffix=reload-refresh-AdCugX
test ! -e "$backup"
for profile in debug release; do
  case "$profile" in
    debug) expected=998b10c1a8745116ebdb7254bad43530b74533ce3673f7a4a81cf0d5fc2bce14; old=0fb1f77c203f2e105349ee858336837ea6f80c863445c28bc444b931a4add418 ;;
    release) expected=9f61b78ab1ce300bbb714a4b6399a33c05c0ed043e2f78d1e1485fed43a4b203; old=0134d6e882746b929c1d9ea26e9bc9ac9cf3672787089cf743dbdace993a9098 ;;
  esac
  binary="$root/artifacts/minicore-tui-$profile"
  target="$repo/target/$profile/minicore-tui"
  test -f "$target" && test ! -L "$target"
  test "$(shasum -a 256 "$binary" | awk '{print $1}')" = "$expected"
  test "$(shasum -a 256 "$target" | awk '{print $1}')" = "$old"
  test ! -e "$repo/target/$profile/.minicore-tui-$suffix"
  codesign --verify --strict "$binary"
done
test ! -e "$repo/target/debug/.minicore-tui-$suffix.dSYM"
test "$(dwarfdump --uuid "$root/artifacts/minicore-tui-debug" | awk '{print $2}')" = "$(dwarfdump --uuid "$root/artifacts/minicore-tui-debug.dSYM" | awk '{print $2}')"
mkdir -p "$backup/debug" "$backup/release"
for profile in debug release; do
  cp -p "$root/artifacts/minicore-tui-$profile" "$repo/target/$profile/.minicore-tui-$suffix"
  cmp "$root/artifacts/minicore-tui-$profile" "$repo/target/$profile/.minicore-tui-$suffix"
done
cp -R "$root/artifacts/minicore-tui-debug.dSYM" "$repo/target/debug/.minicore-tui-$suffix.dSYM"
for profile in debug release; do
  target="$repo/target/$profile/minicore-tui"
  old_inode=$(stat -f %i "$target")
  old_sha=$(shasum -a 256 "$target" | awk '{print $1}')
  ln "$target" "$backup/$profile/minicore-tui"
  test "$(stat -f %i "$target")" = "$old_inode"
  mv -f "$repo/target/$profile/.minicore-tui-$suffix" "$target"
  test "$(stat -f %i "$backup/$profile/minicore-tui")" = "$old_inode"
  test "$(shasum -a 256 "$backup/$profile/minicore-tui" | awk '{print $1}')" = "$old_sha"
  cmp "$root/artifacts/minicore-tui-$profile" "$target"
  codesign --verify --strict "$target"
  printf '%s old_inode=%s old_sha256=%s new_inode=%s new_sha256=%s backup=%s\n' "$target" "$old_inode" "$old_sha" "$(stat -f %i "$target")" "$(shasum -a 256 "$target" | awk '{print $1}')" "$backup/$profile/minicore-tui"
done
if test -d "$repo/target/debug/minicore-tui.dSYM"; then
  mv "$repo/target/debug/minicore-tui.dSYM" "$backup/debug/minicore-tui.dSYM"
fi
mv "$repo/target/debug/.minicore-tui-$suffix.dSYM" "$repo/target/debug/minicore-tui.dSYM"
dwarfdump --uuid "$repo/target/debug/minicore-tui" "$repo/target/debug/minicore-tui.dSYM"
for profile in debug release; do
  cmp "$root/artifacts/minicore-agent-$profile" "/Users/zzq/Develops/minicore-agent/target/$profile/minicore-agent"
done
printf 'INSTALLATION_PASS TUI-only atomic per-file replacements; no user config/store edits or process restart\n'
