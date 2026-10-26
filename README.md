# Lusa

**Lusa** is a dynamic language inspired by Lua, written 100% in Rust (only
`std`, zero dependencies), with an optional twist of **Rust-style ownership**
— value moves, borrows and clones with **cargo-style diagnostics**. Module 8.5
adds a typed C11 backend (`u32`/`i32`/`i128`/`f32`/`bool`/`string`), RAII drops,
host-toolchain native builds, and `unsafe` volatile access for hardware
registers. Module 9 brings the same typed/`unsafe` surface to the interpreter
and bytecode VM (with safe simulated memory), plus exact `i128` integer
literals. Module 10
adds native keyed tables, colon methods, and `pairs`/`ipairs` lowering to the
C11 backend. Module 11 adds static structs, raw pointers, and kernel
intrinsics. Module 12 adds cooperative coroutines and protects native runtime
failures with checked `pcall` recovery and RAII cleanup. Module 13 adds one
Lua-style string pattern engine shared by the interpreter, bytecode VM, and C11
backend: `find`, `match`, `gmatch`, and `gsub` with bounded matching, static
pattern diagnostics, and RAII-tracked temporary strings. Module 14 completes
the v1.0.0 language with a compile-time borrow checker, explicit `move`
expressions, stronger aliasing diagnostics, and borrow support in the bytecode
VM. Module 15 turns structs into literals with defaults and deep RAII, adds
zero-overhead `match`, compile-time `import`/`mod` composition, and GNU-style
inline assembly. Module 16 adds fixed-size `[T; N]` arrays, zero-based
indexing, address-of (`&place`), and raw-pointer arithmetic for buffering and
AST work. Module 17 adds volatile MMIO access (`read_volatile`/
`write_volatile`), packed structs (`#[packed]`), and explicit alignment
(`#[repr(align(N))]`). Module 18 adds macro-style inline assembly
(`asm!("...")`) for privileged instructions, fences, and architecture-specific
hardware controls. Module 19 adds custom startup symbols (`#[entry]`),
freestanding `#[no_std]` programs without the hosted C runtime, and
`#[link_section = "..."]` placement for functions, globals, and promoted
top-level locals. Module 20 adds the global allocator contract
(`#[global_allocator]`), the `lusa_heap_alloc`/`lusa_heap_dealloc` dispatch, and
the first growable collections: `Vec<T>` with push/pop/indexing and `Box<T>`
owned pointers, both with RAII drops on hosted and freestanding targets.
Module 21 adds safe error handling: built-in `Result<T, E>` and `Option<T>`
sums, `Ok`/`Err`/`Some`/`None` constructors, pattern matching, combinators, and
the Rust-style `?` operator that lowers to C11 checks and early returns.

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
| `borrow_checker.lusa`     | static aliasing, method borrows, explicit `move`, unsafe escape |
| `typed_native.lusa`       | typed C11 build with RAII and native output           |
| `unsafe_hardware.lusa`    | simulated MMIO registers and volatile intrinsics      |
| `structs_pointers.lusa`   | static structs and raw pointer access                 |
| `error_recovery.lusa`     | `pcall`, error strings, and native cleanup            |
| `coroutines.lusa`         | create/resume/yield/status/close lifecycle            |
| `string_patterns.lusa`    | `find`/`match`/`gmatch`/`gsub` with captures and anchors |
| `struct_match.lusa`       | struct literals, nested deep clone, and `match` arms |
| `photocraft.lusa`         | mini photo-lab: 2D image in tables → filters → ASCII-art |
| `baremetal_boot.lusa`     | `#[no_std]` `_start` entry with sectioned code/data |
| `dynamic_heap.lusa`       | `Vec<T>` growth/pop/clear and `Box<T>` owned pointers |
| `error_handling.lusa`     | `Result`/`Option`, combinators, `?`, and `match` arms |

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
  - `local u = t` and `local u = move t` transfer ownership; later uses raise
    **E1001**;
  - `local v = clone t` deep-copies (**independent**);
  - `local b = borrow t` creates an immutable alias. Multiple immutable
    aliases are allowed; moving or mutating the owner while an alias is live →
    **E1002**; writing through the immutable alias → **E1004**.
- **Methods**: `o:m()` passes the receiver as a *mutable borrow* —
  `function o:m() self.x = ... end` works naturally. While the call evaluates
  its arguments, other reads, borrows, or moves of the receiver are rejected
  with **E1002**. The C11 front end performs the same analysis statically with
  Cargo-style spans and zero generated-code overhead.
