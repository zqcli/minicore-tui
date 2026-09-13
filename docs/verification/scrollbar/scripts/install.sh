#!/usr/bin/env bash
set -euo pipefail
root=/tmp/minicore-scrollbar.jB2v84
repo=/Users/zzq/Develops/minicore-tui
backup="$repo/target/preserved-before-scrollbar-jB2v84"
suffix=scrollbar-jB2v84
test ! -e "$backup"
for profile in debug release; do
 case "$profile" in
 debug) expected=f1a9eb3516e5ed1f9632c1ef4b1fe1cc622aa8545878a30141cc7e9b61e1917e; old=998b10c1a8745116ebdb7254bad43530b74533ce3673f7a4a81cf0d5fc2bce14 ;;
 release) expected=c4d97f442290e5c3c58496ce46719d3138cfbabb17e6ad87a3d53364bacbf019; old=9f61b78ab1ce300bbb714a4b6399a33c05c0ed043e2f78d1e1485fed43a4b203 ;;
 esac
 target="$repo/target/$profile/minicore-tui"
 test -f "$target" && test ! -L "$target"
 test "$(shasum -a 256 "$root/artifacts/minicore-tui-$profile" | awk '{print $1}')" = "$expected"
 test "$(shasum -a 256 "$target" | awk '{print $1}')" = "$old"
 test ! -e "$repo/target/$profile/.minicore-tui-$suffix"
 codesign --verify --strict "$root/artifacts/minicore-tui-$profile"
done
test ! -e "$repo/target/debug/.minicore-tui-$suffix.dSYM"
test ! -L "$repo/target/debug/minicore-tui.dSYM"
test "$(dwarfdump --uuid "$root/artifacts/minicore-tui-debug" | awk '{print $2}')" = "$(dwarfdump --uuid "$root/artifacts/minicore-tui-debug.dSYM" | awk '{print $2}')"
check_agent() {
 test "$(shasum -a 256 /Users/zzq/Develops/minicore-agent/target/debug/minicore-agent | awk '{print $1}')" = 443b88b385d778a41303540f940718cf85968a375ca0f334437f9ff42c4703e6
 test "$(shasum -a 256 /Users/zzq/Develops/minicore-agent/target/release/minicore-agent | awk '{print $1}')" = 57a1c3722cf05c251f499b8a1e4d50032e9c8b3ce9e4efcd27028ff4dfc73914
}
check_agent
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
 mv -f "$repo/target/$profile/.minicore-tui-$suffix" "$target"
 test "$(stat -f %i "$backup/$profile/minicore-tui")" = "$old_inode"
 test "$(shasum -a 256 "$backup/$profile/minicore-tui" | awk '{print $1}')" = "$old_sha"
 cmp "$root/artifacts/minicore-tui-$profile" "$target"
 codesign --verify --strict "$target"
 printf '%s old_inode=%s old_sha256=%s new_inode=%s new_sha256=%s backup=%s\n' "$target" "$old_inode" "$old_sha" "$(stat -f %i "$target")" "$(shasum -a 256 "$target" | awk '{print $1}')" "$backup/$profile/minicore-tui"
done
if test -d "$repo/target/debug/minicore-tui.dSYM"; then mv "$repo/target/debug/minicore-tui.dSYM" "$backup/debug/minicore-tui.dSYM"; fi
mv "$repo/target/debug/.minicore-tui-$suffix.dSYM" "$repo/target/debug/minicore-tui.dSYM"
dwarfdump --uuid "$repo/target/debug/minicore-tui" "$repo/target/debug/minicore-tui.dSYM"
check_agent
printf 'INSTALLATION_PASS: TUI-only per-file atomic replacement; old inodes preserved; Agent unchanged; no user-process restart\n'
