<#
Pulls the real axon.db (+ WAL/SHM) from the deployed axon GCP instance for
fine-tune data extraction. Read-only on the remote side.

Uses plain scp against AXON_TARGET_SERVER from .deploy.env rather than
`gcloud compute scp` -- the gcloud CLI isn't installed on this machine, but
AXON_TARGET_SERVER already gives direct SSH access to the same instance
(confirmed reachable with the existing local SSH key).
#>

$ErrorActionPreference = "Stop"
$RepoRoot = Resolve-Path "$PSScriptRoot\..\.."
$EnvFile = Join-Path $RepoRoot ".deploy.env"

if (-not (Test-Path $EnvFile)) {
    throw ".deploy.env not found at $EnvFile"
}

$envVars = @{}
Get-Content $EnvFile | ForEach-Object {
    if ($_ -match '^\s*([A-Z_]+)\s*=\s*"?([^"]*)"?\s*$') {
        $envVars[$Matches[1]] = $Matches[2]
    }
}

$Target = $envVars["AXON_TARGET_SERVER"]
if (-not $Target) { throw "AXON_TARGET_SERVER not set in .deploy.env" }

$RemoteDir = $envVars["AXON_REMOTE_DIR"]
if (-not $RemoteDir) { throw "AXON_REMOTE_DIR not set in .deploy.env" }

$RemotePath = "$RemoteDir/axon/core/memory/axon.db"
$DestDir = Join-Path $RepoRoot "finetune\data\raw"
New-Item -ItemType Directory -Force -Path $DestDir | Out-Null

Write-Host "Pulling axon.db (+wal/shm) from $Target`:$RemotePath ..."
scp "${Target}:${RemotePath}" "${Target}:${RemotePath}-wal" "${Target}:${RemotePath}-shm" $DestDir

Write-Host "Done. Files in ${DestDir}:"
Get-ChildItem $DestDir | Format-Table Name, Length, LastWriteTime
