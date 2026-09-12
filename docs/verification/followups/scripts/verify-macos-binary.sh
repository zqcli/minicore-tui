#!/bin/sh
set -eu

fail() {
    printf 'error: %s\n' "$*" >&2
    return 1
}

text_offset_from_load_commands() {
    printf '%s\n' "$1" | awk '
        /^Load command / { in_section = 0 }
        $1 == "Section" && NF == 1 {
            in_section = 1
            sectname = ""
            segname = ""
            next
        }
        in_section && $1 == "sectname" { sectname = $2; next }
        in_section && $1 == "segname" { segname = $2; next }
        in_section && $1 == "offset" {
            if (sectname == "__text" && segname == "__TEXT") {
                print $2
                exit
            }
            in_section = 0
        }
    '
}

signature_info_from_load_commands() {
    printf '%s\n' "$1" | awk '
        /^Load command / { in_signature = 0 }
        $1 == "cmd" && $2 == "LC_CODE_SIGNATURE" {
            in_signature = 1
            dataoff = ""
            datasize = ""
            next
        }
        in_signature && ($1 == "dataoff" || $1 == "datasize") {
            if ($1 == "dataoff") {
                dataoff = $2
            } else {
                datasize = $2
            }
            if (dataoff != "" && datasize != "") {
                print dataoff, datasize
                exit
            }
            next
        }
    '
}

has_signature_command() {
    printf '%s\n' "$1" | awk '
        $1 == "cmd" && $2 == "LC_CODE_SIGNATURE" { print "yes"; exit }
    '
}

