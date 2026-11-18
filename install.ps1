param(
    [string]$Destination = "$env:USERPROFILE/.local/bin",
    [switch]$Force
)

$ErrorActionPreference = 'Stop'
$source = Join-Path $PSScriptRoot 'target/release/ee.exe'
if (-not (Test-Path -LiteralPath $source -PathType Leaf)) { throw 'Run ./build.ps1 first.' }
$target = Join-Path $Destination 'ee.exe'
if ((Test-Path -LiteralPath $target) -and -not $Force) {
    throw 'ee.exe already exists. Close ee and run ./install.ps1 -Force to upgrade.'
}
New-Item -ItemType Directory -Path $Destination -Force | Out-Null
Copy-Item -LiteralPath $source -Destination $target -Force:$Force
Write-Host "Installed: $target"
Write-Host 'Run ee if this directory is already on PATH. Setup does not change PATH.'
