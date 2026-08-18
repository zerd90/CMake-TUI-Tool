$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
Set-Location -Path $root

$name = "cmake-tui-tool"
$versionLine = (Select-String -Path "Cargo.toml" -Pattern '^version' | Select-Object -First 1).Line
$version = $versionLine -replace '.*"([^"]+)".*', '$1'

$archMap = @{ AMD64 = "x86_64"; ARM64 = "aarch64"; x86 = "x86" }
$arch = $archMap[$env:PROCESSOR_ARCHITECTURE]
if (-not $arch) { $arch = $env:PROCESSOR_ARCHITECTURE.ToLower() }

Write-Host "Building $name v$version (release) for windows/$arch..." -ForegroundColor Cyan
cargo build --release
if ($LASTEXITCODE -ne 0) {
    Write-Error "cargo build failed"
}

$exe = Join-Path $root "target\release\$name.exe"
if (-not (Test-Path -LiteralPath $exe)) {
    Write-Error "Build finished but executable not found: $exe"
}
Write-Host "Executable: $exe" -ForegroundColor Green

$dist = Join-Path $root "target\dist"
New-Item -ItemType Directory -Force -Path $dist | Out-Null
$archive = Join-Path $dist "$name-$version-windows-$arch.zip"
Compress-Archive -Path $exe -DestinationPath $archive -Force
Write-Host "Packaged: $archive" -ForegroundColor Green
