@echo off
rem install-lusa.cmd - installs the Lusa runtime (runs the PowerShell logic)
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0install-lusa.ps1"
