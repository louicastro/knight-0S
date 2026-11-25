<#
.SYNOPSIS
    Verifies the KnightOS Phase 1+ development toolchain on Windows.
.DESCRIPTION
    Checks for Rust, Cargo targets, LLVM/Clang, NASM, QEMU (x86_64 & aarch64),
    OVMF/AAVMF UEFI firmware, GDB, Git, and Python/Just build automation tools.
#>

$ErrorActionPreference = "Continue"

# Add standard Windows installation directories to process PATH if present
foreach ($extraDir in @(
    "C:\Program Files\qemu",
    "C:\Program Files\LLVM\bin",
    "C:\Program Files\NASM"
)) {
    if ((Test-Path $extraDir) -and ($env:PATH -notlike "*$extraDir*")) {
        $env:PATH = "$extraDir;$env:PATH"
    }
}

function Test-Tool {
    param(
        [string]$Name,
        [string]$Command,
        [string]$VersionArgs = "--version",
        [bool]$Required = $true
    )
    $cmd = Get-Command $Command -ErrorAction SilentlyContinue
    if ($null -eq $cmd) {
        $tag = if ($Required) { "MISSING [REQUIRED]" } else { "MISSING [OPTIONAL]" }
        Write-Host ("{0,-24} : {1}" -f $Name, $tag) -ForegroundColor $(if ($Required) { "Red" } else { "Yellow" })
        return (-not $Required)
    }
    try {
        $out = & $Command $VersionArgs.Split(" ") 2>&1 | Select-Object -First 1
        Write-Host ("{0,-24} : OK ({1})" -f $Name, $out) -ForegroundColor Green
    } catch {
        Write-Host ("{0,-24} : FOUND at {1}" -f $Name, $cmd.Source) -ForegroundColor Green
    }
    return $true
}

Write-Host "=== KnightOS Toolchain Check (Windows) ===" -ForegroundColor Cyan

$allOk = $true
$allOk = (Test-Tool "Git" "git" "--version" $true) -and $allOk
$allOk = (Test-Tool "Rust compiler (rustc)" "rustc" "--version" $true) -and $allOk
$allOk = (Test-Tool "Cargo" "cargo" "--version" $true) -and $allOk
$allOk = (Test-Tool "Rustup" "rustup" "--version" $true) -and $allOk
$allOk = (Test-Tool "Clang (LLVM)" "clang" "--version" $true) -and $allOk
$allOk = (Test-Tool "LLD linker (lld)" "lld" "--version" $true) -and $allOk
$allOk = (Test-Tool "NASM assembler" "nasm" "-v" $true) -and $allOk
$allOk = (Test-Tool "QEMU x86_64" "qemu-system-x86_64" "--version" $true) -and $allOk
$allOk = (Test-Tool "QEMU aarch64" "qemu-system-aarch64" "--version" $true) -and $allOk
$allOk = (Test-Tool "LLDB debugger" "lldb" "--version" $false) -and $allOk
$allOk = (Test-Tool "GDB (multiarch/x86)" "gdb" "--version" $false) -and $allOk
$allOk = (Test-Tool "Just task runner" "just" "--version" $false) -and $allOk
$allOk = (Test-Tool "Python 3" "python" "--version" $false) -and $allOk

Write-Host ""
Write-Host "=== Installed Rust Targets ===" -ForegroundColor Cyan
if (Get-Command rustup -ErrorAction SilentlyContinue) {
    $installed = rustup target list --installed
    $needed = @(
        "x86_64-unknown-uefi",
        "aarch64-unknown-uefi",
        "x86_64-unknown-none",
        "aarch64-unknown-none"
    )
    foreach ($t in $needed) {
        if ($installed -contains $t) {
            Write-Host ("{0,-24} : INSTALLED" -f $t) -ForegroundColor Green
        } else {
            Write-Host ("{0,-24} : NOT INSTALLED (run: rustup target add {0})" -f $t) -ForegroundColor Red
            $allOk = $false
        }
    }
}

Write-Host ""
Write-Host "=== UEFI Firmware Search (QEMU) ===" -ForegroundColor Cyan
$fwCandidates = @(
    "C:\Program Files\qemu\share\edk2-x86_64-code.fd",
    "C:\Program Files\qemu\share\edk2-aarch64-code.fd",
    "$env:ProgramFiles\qemu\share\edk2-x86_64-code.fd",
    "$env:ProgramFiles\qemu\share\edk2-aarch64-code.fd"
)
foreach ($fw in $fwCandidates | Select-Object -Unique) {
    if (Test-Path $fw) {
        Write-Host ("Found firmware          : {0}" -f $fw) -ForegroundColor Green
    }
}

Write-Host ""
if ($allOk) {
    Write-Host "Toolchain check PASSED." -ForegroundColor Green
    exit 0
} else {
    Write-Host "Toolchain check INCOMPLETE. See docs/DEVELOPMENT_SETUP.md for installation steps." -ForegroundColor Yellow
    exit 1
}
