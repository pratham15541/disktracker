@echo off
setlocal enabledelayedexpansion

:: Check for administrative privileges
net session >nul 2>&1
if %errorLevel% neq 0 (
    echo Error: This installer must be run with Administrator privileges.
    echo Please open Command Prompt or PowerShell as Administrator, then run this installer again.
    pause
    exit /b 1
)

:: Run PowerShell installer script
echo Running PowerShell installation script...
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0install.ps1"

if %errorLevel% neq 0 (
    echo.
    echo Installation failed.
) else (
    echo.
    echo Installation completed successfully.
)
pause
