#!/usr/bin/env bash
set -uo pipefail
root=/root/minicore-tui-closeout-20261003
cap=src-close2-final-c70c73c4
out=$root/logs/$cap
mkdir -p "$out"
: > "$out/driver-status.txt"
failed=0
for tc in stable 1.85.0; do
  printf 'running full %s %s\n' "$tc" "$(date -u +%FT%TZ)" >> "$out/driver-status.txt"
  bash "$root/qa-close2-full.sh" "$cap" "$tc" > "$out/driver-full-$tc.log" 2>&1
  rc=$?
  printf 'full_%s=%s\n' "$tc" "$rc" >> "$out/driver-status.txt"
  [ "$rc" -eq 0 ] || failed=1
 done
for tc in stable 1.85.0; do
  printf 'running perfpty %s %s\n' "$tc" "$(date -u +%FT%TZ)" >> "$out/driver-status.txt"
  bash "$root/qa-close2-perfpty.sh" "$cap" "$tc" > "$out/driver-perfpty-$tc.log" 2>&1
  rc=$?
  printf 'perfpty_%s=%s\n' "$tc" "$rc" >> "$out/driver-status.txt"
  [ "$rc" -eq 0 ] || failed=1
 done
printf 'driver_exit=%s\nfinished=%s\n' "$failed" "$(date -u +%FT%TZ)" >> "$out/driver-status.txt"
printf '%s\n' "$failed" > "$out/driver-completed.txt"
exit "$failed"
