# Builds target/release/foxing-<version>-x64.msi from the release exe. Run after cargo build --release.
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')

$version = (cargo metadata --no-deps --format-version 1 | ConvertFrom-Json).packages[0].version
$exe = (Resolve-Path 'target/release/foxing.exe').Path
$msi = "target/release/foxing-$version-x64.msi"

dotnet tool restore | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'dotnet tool restore failed' }
dotnet wix build installer/foxing.wxs -arch x64 -d "Version=$version" -d "ExePath=$exe" -o $msi
if ($LASTEXITCODE -ne 0) { throw 'wix build failed' }
(Resolve-Path $msi).Path
