@echo off
rem Thin shim so tools that want an executable (the Tauri CLI's build.runner, see src-tauri/tauri.windows.conf.json)
rem can run cargo through scripts/win-cargo.ps1. Prefers PowerShell 7 (pwsh), falls back to Windows PowerShell.
where pwsh >nul 2>nul && (pwsh -NoProfile -ExecutionPolicy Bypass -File "%~dp0win-cargo.ps1" %* & exit /b %errorlevel%)
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0win-cargo.ps1" %*
exit /b %errorlevel%
