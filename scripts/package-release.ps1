<#
.SYNOPSIS
    Build a SANITIZED Windows release bundle for GitHub Releases.

.DESCRIPTION
    Produces axon-windows-x86_64.zip containing:
        axon.exe                     (native Windows build)
        static\                      (built Vue dashboard)
        config\                      (models.toml + config, minus ssh secrets)
        tools\                       (empty - populated at runtime)
        memory\                      (schema only, no *.db)
        .env.example
        credentials.example.json
        bin\wake-model-builder.exe   (wake-word enrollment companion binary,
                                      if the build succeeded — optional)

    Ships NO real .env, NO credentials.json, and NO SSH keys - safe to publish.
    axon.exe needs these sibling folders on disk, which is why the Windows
    release is a zip bundle rather than a lone .exe.

    Run from the repo root:
        powershell -ExecutionPolicy Bypass -File scripts\package-release.ps1
#>
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
$Root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
Set-Location $Root
$Dist = Join-Path $Root 'dist-release-win'
$Out  = Join-Path $Root 'axon-windows-x86_64.zip'

function Info($m) { Write-Host "[->] $m" -ForegroundColor Cyan }
function Ok($m)   { Write-Host "[OK] $m" -ForegroundColor Green }
function Step($m) { Write-Host "`n=== $m ===" -ForegroundColor White }

# ── 1. Build the dashboard ───────────────────────────────────────────────────
Step "Building dashboard (axon-ui)"
Push-Location (Join-Path $Root 'axon-ui')
if (-not (Test-Path 'node_modules')) { Info "npm ci ..."; npm ci --no-fund --no-audit }
$env:AXON_NODE_TYPES_OUT = Join-Path $Root 'crates\axon-agent\assets\node_types.json'
npm run build
if ($LASTEXITCODE -ne 0) { throw "UI build failed." }
Pop-Location
$static = Join-Path $Root 'crates\axon-agent\static'
if (Test-Path $static) { Remove-Item $static -Recurse -Force }
New-Item -ItemType Directory -Path $static -Force | Out-Null
Copy-Item (Join-Path $Root 'axon-ui\dist\*') $static -Recurse -Force
Ok "Dashboard built."

# ── 2. Build the agent (native release .exe) ─────────────────────────────────
Step "Building agent (release)"
# Stop a running instance so the .exe isn't locked.
Get-Process -Name axon -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
cargo build --release -p axon
if ($LASTEXITCODE -ne 0) { throw "cargo build failed." }
$exe = Join-Path $Root 'target\release\axon.exe'
if (-not (Test-Path $exe)) { throw "axon.exe not found at $exe" }
Ok "Binary built: $exe"

# ── 2b. Build wake-model-builder (wake-word enrollment companion binary) ─────
# Standalone crate with its own workspace — see wake-model-builder\Cargo.toml
# for why (rustpotter's candle-core dependency pin conflicts with axon-agent's
# own tree, so axon-agent shells out to this binary instead of linking
# rustpotter directly). Built natively; if this step fails the release still
# succeeds — /api/wakeword/build just reports "not found" until an admin
# builds and drops the binary in bin\ manually.
Step "Building wake-model-builder (wake-word enrollment)"
$wakeBuilderExe = $null
Push-Location (Join-Path $Root 'wake-model-builder')
cargo build --release
$wakeBuilderOk = ($LASTEXITCODE -eq 0)
Pop-Location
if ($wakeBuilderOk) {
    $candidate = Join-Path $Root 'wake-model-builder\target\release\wake-model-builder.exe'
    if (Test-Path $candidate) { $wakeBuilderExe = $candidate }
}
if ($wakeBuilderExe) {
    Ok "wake-model-builder built: $wakeBuilderExe"
} else {
    Write-Host "[!] wake-model-builder build failed or skipped - the dashboard's wake-word enrollment endpoint will report 'not found' until it's built separately." -ForegroundColor Yellow
}

# ── 3. Assemble the sanitized bundle ─────────────────────────────────────────
Step "Assembling bundle"
if (Test-Path $Dist) { Remove-Item $Dist -Recurse -Force }
New-Item -ItemType Directory -Path $Dist -Force | Out-Null

Copy-Item $exe (Join-Path $Dist 'axon.exe') -Force
Copy-Item $static (Join-Path $Dist 'static') -Recurse -Force
Copy-Item (Join-Path $Root 'crates\axon-agent\config') (Join-Path $Dist 'config') -Recurse -Force
# Strip any local SSH secrets under config\.
Remove-Item (Join-Path $Dist 'config\ssh_keys')          -Recurse -Force -ErrorAction SilentlyContinue
Remove-Item (Join-Path $Dist 'config\ssh_servers.json')  -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path (Join-Path $Dist 'tools')  -Force | Out-Null
New-Item -ItemType Directory -Path (Join-Path $Dist 'memory') -Force | Out-Null

$memSrc = Join-Path $Root 'crates\axon-agent\memory'
if (Test-Path $memSrc) {
    Get-ChildItem $memSrc -File -Recurse |
        Where-Object { $_.Extension -notin '.db','.sqlite','.db-wal','.db-shm' } |
        ForEach-Object { Copy-Item $_.FullName (Join-Path $Dist 'memory') -Force }
}
Copy-Item (Join-Path $Root 'crates\axon-agent\.env.example') (Join-Path $Dist '.env.example') -Force
if ($wakeBuilderExe) {
    New-Item -ItemType Directory -Path (Join-Path $Dist 'bin') -Force | Out-Null
    Copy-Item $wakeBuilderExe (Join-Path $Dist 'bin\wake-model-builder.exe') -Force
}

@'
{
  "note": "OAuth app client IDs/secrets for in-process integrations. Copy to credentials.json and fill in, OR add them from the dashboard Services page.",
  "google":    { "client_id": "", "client_secret": "" },
  "microsoft": { "client_id": "", "client_secret": "" },
  "facebook":  { "app_id": "", "app_secret": "" }
}
'@ | Set-Content -Path (Join-Path $Dist 'credentials.example.json') -Encoding UTF8

# ── 4. Archive ───────────────────────────────────────────────────────────────
Step "Creating archive"
if (Test-Path $Out) { Remove-Item $Out -Force }
Compress-Archive -Path (Join-Path $Dist '*') -DestinationPath $Out -Force

# ── 5. Verify no secrets leaked in ───────────────────────────────────────────
Add-Type -AssemblyName System.IO.Compression.FileSystem
$zip = [System.IO.Compression.ZipFile]::OpenRead($Out)
$bad = $zip.Entries | Where-Object {
    $_.FullName -match '(^|/)\.env$' -or $_.FullName -match 'credentials\.json$' -or
    $_.FullName -match 'ssh_keys/'   -or $_.FullName -match 'tokens\.json$' -or
    $_.FullName -match '\.key$'
}
$zip.Dispose()
if ($bad) {
    Write-Host "[!] SECRET DETECTED in the archive - aborting. Do NOT upload:" -ForegroundColor Yellow
    $bad | ForEach-Object { Write-Host "    $($_.FullName)" }
    exit 1
}

Write-Host ""
Ok ("Clean bundle ready: {0}  ({1:N1} MB)" -f $Out, ((Get-Item $Out).Length/1MB))
Info "Upload this to the GitHub release as: axon-windows-x86_64.zip"
