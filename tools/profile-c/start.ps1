<#
.SYNOPSIS
    Downloads, configures and starts the profile-C services (PostgreSQL, Qdrant,
    Neo4j) as user processes on 127.0.0.1. No administrator rights, no Windows
    services, nothing installed outside -Root.

.DESCRIPTION
    Every artifact is pinned in versions.json with a SHA-256 digest, so a run is
    reproducible and a tampered download is refused. Start is idempotent: an
    already-running service is reported and left alone.

.PARAMETER Root
    Where the artifacts, data directories and logs live. Default F:\ukma-services.

.PARAMETER Neo4jPassword
    Development password for the neo4j user. Default ukma-dev-pass.

.PARAMETER SkipDownload
    Fail instead of downloading a missing artifact.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File tools\profile-c\start.ps1
    . .\tools\profile-c\env.ps1     # sets UKMA_POSTGRES_DSN etc.
    cargo test -p ukma-adapter-postgres -p ukma-adapter-qdrant -p ukma-adapter-neo4j
#>
[CmdletBinding()]
param(
    [string] $Root = "",
    [string] $Neo4jPassword = "ukma-dev-pass",
    [switch] $SkipDownload
)

$ErrorActionPreference = "Stop"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$versions = Get-Content -Raw (Join-Path $here "versions.json") | ConvertFrom-Json
if ([string]::IsNullOrWhiteSpace($Root)) { $Root = $versions.root_default }
$Root = [System.IO.Path]::GetFullPath($Root)
New-Item -ItemType Directory -Force -Path $Root | Out-Null

$ports = $versions.ports
$curl = Join-Path $env:SystemRoot "System32\curl.exe"
$tar = Join-Path $env:SystemRoot "System32\tar.exe"

function Write-Step([string] $message) { Write-Host ("==> " + $message) }

function Start-Detached([string] $exe, [string[]] $arguments, [string] $workingDirectory, [string] $log) {
    # Win32_Process.Create returns immediately and inherits no handles, so the
    # script finishes while the server keeps running. `Start-Process` with
    # redirected stdio would keep the caller attached to the child's pipes, which
    # looks exactly like a hang.
    $quoted = ($arguments | ForEach-Object { '"' + $_ + '"' }) -join ' '
    $commandLine = 'cmd /c cd /d "' + $workingDirectory + '" && "' + $exe + '" ' + $quoted + ' > "' + $log + '" 2>&1'
    $result = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{ CommandLine = $commandLine }
    if ($result.ReturnValue -ne 0) { throw ("cannot start " + $exe + ": Win32_Process.Create returned " + $result.ReturnValue) }
    return $result.ProcessId
}

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

function Get-Artifact([string] $name) {
    $artifact = $versions.artifacts | Where-Object { $_.name -eq $name }
    if (-not $artifact) { throw "unknown artifact $name" }
    $file = Join-Path $Root $artifact.file
    if (-not (Test-Path $file)) {
        if ($SkipDownload) { throw "$file is missing and -SkipDownload was given" }
        Write-Step ("downloading " + $artifact.name + " " + $artifact.version + " (" + [math]::Round($artifact.bytes / 1MB) + " MB)")
        & $curl -s -L --retry 2 -A "Mozilla/5.0 (Windows NT 10.0; Win64; x64)" -o $file $artifact.url
        if ($LASTEXITCODE -ne 0) { throw "download failed for $($artifact.url)" }
    }
    $actual = (Get-FileHash $file -Algorithm SHA256).Hash.ToLower()
    if ($actual -ne $artifact.sha256) {
        throw ("digest mismatch for " + $file + ": expected " + $artifact.sha256 + ", got " + $actual)
    }
    return $artifact
}

