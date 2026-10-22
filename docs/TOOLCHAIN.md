# Lusa Toolchain (`lusalang`)

Complete guide to installing/running Lusa on Windows.
The **language** itself is documented in [`LANGUAGE.md`](LANGUAGE.md).

---

## 1. Philosophy

- **Using Lusa does NOT depend on cargo or Rust.** `lusa.exe` is a standalone
  executable (100% `std`, zero external dependencies, statically linked CRT —
  no Visual C++ Redistributable needed on any machine).
- Cargo is only required **to rebuild the engine** (if you touch the
  interpreter's Rust code) or **to create Rust hosts** embedding Lusa.
- `lusalang` is the "package-manager face" of the same binary: subcommands in
  `npm`/`cargo` style (`run`, `new`, `check`, `repl`, `where`, `version`).

---

## 2. Installation

From the project root (`Lang\lusa\`):

```sh
install-lusa.cmd
```

What it does:

1. Locates the binary: `lusa.exe` next to the script (portable package),
   then `target\release\lusa.exe` (preferred repo build), then `target\debug`.
2. Copies it to **`%LOCALAPPDATA%\Lusa\bin\`** (`lusa.exe` + `lusalang.cmd`).
3. Adds that folder to the **user PATH** (registry key `HKCU\Environment`,
   idempotent — never duplicates or truncates).
4. Sends the `WM_SETTINGCHANGE` system notification.

> ⚠️ **Open a NEW terminal** after installing: terminals already open keep
> the old PATH.

To **update** the runtime after a change:

```sh
cargo build --release   # in the project root (Lang\lusa)
install-lusa.cmd        # reinstalls the new version to %LOCALAPPDATA%\Lusa\bin
```

### Installing and updating without this repo (end users)

You do **not** need the source repo: the CLI can install and update itself
from the GitHub releases, using only the `curl.exe`/`tar.exe` that ship with
Windows.

Install from PowerShell (once):

```powershell
curl.exe -fsSL -o "$env:TEMP\lusa-setup.zip" `
  https://github.com/syntax-08/lusa/releases/latest/download/lusa-setup.zip
tar.exe -xf "$env:TEMP\lusa-setup.zip" -C "$env:TEMP"
mkdir "$env:LOCALAPPDATA\Lusa\bin" -Force
Copy-Item "$env:TEMP\lusa.exe" "$env:LOCALAPPDATA\Lusa\bin\lusa.exe" -Force
```

Then add `%LOCALAPPDATA%\Lusa\bin` to your user PATH once:

```powershell
$p = [Environment]::GetEnvironmentVariable("Path", "User")
if (-not $p.Contains("%LOCALAPPDATA%\Lusa\bin") -and -not $p.Contains("$env:LOCALAPPDATA\Lusa\bin")) {
  [Environment]::SetEnvironmentVariable("Path", "$p;$env:LOCALAPPDATA\Lusa\bin", "User")
}
```

Open a new terminal afterwards (running terminals keep the old PATH).

Update whenever you like:

```sh
lusalang self-update          # downloads the latest release + replaces itself
lusalang self-update --check  # only reports the newest release
lusalang self-update --force  # reinstall even if versions match
```

The update never bricks your install: the running exe is first renamed to
`lusa.exe.old`, the new one takes its place, and the backup is removed on
success — if something interrupts it, just rename `lusa.exe.old` back.

---

## 3. Commands

| command                          | effect                                           | detail                                        |
|----------------------------------|--------------------------------------------------|-----------------------------------------------|
| `lusalang version` / `lusa -v`   | runtime version                                  |                                               |
| `lusalang run <file.lusa>`       | runs a program in the interpreter                | path relative to the current directory        |
| `lusalang run <file.lusa> --vm`  | runs through the **bytecode VM**                 | same program, compiled to instructions first  |
| `lusalang run <file.lusa> --native` | transpiles to C11 and runs a native executable | uses the host C compiler; accepts program args |
| `lusalang build <file.lusa>`     | compiles to a native executable via C11           | `-o <out>`, `--cc <cc>`, `--emit-c`           |
| `lusalang build <file> --emit-c` | writes the intermediate `.c` instead of building  | default `<file>.c`                            |
| `lusalang check <file.lusa>`     | validates **lex+parse** and exits (0 ok, 1 fail) | **does not execute**; only catches code errors |
| `lusalang check <file> --typed`  | runs the C11 type checker                         | Cargo-style `error[E....]` diagnostics         |
| `lusalang new <name>`            | creates `<name>\main.lusa` + `<name>\README.md`  | scaffold ships with a working demo            |
| `lusalang repl` / `lusa`         | interactive REPL                                 | multi-line, `:quit` to exit                   |
| `lusalang where`                 | path of the installed runtime                    |                                               |
| `lusalang self-update`           | updates the runtime from GitHub releases         | `--check` (report only), `--force`            |
| `lusa <file.lusa>`               | direct shortcut (runs the file)                  |                                               |
| `lusa -e "<code>"`               | inline code                                      | see the quoting note below                    |
| `lusalang --help`                | help                                             |                                               |

### Windows quoting note

**PowerShell 5.1** mangles double quotes when passing arguments to native
executables — for inline code prefer `cmd.exe` or write to a `.lusa` file:

```sh
cmd /c "lusa -e ""print('ohana')"""
```

`.lusa` files **never** have this problem.

---

## 4. Projects (`lusalang new`)

```sh
lusalang new mygame
cd mygame
lusalang run main.lusa
```

Structure created:

```
mygame\
├── main.lusa    ← entry point (working demo: gradient + table data)
└── README.md    ← how to run
```

**Multi-file:** Lusa does **not have `require`/`dofile` yet** (roadmap).
Organize modules as blocks/files run one at a time, or keep a single
well-structured `main.lusa`. Large data can be generated at boot
("asset bake") or embedded as tables at the end of the file.

---

## 5. Building the language (engine developers only)

Requires Rust (cargo) ≥ 1.70. Commands, from the project root:

```sh
cargo build             # debug build (fast to compile, slow to run)
cargo build --release   # release turbo: lto + codegen-units=1 + opt-level=3
cargo test --all-targets  # unit + integration + self-hosted + C11 backend tests
cargo run               # opens the REPL straight from source
```

Release profile configured in `Cargo.toml`:

```toml
[profile.release]
opt-level = 3
lto = true
codegen-units = 1
```

Additionally, `.cargo\config.toml` enables `crt-static` on Windows — the
built `lusa.exe` runs on any Windows 10/11 x64 with **zero runtime
prerequisites** (no Visual C++ Redistributable).

### Official benchmark — `bench.lusa`

Composite load: recursive `fib(24)` + a 2M-iteration arithmetic loop +
200K interpolated strings + 100K nested-table creations.

| build                     | total time |
|---------------------------|------------|
| `target\debug\lusa.exe`   | ~25.2 s    |
| `target\release\lusa.exe` | ~5.5 s     |
| **speedup**               | **~4.6×**  |

Measure with PowerShell:

```powershell
$d = Measure-Command { & .\target\debug\lusa.exe bench.lusa > $null }
$r = Measure-Command { & .\target\release\lusa.exe bench.lusa > $null }
```

### Native C11 backend (Module 8.5)

`lusalang build` and `lusalang run --native` embed `lexer.lusa`,
`parser.lusa`, and `compiler_bootstrap.lusa` in `lusa.exe`. The self-hosted
pipeline optimizes and type-checks the AST, emits a portable C11 translation
unit, and invokes the host C compiler:

```sh
lusalang build hello.lusa                  # hello.exe (or hello on Unix)
lusalang build hello.lusa --emit-c         # hello.c, no C compiler invoked
lusalang build hello.lusa -o app.exe --cc "C:\path\to\clang.exe"
lusalang run hello.lusa --native           # build to a temp dir and run
lusalang check hello.lusa --typed          # type check only
```

The intermediate C file contains:

- `LusaString`/`LusaTable` structs with a 32-byte `LusaObj` header;
- `malloc`/`calloc`/`realloc`/`free` allocation and deep RAII drops;
- `uint32_t`, `int32_t`, `lusa_i128`, and `float` numeric mappings;
- `unsafe do ... end` lowered to `*(volatile uint32_t *)(uintptr_t)address`
  reads/writes for registers and memory-mapped hardware.

Compiler discovery order is `--cc`, `LUSA_CC`, `CC`, `PATH`
(`cc`/`gcc`/`clang`/`tcc`/`cl`), then Visual Studio via `vswhere.exe`. GCC,
Clang, TCC, and MSVC are supported; for MSVC the wrapper runs the matching
`vcvars64.bat`/`vcvarsall.bat x64` so `INCLUDE`/`LIB` are configured even
outside a Developer Command Prompt. If no compiler is found, `build` reports
the paths/flags to configure; the interpreter and bytecode VM never require a C
toolchain.

The old PE32+/ELF64 image emitter and its `emit_binary` API were removed. See
[`SELF_HOSTING.md`](SELF_HOSTING.md) for the compiler architecture and
[`LANGUAGE.md`](LANGUAGE.md) section 9 for the typed/unsafe language surface.

---

## 6. Taking Lusa to another PC

**Ready-made package:** `setup\lusa-setup.zip` (~350 KB) — contains
`lusa.exe`, `install-lusa.cmd`, `install-lusa.ps1` and `README.txt`.

On a new machine (Windows 10/11 x64):

1. Unzip anywhere.
2. Double-click `install-lusa.cmd` (installs to `%LOCALAPPDATA%\Lusa\bin`
   + user PATH, exactly like the first machine). No cargo, no Rust, no
   Visual C++ Redistributable required.
3. Open a new terminal → `lusalang version`.

**Portable mode (zero install):** run `lusa.exe` straight from the unzipped
folder, without touching the PATH.

**Rebuild route (dev):** copy the whole `Lang\lusa` folder and run
`cargo build --release` — only needed to modify the engine on that machine.

To refresh the package after engine changes: `cargo build --release`,
copy the exe into `setup\`, delete the old zip and re-zip the folder.

---

## 7. Toolchain roadmap

| phase | item                                                    |
|-------|---------------------------------------------------------|
| 1 ✅ | `run / check / new / repl / where / version` + installer, portable package |
| 2 ✅ | `lusalang build` — `.lusa` → C11 → native executable through the host GCC/Clang/TCC/MSVC |
| 3    | `lusalang pack` — bundles script(s) + runtime into a single standalone `.exe` to distribute your projects |
| 4    | `lusalang require` / multi-file module system           |
| 5    | JIT (long term); AOT via C11 is available now          |

### "Can Lusa get its own compiler?"

Yes — two engines already ship:

1. **Bytecode + VM** (`lusalang run --vm`): AST → instructions → stack-based VM
   inside `lusa.exe`; no C toolchain needed.
2. **AOT C11 backend** (`lusalang build` / `run --native`): typed AST → portable
   C11 → host compiler. This is the hardware/portability path and the only one
   that accepts `unsafe` blocks and raw volatile memory access.

A JIT remains a long-term possibility; the C11 backend is the current native
route.

## Packages: installing libraries with `lusalang install`

Lusa packages are plain GitHub repositories holding `.lusa` files and a tiny
`lusa.toml` manifest. Installing uses Windows' built-in `curl` and `tar` —
no external dependency manager.

```cmd
lusalang install syntax-08/lusa-ui     :: install (optionally: repo#v0.1.0 for a tag)
lusalang packages                      :: list installed packages
lusalang uninstall ui                  :: remove one
```

Packages land in `%LOCALAPPDATA%\Lusa\packages\<name>` and are visible to every
Lusa program via `require("name")`. Search priority: `$LUSA_PACKAGES`, then
`<project>/packages`, then the global root above.

### Writing a package

A package is a folder with `lusa.toml`:

```toml
name = "ui"
version = "0.1.0"
description = "Terminal interface (TUI) kit for Lusa"
entry = "ui.lusa"
```

and the entry file — functionally, a module that returns its exports:

```lusa
local function draw() ... end

return {
    draw = clone draw,
    version = "0.1.0",
}
```

> Tip: export your functions with `clone f`. A bare `f = f` inside the
> return-table constructor *moves* the local into the table (ownership!), and
> a module function is usually worth calling more than once. Strings are
> scalars and can be exported directly.

Usage on the consumer side — bind exports to locals before calling:

```lusa
local ui = require("ui")
local text = ui.text
print(text(1, 1, "hello from a package", 14))
```

The reference package lives at [syntax-08/lusa-ui](https://github.com/syntax-08/lusa-ui) —
a full TUI kit (colors, boxes, progress bars) you can crack open as a template.

## Language server: `lusalang lsp`

Lusa ships an LSP (Language Server Protocol) implementation over stdio,
written on `std` only. Editors use it to run the real checker inside the
editor instead of only highlighting:

- **syntax errors appear live** with the E-code (E0001 lexer, E0101 parser),
  underlined with the full cargo-style message; they clear as soon as the
  code is fixed
- full-document sync; UTF-16 positions per the LSP spec

The [Zed extension](https://github.com/syntax-08/zed-lusa) starts it
automatically for `.lusa` files. Manually (WSL/Linux/other is possible since
the binary is pure std):

```cmd
lusalang lsp
```

Editors talk to it via `Content-Length` JSON-RPC frames on stdio — a generic
LSP client configuration works. Diagnostics currently cover lex/parse
(static); ownership errors (E1xxx) are runtime checks and appear when you
`run` the program. Completion and hover are on the roadmap.


---

## 8. Two engines: interpreter and bytecode VM

Lusa ships two execution engines that produce **identical results** (the
integration suite includes output and error-code parity checks between them):

| engine      | how it runs                                       | when to use                            |
|-------------|---------------------------------------------------|----------------------------------------|
| interpreter | walks the AST directly                            | default; best diagnostics with spans   |
| bytecode VM | compiles to instructions, then runs a stack VM   | `--vm` flag; the base for future speed |

```sh
lusalang run main.lusa        # interpreter (default)
lusalang run main.lusa --vm   # compile + run on the bytecode VM
```

Both speak the same language, including the ownership model (`move` via plain
assignment, `clone`, `borrow` — note: `borrow` is interpreter-only for now),
pcall error handling and E-codes.

### VM hot-path notes

The VM is deliberately built around the operations that dominate real programs
(loops, local reads/writes and recursive calls), while keeping the runtime
`std`-only:

- **VM→VM calls use a trampoline.** `call_plain`/`call_method` push the callee
  frame and return to the same interpreter loop. There is no Rust recursion per
  Lusa call, so deep recursion and `pcall` unwind through frames, not the native
  stack.
- **Locals are direct by default.** A frame stores `SlotCell::Direct(SlotV)`.
  The first time a closure captures a local, that single slot is promoted to
  `Shared(Rc<RefCell<SlotV>>)`. Non-capturing functions therefore avoid an `Rc`
  allocation and a `RefCell` borrow on every `GetLoc`/`SetLoc`.
- **Numeric loop control is a fast path.** `ForNumStep` reads the three control
  slots in one combined check, updates the loop variable in place and patches
  `pc` without re-decoding the instruction stream.
- **Single-value calls truncate on return.** When a call is immediately used
  where one value is required, the compiler fuses its `TruncateToOne` into a
  `CallOne`/`MethodCallOne` variant. The callee return path keeps the first
  result (or produces `nil` for a zero-result call), avoiding a separate
  dispatch.
- **Globals use a cheap name hash and a versioned read cache.** Variable-name
  maps use a small FNV-1a hasher instead of SipHash; `GetGlob` keeps a one-entry
  cache guarded by a globals `version`, so a cached value can never survive a
  global assignment.
- **The instruction stream is allocation-free.** `Instr` is `Copy` and compact,
  fixed arguments stay on the shared value stack, and frame slot vectors are
  recycled between calls.

#### Measured hot-path gains

Release build, Windows x64, CPU time (`TotalProcessorTime`, not wall clock),
medians of three runs:

| workload                         | v0.2.0 baseline | current VM | gain   |
|----------------------------------|-----------------|------------|--------|
| 1M-iteration arithmetic loop     | ~1,969 ms       | ~78 ms     | ~25×   |
| recursive `fib(32)`              | ~23,359 ms      | ~1,641 ms  | ~14×   |
| recursive `fib(33)`              | —               | ~2,734 ms  | —      |
| 10M global reads                 | —               | ~828 ms    | —      |
| 10M local reads                  | —               | ~688 ms    | —      |

The numbers are machine-specific; re-run the same benchmark files with
`Start-Process` and measure `TotalProcessorTime` for a comparable result.
