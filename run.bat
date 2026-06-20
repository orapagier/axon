@echo off
setlocal enabledelayedexpansion

echo ==========================================
echo   AXON Automation: Build ^& Run
echo ==========================================

:: Step 1: Build Frontend
echo [1/3] Building Frontend (axon-ui)...
cd /d "%~dp0axon-ui"

if not exist "node_modules\.bin\vite.cmd" (
    echo [INFO] node_modules or vite binary missing. Running npm install...
    call npm install
    if %errorlevel% neq 0 (
        echo [ERROR] npm install failed!
        pause
        exit /b %errorlevel%
    )
)

call npm run build
if %errorlevel% neq 0 (
    echo [ERROR] Frontend build failed!
    pause
    exit /b %errorlevel%
)

:: Step 2: Sync Static Files to Backend
echo [2/3] Deploying static files to backend...
cd /d "%~dp0"
if not exist "crates\axon-agent\static" mkdir "crates\axon-agent\static"

:: Clean old files using PowerShell for better reliability with wildcards
powershell -Command "Remove-Item -Path 'crates\axon-agent\static\*' -Recurse -Force -ErrorAction SilentlyContinue"

:: Copy new files
powershell -Command "Copy-Item -Path 'axon-ui\dist\*' -Destination 'crates\axon-agent\static' -Recurse -Force"
if %errorlevel% neq 0 (
    echo [ERROR] Failed to copy static files!
    pause
    exit /b %errorlevel%
)

:: Step 3: Run Backend
echo [3/3] Starting Backend Server (axon-agent)...
cd /d "%~dp0crates\axon-agent"

:: Stop any existing axon process to avoid build locks
powershell -Command "Stop-Process -Name axon -Force -ErrorAction SilentlyContinue"

echo Note: Press Ctrl+C to stop the server when finished.
cargo run <nul
