param([switch]$SkipBuild,[switch]$StageOnly,[switch]$FinalRelease,[switch]$Portable)
# Default iterations deliver source only. Portable prepares components on first use.
if (-not $FinalRelease -and -not $Portable) { & (Join-Path $PSScriptRoot 'package-source.ps1'); return }
$ErrorActionPreference='Stop'
$projectRoot=(Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$version=(Get-Content -Raw -LiteralPath (Join-Path $projectRoot 'package.json') | ConvertFrom-Json).version
Set-Location -LiteralPath $projectRoot
. (Join-Path $PSScriptRoot 'env.ps1')
. (Join-Path $PSScriptRoot 'release-build-env.ps1')
if($FinalRelease){& node scripts/verify-distribution.cjs distribution-support --require-complete}else{& node scripts/verify-distribution.cjs distribution-support}
if($LASTEXITCODE -ne 0){throw 'Distribution material verification failed'}
if (-not $SkipBuild) {
  & node scripts/version.cjs
  if ($LASTEXITCODE -ne 0) { throw 'Version mismatch' }
  & node node_modules/typescript/bin/tsc
  if ($LASTEXITCODE -ne 0) { throw 'Frontend type check failed' }
  & node node_modules/vite/bin/vite.js build
  if ($LASTEXITCODE -ne 0) { throw 'Frontend build failed' }
  & cargo build --locked --manifest-path src-tauri/Cargo.toml --release --bin arkcut --bin arkcut-updater --features desktop,custom-protocol
  if ($LASTEXITCODE -ne 0) { throw 'Rust desktop build failed' }
}
& node scripts/generate-distribution-docs.cjs --check
if($LASTEXITCODE -ne 0){throw 'Generated distribution documents do not match the locked dependencies'}
$release=Join-Path $projectRoot 'src-tauri/target/release/arkcut.exe'
if (-not (Test-Path -LiteralPath $release)) { throw 'Release binary missing' }
$stage=Join-Path $projectRoot ('dist/ArkCut-'+$version+'-win64-light')
if (Test-Path -LiteralPath $stage) { throw 'Use a fresh staging directory; move the previous candidate out before packaging' }
if (Test-Path -LiteralPath (Join-Path $stage '项目存档')) { throw 'Package staging contains user archives; preserve them and use a fresh release directory' }
New-Item -ItemType Directory -Force $stage,(Join-Path $stage 'licenses'),(Join-Path $stage 'sources'),(Join-Path $stage 'docs') | Out-Null
Copy-Item -LiteralPath $release -Destination (Join-Path $stage 'ArkCut.exe') -Force
Copy-Item -LiteralPath 'src-tauri/target/release/arkcut-updater.exe' -Destination (Join-Path $stage 'ArkCutUpdater.exe') -Force
Copy-Item -LiteralPath 'release/portable-README.md' -Destination (Join-Path $stage 'README.md') -Force
Copy-Item -LiteralPath 'LICENSE' -Destination $stage -Force
foreach($file in @('docs/用户指南.md','docs/更新与恢复.md')) {Copy-Item -LiteralPath $file -Destination (Join-Path $stage 'docs') -Force}
# Copy only the reviewed lightweight inventory, never old runtime archives.
$distribution=Get-Content -LiteralPath 'release/distribution-files.json' -Raw | ConvertFrom-Json
foreach($item in $distribution.files){
  $destination=Join-Path $stage $item.path
  New-Item -ItemType Directory -Force (Split-Path -Parent $destination) | Out-Null
  Copy-Item -LiteralPath (Join-Path 'distribution-support' $item.path) -Destination $destination
}
# Retain the resolved Windows dependency licenses, including build dependencies.
& node scripts/collect-rust-licenses.cjs (Join-Path $stage 'licenses/rust')
if($LASTEXITCODE -ne 0){throw 'Resolved Rust license collection failed'}
foreach ($package in Get-ChildItem -Directory -LiteralPath 'node_modules/.pnpm') {
  $nested=Join-Path $package.FullName 'node_modules'
  if (-not (Test-Path -LiteralPath $nested)) { continue }
  foreach ($licenseFile in Get-ChildItem -LiteralPath $nested -Recurse -File -Depth 2 -ErrorAction SilentlyContinue | Where-Object {$_.Name -match '^(LICENSE|LICENCE|COPYING|NOTICE)(\.|$)'}) {
    $dest=Join-Path $stage ('licenses/npm/'+$package.Name.Replace(':','_'))
    New-Item -ItemType Directory -Force $dest | Out-Null
    Copy-Item -LiteralPath $licenseFile.FullName -Destination (Join-Path $dest ($licenseFile.Directory.Name+'-'+$licenseFile.Name)) -Force
  }
}
# Use the same source-only builder for embedded and standalone source delivery.
& (Join-Path $PSScriptRoot 'package-source.ps1')
if (-not $?) { throw 'Source packaging failed' }
$sourceZip=Join-Path $projectRoot ('dist/ArkCut-'+$version+'-source.zip')
Copy-Item -LiteralPath $sourceZip -Destination (Join-Path $stage 'sources') -Force
$snapshot=& node -e "process.stdout.write(require('./scripts/source-list.cjs').snapshot())"
if($LASTEXITCODE -ne 0){throw 'Source snapshot failed'}
$distribution=Get-Content -LiteralPath 'release/distribution-files.json' -Raw | ConvertFrom-Json
$runtime=Get-Content -LiteralPath 'release/runtime-lock.json' -Raw | ConvertFrom-Json
$sourceCommit=if($env:ARKCUT_SOURCE_COMMIT){$env:ARKCUT_SOURCE_COMMIT}else{$env:GITHUB_SHA}
$manifest=@{product='ArkCut';version=$version;releaseStatus='local-candidate';distributionMode='light-online-prepare';sourceSnapshot=$snapshot;sourceCommit=$sourceCommit;buildRun=$env:GITHUB_RUN_ID;platform='windows-x86_64';webView2=$runtime.webview2.version;ffmpeg=$runtime.ffmpeg.version;builtDate=(Get-Date -Format 'yyyy-MM-dd');cleanWindows10And11Acceptance='not-verified';distributionSourceAudit=$distribution.status;distributionMaterialCount=$distribution.files.Count;files=@()}
foreach ($file in Get-ChildItem -Recurse -File -LiteralPath $stage) { if ($file.FullName -ne (Join-Path $stage 'manifest.json')) { $manifest.files+=[pscustomobject]@{path=$file.FullName.Substring($stage.Length+1);bytes=$file.Length;sha256=(Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant()} } }
$manifest.unpackedBytes=($manifest.files | Measure-Object -Property bytes -Sum).Sum
$manifest | ConvertTo-Json -Depth 8 | Set-Content -Encoding utf8 (Join-Path $stage 'manifest.json')
node (Join-Path $PSScriptRoot 'audit-public.cjs') $stage
if($LASTEXITCODE -ne 0){throw 'Public package privacy audit failed'}
if ($StageOnly) { Write-Output $stage; return }
$zip=Join-Path $projectRoot ('dist/ArkCut-'+$version+'-win64-light.zip')
# A few upstream license files have pre-ZIP timestamps. Normalize metadata on
# copied staging files only; the originals and all file contents stay intact.
foreach ($file in Get-ChildItem -Recurse -File -LiteralPath $stage) {
  if ($file.LastWriteTime.Year -lt 1980) { $file.LastWriteTime=[datetime]'1980-01-02T00:00:00' }
}
Compress-Archive -LiteralPath $stage -DestinationPath $zip -Force
Get-FileHash -LiteralPath $zip,$sourceZip -Algorithm SHA256 | ForEach-Object { $_.Hash.ToLowerInvariant()+'  '+(Split-Path -Leaf $_.Path) } | Set-Content -Encoding ascii dist/SHA256SUMS.txt
$auditRoot=Join-Path $projectRoot ('outputs/release-audit/'+$version)
New-Item -ItemType Directory -Force $auditRoot | Out-Null
@{zip=(Split-Path -Leaf $zip);zipBytes=(Get-Item -LiteralPath $zip).Length;unpackedBytes=$manifest.unpackedBytes;files=$manifest.files.Count;sourceZip=(Split-Path -Leaf $sourceZip);sha256=(Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLowerInvariant()} | ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $auditRoot 'package-result.json')
Get-Content (Join-Path $auditRoot 'package-result.json')


