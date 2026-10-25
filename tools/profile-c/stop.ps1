<#
.SYNOPSIS
    Stops the profile-C services started by start.ps1.

.DESCRIPTION
    PostgreSQL shuts down through its own control utility (fast mode), Neo4j
    through its launcher; Qdrant has no control channel, so its process is
    stopped by the specific PID that owns the listening port. Nothing is stopped
    by name, so an unrelated qdrant/postgres/java process is never touched.

.PARAMETER Root
    Where the services were installed. Default F:\ukma-services.
#>
[CmdletBinding()]
param([string] $Root = "")

$ErrorActionPreference = "Stop"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$versions = Get-Content -Raw (Join-Path $here "versions.json") | ConvertFrom-Json
if ([string]::IsNullOrWhiteSpace($Root)) { $Root = $versions.root_default }
$Root = [System.IO.Path]::GetFullPath($Root)
$ports = $versions.ports

function Write-Step([string] $message) { Write-Host ("==> " + $message) }

function Test-Port([int] $port) {
    $client = New-Object System.Net.Sockets.TcpClient
    try {
        $client.Connect("127.0.0.1", $port)
        return $true
    } catch {
        return $false
    } finally {
        $client.Dispose()
    }
}

function Get-PortOwner([int] $port) {
    $connection = Get-NetTCPConnection -LocalPort $port -State Listen -ErrorAction SilentlyContinue |
        Select-Object -First 1
    if ($connection) { return $connection.OwningProcess }
    return 0
}

# ------------------------------------------------------------ PostgreSQL -----
$pgCtl = Join-Path $Root "pg\pgsql\bin\pg_ctl.exe"
if (Test-Path $pgCtl) {
    $owner = Get-PortOwner $ports.postgres
    if ($owner -ne 0) {
        Write-Step "stopping postgresql (fast shutdown)"
        & $pgCtl -D (Join-Path $Root "pgdata") -m fast stop 2>&1 | Out-Null
        if ($LASTEXITCODE -ne 0) {
            Write-Step "pg_ctl could not stop it; terminating the listening PID $owner"
            Stop-Process -Id $owner -Force -ErrorAction SilentlyContinue
        }
    } else {
        Write-Step "postgresql is not listening"
    }
} else {
    Write-Step "postgresql is not installed under $Root"
}

# ----------------------------------------------------------------- Neo4j -----
$neo4j = Get-ChildItem $Root -Directory -Filter "neo4j-community-*" -ErrorAction SilentlyContinue |
    Select-Object -First 1
$jdk = Get-ChildItem $Root -Directory -Filter "jdk-21*" -ErrorAction SilentlyContinue |
    Select-Object -First 1
if ($neo4j) {
    if (Test-Port $ports.neo4j_http) {
        if ($jdk) {
            $env:JAVA_HOME = $jdk.FullName
            $env:PATH = (Join-Path $jdk.FullName "bin") + ";" + $env:PATH
        }
        Write-Step "stopping neo4j"
        & (Join-Path $neo4j.FullName "bin\neo4j.bat") stop 2>&1 | Out-Null
        Start-Sleep -Seconds 3
    } else {
        Write-Step "neo4j is not listening"
    }
    $owner = Get-PortOwner $ports.neo4j_http
    if ($owner -eq 0) { $owner = Get-PortOwner $ports.neo4j_bolt }
    if ($owner -ne 0) {
        Write-Step "neo4j is still up; terminating the listening PID $owner"
        Stop-Process -Id $owner -Force -ErrorAction SilentlyContinue
    }
} else {
    Write-Step "neo4j is not installed under $Root"
}

# ---------------------------------------------------------------- Qdrant -----
if (Test-Port $ports.qdrant_http) {
    $owner = Get-PortOwner $ports.qdrant_http
    if ($owner -ne 0) {
        Write-Step "stopping qdrant (PID $owner)"
        Stop-Process -Id $owner -Force -ErrorAction SilentlyContinue
        Start-Sleep -Seconds 2
    }
} else {
    Write-Step "qdrant is not listening"
}

# -------------------------------------------------------------- verify -------
Start-Sleep -Seconds 2
$still = @()
foreach ($entry in @(@("postgres", $ports.postgres), @("qdrant", $ports.qdrant_http), @("neo4j", $ports.neo4j_http))) {
    $client = New-Object System.Net.Sockets.TcpClient
    try {
        $client.Connect("127.0.0.1", [int]$entry[1])
        $still += $entry[0]
    } catch {
    } finally {
        $client.Dispose()
    }
}
if ($still.Count -gt 0) {
    throw ("still listening: " + ($still -join ", "))
}
Write-Step "profile C is down (ports " + $ports.postgres + ", " + $ports.qdrant_http + ", " + $ports.neo4j_http + " are free)"
