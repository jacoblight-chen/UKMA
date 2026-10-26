<#
.SYNOPSIS
    Install (or remove) the UKMA CLI. No Node.js, no compiler, no toolchain.

.DESCRIPTION
    Downloads the prebuilt `ukma` release for this platform, verifies its
    SHA-256 when the release publishes one, puts it in the install directory,
    and adds that directory to the user PATH.

    `-FromFile` installs a binary you built yourself and never touches the
    network, so it also works on an air-gapped machine:

        cargo build -p ukma-cli --release
        .\scripts\install.ps1 -FromFile .\target\release\ukma.exe

    After installing, run `ukma install` in a new terminal to wire your coding
    agents, then `ukma init` inside each project.

.PARAMETER Version
    The release to install (`latest` by default, or e.g. `1.2.3`).

.PARAMETER Dir
    The install directory (default: %LOCALAPPDATA%\UKMA\bin).

.PARAMETER FromFile
    Install this local `ukma.exe` instead of downloading a release.

.PARAMETER BaseUrl
    The release base URL (default: the project's GitHub releases page).

.PARAMETER NoPath
    Do not modify the user PATH.

.PARAMETER Uninstall
    Unwire the agents (`ukma uninstall --yes`) and delete the binary and the
    PATH entry. Project directories (`.ukma/`) are left alone.

.PARAMETER Force
    Overwrite an existing `ukma.exe` without asking.

.EXAMPLE
    irm https://raw.githubusercontent.com/jacoblight-chen/UKMA/main/scripts/install.ps1 | iex

.EXAMPLE
    .\scripts\install.ps1 -FromFile .\target\release\ukma.exe -NoPath
#>
[CmdletBinding(SupportsShouldProcess = $true)]
param(
    [string]$Version = 'latest',
    [string]$Dir = '',
    [string]$FromFile = '',
    [string]$BaseUrl = 'https://github.com/jacoblight-chen/UKMA/releases',
    [switch]$NoPath,
    [switch]$Uninstall,
    [switch]$Force
)

$ErrorActionPreference = 'Stop'

function Write-Result([string]$message) {
    # `-WhatIf` already reports every skipped write; do not claim the result.
    if ($WhatIfPreference) { return }
    Write-Host $message
}

function Get-InstallDir {
    if ($Dir -ne '') { return $Dir }
    $local = $env:LOCALAPPDATA
    if ([string]::IsNullOrEmpty($local)) {
        $local = Join-Path $HOME 'AppData\Local'
    }
    return Join-Path $local 'UKMA\bin'
}

function Get-TargetTriple {
    $arch = $env:PROCESSOR_ARCHITECTURE
    if ($arch -eq 'ARM64') { return 'aarch64-pc-windows-msvc' }
    if ($arch -eq 'x86') {
        throw 'UKMA ships 64-bit builds only; 32-bit Windows is not supported.'
    }
    return 'x86_64-pc-windows-msvc'
}

function Confirm-Step([string]$target, [string]$action) {
    # `irm ... | iex` (and `powershell -Command -`) run the script body without a
    # bound `$PSCmdlet`, so `ShouldProcess` does not exist there. Fall back to the
    # what-if preference itself instead of crashing on a null method call.
    if ($null -ne $PSCmdlet) {
        return $PSCmdlet.ShouldProcess($target, $action)
    }
    return (-not $WhatIfPreference)
}

function Get-UkmaVersion([string]$exe) {
    # A stale or corrupt file at the target path must not turn the installer into
    # an exception: treat "cannot run it" as "not installed".
    try {
        $version = (& $exe --version) 2>$null
        if ($LASTEXITCODE -eq 0) { return $version }
    } catch {
        return $null
    }
    return $null
}

function Add-UserPath([string]$entry) {
    $current = [Environment]::GetEnvironmentVariable('Path', 'User')
    if ([string]::IsNullOrEmpty($current)) { $current = '' }
    $entries = $current.Split(';', [System.StringSplitOptions]::RemoveEmptyEntries)
    if ($entries -contains $entry) { return $false }
    $updated = (@($entries) + $entry) -join ';'
    if (Confirm-Step $entry 'add to the user PATH') {
        [Environment]::SetEnvironmentVariable('Path', $updated, 'User')
    }
    return $true
}

function Remove-UserPath([string]$entry) {
    $current = [Environment]::GetEnvironmentVariable('Path', 'User')
    if ([string]::IsNullOrEmpty($current)) { return $false }
    $entries = $current.Split(';', [System.StringSplitOptions]::RemoveEmptyEntries)
    if (-not ($entries -contains $entry)) { return $false }
    $kept = @($entries | Where-Object { $_ -ne $entry })
    if (Confirm-Step $entry 'remove from the user PATH') {
        [Environment]::SetEnvironmentVariable('Path', ($kept -join ';'), 'User')
    }
    return $true
}

function Resolve-AssetUrl([string]$asset) {
    if ($Version -eq 'latest') {
        return "$BaseUrl/latest/download/$asset"
    }
    $pinned = $Version.TrimStart('v')
    return "$BaseUrl/download/v$pinned/$asset"
}

$installDir = Get-InstallDir
$exe = Join-Path $installDir 'ukma.exe'

if ($Uninstall) {
    Write-Host "Removing the UKMA CLI from $installDir"
    $onPath = Get-Command ukma -ErrorAction SilentlyContinue
    if ($onPath) {
        Write-Host '  unwiring the agents (ukma uninstall --yes)'
        & $onPath.Source uninstall --yes
    }
    if (Test-Path $exe) {
        Remove-Item $exe -Force -ErrorAction SilentlyContinue
        Write-Host "  deleted $exe"
    }
    # `ukma uninstall` vacates a running image by renaming it; sweep that file too.
    $stale = "$exe.old"
    if (Test-Path $stale) {
        Remove-Item $stale -Force -ErrorAction SilentlyContinue
        Write-Host "  removed $stale"
    }
    if (Remove-UserPath $installDir) {
        Write-Host "  removed $installDir from the user PATH (open a new terminal)"
    }
    Write-Host 'Project directories (.ukma) were left alone; remove them with `ukma uninit` per project.'
    exit 0
}

$createdInstallDir = $false
if (-not (Test-Path $installDir)) {
    New-Item -ItemType Directory -Force -Path $installDir | Out-Null
    $createdInstallDir = $true
}

if (Test-Path $exe) {
    if (-not $Force) {
        $existing = Get-UkmaVersion $exe
        if ($existing) {
            Write-Host "ukma is already installed: $existing ($exe)"
            Write-Host 'Re-run with -Force to replace it, or run `ukma upgrade`.'
            exit 0
        }
    }
}

if ($FromFile -ne '') {
    if (-not (Test-Path $FromFile)) {
        throw "not found: $FromFile"
    }
    Copy-Item -Path $FromFile -Destination $exe -Force
    Write-Result "Installed ukma from $FromFile"
}
else {
    $triple = Get-TargetTriple
    $asset = "ukma-$triple.zip"
    $url = Resolve-AssetUrl $asset
    $temp = Join-Path ([System.IO.Path]::GetTempPath()) ([System.IO.Path]::GetRandomFileName())
    New-Item -ItemType Directory -Force -Path $temp | Out-Null
    $zip = Join-Path $temp $asset
    try {
        Write-Host "Downloading $url"
        try {
            Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $zip
        }
        catch {
            throw "download failed: $($_.Exception.Message)`nInstall a local build instead: .\scripts\install.ps1 -FromFile .\target\release\ukma.exe"
        }
        $expected = ''
        try {
            $sha = Invoke-WebRequest -UseBasicParsing -Uri "$url.sha256"
            # `Content` arrives as a byte array when the server sends no charset,
            # and `-split` on bytes yields byte values ("49"), not the digest.
            $raw = $sha.Content
            if ($raw -is [byte[]]) {
                $raw = [System.Text.Encoding]::UTF8.GetString($raw)
            }
            $expected = ([string]$raw -split '\s+')[0].Trim().ToLowerInvariant()
        }
        catch {
            Write-Host '  (no SHA-256 published for this release; skipping verification)'
        }
        if ($expected -ne '') {
            if ($expected -notmatch '^[0-9a-f]{64}$') {
                throw "the published SHA-256 for $asset is not a 64-character digest: $expected"
            }
            $actual = (Get-FileHash -Path $zip -Algorithm SHA256).Hash.ToLowerInvariant()
            if ($expected -ne $actual) {
                throw "SHA-256 mismatch: expected $expected, got $actual"
            }
            Write-Host '  SHA-256 verified'
        }
        Expand-Archive -Path $zip -DestinationPath $temp -Force
        $found = Get-ChildItem -Path $temp -Filter 'ukma.exe' -Recurse |
            Where-Object { $_.FullName -ne $zip } | Select-Object -First 1
        if (-not $found) {
            throw "the archive $asset does not contain ukma.exe"
        }
        Copy-Item -Path $found.FullName -Destination $exe -Force
        Write-Result "Installed $($found.Name) to $exe"
    }
    finally {
        Remove-Item -Path $temp -Recurse -Force -ErrorAction SilentlyContinue
        # A failed install must not leave an empty directory (or a PATH entry)
        # behind for the next attempt to trip over.
        if ($createdInstallDir -and (Test-Path $installDir)) {
            if (-not (Get-ChildItem -Path $installDir -Force | Select-Object -First 1)) {
                Remove-Item -Path $installDir -Recurse -Force -ErrorAction SilentlyContinue
            }
        }
    }
}

if (-not $NoPath) {
    if (Add-UserPath $installDir) {
        Write-Result "Added $installDir to the user PATH (open a new terminal to pick it up)"
    }
}
$env:Path = "$installDir;$env:Path"

Write-Host ''
if (Test-Path $exe) {
    $installed = Get-UkmaVersion $exe
    if ($installed) {
        Write-Host "$installed is installed."
    } else {
        Write-Host "installed to $exe"
    }
    Write-Host 'Next steps:'
    Write-Host '  1. open a new terminal'
    Write-Host '  2. ukma install      # wire Claude Code, Cursor, Codex, Copilot, ...'
    Write-Host '  3. ukma init         # inside each project'
}
else {
    Write-Host "Dry run: nothing was written to $installDir."
}
