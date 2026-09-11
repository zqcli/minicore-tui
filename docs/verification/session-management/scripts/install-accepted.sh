#!/bin/bash
# Install only the remotely built and natively accepted artifacts.
set -euo pipefail
root=/tmp/minicore-session-panels.M0rGv9
artifacts="$root/final-artifacts"
backup_name=preserved-before-session-M0rGv9
for project in minicore-agent minicore-tui; do
  repo="/Users/zzq/Develops/$project"
  test ! -e "$repo/target/$backup_name"
  for profile in debug release; do
    binary="$artifacts/$project-$profile"
    case "$project-$profile" in
      minicore-agent-debug) expected=84c85c768f17741ac69e49e81da1c7b2dd92d7c4eed360a51d43e337c86d0bc8 ;;
      minicore-agent-release) expected=aee24246491bdb13ffb95dff4fccc95895800e5f19b67c51c815132540385642 ;;
      minicore-tui-debug) expected=1e7bff4e063c47a42663b1a509d63247e34891a194bc041054cbd4aa2513cb57 ;;
      minicore-tui-release) expected=11408c7d5a557d99feb4bdbe3160d4133467dedededb499e037990e95932cb6d ;;
    esac
    test "$(shasum -a 256 "$binary" | awk '{print $1}')" = "$expected"
    codesign --verify --strict "$binary"
    test -f "$repo/target/$profile/$project"
    test ! -L "$repo/target/$profile/$project"
    test ! -e "$repo/target/$profile/.$project-session-M0rGv9"
  done
  test ! -e "$repo/target/debug/.$project-session-M0rGv9.dSYM"
  binary_uuid=$(dwarfdump --uuid "$artifacts/$project-debug" | awk '{print $2}')
  dsym_uuid=$(dwarfdump --uuid "$artifacts/$project-debug.dSYM" | awk '{print $2}')
  test "$binary_uuid" = "$dsym_uuid"
done
# Stage all four binaries and both symbol bundles before replacing any binary.
for project in minicore-agent minicore-tui; do
  repo="/Users/zzq/Develops/$project"
  mkdir -p "$repo/target/$backup_name/debug" "$repo/target/$backup_name/release"
  for profile in debug release; do
    cp -p "$artifacts/$project-$profile" "$repo/target/$profile/.$project-session-M0rGv9"
    cmp "$artifacts/$project-$profile" "$repo/target/$profile/.$project-session-M0rGv9"
  done
  cp -R "$artifacts/$project-debug.dSYM" "$repo/target/debug/.$project-session-M0rGv9.dSYM"
done
for project in minicore-agent minicore-tui; do
  repo="/Users/zzq/Develops/$project"
  for profile in debug release; do
    target="$repo/target/$profile/$project"
    backup="$repo/target/$backup_name/$profile/$project"
    old_inode=$(stat -f %i "$target")
    old_sha=$(shasum -a 256 "$target" | awk '{print $1}')
    ln "$target" "$backup"
    mv -f "$repo/target/$profile/.$project-session-M0rGv9" "$target"
    test "$(stat -f %i "$backup")" = "$old_inode"
    test "$(shasum -a 256 "$backup" | awk '{print $1}')" = "$old_sha"
    cmp "$artifacts/$project-$profile" "$target"
    codesign --verify --strict "$target"
    printf '%s old_inode=%s old_sha256=%s new_inode=%s new_sha256=%s backup=%s\n' \
      "$target" "$old_inode" "$old_sha" "$(stat -f %i "$target")" \
      "$(shasum -a 256 "$target" | awk '{print $1}')" "$backup"
  done
  if test -d "$repo/target/debug/$project.dSYM"; then
    mv "$repo/target/debug/$project.dSYM" "$repo/target/$backup_name/debug/$project.dSYM"
  fi
  mv "$repo/target/debug/.$project-session-M0rGv9.dSYM" "$repo/target/debug/$project.dSYM"
  dwarfdump --uuid "$repo/target/debug/$project" "$repo/target/debug/$project.dSYM"
done
printf 'INSTALLATION_PASS no user config/store edits; no user process restart\n'
