<#
.SYNOPSIS
    Build axon-voice - prompts for Debug or Release, then runs the matching Gradle assemble task.
.PARAMETER Variant
    Skip the prompt by passing "Debug" or "Release" directly, e.g. .\build.ps1 -Variant Release
#>
param(
    [ValidateSet("Debug", "Release")]
    [string]$Variant
)

$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

if (-not $Variant) {
    Write-Host ""
    Write-Host "axon-voice build" -ForegroundColor Cyan
    Write-Host "  [1] Debug"
    Write-Host "  [2] Release"
    $choice = Read-Host "Select build variant (1/2)"
    $Variant = switch ($choice) {
        "1" { "Debug" }
        "2" { "Release" }
        default {
            Write-Host "Invalid choice: '$choice'" -ForegroundColor Red
            exit 1
        }
    }
}

# AGP 8.7.x rejects JDK 25+ as the daemon JVM; gradle.properties already pins
# org.gradle.java.home to Android Studio's JBR, but set JAVA_HOME too in case
# this shell invokes any tooling that reads it directly (matches README).
$jbrHome = "C:\Program Files\Android\Android Studio\jbr"
if (Test-Path $jbrHome) {
    $env:JAVA_HOME = $jbrHome
}

if ($Variant -eq "Release" -and -not (Test-Path (Join-Path $PSScriptRoot "keystore.properties"))) {
    Write-Host "Warning: keystore.properties not found - release APK will build unsigned." -ForegroundColor Yellow
}

Write-Host "Building $Variant..." -ForegroundColor Cyan
& .\gradlew.bat ":app:assemble$Variant"

if ($LASTEXITCODE -ne 0) {
    Write-Host "Build failed (exit $LASTEXITCODE)." -ForegroundColor Red
    exit $LASTEXITCODE
}

$outDir = Join-Path $PSScriptRoot "app\build\outputs\apk\$($Variant.ToLower())"
$apk = Get-ChildItem -Path $outDir -Filter "*.apk" -ErrorAction SilentlyContinue |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1

if ($apk) {
    Write-Host "Build succeeded: $($apk.FullName)" -ForegroundColor Green
} else {
    Write-Host "Build succeeded, but no APK found under $outDir" -ForegroundColor Yellow
}
