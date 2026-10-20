# Lusa

**Lusa** is a dynamic language inspired by Lua, written 100% in Rust (only
`std`, zero dependencies), with an optional twist of **Rust-style ownership**
— value moves, borrows and clones with **cargo-style diagnostics**.

```
lusa> local t = {1, 2, 3}
lusa> local u = t            -- move: u is now the owner
lusa> print(t)
error[E1001]: use of moved-out value `t`
 --> <repl>:1:7
  | print(t)
  |      ^
note: `t` moved to `u` here
 --> <repl>:1:11
help: `t` no longer holds the value; use `clone t` (copy) or
      `borrow t` (keep the original) when you still need it
```

## Install and use (`lusalang`)

Installation on Windows — **no cargo in daily use**:

- **Easiest:** download [`lusa-setup.zip`](https://github.com/syntax-08/lusa/releases/latest),
  unzip it, and double-click `install-lusa.cmd`.
- **From source:** run `install-lusa.cmd` from this repository, or build with
  cargo (`cargo build --release`, exe lands in `target/release/lusa.exe`).

Then, from any new terminal:

| command                         | effect                                     |
|---------------------------------|--------------------------------------------|
| `lusalang version`              | runtime version (`lusa -v`)                |
| `lusalang run main.lusa`        | runs a program                             |
| `lusalang check main.lusa`      | validates (lex+parse) without running      |
| `lusalang new myproject`        | creates a new project with a ready scaffold |
| `lusalang install syntax-08/lusa-ui` | install a package (library) from GitHub |
| `lusalang packages`             | list installed packages                    |
| `lusalang uninstall <name>`     | remove an installed package                |
| `lusalang repl`                 | interactive REPL                           |
| `lusalang where`                | where the runtime is installed             |
| `lusa main.lusa`                | direct shortcut (same binary)              |
| `lusa -e "print('hi')"`         | inline code (see the Windows note)         |

See [`docs/TOOLCHAIN.md`](docs/TOOLCHAIN.md) for the full toolchain guide
(including installing on another PC from `setup\lusa-setup.zip`).

### Build the language (only if you touch the engine)

```sh
cargo build             # debug
cargo build --release   # release turbo (opt-level 3 + LTO) — use this to install
cargo test              # integration suite (19 tests)
```

The resulting binary (`target\release\lusa.exe`) is standalone: **running
Lusa programs never needs cargo or Rust** — cargo is only required to rebuild
the engine or create Rust hosts embedding Lusa.

### Windows PowerShell note

PowerShell 5.1 mangles double quotes when passing arguments to native
executables. For inline code use `cmd.exe`, or write to a `.lusa` file:

```powershell
cmd /c "target\debug\lusa.exe -e ""print('ohana')"""
```

## Examples

See [`examples/`](examples/):

| file                      | contents                                              |
|---------------------------|-------------------------------------------------------|
| `hello.lusa`              | hello world + interpolation                           |
| `basics.lusa`             | values, operators, control flow                       |
| `closures.lusa`           | closures, captures, methods with `self`               |
| `stdlib.lusa`             | `string`, `table`, `math`, iterators                  |
| `ownership.lusa`          | `clone` / `borrow` / move in action                   |
| `ownership-error.lusa`    | E1001/E1002/E1004 error codes, commented              |
| `photocraft.lusa`         | mini photo-lab: 2D image in tables → filters → ASCII-art |

Also in the root: `bench.lusa` — reference workload for measuring speed
(debug × release). Typical measurement on this machine: **4.6× faster** on
the release turbo build.

```sh
cargo run -- examples/basics.lusa
```

## Semantics (summary)

- Lua-like syntax: `local`, `if/elseif/else/end`, `while`, `repeat/until`,
  numeric and generic `for`, `function`, `...`, tables with array+hash knit,
  multiple returns, closures, `~=` `..` `#` `:` `^` `//` `%`.
- **Interpolation**: `"\{expr}"` synthesizes text from string/number
  expressions; `\u{XXXX}` for unicode; `--` and `--[[ ... ]]` for comments.
- **Values**: `nil bool number string` are *Copy* (value semantics).
  `table function` are *heap* and move by default:
  - `local u = t` *moves* (t becomes a "moved-out" slot → uses raise **E1001**);
  - `local v = clone t` deep-copies (**independent**);
  - `local b = borrow t` creates an immutable alias; moving while aliased →
    **E1002**; writing through the immutable alias → **E1004**.
- **Methods**: `o:m()` passes the receiver as a *mutable borrow* —
  `function o:m() self.x = ... end` works naturally (mutating `self`).
- **stdlib**: `print type tostring tonumber error assert pairs ipairs`
  + `string` `table` `math` modules.
- **Errors** in cargo style (`error[E1001]: ...`, `note:` and `help:`),
  E-codes: lexer (E0001), parser (E0101), ownership (E1001–E1005),
  runtime types (E2001–E2008), `error`/`assert` (E9000/E9100).
  Full table in [`docs/LANGUAGE.md`](docs/LANGUAGE.md).

## Code architecture

```
src/
  tokens.rs  lexer.rs     # tokens and lexing (strings with recursive interpolation)
  ast.rs     parser.rs    # AST — Lua-like syntax tree
  value.rs                # values, tables, ownership/borrow registry
  scope.rs   interp.rs    # scopes, frames, execution, move rules
  eval.rs     builtins.rs # expression evaluation; stdlib
  error.rs    main.rs     # LusaError + cargo-style render; CLI/REPL
  lib.rs                  # public API (used by tests/integration.rs)
```

Pipeline: lexer → parser → AST → tree-walking interpreter
(`interp.rs` runs statements; `eval.rs` evaluates expressions).

## Roadmap

- Bytecode VM (today it is tree-walking)
- pcall / coroutines
- string patterns (gmatch/find with patterns, today literal-only)
- explicit `move`/`copy` labels and a complete borrow checker
- linter-style analysis-time errors (rustc-friendly)

## License

MIT.
