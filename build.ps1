# Compila o telinha com Rust (target x86_64-pc-windows-gnullvm) + LLVM-MinGW.
# Uso:  .\build.ps1     (gera target\x86_64-pc-windows-gnullvm\release\telinha.exe)
#
# Requisitos (uma vez):
#   winget install Rustlang.Rustup
#   winget install MartinStorsjo.LLVM-MinGW.UCRT
#   rustup target add x86_64-pc-windows-gnullvm

$clang = Get-ChildItem "$env:LOCALAPPDATA\llvm-mingw", "$env:LOCALAPPDATA\Microsoft\WinGet\Packages" -Recurse -Filter x86_64-w64-mingw32-clang.exe -ErrorAction SilentlyContinue |
    Select-Object -First 1
if (-not $clang) { throw "LLVM-MinGW não encontrado. Instale com: winget install MartinStorsjo.LLVM-MinGW.UCRT" }

$env:Path = "$env:USERPROFILE\.cargo\bin;$($clang.DirectoryName);$env:Path"
cargo build --release
if ($LASTEXITCODE -eq 0) { Write-Host "`nPronto: target\x86_64-pc-windows-gnullvm\release\telinha.exe" -ForegroundColor Green }
exit $LASTEXITCODE
