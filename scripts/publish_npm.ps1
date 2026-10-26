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

.NOTES
Full runbook (CI trusted publishing, first-publish pitfalls, rollback):
docs/publishing.md
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
        if ($output -match 'two-factor|2fa|EOTP') {
            throw @'
npm refused the publish because the credential cannot satisfy the two-factor check.
Two cases, and only one of them needs a code:
  - the account HAS 2FA enabled: pass a code with -Otp <6-digit>, or use a token
    created with "Bypass 2FA"
  - the account has 2FA DISABLED but the token is a Granular Access Token without
    "Bypass 2FA": no code exists to type. Either recreate the token with
    "Bypass 2FA" checked (Automation tokens always bypass), or `npm logout` and
    `npm login` so the web session credential is used instead of the token.
'@
        }
        if ($output -match 'cannot publish over the previously published') {
            $packedVersion = (Get-Content package.json -Raw | ConvertFrom-Json).version
            throw @"
ukma-cli@$packedVersion is already on the registry, so npm refuses this publish
(it checks on dry runs too). Bump crates/ukma-cli/Cargo.toml, or pass
-Tag v<new-version> to align npm/package.json from the tag.
"@
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
