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
cargo test              # 59 tests (6 unit + 51 integration + 2 self-hosted)
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
- **stdlib**: `print type tostring tonumber error assert pairs ipairs io_read_file`
  + `string` `table` `math` modules.
- **Errors** in cargo style (`error[E1001]: ...`, `note:` and `help:`),
  E-codes: lexer (E0001), parser (E0101), ownership (E1001–E1005),
  runtime types and I/O (E2001–E2009), `error`/`assert` (E9000/E9100).
  Full table in [`docs/LANGUAGE.md`](docs/LANGUAGE.md).

## Code architecture

```
src/
  tokens.rs  lexer.rs     # tokens and lexing (strings with recursive interpolation)
  ast.rs     parser.rs    # AST — Lua-like syntax tree
  value.rs                # values, tables, ownership/borrow registry
  scope.rs   interp.rs    # scopes, frames, execution, move rules
  eval.rs     builtins.rs # expression evaluation; stdlib
  compiler.rs vm.rs       # AST → bytecode compiler; register-based VM
  error.rs    main.rs     # LusaError + cargo-style render; CLI/REPL
  lib.rs                  # public API (used by tests/integration.rs)
lexer.lusa                # self-hosted lexer written in Lusa
parser.lusa               # self-hosted recursive-descent parser
compiler.lusa             # self-hosted bytecode compiler
docs/SELF_HOSTING.md      # self-hosted front-end reference
```

Pipeline:

```text
lexer → parser → AST → tree-walking interpreter (default)
                    └→ compiler → bytecode chunks → VM (`--vm`)
```

## Self-hosted lexer (`lexer.lusa`)

The first compiler stage also has a complete implementation in Lusa itself,
at the repository root. It reads a source string and returns an owned table of
tokens:

```lua
local Lexer = require("lexer")
local tokens = Lexer.lex("local x = 0x1F + 1")
-- tokens[1] = {kind = "keyword", value = "local", line = 1, column = 1, ...}
```

Install it as a package (`packages/lexer/init.lusa`) or load the file through
the host API. Each token carries `kind`, `value`, `line`, `column`, `start`
and `finish`; strings additionally carry `text`, `quote` and `interpolated`.

Supported today:

- names, keywords (including the reserved `move`), decimal/hex/exponent
  numbers, basic and compound operators, punctuation;
- single- and double-quoted strings with Lusa escapes, `\u{...}` validation,
  and balanced `\{...}` interpolation (including nested braces, strings and
  comments);
- line and block comments, UTF-8 names, BOM handling, and 1-based line/column
  tracking through strings and comments;
- `return move(tokens)` for ownership transfer, and `borrow self` in read-only
  helpers. The bytecode VM lacks `borrow`, so the lexer probes once and selects
  a direct-read fallback there; both engines produce identical tokens.

Lexical failures raise `E9000` with a `lexer:line:column:` prefix. The Rust
lexer still owns `E0001` diagnostics for programs parsed by the native engine.

## Self-hosted parser and compiler

`parser.lusa` turns the lexer's token table into a nested-table AST, and
`compiler.lusa` lowers that AST to a portable numeric bytecode image. Both use
module-level state (single-threaded, non-reentrant) and are tested on the
interpreter and `--vm`:

```lua
local Lexer = require("lexer")
local Parser = require("parser")
local Compiler = require("compiler")

local ast = Parser.parse(Lexer.lex("local a = 1 + 2 return a"))
local image = Compiler.compile(clone ast)
-- image = {version=1, code={...}, constants={...}, lines={...}, nlocals=1}
```

The parser consumes the token table by value and only reads scalar token fields.
The compiler consumes its AST argument; pass `clone ast` when the AST must stay
available. The reference subset covers locals, name assignment, calls,
`if`/`elseif`/`else`, `while`, `repeat`, `do`, `return`, `break`, literals,
names, parentheses, unary/binary operators, `and`/`or`, `clone` and `borrow`.
Tables, fields/indexes/methods, function definitions, numeric/generic `for`,
varargs and interpolated-string compilation are extension points and report
explicit errors.

The image uses native `Instr` numbers with big-endian `u16` operands,
zero-based constant/local slots, and absolute 1-based byte-offset jumps. See
[`docs/SELF_HOSTING.md`](docs/SELF_HOSTING.md) for the AST schema, full opcode
table, exact byte example, ownership rules and test coverage.

## Bytecode VM: registers and source lines

`compiler.rs` resolves every local binding to a numeric register at compile
time. `GetLoc`/`SetLoc` carry that slot index, and the VM executes them as
direct frame-slot indexing — there is no name lookup on the hot path. Registers
are allocated from a scope-aware free list, so sibling blocks reuse slots and
`nlocals` is a high-water mark rather than a declaration count (`bench.lusa`
goes from 13 to 7). Captured locals are promoted to shared cells and detached
with `ResetSlot` before reuse, so closures from different loop iterations do
not alias.

Every instruction also records its original source line and local binding name
in side tables parallel to the instruction stream. This is cold metadata: the
dispatch loop never reads it. Only ownership/borrow errors call
`Chunk::span_at`/`Chunk::local_name_at`, which lets the VM report the exact
source line even though registers are reused:

```text
error[E1001]: use of moved-out value `t`
 --> moved.lusa:3:1
  |
3 | print(t)
  | -------^
```

Enable the bytecode engine with `--vm`:

```sh
lusalang run main.lusa --vm
```

## Roadmap

- Expand bytecode VM coverage (`borrow` is interpreter-only today) and make it
  the default engine
- pcall / coroutines
- string patterns (gmatch/find with patterns, today literal-only)
- explicit `move`/`copy` labels and a complete borrow checker
- linter-style analysis-time errors (rustc-friendly)

## License

MIT.
