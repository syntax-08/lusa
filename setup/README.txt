=== INSTALLING LUSA ON ANY PC (Windows 10/11 64-bit) ===

1. Unzip this folder anywhere (e.g. the Desktop).
2. Double-click  install-lusa.cmd
   (installs the runtime to %LOCALAPPDATA%\Lusa\bin and adds it to the user
    PATH; no cargo, Rust or prerequisites of any kind needed).
3. Open a NEW TERMINAL and test:

   lusalang version
   lusalang repl
   lusalang new myproject

To compile a native executable, install GCC, Clang, TCC, or Visual Studio
Build Tools and run:

   lusalang build myproject\main.lusa

`lusalang build --emit-c` writes the portable C11 intermediate instead. The
interpreter and `--vm` never need a C compiler. Unsafe hardware access uses
`unsafe do ... end` + unsafe_ptr_cast/unsafe_read_mem/unsafe_write_mem and is
only available through `lusalang build` / `run --native`.

If you prefer NOT to install (portable mode): just run lusa.exe directly
from the unzipped folder, without touching the PATH:

   .\lusa.exe someprogram.lusa

PowerShell blocked? Right-click install-lusa.cmd > Run, or execute:

   powershell -NoProfile -ExecutionPolicy Bypass -File .\install-lusa.ps1
