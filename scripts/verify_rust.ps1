$ErrorActionPreference = 'Stop'
Set-Location (Resolve-Path "$PSScriptRoot/..")
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { throw 'cargo not found. Install Rust from https://rustup.rs/ then reopen VS Code.' }
$commands = @(
  @('fmt','--all','--','--check'),
  @('check','--workspace','--all-targets'),
  @('clippy','--workspace','--all-targets','--','-D','warnings'),
  @('test','--workspace','--all-targets')
)
foreach ($argsForCargo in $commands) {
  & cargo @argsForCargo
  if ($LASTEXITCODE -ne 0) { throw "Cargo $($argsForCargo[0]) failed ($LASTEXITCODE)" }
}
