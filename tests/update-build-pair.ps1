param()
$ErrorActionPreference='Stop'
$root=(Resolve-Path (Join-Path $PSScriptRoot '..')).Path
Set-Location -LiteralPath $root
. ./scripts/env.ps1
. ./scripts/release-build-env.ps1
$env:CARGO_INCREMENTAL='0'
node node_modules/vite/bin/vite.js build
if($LASTEXITCODE -ne 0){throw 'Production frontend failed'}
cargo build --offline --locked --manifest-path src-tauri/Cargo.toml --release --bin arkcut --bin arkcut-updater --features desktop,custom-protocol
if($LASTEXITCODE -ne 0){throw 'Production backup build failed'}
New-Item -ItemType Directory -Force tests/results/revision17-light-final/binaries/production | Out-Null
Copy-Item src-tauri/target/release/arkcut.exe,src-tauri/target/release/arkcut-updater.exe tests/results/revision17-light-final/binaries/production -Force
$files=@('package.json','src-tauri/Cargo.toml','src-tauri/Cargo.lock','src-tauri/tauri.conf.json')
$original=@{}
foreach($file in $files){$original[$file]=[IO.File]::ReadAllBytes((Join-Path $root $file))}
try {
 foreach($v in @('0.17.0','0.17.1')){
  $package=Get-Content package.json -Raw | ConvertFrom-Json
  $package.version=$v
  [IO.File]::WriteAllText((Join-Path $root 'package.json'),($package | ConvertTo-Json -Depth 9),[Text.UTF8Encoding]::new($false))
  node scripts/version.cjs --write
  node node_modules/vite/bin/vite.js build
  if($LASTEXITCODE -ne 0){throw 'Pair frontend failed'}
  cargo build --offline --manifest-path src-tauri/Cargo.toml --release --bin arkcut --bin arkcut-updater --features desktop,custom-protocol,update-test-hooks,component-test-hooks
  if($LASTEXITCODE -ne 0){throw 'Pair binary failed'}
  $out=Join-Path $root ('tests/results/revision17-light-final/binaries/'+$v)
  New-Item -ItemType Directory -Force $out | Out-Null
  Copy-Item src-tauri/target/release/arkcut.exe,src-tauri/target/release/arkcut-updater.exe $out -Force
 }
} finally {
 foreach($file in $files){[IO.File]::WriteAllBytes((Join-Path $root $file),$original[$file])}
 node node_modules/vite/bin/vite.js build
 Copy-Item tests/results/revision17-light-final/binaries/production/arkcut.exe,tests/results/revision17-light-final/binaries/production/arkcut-updater.exe src-tauri/target/release -Force
}
