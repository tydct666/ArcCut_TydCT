$localCargo = Join-Path $PSScriptRoot '../tools/cargo'
if (Test-Path (Join-Path $localCargo 'bin/cargo.exe')) {
  $env:CARGO_HOME = $localCargo
  $env:RUSTUP_HOME = Join-Path $PSScriptRoot '../tools/rustup'
  $env:PATH = "$env:CARGO_HOME\bin;" + $env:PATH
}
# CI and public checkouts use their normal PATH. Local toolchains are optional.
