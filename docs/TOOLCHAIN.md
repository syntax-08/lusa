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

---

## 3. Commands

| command                          | effect                                           | detail                                        |
|----------------------------------|--------------------------------------------------|-----------------------------------------------|
| `lusalang version` / `lusa -v`   | runtime version                                  |                                               |
| `lusalang run <file.lusa>`       | runs a program                                   | path relative to the current directory        |
| `lusalang check <file.lusa>`     | validates **lex+parse** and exits (0 ok, 1 fail) | **does not execute**; only catches code errors |
| `lusalang new <name>`            | creates `<name>\main.lusa` + `<name>\README.md`  | scaffold ships with a working demo            |
| `lusalang repl` / `lusa`         | interactive REPL                                 | multi-line, `:quit` to exit                   |
| `lusalang where`                 | path of the installed runtime                    |                                               |
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
cargo test              # integration suite (19 tests)
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
| 2    | `lusalang build` — `.lusa` → bytecode (`.lub`) compiler + VM running **without cargo**; typical gain 3–10× over the tree-walk |
| 3    | `lusalang pack` — bundles script(s) + runtime into a single standalone `.exe` to distribute your projects |
| 4    | `lusalang require` / multi-file module system           |
| 5    | JIT (long term) or AOT via transpilation (very long)    |

### "Can Lusa get its own compiler?"

Yes — three possible routes:

1. **Bytecode + VM embedded in `lusalang build`** (recommended): real AST →
   instructions → stack-based VM inside the same `lusa.exe`. A real compiler,
   zero dependencies, runs anywhere without cargo.
2. **AOT**: transpile `.lusa` to C/Rust and generate a native `.exe`. Requires
   a runtime embedded in the output — weeks of work, but feasible.
3. **JIT**: LuaJIT-style; only worth it in a much more mature MVP.

Route 1 is outlined in the `README.md` roadmap (`compiler.rs` + `vm.rs`).

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
