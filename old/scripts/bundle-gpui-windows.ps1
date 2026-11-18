$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { 'target' }
$metadata = cargo metadata --no-deps --format-version 1 | ConvertFrom-Json
$version = ($metadata.packages | Where-Object { $_.name -eq 'threadlane-gpui' }).version
$arch = switch ($env:PROCESSOR_ARCHITECTURE) {
    'AMD64' { 'x86_64' }
    'ARM64' { 'aarch64' }
    default { $env:PROCESSOR_ARCHITECTURE }
}

cargo build --locked --release --bin threadlane-gpui

$stage = Join-Path $targetDir 'release/threadlane-windows'
$archive = Join-Path $targetDir "release/Threadlane-$version-windows-$arch.zip"
Remove-Item -Recurse -Force $stage, $archive -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path $stage | Out-Null
Copy-Item (Join-Path $targetDir 'release/threadlane-gpui.exe') (Join-Path $stage 'Threadlane.exe')
Copy-Item 'resources/icon_512.png' (Join-Path $stage 'Threadlane.png')
Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $archive

Write-Host "Created $archive"
