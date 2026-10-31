@echo off
rem Forwards to win-cargo.ps1 for tools that need an executable taking cargo's subcommand first, like the Tauri CLI runner.
where pwsh >nul 2>nul && (pwsh -NoProfile -ExecutionPolicy Bypass -File "%~dp0win-cargo.ps1" %* & exit /b %errorlevel%)
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0win-cargo.ps1" %*
exit /b %errorlevel%
