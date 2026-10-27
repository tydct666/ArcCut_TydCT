param()
$ErrorActionPreference='Stop'
$taskRoot=(Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$version=(Get-Content -Raw -LiteralPath (Join-Path $taskRoot 'package.json') | ConvertFrom-Json).version
$stage=Join-Path $taskRoot ('dist/.source-stage-'+$version+'-'+[guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force (Join-Path $taskRoot 'dist') | Out-Null
$auditRoot=Join-Path $taskRoot ('outputs/release-audit/'+$version)
New-Item -ItemType Directory -Force $auditRoot | Out-Null
node (Join-Path $PSScriptRoot 'source-list.cjs') $stage | Set-Content -LiteralPath (Join-Path $auditRoot 'source-audit.json') -Encoding UTF8
if($LASTEXITCODE -ne 0){throw 'Source whitelist audit failed'}
node (Join-Path $PSScriptRoot 'audit-public.cjs') $stage
if($LASTEXITCODE -ne 0){throw 'Public source privacy audit failed'}
$zip=Join-Path $taskRoot ('dist/ArkCut-'+$version+'-source.zip')
Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zip -Force
@{version=$version;kind='source-only';zip=(Split-Path -Leaf $zip);bytes=(Get-Item -LiteralPath $zip).Length;sha256=(Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLowerInvariant()} | ConvertTo-Json | Set-Content -Encoding UTF8 -LiteralPath (Join-Path $auditRoot 'source-package-result.json')
$resolvedStage=(Resolve-Path -LiteralPath $stage).Path
$resolvedDist=(Resolve-Path -LiteralPath (Join-Path $taskRoot 'dist')).Path
if ((Split-Path -Parent $resolvedStage) -ne $resolvedDist -or (Split-Path -Leaf $resolvedStage) -notlike ('.source-stage-'+$version+'-*')) { throw 'Refusing unexpected staging cleanup path' }
if(Get-ChildItem -LiteralPath $resolvedStage -Recurse -Force | Where-Object {$_.Attributes -band [IO.FileAttributes]::ReparsePoint}){throw 'Source staging contains reparse points'}
Remove-Item -LiteralPath $resolvedStage -Recurse -Force
Get-Content -LiteralPath (Join-Path $auditRoot 'source-package-result.json')