- **Heap collections**: `Vec<T>` is a growable 0-based buffer with `push`,
  `pop`, `len`, `capacity`, `is_empty`, `reserve`, and `clear`; `Box<T>` is an
  owned pointer read and written with `*b`. Both own their contents and run
  RAII drops in every engine.
- **stdlib**: `print type tostring tonumber error assert pcall pairs ipairs`
  `io_read_file` `io_write_binary` `io_read_binary` + `string` `table` `math`
  `json` modules, and `coroutine.create/resume/yield/status/close/isyieldable`.
  `string` includes Lua-style `find`, `match`, `gmatch`, and `gsub` (see
  [String patterns](#string-patterns-module-13)).
- **Errors** in cargo style (`error[E1001]: ...`, `note:` and `help:`),
  E-codes: lexer (E0001), parser (E0101), ownership (E1001–E1005),
  runtime types, patterns, and I/O (E2001–E2011), `error`/`assert`
  (E9000/E9100). Native static checks add E0323/E0324 for statically
  evaluable patterns and replacement captures. Full table in
  [`docs/LANGUAGE.md`](docs/LANGUAGE.md).

## Code architecture

```
src/
  tokens.rs  lexer.rs     # tokens and lexing (strings with recursive interpolation)
  ast.rs     parser.rs    # AST — Lua-like syntax tree
  value.rs                # values, tables, coroutines, ownership registry
  coroutine.rs            # isolated stacks and cooperative resume/yield
  scope.rs   interp.rs    # scopes, frames, execution, move rules
  eval.rs     builtins.rs # expression evaluation; stdlib
  compiler.rs vm.rs       # AST → bytecode compiler; register-based VM
  pattern.rs              # bounded Lua-style matcher shared by interp + VM
  pattern_runtime.c       # C11 matcher appended to every native translation unit
  error.rs    main.rs     # LusaError + cargo-style render; CLI/REPL
  cbackend.rs             # embedded self-hosted C11 backend + host compiler discovery
  imports.rs              # compile-time `import`/`mod` expansion and deduplication
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
| `bool` | `bool` (`<stdbool.h>`) |
| `string` | `LusaString *` (`.data` is NUL-terminated for C/FFI) |
| `[T]` / `[[T]]` | `LusaTable *` with a homogeneous element kind |

Untyped integer literals infer `i32`; untyped decimal literals infer `f32`.
Unannotated locals keep this static native inference; the interpreter and
bytecode VM continue to represent every number as `f64`. The checker runs
before emission and aborts with Cargo-style diagnostics such as
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
  an owned value. The static drop list is never consumed while emitting
  declarations or error paths, so ordinary nested-scope exits clean up exactly
  once.
- A move clears the source slot and cancels its drop; `clone` deep-copies
  strings/tables; `borrow` emits a non-owning alias with no drop.
- Strings keep a length in `LusaObj` (embedded NUL bytes are valid) and a
  NUL-terminated `data` pointer for calls into C.

### Unsafe hardware access

```lusa
unsafe do
    local reg: i128 = unsafe_ptr_cast(0x40000000)
    unsafe_write_mem(reg, 1)
    local value: u32 = unsafe_read_mem(reg)
    print(value)
end
```

This lowers to `*(volatile uint32_t *)(uintptr_t)address = value` for writes and
`(*(volatile uint32_t *)(uintptr_t)address)` for reads in native builds - valid
C11 for GCC, Clang, TCC, and MSVC. The raw intrinsics are rejected outside an
`unsafe` block (`E0316`) by the C backend. The interpreter and `--vm` instead
use a safe simulated-memory map with `f64` cells, so fractional values
round-trip and the same script can be tested without touching hardware. Native
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

### Error recovery and coroutines (Module 12)

Dynamic `pcall` keeps its Lua-style result list: `true, ...` on success or
`false, "E-code: message"` on failure. Native `pcall` is a compiler-lowered,
local-initializer form with the same error string and deterministic cleanup:

```lusa
function fail_after_alloc(): i32
    unsafe do
        local buffer: *mut u32 = unsafe_alloc(64)
        error("failure after allocation")
    end
    return 0
end

local ok, err = pcall(fail_after_alloc)
print(ok, err)                 -- false	E9000: failure after allocation
print("still running")
```

Native `pcall` accepts `local ok, err = pcall(f, ...)` (result discarded) or
`local ok, result, err = pcall(f, ...)`, where `f` is a direct named function.
It records an allocation mark, fails without terminating the process, and
releases `unsafe_alloc` buffers created after the call began. Ordinary locals
are released by the same RAII drops used on a normal return. Uncaught errors
print `lusa runtime: <E-code>: <message>` and exit with status `1`.

Coroutines use isolated stacks in every engine. The dynamic engines support
general functions and multiple values; the native backend uses Windows fibers
or POSIX `ucontext` and supports a named zero-parameter `i32` entry function
with one `i32` payload per resume/yield:

```lusa
function worker(): i32
    local incoming: i32 = coroutine.yield(10)
    return incoming + 1
end

local co = coroutine.create(worker)
local ok1, value1 = coroutine.resume(co, 0)
print(ok1, value1)             -- true	10
local ok2, value2 = coroutine.resume(co, 41)
print(ok2, value2)             -- true	42
```

`coroutine.status`, `coroutine.isyieldable`, and `coroutine.close` complete the
lifecycle. Closing a suspended native coroutine resumes it through its normal
drop paths before freeing the stack. See
[`docs/LANGUAGE.md`](docs/LANGUAGE.md) section 9.6 for the exact native
subset and the deliberate differences between the native and dynamic
resume/error slots.

```sh
lusalang run examples/error_recovery.lusa
lusalang run examples/error_recovery.lusa --native
lusalang run examples/coroutines.lusa --vm
```

### String patterns (Module 13)

`string.find`, `string.match`, `string.gmatch`, and `string.gsub` share one
Lua-style pattern engine in every engine. The dynamic implementations use
`src/pattern.rs`; the native backend appends `src/pattern_runtime.c` to the
generated C11 translation unit. Patterns operate on Unicode scalar values, so
indices count characters rather than UTF-8 bytes. Matching is depth- and
step-bounded, which turns pathological backtracking into a catchable `E2011`
error instead of a hang.

```lusa
local line = "release=7 build=42"
local start_pos, end_pos = string.find(line, "%d+")
print(start_pos, end_pos)                 -- 9	9

local build = string.match(line, "build=(%d+)")
print(build)                              -- 42

local rewritten, count = string.gsub(line, "(%a+)=(%d+)", "%1:<%2>")
print(rewritten, count)                   -- release:<7> build:<42>	2

for word in string.gmatch(line, "%a+") do
    print(word)                           -- release, build
end
```

Native builds lower the builtins to direct runtime calls
(`lusa_string_find_start`, `lusa_string_find_values`, `lusa_string_match`,
`lusa_string_gsub`, `lusa_string_gmatch_new`, and
`lusa_string_gmatch_next`). `gmatch` iterators and capture strings are owned by
the enclosing loop scope and released on `break`, `return`, or scope exit.
Native `find` also supports `local start, finish = ...`, while
`local text, count = string.gsub(...)` binds both results.

Literal patterns and replacements are validated before C emission: malformed
patterns report `E0323`, and replacement captures that do not exist report
`E0324`. Dynamic pattern strings are validated at runtime and raise `E2010`,
which `pcall` can catch.

```sh
lusalang run examples/string_patterns.lusa
lusalang run examples/string_patterns.lusa --vm
lusalang run examples/string_patterns.lusa --native
```

### Static borrow checking and explicit move (Module 14)

The C11 front end runs a compile-time ownership pass over every typed function.
It tracks moved variables and live aliases without emitting any runtime borrow
machinery. The dynamic engines enforce the same rules through their ownership
registry, so diagnostics and E-codes agree:

```lusa
local t = { x = 1 }
do
    local a = borrow t       -- multiple immutable aliases are allowed
    local b = borrow t
    print(a.x, b.x)
end

local copy = clone t         -- deep copy; t stays alive
copy.x = 9

function t:bump(n: i32): i32
    self.x = self.x + n
    return self.x
end

t:bump(2)                    -- mutable receiver borrow
local u = move t             -- explicit transfer; t is now moved-out
print(u.x, copy.x)           -- 3	9
```

The checker rejects these cases before C emission:

```lusa
local t = { 1 }
local u = t
print(t)                     -- error[E1001]: use of moved-out value `t`

local t = { 1 }
local view = borrow t
local u = t                  -- error[E1002]: cannot move out of `t`
                             -- note: borrowed by `view`

local t = { x = 1 }
local view = borrow t
view.x = 2                   -- error[E1004]: write through immutable borrow
```

`move t` is now a real expression in the interpreter, bytecode VM, reference
compiler, and C11 backend. The VM supports `borrow` directly and releases aliases
at block/loop exit, including `break`. `unsafe do ... end` remains the explicit
escape hatch: raw pointer operations and writes through aliases bypass the
borrow rules while the surrounding safe code stays checked.

Borrow analysis is zero-cost: generated C carries no borrow metadata. RAII
cleanup remains a compile-time drop list, emitted in reverse declaration order
at normal scope exits, `return`/`break`, and checked error paths.

```sh
lusalang run examples/borrow_checker.lusa
lusalang run examples/borrow_checker.lusa --vm
lusalang run examples/borrow_checker.lusa --native
```

### Struct literals, match, and modules (Module 15)

Structs support both brace and bare declaration forms, literals with defaults,
nested struct and heap fields, and deep RAII:

```lusa
struct Point { x: i32, y: i32 }
struct Label
    origin: Point,
    text: string
end

local p = Point{x: 10}           -- y defaults to 0
local tag = Label{origin: p, text: "corner"}
tag.origin.x = 4
local copy = clone tag           -- Point is copied; string is deep-cloned
print(tag.origin.x, copy.text)   -- 4	corner
```

Each declaration emits a native C typedef with fields in declaration order.
In the dynamic engines, structs made only of scalar fields are Copy; structs
that own `string`/`table` fields move like tables. The native checker applies
the same ownership model. Generated `lusa_drop_<Name>` and
`lusa_clone_<Name>` helpers deep-release and deep-copy nested struct, string,
and table fields, so RAII stays exact.

`match` evaluates its value once and lowers to ordinary `if`/`elseif` code:

```lusa
match kind with
| 0 => print("zero")
| 1 => print("one")
| other => print("many", other)
end
```

Literal arms compare with `==`; a single name binds the value (moving it) and
`_` is the wildcard. The checker validates ownership separately in each arm,
and generated C contains no dispatcher or runtime arm table.

Imports are resolved before parsing and merged into one symbol table:

```lusa
import "geo/point.lusa"   -- relative to the importing file
mod counter               -- counter.lusa, counter/mod.lusa, or counter/main.lusa

local p = Point{x: 1, y: 2}
local c = bump(Counter{count: 4})
```

Expansion is recursive, relative to the importing file, and deduplicated by
canonical path, so a module included twice contributes its definitions once.
The same expansion runs for the interpreter, bytecode VM, native build, and
`--emit-c`.

Inline assembly is available through `asm!("...")` (and the
backward-compatible `unsafe_asm("...")` spelling) behind a portable macro:

```c
#define LUSA_ASM(text) ({ __asm__ __volatile__ (text : : ); })
```

```lusa
unsafe do
    asm!("wfe")
end
```

GCC and Clang emit the instruction; toolchains without inline assembly use a
no-op definition. See
[Inline assembly and hardware controls (Module 18)](#inline-assembly-and-hardware-controls-module-18)
for the full surface.

```sh
lusalang run examples/struct_match.lusa
lusalang run examples/struct_match.lusa --vm
lusalang run examples/struct_match.lusa --native
```

### Fixed arrays and address-of (Module 16)

Fixed arrays have a compile-time length and contiguous C11 storage. `[T; N]`
is distinct from the existing `[T]` (`table<T>`) spelling, and indexing is
zero-based:

```lusa
local samples: [i32; 4] = [10, 20, 30, 40]
samples[1] = samples[0] + samples[2]
print(samples[0], samples[3], #samples)   -- 10	40	4
```

Native declarations use real C declarators (`int32_t samples[4]`), including
inside structs (`LusaString *items[2]`). Arrays whose element type owns heap
storage (`string`, `table`, or a heap-bearing struct) get a generated RAII drop
loop on every scope exit, so elements are released exactly once. The dynamic
engines represent fixed arrays as zero-based tables and report `#a` from key
`0`, keeping the same index arithmetic.

`&place` produces a mutable raw pointer, and `*ptr` reads or writes through it.
Dereferences and pointer arithmetic remain gated by `unsafe`:

```lusa
local x: u32 = 5
local p: *mut u32 = &x
unsafe do
    *p = 9
    local q: *mut u32 = p + 2
    print(x, *q)
end
```

`&x`, `&obj.field`, `&a[i]`, and `&*p` are accepted. Native pointer offsets use
ordinary C pointer arithmetic, which scales by the element size. Native arrays
are not passed or returned by value and whole-array assignment is rejected; use
element assignments or a raw pointer. The dynamic engines simulate raw
addresses in a process-local cell map, so writes through a simulated pointer do
not alias the originating local (native code does).

```sh
lusalang run examples/arrays_pointers.lusa
lusalang run examples/arrays_pointers.lusa --vm
lusalang run examples/arrays_pointers.lusa --native
```

### Volatile MMIO and struct layout (Module 17)

Polling loops need loads that the optimizer cannot remove, and register blocks
need layouts that match the datasheet. `ptr.read_volatile()` and
`ptr.write_volatile(value)` (also `ptr:read_volatile()`, or the
`volatile_read(ptr)`/`volatile_write(ptr, value)` builtins) emit typed
`*(volatile T *)` accesses in C11. They are only available inside `unsafe`:

```lusa
unsafe do
    local status: *mut u32 = unsafe_alloc(16)
    status.write_volatile(0)
    while status.read_volatile() == 0 do
        -- device polling; volatile keeps the load in the loop
    end
end
```

The element type comes from the pointer (`*mut u32`, `*mut i32`, `*mut i128`,
`*mut f32`, or `*mut bool`). Heap elements such as `string`, tables, and
structs are rejected, so a volatile access can never bypass RAII. The
interpreter and VM keep simulating addresses safely; native code performs real
volatile loads and stores.

Struct declarations accept representation attributes:

```lusa
#[packed]
struct GpioConfig {
    mode: u32,
    pull: u32,
    speed: u32,
}

#[repr(align(16))]
struct DmaDescriptor {
    source: u32,
    destination: u32,
    control: u32,
}
```

`#[packed]` removes inter-field padding, and `#[repr(align(N))]` raises the
struct's alignment, where `N` is a power of two from 1 to 4096. The C11 backend
emits portable `LUSA_PACKED`, `LUSA_PACK_BEGIN`/`LUSA_PACK_END`, and
`LUSA_ALIGNAS(n)` macros covering GCC/Clang attributes, MSVC
`__declspec`/`#pragma pack`, and the C11 `_Alignas` fallback. Duplicate or
unsupported attributes and invalid alignments are rejected before code
generation.

Run the bare-metal example in every engine:

```sh
lusalang run examples/baremetal_gpio.lusa
lusalang run examples/baremetal_gpio.lusa --vm
lusalang run examples/baremetal_gpio.lusa --native
lusalang build examples/baremetal_gpio.lusa --emit-c
```

`#[packed]` and `#[repr(align(N))]` change native layout only; the dynamic
engines accept the same source and keep their fixed-field simulation.

### Inline assembly and hardware controls (Module 18)

`asm!("...")` emits a raw assembly statement and is only available inside
`unsafe`:

```lusa
unsafe do
    asm!("cli")            -- disable interrupts (ring 0)
    local status: u32 = 1
    asm!("nop; nop")       -- keep the sequence explicit
    asm!("sei")            -- enable interrupts
    print(status)
end
```

Both parsers desugar the macro syntax into an `asm("...")` call, so the
interpreter, bytecode VM, and C11 backend share one argument check and one
`unsafe` gate. Native builds lower the call to `LUSA_ASM("...")`:

```c
#define LUSA_ASM(text) ({ __asm__ __volatile__ (text : : ); })
```

- GCC and Clang receive the text as a volatile inline-assembly statement.
  MSVC and other toolchains keep the program buildable with a no-op definition.
- The interpreter and `--vm` treat assembly as a sandboxed no-op, so control
  flow and register simulation can be tested without executing instructions.
- Outside `unsafe`, the C backend reports `E0316` and the dynamic engines
  report `E0321`.
- The text must be a plain, non-empty string literal. Quotes, backslashes,
  and newlines are rejected with `E0101` before code generation.
- `unsafe_asm("...")` keeps working as an alias with the same lowering.

Privileged instructions such as `cli`/`sei` only execute at ring 0. The example
uses `nop` so it runs safely on a desktop:

```sh
lusalang run examples/baremetal_asm.lusa
lusalang run examples/baremetal_asm.lusa --vm
lusalang run examples/baremetal_asm.lusa --native
lusalang build examples/baremetal_asm.lusa --emit-c
```

`tests/module18.rs` covers the GNU volatile macro, multiple instructions,
`unsafe` gating in all three engines, malformed strings and arity, shared
lowering with `unsafe_asm`, control-flow execution in the dynamic engines, and
a native build-and-run.

### Bare-metal entry points and linker sections (Module 19)

`#[no_std]` switches the C11 backend to a freestanding translation unit: the
hosted runtime, `int main`, the coroutine runtime, the pattern matcher, and the
allocator are omitted, leaving only `<stdint.h>`, `<stdbool.h>`, `<stddef.h>`,
the assembly/section macros, checked integer helpers, and a minimal error flag.
It must be the first statement in the file. All top-level `local` declarations
are promoted to file-scope globals so the entry function and helpers can share
them.

`#[entry]` marks one parameterless top-level function as the exported startup
symbol. The function keeps its normal Lusa name (`_start` below); the generated
wrapper returns `void`, runs the top-level statements in
`static int lusa_program_body(void)` first, and then calls the function. The
only exception is a function literally named `main`: C requires `main` to
return `int`, so that wrapper is emitted as `int main(void)` and returns `0`
(`1` if the program body reports a failure), which keeps hosted native builds
portable to macOS Clang:

```lusa
#[no_std]

#[link_section = ".data.boot"]
local state: u32 = 0

#[link_section = ".text.boot"]
#[entry]
function _start() : void
    state = state + 1
    unsafe do
        asm!("nop")
    end
end
```

`#[link_section = "..."]` is accepted on top-level functions, top-level
`local` declarations, and top-level implicit globals. Names are validated as
1–255 ASCII bytes from `[A-Za-z0-9_.$-]` (`E0101`). Every placement emits a
comment plus a portable macro:

```c
/* #[link_section = ".text.boot"] -> __attribute__((section(".text.boot"))) */
LUSA_LINK_SECTION(".text.boot")
static void f1(LusaCoro *lusa_co) { ... }
```

GCC/Clang define `LUSA_LINK_SECTION(name)` as
`__attribute__((section(name)))`. MSVC has no GNU code-section attribute, so
the macro expands to nothing and the comment documents the intended lowering.
A sectioned top-level `local` is promoted to a file-scope global; if only some
names have initializers, the promotion is rejected with `E0325`.

```sh
lusalang run examples/baremetal_boot.lusa
lusalang run examples/baremetal_boot.lusa --vm
lusalang build examples/baremetal_boot.lusa --emit-c
```

The dynamic engines accept and ignore `#[no_std]` and run `#[entry]` as an
ordinary function, so the same source stays testable on a desktop. Freestanding
programs currently support scalars, structs, pointers, `asm!`, and control
flow; strings, tables, `print`, coroutine APIs, and `unsafe_alloc` still need
the hosted runtime. `tests/module19.rs` covers the freestanding preamble, entry
ordering and `void` return, section lowering, local promotion, parser and
semantic diagnostics, dynamic parity, and native hosted/no_std builds.

### Heap allocation and dynamic collections (Module 20)

`#[global_allocator]` plugs a custom heap into the C11 backend. The attribute
marks either half of the contract:

```lusa
#[global_allocator]
function my_alloc(size: u32, align: u32): *mut u32
    unsafe do
        return unsafe_alloc(size)
    end
end

#[global_allocator]
function my_dealloc(ptr: *mut u32, size: u32, align: u32): void
    -- release or recycle the block
end
```

The allocator must return `*mut u32`; the deallocator receives the original
size and alignment. Hosted programs use `malloc`/`free` when no allocator is
provided; `#[no_std]` programs use a static 64 KiB bump arena instead. This is
what lets `Vec<T>` and `Box<T>` work on bare metal without libc.

`Vec<T>` is a growable 0-based buffer. Literals use the array syntax and grow
geometrically (4, 8, 16, ...):

```lusa
local values: Vec<i32> = [4, 8, 15]
values:push(16)
print(values:len(), values:capacity())   -- 4 8
print(values[0], values:pop())           -- 4 16
values:reserve(32)
values:clear()
print(values:len(), values:is_empty())   -- 0 true
```

`Box<T>` is an owned pointer. It can be initialized implicitly from a value or
explicitly with `Box(value)`; `*b` reads and writes the pointee without the raw
pointer `unsafe` gate:

```lusa
local answer: Box<i32> = 41
*answer = *answer + 1
print(*answer)                            -- 42

local message = Box("automatic")
print(*message)
```

Owned elements (`Vec<string>`, `Box<string>`, nested structs) are released by
the generated per-element drop helpers at scope exit, on `clear`, and when the
program ends. `Vec<T>` values cannot yet be passed or returned by value; use a
raw pointer for that boundary (`E0312`).

```sh
lusalang run examples/dynamic_heap.lusa
lusalang run examples/dynamic_heap.lusa --vm
lusalang run examples/dynamic_heap.lusa --native
lusalang build examples/dynamic_heap.lusa --emit-c
```

`tests/module20.rs` covers heap growth, custom allocator dispatch, freestanding
arena emission, `Vec<string>`/`Box<string>` RAII, allocator diagnostics, and
native + dynamic parity.

### Safe error handling with `Result`, `Option`, and `?` (Module 21)

`Result<T, E>` and `Option<T>` are built-in sum types. Construct values with
`Ok(value)`, `Err(error)`, `Some(value)`, and the bare `None` (also callable as
`None()`):

```lusa
function parse(n: i32): Result<i32,string>
    if n < 0 then
        return Err("negative")
    end
    return Ok(n * 2)
end

function maybe(n: i32): Option<i32>
    if n == 0 then
        return None
    end
    return Some(n)
end
```

The `?` operator propagates the failure variant and evaluates to the success
payload. It only works inside a function returning a matching sum kind, and the
propagated error type must be assignable to the function's error type:

```lusa
function double(n: i32): Result<i32,string>
    local value: i32 = parse(n)?
    return Ok(value + 1)
end

function plus_ten(n: i32): Option<i32>
    local value: i32 = maybe(n)?
    return Some(value + 10)
end

print(double(3):unwrap())        -- 7
print(double(-1):unwrap_err())   -- negative
print(plus_ten(5):unwrap())      -- 15
print(plus_ten(0):is_none())     -- true
```

Match patterns bind payloads without moving the scrutinee:

```lusa
match parse(-2) with
| Ok(value) => print("ok", value)
| Err(error) => print("error", error)
end

match maybe(0) with
| Some(value) => print("some", value)
| None => print("none")
end
```

Combinators are methods on both sums. Wrong-variant use is a catchable
`E9999` runtime error:

| type     | methods                                                |
|----------|--------------------------------------------------------|
| `Result` | `is_ok`, `is_err`, `unwrap`, `unwrap_err`, `unwrap_or`, `expect` |
| `Option` | `is_some`, `is_none`, `unwrap`, `unwrap_or`, `expect`  |

```lusa
print(Ok(5):unwrap_or(9), Err("bad"):unwrap_or(9))  -- 5 9
print(Some(2):unwrap_or(0), None:unwrap_or(0))      -- 2 0
print(Ok(7):expect("ok"), Some(8):expect("some"))   -- 7 8
print(Ok(5))      -- Ok(5)
print(Err("x"))   -- Err(x)
print(Some("hi")) -- Some(hi)
print(None)       -- None
```

The dynamic engines represent sums as tagged tables and clone payloads out of
the receiver; `?` is non-consuming there. The C11 backend uses
`LusaResult { bool is_ok; void *ok; void *err; }` and
`LusaOption { bool is_some; void *value; }` with heap-boxed payloads, generated
per-type clone/drop/take helpers, and early return checks. Sum parameters are
owned by the callee; arguments are moved or cloned at the call site. `Vec<T>`,
`Box<T>`, structs, strings, and nested sums can be payloads; fixed arrays are
rejected (`E0301`). In `#[no_std]` builds, sums keep their scalar and struct
payloads through the static arena, but `string` payloads and the `print`
helpers require the hosted runtime.

```sh
lusalang run examples/error_handling.lusa
lusalang run examples/error_handling.lusa --vm
lusalang run examples/error_handling.lusa --native
lusalang build examples/error_handling.lusa --emit-c
```

`tests/module21.rs` covers constructors, combinators, `?` through branches and
loops, struct/`Vec`/`Box`/nested payloads, sum parameters and fields, `match`
binding and returns, freestanding scalar sums, typed diagnostics, runtime
wrong-variant errors, and native + dynamic parity.

### Host compiler discovery

`--cc` → `LUSA_CC` → `CC` → `cc`/`gcc`/`clang`/`tcc`/`cl` on `PATH` → Visual
Studio via `vswhere.exe` (and runs `vcvars64.bat` when needed). GCC/Clang/TCC
use `-std=c11 -O2`; MSVC uses `/nologo /TC /O2 /std:c11`. If no compiler is
found, the interpreter and bytecode VM still work; only `build`/`run --native`
require a C toolchain.

Runnable examples: `examples/typed_native.lusa` (build/run),
`examples/baremetal_gpio.lusa` (volatile polling with packed/aligned register
layouts; uses `unsafe_alloc` so it runs on a desktop),
`examples/baremetal_asm.lusa` (inline assembly fences and a `nop` polling
loop), `examples/baremetal_boot.lusa` (freestanding `_start` with sectioned
code/data), and `examples/unsafe_hardware.lusa` (simulated memory in
`run`/`--vm`; replace the illustrative MMIO address before native execution).

`tests/c_backend.rs` covers typed mapping, functions/tables, moves/clone/drops,
Cargo-style diagnostics, unsafe volatile emission, and a native build/run test
that skips when no compiler is installed. `tests/module12.rs` adds native
`pcall` recovery, allocation marks, uncaught-error exits, coroutine lifecycle,
nested yields, and dynamic parity. `tests/module15.rs` adds both struct
declaration forms, literals/defaults/nested deep clone, `match` arms and
per-arm ownership, recursive import/module deduplication, and inline assembly.
`tests/module16.rs` adds fixed-array indexing, C declarators, array drop loops,
address-of/pointer arithmetic lowering, `unsafe` gating, and a native
build-and-run check. `tests/module17.rs` adds typed volatile lowering for every
method spelling, scalar-element restrictions, value type checking,
packed/aligned C layout macros, attribute validation, dynamic parity, and a
native volatile/packed/aligned build-and-run. `tests/module18.rs` adds
`asm!("...")` emission, multiple instructions, `unsafe` gating, malformed
input diagnostics, shared lowering with `unsafe_asm`, dynamic control-flow
parity, and a native build-and-run. `tests/module19.rs` adds freestanding
`#[no_std]` emission, entry-wrapper ordering, section placement, promoted
locals, `E0101`/`E0325` diagnostics, dynamic parity, and native hosted and
no_std build-and-run checks. `tests/module20.rs` adds `Vec<T>` growth and
method semantics, `Box<T>` dereference/write, `#[global_allocator]` dispatch,
freestanding arena emission, owned-element drops, allocator diagnostics, and
three-engine parity. `tests/module21.rs` adds `Result`/`Option` constructors and
combinators, `?` in branches and loops, rich payloads, sum parameters and
fields, `match` binding, freestanding scalar sums, typed diagnostics, runtime
wrong-variant errors, and native + dynamic parity. See
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

The detailed engineering plan lives in [docs/ROADMAP.md](docs/ROADMAP.md).

Near-term milestones:

- **v0.7.0 (shipped):** static `struct` declarations, raw pointer types with
  arithmetic and volatile dereferences inside `unsafe`, kernel intrinsics
  (`unsafe_nop`, `unsafe_asm`, `unsafe_alloc`), and safe sandbox emulation in
  the interpreter and bytecode VM.
- **v0.8.0 (shipped):** native `pcall` with checked error propagation and RAII
  cleanup, plus cooperative coroutines in the interpreter, VM, and native
  backend.
- **v0.9.0 (shipped):** Lua-style string patterns (`find`, `match`, `gmatch`,
  `gsub`) backed by one bounded matcher shared across engines, with native
  static pattern diagnostics and RAII-tracked temporary strings.
- **v1.0.0 (shipped):** compile-time borrow checking, explicit `move`,
  `clone` deep copies, VM `borrow` support, golden diagnostics, and language
  stabilization.
- **v1.1.0 (shipped):** struct literals with defaults and deep RAII,
  zero-overhead `match`, compile-time `import`/`mod` modules, and GNU-style
  inline assembly.
- **v1.2.0 (shipped):** fixed-size `[T; N]` arrays with zero-based indexing,
  address-of (`&place`), and raw-pointer arithmetic for direct buffer work.
- **v1.3.0 (shipped):** typed volatile MMIO loads/stores (`read_volatile`/
  `write_volatile`), packed structs (`#[packed]`), and explicit alignment
  (`#[repr(align(N))]`) for hardware register layouts.
- **v1.4.0 (shipped):** `asm!("...")` inline assembly inside `unsafe`, lowered
  to GCC/Clang volatile statements, with a shared no-op path in the
  interpreter and bytecode VM.
- **v1.5.0 (shipped):** custom `#[entry]` startup symbols, freestanding
  `#[no_std]` translation units, and `#[link_section = "..."]` placement for
  functions, globals, and promoted top-level locals.
- **v1.6.0 (shipped):** `#[global_allocator]` heap dispatch, freestanding bump
  arena fallback, and RAII-managed `Vec<T>`/`Box<T>` collections usable on
  hosted and bare-metal targets.
- **v1.7.0 (shipped):** built-in `Result<T, E>`/`Option<T>` sums, combinators,
  `match` patterns, and the `?` early-return operator, with C11 checks and
  freestanding scalar payload support.

Also planned: expand bytecode VM coverage (`borrow` is interpreter-only
today), make the VM the default engine, and complete native self-hosting of
the bootstrap compiler (keyed tables, nested fields, heap `clone`, and generic
`for` are already native).

## License

MIT.
