param([switch]$Test, [switch]$Offline)

$ErrorActionPreference = 'Stop'
$tools = Join-Path (Split-Path $PSScriptRoot) '.tools'
$original = @{}
foreach ($name in 'RUSTUP_HOME', 'CARGO_HOME', 'PATH', 'RUSTFLAGS') {
    $original[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
}

Push-Location $PSScriptRoot
try {
    if (Test-Path -LiteralPath "$tools/cargo/bin/cargo.exe") {
        $env:RUSTUP_HOME = "$tools/rustup"
        $env:CARGO_HOME = "$tools/cargo"
        $env:PATH = "$tools/cargo/bin;$tools/llvm-mingw-20261104-ucrt-x86_64/bin;$env:PATH"
        $env:RUSTFLAGS = '-C linker=rust-lld -C linker-flavor=ld.lld'
    }
    $cargoArgs = if ($Test) { @('test', '--locked') } else { @('build', '--release', '--locked') }
    if ($Offline) { $cargoArgs += '--offline' }
    cargo @cargoArgs
    if ($LASTEXITCODE -ne 0) { throw 'ee build/test failed.' }
} finally {
    Pop-Location
    foreach ($name in $original.Keys) {
        if ($null -eq $original[$name]) {
            Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue
        } else {
            [Environment]::SetEnvironmentVariable($name, $original[$name], 'Process')
        }
    }
}
