#!/bin/bash
# Only remotely built, natively accepted artifacts enter existing install paths.
set -euo pipefail
root=/tmp/minicore-followups.AdCugX/accepted
artifacts="$root/artifacts"
backup_name=preserved-before-followups-AdCugX
suffix=followups-AdCugX
for project in minicore-agent minicore-tui; do
  repo="/Users/zzq/Develops/$project"
  test ! -e "$repo/target/$backup_name"
  for profile in debug release; do
    binary="$artifacts/$project-$profile"
    case "$project-$profile" in
      minicore-agent-debug) expected=443b88b385d778a41303540f940718cf85968a375ca0f334437f9ff42c4703e6 ;;
      minicore-agent-release) expected=57a1c3722cf05c251f499b8a1e4d50032e9c8b3ce9e4efcd27028ff4dfc73914 ;;
      minicore-tui-debug) expected=0fb1f77c203f2e105349ee858336837ea6f80c863445c28bc444b931a4add418 ;;
      minicore-tui-release) expected=0134d6e882746b929c1d9ea26e9bc9ac9cf3672787089cf743dbdace993a9098 ;;
    esac
    test "$(shasum -a 256 "$binary" | awk '{print $1}')" = "$expected"
    codesign --verify --strict "$binary"
    test -f "$repo/target/$profile/$project"
    test ! -L "$repo/target/$profile/$project"
    test ! -e "$repo/target/$profile/.$project-$suffix"
  done
  test ! -e "$repo/target/debug/.$project-$suffix.dSYM"
  binary_uuid=$(dwarfdump --uuid "$artifacts/$project-debug" | awk '{print $2}')
  dsym_uuid=$(dwarfdump --uuid "$artifacts/$project-debug.dSYM" | awk '{print $2}')
  test "$binary_uuid" = "$dsym_uuid"
done
for project in minicore-agent minicore-tui; do
  repo="/Users/zzq/Develops/$project"
  mkdir -p "$repo/target/$backup_name/debug" "$repo/target/$backup_name/release"
  for profile in debug release; do
    cp -p "$artifacts/$project-$profile" "$repo/target/$profile/.$project-$suffix"
    cmp "$artifacts/$project-$profile" "$repo/target/$profile/.$project-$suffix"
  done
  cp -R "$artifacts/$project-debug.dSYM" "$repo/target/debug/.$project-$suffix.dSYM"
done
for project in minicore-agent minicore-tui; do
  repo="/Users/zzq/Develops/$project"
  for profile in debug release; do
    target="$repo/target/$profile/$project"
    backup="$repo/target/$backup_name/$profile/$project"
    old_inode=$(stat -f %i "$target")
    old_sha=$(shasum -a 256 "$target" | awk '{print $1}')
    ln "$target" "$backup"
    mv -f "$repo/target/$profile/.$project-$suffix" "$target"
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
  mv "$repo/target/debug/.$project-$suffix.dSYM" "$repo/target/debug/$project.dSYM"
  dwarfdump --uuid "$repo/target/debug/$project" "$repo/target/debug/$project.dSYM"
done
printf 'INSTALLATION_PASS no user config/store edits; no user process restart\n'
