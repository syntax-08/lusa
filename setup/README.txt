=== INSTALLING LUSA ON ANY PC (Windows 10/11 64-bit) ===

1. Unzip this folder anywhere (e.g. the Desktop).
2. Double-click  install-lusa.cmd
   (installs the runtime to %LOCALAPPDATA%\Lusa\bin and adds it to the user
    PATH; no cargo, Rust or prerequisites of any kind needed).
3. Open a NEW TERMINAL and test:

   lusalang version
   lusalang repl
   lusalang new myproject

If you prefer NOT to install (portable mode): just run lusa.exe directly
from the unzipped folder, without touching the PATH:

   .\lusa.exe someprogram.lusa

PowerShell blocked? Right-click install-lusa.cmd > Run, or execute:

   powershell -NoProfile -ExecutionPolicy Bypass -File .\install-lusa.ps1