function Expand-Artifact([string] $name) {
    $artifact = Get-Artifact $name
    $target = Join-Path $Root $artifact.extract_to
    $marker = Join-Path $Root (".extracted-" + $artifact.name + "-" + $artifact.version)
    if (Test-Path $marker) {
        Write-Step ($artifact.name + " already extracted")
        return $artifact
    }
    New-Item -ItemType Directory -Force -Path $target | Out-Null
    Write-Step ("extracting " + $artifact.file + " -> " + $target)
    & $tar -xf (Join-Path $Root $artifact.file) -C $target
    if ($LASTEXITCODE -ne 0) { throw "extraction failed for $($artifact.file)" }
    Set-Content -Path $marker -Value $artifact.sha256
    return $artifact
}

function Find-Directory([string] $pattern) {
    $match = Get-ChildItem $Root -Directory -Filter $pattern | Select-Object -First 1
    if (-not $match) { throw "no directory matching $pattern under $Root" }
    return $match.FullName
}

# ---------------------------------------------------------------- Qdrant -----
Write-Step "qdrant"
$null = Expand-Artifact "qdrant"
$qdrant = Get-ChildItem $Root -File -Filter "qdrant.exe" | Select-Object -First 1
if (-not $qdrant) { throw "qdrant.exe is missing after extraction" }
if (Test-Port $ports.qdrant_http) {
    Write-Step ("qdrant already listening on " + $ports.qdrant_http)
} else {
    $env:QDRANT__SERVICE__HOST = "127.0.0.1"
    $env:QDRANT__SERVICE__HTTP_PORT = [string]$ports.qdrant_http
    $env:QDRANT__SERVICE__GRPC_PORT = [string]$ports.qdrant_grpc
    $env:QDRANT__STORAGE__STORAGE_PATH = Join-Path $Root "qdrant-data"
    $env:QDRANT__TELEMETRY_DISABLED = "true"
    $null = Start-Detached $qdrant.FullName @() $Root (Join-Path $Root "qdrant.log")
}

# ------------------------------------------------------------ PostgreSQL -----
Write-Step "postgresql"
$null = Expand-Artifact "postgres"
$pgBin = Join-Path $Root "pg\pgsql\bin"
if (-not (Test-Path (Join-Path $pgBin "postgres.exe"))) { throw "postgres.exe is missing under $pgBin" }
$pgData = Join-Path $Root "pgdata"
if (-not (Test-Path (Join-Path $pgData "PG_VERSION"))) {
    Write-Step "initialising the cluster (trust auth, 127.0.0.1 only)"
    & (Join-Path $pgBin "initdb.exe") -D $pgData -U postgres -A trust --encoding=UTF8 --locale=C | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "initdb failed" }
}
if (Test-Port $ports.postgres) {
    Write-Step ("postgresql already listening on " + $ports.postgres)
} else {
    # The postmaster itself is the long-running process: pg_ctl would hand the
    # console to a child and a killed console takes the server with it.
    $listen = "listen_addresses=127.0.0.1"
    $null = Start-Detached (Join-Path $pgBin "postgres.exe") `
        @("-D", $pgData, "-p", [string]$ports.postgres, "-c", $listen) $Root (Join-Path $Root "postgres.log")
}

# ----------------------------------------------------------------- Neo4j -----
Write-Step "neo4j"
$null = Expand-Artifact "jdk"
$null = Expand-Artifact "neo4j"
$jdk = Find-Directory "jdk-21*"
$neo4j = Find-Directory "neo4j-community-*"
$env:JAVA_HOME = $jdk
$env:PATH = (Join-Path $jdk "bin") + ";" + $env:PATH
$conf = Join-Path $neo4j "conf\neo4j.conf"
$text = Get-Content $conf
if (-not ($text -match "^server.default_listen_address")) {
    Add-Content $conf "`nserver.default_listen_address=127.0.0.1"
}
if (-not ($text -match "^server.memory.heap.max_size")) {
    Add-Content $conf "server.memory.heap.initial_size=512m`nserver.memory.heap.max_size=1g"
}
$passwordFile = Join-Path $Root "neo4j-initialised"
if (-not (Test-Path $passwordFile)) {
    Write-Step "setting the development password (must happen before first start)"
    & (Join-Path $neo4j "bin\neo4j-admin.bat") dbms set-initial-password $Neo4jPassword 2>&1 | Out-Null
    Set-Content -Path $passwordFile -Value $Neo4jPassword
}
if (Test-Port $ports.neo4j_http) {
    Write-Step ("neo4j already listening on " + $ports.neo4j_http)
} else {
    # neo4j.bat -> neo4j.ps1 resolves java.exe from JAVA_HOME *inside* the detached
    # shell, so the variable is exported there rather than inherited.
    $line = 'cmd /c cd /d "' + $neo4j + '" && set "JAVA_HOME=' + $jdk + '" && set "PATH=' + (Join-Path $jdk "bin") + ';%PATH%" && call "bin\neo4j.bat" console > "' + (Join-Path $Root "neo4j.log") + '" 2>&1'
    $created = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{ CommandLine = $line }
    if ($created.ReturnValue -ne 0) { throw ("cannot start neo4j: " + $created.ReturnValue) }
}

