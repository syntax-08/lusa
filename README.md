# Lusa

**Lusa** is a dynamic language inspired by Lua, written 100% in Rust (only
`std`, zero dependencies), with an optional twist of **Rust-style ownership**
— value moves, borrows and clones with **cargo-style diagnostics**. Module 8.5
adds a typed C11 backend (`u32`/`i32`/`i128`/`f32`), RAII drops, host-toolchain
native builds, and `unsafe` volatile access for hardware registers. Module 9
brings the same typed/`unsafe` surface to the interpreter and bytecode VM
(with safe simulated memory), plus exact `i128` integer literals. Module 10
adds native keyed tables, colon methods, and `pairs`/`ipairs` lowering to the
C11 backend.

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
| `lusalang run main.lusa`        | runs a program in the interpreter          |
| `lusalang run main.lusa --native` | builds/runs through the C11 backend       |
| `lusalang build main.lusa`      | compiles a native executable via C11        |
| `lusalang check main.lusa`      | validates (lex+parse) without running      |
| `lusalang check main.lusa --typed` | runs the static type checker            |
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
cargo test --all-targets  # unit + integration + self-hosted + C11 backend tests
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
  `io_write_binary` `io_read_binary` + `string` `table` `math` `json` modules.
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
  cbackend.rs             # embedded self-hosted C11 backend + host compiler discovery
  lib.rs                  # public API (used by tests/integration.rs)
lexer.lusa                # self-hosted lexer written in Lusa
parser.lusa               # self-hosted recursive-descent parser (typed subset)
compiler.lusa             # self-hosted bytecode compiler
compiler_bootstrap.lusa   # Module 8.5 optimizer + checker + C11 emitter
docs/SELF_HOSTING.md      # self-hosted front-end and C11 backend reference
docs/RELEASE_NOTES.md     # release-notes structure enforced by CI
```

Pipeline:

```text
lexer → parser → AST → tree-walking interpreter (default)
                    ├→ compiler → bytecode chunks → VM (`--vm`)
                    └→ optimizer → static checker → C11 emitter
                                           → host GCC/Clang/TCC/MSVC
                                           → native executable
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

## Module 8.5: typed C11 backend (`compiler_bootstrap.lusa`)

The native path no longer writes x86_64 instructions or PE/ELF headers. It
transpiles the checked AST to portable C11 and invokes the host C compiler:

```sh
lusalang build hello.lusa                 # native executable
lusalang build hello.lusa --emit-c        # intermediate hello.c
lusalang run hello.lusa --native          # build + run
lusalang check hello.lusa --typed         # static check only
```

```lua
local ast = Parser.parse(Lexer.lex(source))
local c_source = Compiler.emit_c(borrow ast)   -- borrow keeps the AST readable
local result = Compiler.bootstrap(Lexer, Parser, source)
```

### Typed subset

`local x: u32 = 10` and `function read(pin: u32): f32` are accepted by the
self-hosted parser. Supported types map to C11 as follows:

| Lusa | C11 |
|------|-----|
| `u32` | `uint32_t` |
| `i32` | `int32_t` |
| `i128` | `lusa_i128` (`__int128`, or `long long` on MSVC) |
| `f32` | `float` |
| `[T]` / `[[T]]` | `LusaTable *` with a homogeneous element kind |

Untyped integer literals infer `i32`; untyped decimal literals infer `f32`.
The checker runs before emission and aborts with Cargo-style diagnostics such as
`error[E0308]: mismatched types ... --> lusa:1:16`.

Pure integer tokens keep their exact spelling. A literal such as
`123456789012345678901234567890` is emitted as
`lusa_i128_parse("123456789012345678901234567890")`, preserving all 128 bits
instead of passing through `f64`. GCC/Clang provide full `__int128`; MSVC's
fallback is 64-bit.

### Heap and RAII

`Compiler.memory_layout()` is the ABI source of truth:

- `LusaString`/`LusaTable` begin with a 32-byte `LusaObj` header
  (`type`, `length`, `flags`, `size`).
- Allocation uses `malloc`/`calloc`/`realloc`; release uses `free`.
- Owned locals and globals are deep-dropped in reverse declaration order at
  lexical exits, before `return`/`break`, and before an assignment overwrites
  an owned value.
- A move clears the source slot and cancels its drop; `clone` deep-copies
  strings/tables; `borrow` emits a non-owning alias with no drop.

### Unsafe hardware access

```lusa
unsafe do
    local reg: i128 = unsafe_ptr_cast(0x40000000)
    unsafe_write_mem(reg, 1)
    local value: u32 = unsafe_read_mem(reg)
    print(value)
end
```

This lowers to `*(volatile uint32_t *)(uintptr_t)address = value` in native
builds. The raw intrinsics are rejected outside an `unsafe` block (`E0316`) by
the C backend. The interpreter and `--vm` instead use a safe simulated-memory
map, so the same script can be tested without touching hardware. Native
execution requires valid addresses; the backend deliberately does no memory
sandboxing or bounds checks.

### Keyed tables, methods, and iterators

The C11 backend now supports heterogeneous string-keyed tables through a native
open-addressing map:

```lusa
local user = { id = 1, name = "Lusa", profile = { city = "Oslo" } }
print(user.id, user.name, user.profile.city)
user.name = "New"
user.profile.city = "Bergen"
```

Colon methods compile to static C functions with a non-owning `self` receiver:

```lusa
function user:rename(next_name: string): void
    self.name = next_name
end
user:rename("New")
```

Generic `for` lowers both keyed and array iteration:

```lusa
for key, value in pairs(user) do
    print(key, value)
end

for index, value in ipairs({10, 20, 30}) do
    print(index, value)
end
```

RAII scopes are entered per iteration, so temporary strings/tables allocated in
the loop body are dropped before the next iteration.

### Host compiler discovery

`--cc` → `LUSA_CC` → `CC` → `cc`/`gcc`/`clang`/`tcc`/`cl` on `PATH` → Visual
Studio via `vswhere.exe` (and runs `vcvars64.bat` when needed). GCC/Clang/TCC
use `-std=c11 -O2`; MSVC uses `/nologo /TC /O2 /std:c11`. If no compiler is
found, the interpreter and bytecode VM still work; only `build`/`run --native`
require a C toolchain.

Runnable examples: `examples/typed_native.lusa` (build/run) and
`examples/unsafe_hardware.lusa` (simulated memory in `run`/`--vm`; replace the
illustrative MMIO address before native execution).

`tests/c_backend.rs` covers typed mapping, functions/tables, moves/clone/drops,
Cargo-style diagnostics, unsafe volatile emission, and a native build/run test
that skips when no compiler is installed. See
[`docs/SELF_HOSTING.md`](docs/SELF_HOSTING.md) and
[`docs/LANGUAGE.md`](docs/LANGUAGE.md) section 9 for details.

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
- full native self-hosting: keyed/heterogeneous tables, field/index
  assignment, heap `clone`, closures/functions, and numeric/generic `for`
  parser coverage, then self-compilation of the bootstrap module (the native
  heap runtime and RAII drops are already in place)
- linter-style analysis-time errors (rustc-friendly)

## License

MIT.
