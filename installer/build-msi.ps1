# Gera o instalador MSI do telinha.
# Uso (na raiz do projeto):  .\installer\build-msi.ps1
#
# Requisitos: os mesmos do build.ps1, mais
#   - WiX Toolset 3.14 (binários em %LOCALAPPDATA%\wix314, ou candle/light no PATH)
#   - cloudflared instalado (winget install Cloudflare.cloudflared): o .exe vai dentro do MSI

$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

# 1. Compila (fora do OneDrive/projeto, se CARGO_TARGET_DIR não estiver definido)
if (-not $env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR = "$env:LOCALAPPDATA\telinha-build" }
& "$root\build.ps1"
if ($LASTEXITCODE -ne 0) { throw "falha ao compilar" }
$bin = "$env:CARGO_TARGET_DIR\x86_64-pc-windows-gnullvm\release"

# 2. Junta os arquivos do instalador
$stage = "$env:CARGO_TARGET_DIR\msi-stage"
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item "$bin\telinha.exe", "$bin\telinha-cli.exe", "$root\README.md", "$PSScriptRoot\cloudflared-LICENSE.txt" $stage -Force

$cf = @(
    (Get-Command cloudflared -ErrorAction SilentlyContinue).Source
    "C:\Program Files (x86)\cloudflared\cloudflared.exe"
    "C:\Program Files\cloudflared\cloudflared.exe"
) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
if (-not $cf) { throw "cloudflared não encontrado. Instale com: winget install Cloudflare.cloudflared" }
Copy-Item $cf "$stage\cloudflared.exe" -Force

# 3. WiX
$wix = if (Test-Path "$env:LOCALAPPDATA\wix314\candle.exe") { "$env:LOCALAPPDATA\wix314" } else { Split-Path (Get-Command candle).Source }
$version = (Select-String -Path "$root\Cargo.toml" -Pattern '^version\s*=\s*"(.+)"').Matches[0].Groups[1].Value
$obj = "$env:CARGO_TARGET_DIR\msi-obj\"
$msi = "$root\telinha-$version-x64.msi"

& "$wix\candle.exe" -nologo -arch x64 "-dVersion=$version" "-dBinDir=$stage" -out $obj "$PSScriptRoot\telinha.wxs"
if ($LASTEXITCODE -ne 0) { throw "candle falhou" }
# ICE38/43/57: avisos sobre atalhos per-user num instalador per-machine (comportamento intencional).
& "$wix\light.exe" -nologo -spdb -sice:ICE38 -sice:ICE43 -sice:ICE57 -cultures:pt-BR -out $msi "$obj\telinha.wixobj"
if ($LASTEXITCODE -ne 0) { throw "light falhou" }

Write-Host ("`nPronto: {0} ({1:N1} MB)" -f $msi, ((Get-Item $msi).Length / 1MB)) -ForegroundColor Green
