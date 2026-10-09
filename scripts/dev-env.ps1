# Dot-source this script from PowerShell: . ./scripts/dev-env.ps1
# All changes are limited to the current process; no permanent PATH changes.
$taskRepoRoot = Split-Path -Parent $PSScriptRoot
$taskToolsRoot = Join-Path $taskRepoRoot '.tools'
$taskLlvmBin = Join-Path $taskToolsRoot 'llvm-mingw-20261006-ucrt-x86_64\bin'
$taskCmakeBin = Join-Path $taskToolsRoot 'cmake-4.4.4-windows-x86_64\bin'
$taskNinjaBin = Join-Path $taskToolsRoot 'ninja-1.13.2'
$taskNasmBin = Join-Path $taskToolsRoot 'nasm-3.01'

$env:CARGO_HOME = Join-Path $taskToolsRoot 'cargo'
$env:RUSTUP_HOME = Join-Path $taskToolsRoot 'rustup'
$env:RUSTUP_TOOLCHAIN = '1.99.0-x86_64-pc-windows-gnullvm'
$env:CARGO_BUILD_TARGET = 'x86_64-pc-windows-gnullvm'
$env:CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_LINKER = Join-Path $taskLlvmBin 'x86_64-w64-mingw32-clang.exe'
$env:CC = Join-Path $taskLlvmBin 'x86_64-w64-mingw32-clang.exe'
$env:CXX = Join-Path $taskLlvmBin 'x86_64-w64-mingw32-clang++.exe'
$env:AR = Join-Path $taskLlvmBin 'llvm-ar.exe'
$env:CC_x86_64_pc_windows_gnullvm = $env:CC
$env:CXX_x86_64_pc_windows_gnullvm = $env:CXX
$env:AR_x86_64_pc_windows_gnullvm = $env:AR
$env:LIBCLANG_PATH = $taskLlvmBin
$env:CMAKE_GENERATOR = 'Ninja'
$env:CMAKE_MAKE_PROGRAM = Join-Path $taskNinjaBin 'ninja.exe'
$taskPathEntries = @(
    (Join-Path $env:CARGO_HOME 'bin'), $taskLlvmBin, $taskCmakeBin,
    $taskNinjaBin, $taskNasmBin
)
$env:PATH = (($taskPathEntries + ($env:PATH -split ';')) | Select-Object -Unique) -join ';'

if (-not (Test-Path -LiteralPath (Join-Path $env:CARGO_HOME 'bin\cargo.exe'))) {
    Write-Warning 'This optional script needs the project-local GNU LLVM toolchain. See docs/BUILD.md for standard build setup.'
}
