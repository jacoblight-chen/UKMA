<#
.SYNOPSIS
    Reports whether the profile-C services are up and answering.

.PARAMETER Root
    Where the services were installed. Default F:\ukma-services.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File tools\profile-c\status.ps1
#>
[CmdletBinding()]
param([string] $Root = "")

$ErrorActionPreference = "Stop"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$versions = Get-Content -Raw (Join-Path $here "versions.json") | ConvertFrom-Json
if ([string]::IsNullOrWhiteSpace($Root)) { $Root = $versions.root_default }
$ports = $versions.ports
$curl = Join-Path $env:SystemRoot "System32\curl.exe"

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

$qdrantVersion = "-"
if (Test-Port $ports.qdrant_http) {
    $body = & $curl -s --max-time 5 ("http://127.0.0.1:" + $ports.qdrant_http + "/")
    if ($body -match '"version":"([^"]+)"') { $qdrantVersion = $Matches[1] }
}

$pgVersion = "-"
$psql = Join-Path $Root "pg\pgsql\bin\psql.exe"
if ((Test-Port $ports.postgres) -and (Test-Path $psql)) {
    $pgVersion = (& $psql -h 127.0.0.1 -p $ports.postgres -U postgres -t -A -c "show server_version;" 2>$null)
    if (-not $pgVersion) { $pgVersion = "listening" }
}

$neo4jVersion = "-"
if (Test-Port $ports.neo4j_http) {
    $password = "ukma-dev-pass"
    $passwordFile = Join-Path $Root "neo4j-initialised"
    if (Test-Path $passwordFile) { $password = (Get-Content $passwordFile -Raw).Trim() }
    $probe = Join-Path $Root "probe.json"
    Set-Content -Path $probe -Value '{"statements":[{"statement":"RETURN 1 AS ok"}]}'
    $body = & $curl -s --max-time 10 -u ("neo4j:" + $password) -H "Content-Type: application/json" -d ("@" + $probe) ("http://127.0.0.1:" + $ports.neo4j_http + "/db/neo4j/tx/commit")
    if ($body -match '"errors":\[\]') { $neo4jVersion = "accepting statements" } else { $neo4jVersion = "listening, not ready" }
}

"{0,-12} {1,-11} {2,-8} {3}" -f "service", "port", "state", "detail"
"{0,-12} {1,-11} {2,-8} {3}" -f "postgresql", $ports.postgres, $(if (Test-Port $ports.postgres) { "up" } else { "down" }), $pgVersion
"{0,-12} {1,-11} {2,-8} {3}" -f "qdrant", $ports.qdrant_http, $(if (Test-Port $ports.qdrant_http) { "up" } else { "down" }), $qdrantVersion
"{0,-12} {1,-11} {2,-8} {3}" -f "neo4j", $ports.neo4j_http, $(if (Test-Port $ports.neo4j_http) { "up" } else { "down" }), $neo4jVersion