check_macho_metadata() {
    metadata_header=$1
    metadata_load_commands=$2
    metadata_file_size=$3

    metadata_ncmds=$(printf '%s\n' "$metadata_header" | awk '/MH_MAGIC_64/{print $6; exit}')
    metadata_sizeofcmds=$(printf '%s\n' "$metadata_header" | awk '/MH_MAGIC_64/{print $7; exit}')
    metadata_text_offset=$(text_offset_from_load_commands "$metadata_load_commands")
    metadata_command_count=$(printf '%s\n' "$metadata_load_commands" | awk '/^Load command /{count++} END{print count+0}')
    metadata_command_size_sum=$(printf '%s\n' "$metadata_load_commands" | awk '/^[[:space:]]*cmdsize /{sum += $2} END{print sum+0}')
    metadata_minos=$(printf '%s\n' "$metadata_load_commands" | awk '
        /cmd LC_BUILD_VERSION/ { in_build = 1; next }
        /^Load command / && in_build { exit }
        in_build && $1 == "minos" { print $2; exit }
    ')

    if [ -z "$metadata_ncmds" ] || [ -z "$metadata_sizeofcmds" ] || [ -z "$metadata_text_offset" ]; then
        fail 'could not parse Mach-O header or __TEXT,__text section'
        return 1
    fi
    if [ "$metadata_ncmds" -le 0 ] || [ "$metadata_sizeofcmds" -le 0 ] || [ "$metadata_text_offset" -le 0 ]; then
        fail 'Mach-O header or __TEXT,__text offset is zero'
        return 1
    fi
    if [ "$metadata_command_count" -ne "$metadata_ncmds" ] || \
        [ "$metadata_command_size_sum" -ne "$metadata_sizeofcmds" ]; then
        fail 'load-command count/size does not match Mach-O header'
        return 1
    fi

    metadata_load_end=$((32 + metadata_sizeofcmds))
    if [ "$metadata_load_end" -ge "$metadata_text_offset" ]; then
        fail "load commands end at $metadata_load_end, __TEXT,__text starts at $metadata_text_offset"
        return 1
    fi
    if [ "$metadata_minos" != 11.0 ]; then
        fail "expected macOS minos 11.0, got ${metadata_minos:-missing}"
        return 1
    fi
    if [ -z "$metadata_file_size" ] || [ "$metadata_file_size" -le 0 ]; then
        fail 'file size is missing or zero'
        return 1
    fi

    metadata_has_signature=$(has_signature_command "$metadata_load_commands")
    if [ "$metadata_has_signature" = yes ]; then
        metadata_signature=$(signature_info_from_load_commands "$metadata_load_commands")
        if [ -z "$metadata_signature" ]; then
            fail 'LC_CODE_SIGNATURE has no dataoff/datasize'
            return 1
        fi
        metadata_dataoff=${metadata_signature%% *}
        metadata_datasize=${metadata_signature#* }
        if [ "$metadata_dataoff" -le 0 ] || [ "$metadata_datasize" -le 0 ]; then
            fail 'LC_CODE_SIGNATURE dataoff/datasize must be positive'
            return 1
        fi
        metadata_signature_end=$((metadata_dataoff + metadata_datasize))
        if [ "$metadata_signature_end" -gt "$metadata_file_size" ]; then
            fail "LC_CODE_SIGNATURE ends at $metadata_signature_end, file size is $metadata_file_size"
            return 1
        fi
        printf 'signature_dataoff=%s datasize=%s signature_end=%s file_size=%s\n' \
            "$metadata_dataoff" "$metadata_datasize" "$metadata_signature_end" "$metadata_file_size"
    else
        printf '%s\n' 'signature_command=absent'
    fi

    printf 'ncmds=%s sizeofcmds=%s load_end=%s first_text_offset=%s gap=%s\n' \
        "$metadata_ncmds" "$metadata_sizeofcmds" "$metadata_load_end" \
        "$metadata_text_offset" "$((metadata_text_offset - metadata_load_end))"
    printf 'minos=%s\n' "$metadata_minos"
}

expect_metadata_failure() {
    if check_macho_metadata "$1" "$2" "$3" >/dev/null 2>&1; then
        fail "$4 unexpectedly passed"
        return 1
    fi
}

run_self_test() {
    unsigned_header=$(cat <<'EOF'
Mach header
      magic  cputype cpusubtype  caps    filetype ncmds sizeofcmds      flags
MH_MAGIC_64   X86_64        ALL  0x00     EXECUTE    2       104        PIE
EOF
)
    unsigned_load_commands=$(cat <<'EOF'
Load command 0
      cmd LC_SEGMENT_64
  cmdsize 72
  segname __TEXT
Section
  sectname __text
   segname __TEXT
    offset 256
Load command 1
      cmd LC_BUILD_VERSION
  cmdsize 32
     minos 11.0
EOF
)
    cstring_load_commands=$(cat <<'EOF'
Load command 0
      cmd LC_SEGMENT_64
  cmdsize 72
  segname __TEXT
Section
  sectname __cstring
   segname __TEXT
    offset 256
Load command 1
      cmd LC_BUILD_VERSION
  cmdsize 32
     minos 11.0
EOF
)
    zero_text_load_commands=$(cat <<'EOF'
Load command 0
      cmd LC_SEGMENT_64
  cmdsize 72
  segname __TEXT
Section
  sectname __text
   segname __TEXT
    offset 0
Load command 1
      cmd LC_BUILD_VERSION
  cmdsize 32
     minos 11.0
EOF
)
    signed_header=$(cat <<'EOF'
Mach header
      magic  cputype cpusubtype  caps    filetype ncmds sizeofcmds      flags
MH_MAGIC_64   X86_64        ALL  0x00     EXECUTE    3       120        PIE
EOF
)
    signed_load_commands=$(cat <<'EOF'
Load command 0
      cmd LC_SEGMENT_64
  cmdsize 72
  segname __TEXT
Section
  sectname __text
   segname __TEXT
    offset 256
Load command 1
      cmd LC_BUILD_VERSION
  cmdsize 32
     minos 11.0
Load command 2
      cmd LC_CODE_SIGNATURE
  cmdsize 16
    dataoff 400
   datasize 80
EOF
)
    bad_signature_load_commands=$(printf '%s\n' "$signed_load_commands" | sed 's/dataoff 400/dataoff 500/')

    check_macho_metadata "$unsigned_header" "$unsigned_load_commands" 512 >/dev/null
    check_macho_metadata "$signed_header" "$signed_load_commands" 512 >/dev/null
    expect_metadata_failure "$unsigned_header" "$cstring_load_commands" 512 'cstring-only section fixture'
    expect_metadata_failure "$unsigned_header" "$zero_text_load_commands" 512 'zero __text offset fixture'
    expect_metadata_failure "$signed_header" "$bad_signature_load_commands" 512 'bad signature fixture'
    printf '%s\n' 'self-test=passed (unsigned,signed,cstring,zero-text,bad-signature)'
}

if [ "${1-}" = '--self-test' ]; then
    if [ "$#" -ne 1 ]; then
        printf 'usage: %s --self-test\n' "$0" >&2
        exit 2
    fi
    run_self_test
    exit 0
fi

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/.." && pwd)
binary=${1:-"$repo_root/target/x86_64-apple-darwin/release/minicore-tui"}

if [ "$#" -gt 1 ]; then
    printf 'usage: %s [binary]\n' "$0" >&2
    exit 2
fi
if [ ! -f "$binary" ]; then
    fail "binary not found: $binary"
    exit 1
fi

for tool in file otool nm size dwarfdump codesign stat; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        fail "$tool is required on macOS"
        exit 1
    fi
done

file_info=$(file "$binary")
case "$file_info" in
    *'Mach-O 64-bit executable x86_64'*) ;;
    *)
        fail "expected x86_64 Mach-O, got: $file_info"
        exit 1
        ;;
esac

header=$(otool -hv "$binary")
load_commands=$(otool -l "$binary")
file_size=$(stat -f %z "$binary")
check_macho_metadata "$header" "$load_commands" "$file_size"
printf '%s\n' "$file_info"
nm -arch x86_64 "$binary" >/dev/null
size "$binary" >/dev/null
dwarfdump --uuid "$binary"
printf '%s\n' 'nm=passed size=passed dwarfdump=passed'

if [ "$(has_signature_command "$load_commands")" = yes ]; then
    codesign --verify --strict --verbose=2 "$binary" >/dev/null
    printf '%s\n' 'codesign=valid'
else
    printf '%s\n' 'codesign=unsigned'
fi
