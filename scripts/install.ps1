<#
.SYNOPSIS
    Axon — Windows installer.

.DESCRIPTION
    Installs Axon from a GitHub release bundle, registers it to launch on login
    (Startup apps), and starts it.

    Bootstrap behaviour (as requested):
      * If the release bundle (axon-windows-x86_64.zip) or axon.exe is in the
        current directory, it is used.
      * Otherwise the latest release bundle is downloaded from GitHub.

    NOTE: axon.exe cannot run alone — it serves the dashboard from a 'static'
    folder and reads 'config' and 'tools' from its working directory. The
    Windows release is therefore a zip bundle (axon.exe + static + config +
    tools + .env.example), the Windows twin of the Linux tarball.

.PARAMETER Dir
    Install location. Default: %LOCALAPPDATA%\Axon

.PARAMETER Version
    Release tag to pin (e.g. v0.4.0). Default: latest.

.PARAMETER File
    Use this local .zip bundle instead of downloading.

.PARAMETER NoStartup
    Do not add Axon to Startup apps.

.PARAMETER NoStart
    Do not launch Axon after installing.

.EXAMPLE
    # From a downloaded copy in the current folder:
    powershell -ExecutionPolicy Bypass -File install.ps1

.EXAMPLE
    # Straight from GitHub (downloads the bundle itself):
    irm https://github.com/orapagier/axon/releases/latest/download/install.ps1 | iex
#>
[CmdletBinding()]
param(
    [string]$Dir       = $(if ($env:AXON_DIR) { $env:AXON_DIR } else { Join-Path $env:LOCALAPPDATA 'Axon' }),
    [string]$Version   = 'latest',
    [string]$File      = '',
    [switch]$NoStartup,
    [switch]$NoStart
)

$ErrorActionPreference = 'Stop'
$Repo    = if ($env:AXON_REPO) { $env:AXON_REPO } else { 'orapagier/axon' }
$ZipName = 'axon-windows-x86_64.zip'

function Info($m) { Write-Host "[->] $m" -ForegroundColor Cyan }
function Ok($m)   { Write-Host "[OK] $m" -ForegroundColor Green }
function Warn($m) { Write-Host "[!]  $m" -ForegroundColor Yellow }
function Step($m) { Write-Host "`n=== $m ===" -ForegroundColor White }

Write-Host ""
Write-Host "+==========================================+" -ForegroundColor White
Write-Host "|            Axon - Installer               |" -ForegroundColor White
Write-Host "+==========================================+" -ForegroundColor White

