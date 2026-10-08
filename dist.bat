@echo off
REM Double-click entry: assemble dist\Selectable (release + backfill models).
cd /d "%~dp0"
cargo xtask dist
pause
