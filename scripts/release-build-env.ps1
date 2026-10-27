# Release-only path normalization, including dependency crates and MSVC PDB reference.
$releaseRoot=(Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$releasePrefixes=@($releaseRoot,[Environment]::GetFolderPath('UserProfile'))
if($env:CARGO_HOME){$releasePrefixes+= [IO.Path]::GetFullPath($env:CARGO_HOME)}
if($env:RUSTUP_HOME){$releasePrefixes+= [IO.Path]::GetFullPath($env:RUSTUP_HOME)}
$releaseFlags=@()
if($env:CARGO_ENCODED_RUSTFLAGS){$releaseFlags+= $env:CARGO_ENCODED_RUSTFLAGS.Split([char]31)}
elseif($env:RUSTFLAGS){throw 'Use CARGO_ENCODED_RUSTFLAGS to preserve custom release flags without ambiguous quoting'}
$releaseIndex=0
foreach($releasePrefix in ($releasePrefixes | Where-Object {$_} | Select-Object -Unique)){
 $releaseIndex++
 foreach($form in @($releasePrefix,$releasePrefix.Replace('\','/')) | Select-Object -Unique){$releaseFlags+="--remap-path-prefix=$form=/arkcut-build/$releaseIndex"}
}
$releaseFlags+='-Clink-arg=/PDBALTPATH:%_PDB%'
$env:CARGO_ENCODED_RUSTFLAGS=$releaseFlags -join [char]31
