#!/usr/bin/env bash
set -euo pipefail

echo "=== KnightOS Toolchain Check (POSIX) ==="
missing=0

check_cmd() {
    local label="$1"
    local cmd="$2"
    local req="${3:-1}"
    if command -v "$cmd" >/dev/null 2>&1; then
        printf "%-26s : OK (%s)\n" "$label" "$(command -v "$cmd")"
    else
        if [ "$req" -eq 1 ]; then
            printf "%-26s : MISSING [REQUIRED]\n" "$label"
            missing=1
        else
            printf "%-26s : MISSING [OPTIONAL]\n" "$label"
        fi
    fi
}

check_cmd "Git" git 1
check_cmd "rustc" rustc 1
check_cmd "cargo" cargo 1
check_cmd "rustup" rustup 1
check_cmd "clang" clang 1
check_cmd "lld" lld 1
check_cmd "nasm" nasm 1
check_cmd "qemu-system-x86_64" qemu-system-x86_64 1
check_cmd "qemu-system-aarch64" qemu-system-aarch64 1
check_cmd "gdb-multiarch" gdb-multiarch 0
check_cmd "lldb" lldb 0
check_cmd "just" just 0

echo ""
echo "=== Installed Rust Targets ==="
if command -v rustup >/dev/null 2>&1; then
    installed="$(rustup target list --installed)"
    for t in x86_64-unknown-uefi aarch64-unknown-uefi x86_64-unknown-none aarch64-unknown-none; do
        if echo "$installed" | grep -qx "$t"; then
            printf "%-26s : INSTALLED\n" "$t"
        else
            printf "%-26s : NOT INSTALLED (rustup target add %s)\n" "$t" "$t"
            missing=1
        fi
    done
fi

echo ""
if [ "$missing" -eq 0 ]; then
    echo "Toolchain check PASSED."
else
    echo "Toolchain check INCOMPLETE. See docs/DEVELOPMENT_SETUP.md."
    exit 1
fi
