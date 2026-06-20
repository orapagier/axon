@echo off
REM ============================================================
REM  Axon - one-command push to GitHub
REM  Usage:
REM     push.bat                 -> commit with timestamp, push
REM     push.bat "fix login bug" -> commit with your message, push
REM ============================================================
setlocal

cd /d "%~dp0"

REM --- 1. Make sure a GitHub remote is configured -----------
git remote get-url origin >nul 2>&1
if errorlevel 1 (
    echo.
    echo [setup] No "origin" remote found yet.
    echo         Create an empty repo at https://github.com/new ^(no README^),
    echo         then paste its URL below.
    echo.
    set /p REPO_URL="GitHub repo URL (https://github.com/USER/axon.git): "
    git remote add origin "%REPO_URL%"
    if errorlevel 1 ( echo Failed to add remote. & exit /b 1 )
    echo [setup] Remote added.
)

REM --- 2. Stage everything ----------------------------------
git add -A

REM --- 3. Commit (skip if nothing changed) ------------------
git diff --cached --quiet
if not errorlevel 1 (
    echo Nothing to commit - working tree is clean.
    goto :push
)

if "%~1"=="" (
    for /f "delims=" %%T in ('powershell -NoProfile -Command "Get-Date -Format \"yyyy-MM-dd HH:mm:ss\""') do set MSG=auto: update %%T
) else (
    set MSG=%~1
)
git commit -m "%MSG%"
if errorlevel 1 ( echo Commit failed. & exit /b 1 )

:push
REM --- 4. Push ----------------------------------------------
git push -u origin HEAD
if errorlevel 1 (
    echo.
    echo Push failed. If this is the FIRST push, the remote branch
    echo may not exist yet. It should be created now automatically.
    exit /b 1
)

echo.
echo === Done. Changes are on GitHub. ===
git log --oneline -1
endlocal
