<#
.SYNOPSIS
Publish (or dry-run) the `ukma-cli` npm launcher.

.DESCRIPTION
`npm publish` reads `package.json` from the current directory, and the repository
root deliberately has none — running it there fails with
`ENOENT ... F:\UKMA\package.json`. This script always runs it inside `npm/`, shows
the tarball first, and publishes only when `-Publish` is passed.

.EXAMPLE
.\scripts\publish_npm.ps1

.EXAMPLE
.\scripts\publish_npm.ps1 -Tag v0.1.0 -Publish
#>
[CmdletBinding()]
param(
    [switch]$Publish,
    [string]$Tag = '',
    [string]$Otp = '',
    [switch]$Provenance
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$packageDir = Join-Path $root 'npm'
$cargoToml = Join-Path $root 'crates\ukma-cli\Cargo.toml'

function Write-Step([string]$message) {
    Write-Host "==> $message"
}

# Native tools write progress to stderr; with `$ErrorActionPreference = 'Stop'`
# PowerShell 5.1 turns that into a terminating error, so native calls run with
# the preference relaxed and their exit code checked explicitly.
function Invoke-Npm {
    param([string[]]$NpmArguments)
    foreach ($line in (Invoke-NpmLines $NpmArguments)) {
        Write-Host $line
    }
}

# Native stderr arrives as ErrorRecords; render their message instead of the
# `node.exe : ...` prefix PowerShell adds when formatting them.
function Invoke-NpmLines {
    param([string[]]$NpmArguments)
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $lines = & npm @NpmArguments 2>&1 | ForEach-Object {
            if ($_ -is [System.Management.Automation.ErrorRecord]) {
                $_.Exception.Message
            }
            else {
                "$_"
            }
        }
        return @($lines)
    }
    finally {
        $ErrorActionPreference = $previous
    }
}

function Invoke-NpmCapturing {
    param([string[]]$NpmArguments)
    return ((Invoke-NpmLines $NpmArguments) -join "`n").Trim()
}

function Get-NpmText {
    param([string[]]$NpmArguments)
    return Invoke-NpmCapturing $NpmArguments
}

if (-not (Get-Command npm -ErrorAction SilentlyContinue)) {
    throw 'npm is not on PATH; install Node.js 18 or newer.'
}
if (-not (Test-Path (Join-Path $packageDir 'package.json'))) {
    throw "npm/package.json is missing; is $root the repository root?"
}

Push-Location $packageDir
try {
    Write-Step "package dir $packageDir"

    if ($Tag -ne '') {
        if ($Tag -notmatch '^v\d+\.\d+\.\d+$') {
            throw "tag `"$Tag`" is not of the form vX.Y.Z"
        }
        $crateVersion = (Select-String -Path $cargoToml -Pattern '^version\s*=\s*"([^"]+)"' |
            Select-Object -First 1).Matches.Groups[1].Value
        if ($Tag.Substring(1) -ne $crateVersion) {
            throw "tag $Tag does not match ukma-cli $crateVersion (crates/ukma-cli/Cargo.toml)"
        }
        $packed = (Get-Content package.json -Raw | ConvertFrom-Json).version
        if ($packed -ne $crateVersion) {
            Write-Step "aligning npm/package.json $packed -> $crateVersion"
            if ($Publish) {
                Invoke-Npm @('version', $crateVersion, '--no-git-tag-version', '--allow-same-version')
            }
        }
    }

    Write-Step 'tarball contents'
    Invoke-Npm @('pack', '--dry-run')

    if ($Publish) {
        $whoami = Get-NpmText @('whoami')
        if ($LASTEXITCODE -ne 0 -or $whoami -eq '') {
            throw "not logged in to npm; run `npm login` first ($whoami)"
        }
        Write-Step "publishing as $whoami"
    }
    else {
        Write-Step 'dry run (pass -Publish to upload)'
    }

    $arguments = @('publish', '--access', 'public')
    if ($Provenance) { $arguments += '--provenance' }
    if ($Otp -ne '') { $arguments += @('--otp', $Otp) }
    if (-not $Publish) { $arguments += '--dry-run' }
    $output = Invoke-NpmCapturing $arguments
    Write-Host $output
    if ($LASTEXITCODE -ne 0) {
        if ($output -match 'two-factor|2fa|EOTP' -or $output -match 'E403.*2fa') {
            throw @'
npm rejected the publish because the account requires two-factor authentication.
  - interactive:  .\scripts\publish_npm.ps1 -Publish -Otp <6-digit-code>
  - CI / no prompt: create an npm token with "bypass 2FA" and use it as NPM_TOKEN
    (Automation tokens bypass 2FA; Granular tokens need "Bypass 2FA" enabled)
'@
        }
        throw "npm publish failed with exit code $LASTEXITCODE"
    }
    if ($Publish) {
        Write-Step 'published; verify with `npm view ukma-cli version` and `npx ukma-cli@<version> --version`'
    }
}
finally {
    Pop-Location
}