# ------------------------------------------------------------ readiness -----
Write-Step "waiting for readiness"
$deadline = (Get-Date).AddSeconds(180)
$ready = @{ postgres = $false; qdrant = $false; neo4j = $false }
while ((Get-Date) -lt $deadline) {
    if (-not $ready.postgres -and (Test-Port $ports.postgres)) { $ready.postgres = $true }
    if (-not $ready.qdrant -and (Test-Port $ports.qdrant_http)) { $ready.qdrant = $true }
    if (-not $ready.neo4j -and (Test-Port $ports.neo4j_http)) { $ready.neo4j = $true }
    if ($ready.postgres -and $ready.qdrant -and $ready.neo4j) { break }
    Start-Sleep -Seconds 2
}
foreach ($name in @("postgres", "qdrant", "neo4j")) {
    if (-not $ready[$name]) { throw "$name did not become ready within 180s; see the logs under $Root" }
}

# Neo4j answers on the port before it accepts statements: probe a statement.
$auth = [Convert]::ToBase64String([Text.Encoding]::ASCII.GetBytes("neo4j:" + $Neo4jPassword))
$probe = Join-Path $Root "probe.json"
Set-Content -Path $probe -Value '{"statements":[{"statement":"RETURN 1 AS ok"}]}'
$ok = $false
while ((Get-Date) -lt $deadline) {
    $body = & $curl -s --max-time 10 -H ("Authorization: Basic " + $auth) -H "Content-Type: application/json" -d ("@" + $probe) ("http://127.0.0.1:" + $ports.neo4j_http + "/db/neo4j/tx/commit")
    if ($body -match '"errors":\[\]') { $ok = $true; break }
    Start-Sleep -Seconds 2
}
if (-not $ok) { throw "neo4j did not accept a statement within 180s; see neo4j.log" }

$envFile = Join-Path $here "env.ps1"
$lines = @(
    "# Generated by start.ps1 -- dot-source it to point the adapter tests at these services.",
    ('$env:UKMA_POSTGRES_DSN = "host=127.0.0.1 port=' + $ports.postgres + ' user=postgres dbname=postgres"'),
    ('$env:UKMA_QDRANT_URL = "http://127.0.0.1:' + $ports.qdrant_http + '"'),
    ('$env:UKMA_NEO4J_URL = "http://127.0.0.1:' + $ports.neo4j_http + '"'),
    ('$env:UKMA_NEO4J_USER = "neo4j"'),
    ('$env:UKMA_NEO4J_PASSWORD = "' + $Neo4jPassword + '"')
)
Set-Content -Path $envFile -Value $lines -Encoding ASCII

Write-Step "profile C is up"
Write-Host ("  postgres 127.0.0.1:" + $ports.postgres + " (user postgres, trust auth)")
Write-Host ("  qdrant   http://127.0.0.1:" + $ports.qdrant_http)
Write-Host ("  neo4j    http://127.0.0.1:" + $ports.neo4j_http + " (neo4j / " + $Neo4jPassword + ")")
Write-Host ("  env      " + $envFile + "  ->  . " + $envFile)
Write-Host "  stop     tools\profile-c\stop.ps1"
