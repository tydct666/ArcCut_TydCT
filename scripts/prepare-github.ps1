param()
$ErrorActionPreference='Stop'
$root=(Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$version=(Get-Content -Raw -LiteralPath (Join-Path $root 'package.json') | ConvertFrom-Json).version
$dist=Join-Path $root 'dist'
$repo=Join-Path $dist 'ArcCut_TydCT'
# A source export is replaceable; a repository's history is not. Check before
# creating staging or audit output, including worktrees whose .git is a file.
if(Test-Path -LiteralPath (Join-Path $repo '.git')){
 throw 'Upload destination contains .git; preserved without changes. Export to a fresh directory with scripts/source-list.cjs, then review and sync into the existing repository.'
}
$staging=Join-Path $dist ('.upload-stage-'+[guid]::NewGuid().ToString('N'))
$previous=Join-Path $dist ('.upload-previous-'+[guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force $dist | Out-Null
$auditRoot=Join-Path $root ('outputs/release-audit/'+$version)
New-Item -ItemType Directory -Force $auditRoot | Out-Null
node (Join-Path $PSScriptRoot 'source-list.cjs') $staging | Set-Content -LiteralPath (Join-Path $auditRoot 'upload-source-audit.json') -Encoding UTF8
if($LASTEXITCODE -ne 0){throw 'Source whitelist audit failed'}
node (Join-Path $PSScriptRoot 'audit-public.cjs') $staging
if($LASTEXITCODE -ne 0){throw 'Upload privacy audit failed'}
foreach($p in @($repo,$staging,$previous)){
 if([IO.Path]::GetFullPath((Split-Path -Parent $p)) -ne [IO.Path]::GetFullPath($dist)){throw 'Invalid upload destination'}
 if(Test-Path -LiteralPath $p){if(Get-Item -LiteralPath $p -Force | Where-Object {$_.Attributes -band [IO.FileAttributes]::ReparsePoint}){throw 'Linked destination'};if(Get-ChildItem -LiteralPath $p -Recurse -Force | Where-Object {$_.Attributes -band [IO.FileAttributes]::ReparsePoint}){throw 'Linked upload content'}}
}
if(Test-Path -LiteralPath $repo){Move-Item -LiteralPath $repo -Destination $previous}
try{Move-Item -LiteralPath $staging -Destination $repo}catch{if(Test-Path -LiteralPath $previous){Move-Item -LiteralPath $previous -Destination $repo};throw}
if(Test-Path -LiteralPath $previous){$resolved=(Resolve-Path -LiteralPath $previous).Path;if((Split-Path -Parent $resolved) -ne (Resolve-Path $dist).Path){throw 'Unsafe previous snapshot'};Remove-Item -LiteralPath $resolved -Recurse -Force}
Write-Output $repo
# No remote mutation, push or release is performed by this script.
