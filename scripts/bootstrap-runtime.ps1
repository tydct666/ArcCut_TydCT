param()
$ErrorActionPreference='Stop'
$root=(Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$lock=Get-Content -LiteralPath (Join-Path $root 'release/runtime-lock.json') -Raw | ConvertFrom-Json
$download=Join-Path $root 'tools/downloads'
New-Item -ItemType Directory -Force $download | Out-Null
function Get-Checked($spec,$name){
 $file=Join-Path $download $name
 if(!(Test-Path -LiteralPath $file) -or (Get-FileHash -LiteralPath $file).Hash.ToLowerInvariant() -ne $spec.sha256){
  & curl.exe --fail --location --proto '=https' --max-time 600 --output ($file+'.part') $spec.url
  if($LASTEXITCODE -ne 0){throw 'Runtime download failed'}
  if((Get-FileHash -LiteralPath ($file+'.part')).Hash.ToLowerInvariant() -ne $spec.sha256){throw 'Runtime hash mismatch; no executable will be used'}
  Move-Item -LiteralPath ($file+'.part') -Destination $file -Force
 }
 return $file
}
$ff=Get-Checked $lock.ffmpeg 'ffmpeg.zip'
$ffdir=Join-Path $root 'tools/ffmpeg'
if(!(Test-Path -LiteralPath (Join-Path $ffdir 'ffmpeg-9.0.2-essentials_build/bin/ffmpeg.exe'))){Expand-Archive -LiteralPath $ff -DestinationPath $ffdir -Force -ErrorAction Stop}
$media=Join-Path $root 'tools/media'
New-Item -ItemType Directory -Force $media | Out-Null
foreach($name in @('ffmpeg','ffprobe')){
 $file=Join-Path $ffdir ('ffmpeg-9.0.2-essentials_build/bin/'+$name+'.exe')
 if((Get-FileHash -LiteralPath $file).Hash.ToLowerInvariant() -ne $lock.ffmpeg.($name+'Sha256')){throw 'FFmpeg binary mismatch'}
 Copy-Item -LiteralPath $file -Destination $media -Force
}
Write-Output 'Pinned FFmpeg prepared for development tests; portable packages do not include it.'