# ── Acquire the bundle ───────────────────────────────────────────────────────
Step "Locating release bundle"
$Tmp    = Join-Path ([System.IO.Path]::GetTempPath()) ("axon_" + [System.Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $Tmp -Force | Out-Null
$Extract = Join-Path $Tmp 'bundle'
New-Item -ItemType Directory -Path $Extract -Force | Out-Null

function Expand-Bundle([string]$zip) {
    Info "Extracting $([System.IO.Path]::GetFileName($zip)) ..."
    Expand-Archive -Path $zip -DestinationPath $Extract -Force
}

$localZip = Join-Path (Get-Location) $ZipName
$localExe = Join-Path (Get-Location) 'axon.exe'

if ($File) {
    if (-not (Test-Path $File)) { throw "-File '$File' not found." }
    Info "Using local bundle: $File"
    Expand-Bundle $File
}
elseif (Test-Path $localZip) {
    Info "Found $ZipName in the current directory - using it."
    Expand-Bundle $localZip
}
elseif (Test-Path $localExe) {
    Warn "Found axon.exe but no bundle. axon.exe needs its static/config/tools folders,"
    Warn "so the full bundle will be downloaded and your local axon.exe copied over it."
    $needDownload = $true
}
else {
    $needDownload = $true
}

if ($needDownload) {
    if ($Version -eq 'latest') {
        $url = "https://github.com/$Repo/releases/latest/download/$ZipName"
    } else {
        $url = "https://github.com/$Repo/releases/download/$Version/$ZipName"
    }
    $dl = Join-Path $Tmp $ZipName
    Info "Downloading: $url"
    try {
        [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
        Invoke-WebRequest -Uri $url -OutFile $dl -UseBasicParsing
    } catch {
        throw "Download failed. Check the version/tag, or place $ZipName next to this script.  ($_)"
    }
    Ok ("Downloaded {0:N1} MB" -f ((Get-Item $dl).Length / 1MB))
    Expand-Bundle $dl
    if (Test-Path $localExe) {
        Info "Overlaying your local axon.exe onto the bundle."
        Copy-Item $localExe (Join-Path $Extract 'axon.exe') -Force
    }
}

# Some zips wrap everything in a top folder; find the dir that actually holds axon.exe.
$exe = Get-ChildItem -Path $Extract -Filter 'axon.exe' -Recurse | Select-Object -First 1
if (-not $exe) { throw "axon.exe not found inside the bundle." }
$SrcRoot = $exe.Directory.FullName

# ── Install ──────────────────────────────────────────────────────────────────
Step "Installing to $Dir"
# Stop a running instance so its files can be replaced.
Get-Process -Name axon -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 400

New-Item -ItemType Directory -Path $Dir -Force | Out-Null
# Copy-Item merges into $Dir; it does not delete an existing .env / memory db.
Copy-Item -Path (Join-Path $SrcRoot '*') -Destination $Dir -Recurse -Force
Ok "Files copied."

# ── First-run .env (generate a valid master key) ─────────────────────────────
Step "Configuring environment"
$envFile = Join-Path $Dir '.env'
function New-Key { -join ((1..44) | ForEach-Object { '{0:X}' -f (Get-Random -Max 16) }) }

$generatedKey = $null
if (Test-Path $envFile) {
    Info "Existing .env kept as-is."
} else {
    $example = Join-Path $Dir '.env.example'
    if (-not (Test-Path $example)) { throw ".env.example missing from the bundle." }
    $generatedKey = New-Key
    $lines = Get-Content $example
    $apiKey = New-Key
    $out = foreach ($ln in $lines) {
        if     ($ln -match '^AXON_MASTER_KEY=') { "AXON_MASTER_KEY=$generatedKey" }
        elseif ($ln -match '^AXON_API_KEY=')    { "AXON_API_KEY=$apiKey" }
        else { $ln }
    }
    Set-Content -Path $envFile -Value $out -Encoding UTF8
    Ok "Created .env with a freshly generated master key."
}

# ── Startup app (launch hidden on login, with $Dir as the working directory) ─
Step "Startup registration"
# axon.exe must run with $Dir as CWD (that is where static/, config/, .env live).
# A tiny .vbs launches it hidden so no console window pops up each login.
$vbs = Join-Path $Dir 'start-axon.vbs'
@"
Set sh = CreateObject("WScript.Shell")
sh.CurrentDirectory = "$Dir"
sh.Run """$Dir\axon.exe""", 0, False
"@ | Set-Content -Path $vbs -Encoding ASCII

if (-not $NoStartup) {
    $startup  = [Environment]::GetFolderPath('Startup')
    $lnk      = Join-Path $startup 'Axon.lnk'
    $wsh      = New-Object -ComObject WScript.Shell
    $sc       = $wsh.CreateShortcut($lnk)
    $sc.TargetPath       = "$env:WINDIR\System32\wscript.exe"
    $sc.Arguments        = "`"$vbs`""
    $sc.WorkingDirectory = $Dir
    $sc.Description       = 'Axon Agent'
    $sc.Save()
    Ok "Added to Startup apps (runs on login): $lnk"
} else {
    Info "Skipped Startup registration (-NoStartup)."
}

# ── Launch now ───────────────────────────────────────────────────────────────
Step "Starting Axon"
if (-not $NoStart) {
    Start-Process -FilePath (Join-Path $Dir 'axon.exe') -WorkingDirectory $Dir -WindowStyle Hidden
    Start-Sleep -Seconds 2
    Ok "Axon started."
} else {
    Info "Not started (-NoStart). Launch later with:  wscript `"$vbs`""
}

Remove-Item -Path $Tmp -Recurse -Force -ErrorAction SilentlyContinue

# ── Done ─────────────────────────────────────────────────────────────────────
$port = 3000
$portLine = Select-String -Path $envFile -Pattern '^AXON_PORT=' -ErrorAction SilentlyContinue
if ($portLine) { $port = ($portLine.Line -split '=',2)[1].Trim() }

Write-Host ""
Write-Host "Axon is installed." -ForegroundColor Green
Write-Host "   Dashboard : http://localhost:$port" -ForegroundColor Cyan
Write-Host "   Install   : $Dir"
Write-Host "   Startup   : $(if ($NoStartup) {'not registered'} else {'runs on login'})"
if ($generatedKey) {
    Write-Host ""
    Write-Host "   Save your master key - it is your dashboard login AND the key every" -ForegroundColor Yellow
    Write-Host "   stored secret is encrypted under. It is NOT recoverable if lost:"     -ForegroundColor Yellow
    Write-Host "       AXON_MASTER_KEY = $generatedKey" -ForegroundColor White
}
Write-Host ""
Write-Host "   Next: add at least one LLM provider key in:" -ForegroundColor Gray
Write-Host "       $envFile"
Write-Host "   then restart Axon (Task Manager -> end 'axon', or log off/on)."
Write-Host ""
