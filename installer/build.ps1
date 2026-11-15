# Compila la app en modo release y genera el instalador en target\installer.
# Uso (desde cualquier carpeta):  .\installer\build.ps1
# Requiere Inno Setup 6:          winget install --id JRSoftware.InnoSetup -e --scope user
#
# -SkipCargo: no compila; empaqueta el target\release\nocho.exe que ya está ahí. Lo usa
# GitHub Actions para meter en el instalador el .exe ya firmado.

param([switch]$SkipCargo)

$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot)

# La versión sale de Cargo.toml, para que app e instalador nunca difieran.
$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value

if (-not $SkipCargo) {
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build falló" }
}

$iscc = @(
    "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
    "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) { throw "No se encontró Inno Setup 6 (ISCC.exe)" }

& $iscc /Q "/DAppVersion=$version" installer\nocho.iss
if ($LASTEXITCODE -ne 0) { throw "ISCC falló" }

Write-Host "Instalador: target\installer\nocho-setup-$version.exe"
