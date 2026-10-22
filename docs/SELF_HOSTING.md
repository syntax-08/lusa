# Self-hosted compiler and C11 backend

The repository contains two self-hosted front ends:

```text
source text
    │
    ├── lexer.lusa ──► flat token table
    │       └── parser.lusa ──► nested-table AST
    │               ├── compiler.lusa ──► bytecode image (interpreter/VM)
    │               └── compiler_bootstrap.lusa ──► type checking + C11
    └── src/*.rs ──► the Rust interpreter/VM used to run the self-hosted code
```

`lusalang build` embeds `lexer.lusa`, `parser.lusa`, and
`compiler_bootstrap.lusa` in the Rust binary, runs the self-hosted pipeline
through the interpreter, and invokes the host C compiler. `compiler.lusa` and
the Rust interpreter/VM remain available for `lusalang run` and
`lusalang run --vm`.

## Front-end modules

Each module exposes one table:

```lua
local Lexer = require("lexer")
local Parser = require("parser")
local Compiler = require("compiler")
local CBootstrap = require("compiler_bootstrap")

local tokens = Lexer.lex(source)
local ast = Parser.parse(tokens)
local image = Compiler.compile(clone ast)          -- bytecode (VM)
local c_source = CBootstrap.emit_c(clone ast)      -- C11 backend
```

The parser consumes the token table by value and returns a freshly built
`Program`. `compiler.lusa` consumes its AST. The C backend snapshots the AST
before optimizing so a caller can retain its own copy.

### Supported statements and expressions

Statements: `local` (with optional `: type`), assignment, call statements,
`if`/`elseif`/`else`, `while`, `repeat`, `do`, `unsafe do`, `function`,
`return`, and `break`.

Expressions: `nil`, booleans, decimal and hexadecimal numbers, strings,
names, calls, parentheses, unary `-`/`not`/`#`, arithmetic/concatenation/
comparison operators, `and`/`or`, `clone`, `borrow`, array table constructors,
and one-based index reads (`t[1]`, nested `t[1][2]`).

The reference parser intentionally rejects keyed constructors (`{k = v}`),
field/method access, assignment through a field/index, numeric/generic `for`,
and varargs.

### AST schema

Every node carries `tag`, `line`, and `column`.

| node | fields |
|------|--------|
| `Program` | `body = { stmt... }` |
| `Local` | `names`, `types`, `values` |
| `Assign` | `targets = { Name... }`, `values` |
| `Function` | `name`, `params = { {name,type}... }`, `return_type`, `body` |
| `CallStmt` | `value = Call` |
| `If` | `branches = { {cond, body}... }`, `else_body` |
| `While` | `cond`, `body` |
| `Repeat` | `body`, `cond` |
| `Do` | `body` |
| `Unsafe` | `body` (raw-pointer block) |
| `Return` | `values` |
| `Break` | no extra fields |
| `Nil` / `Bool` / `Number` / `String` | literal payload |
| `Name` | `name` |
| `Paren` | `value` |
| `Unary` | `op`, `value` |
| `Binary` / `Logical` | `op`, `lhs`, `rhs` |
| `Call` | `callee`, `args` |
| `Table` | `items` (array entries only) |
| `Index` | `table`, `index` |
| `Clone` / `Borrow` | `value` |

## Ownership and module state

- `Parser.parse` moves the token table into module state, reads scalar token
  fields, and returns a fresh AST.
- `compiler.lusa` consumes the AST by value; pass `clone ast` to keep it.
- `compiler_bootstrap.lusa` snapshots `borrow_ast.body` before optimizing, so
  `Compiler.emit_c(borrow ast)` leaves the caller's AST readable.
- The interpreter enforces ownership on the self-hosted code itself. Helpers
  that inspect AST nodes use `borrow`, and helpers that transform a node return
  it so the caller can rebind it.
- Scope stacks use depth counters rather than trailing `nil` writes because
  Lusa's `#table` does not shrink in that case.

## Module 8.5: typed C11 backend

### Pipeline

```text
source
  │ lexer.lusa
  ▼
tokens
  │ parser.lusa
  ▼
AST
  │ optimizer (constant folding, dead-code pruning)
  ▼
optimized AST
  │ static checker (annotates ctype/own/borrowed/moved)
  ▼
checked AST
  │ C emitter
  ▼
program.c ──► host GCC / Clang / TCC / MSVC ──► native executable
```

No PE/ELF headers or x86_64 instructions are emitted by the compiler. The C
file is the portable intermediate and can be inspected with `--emit-c`.

### Public API

| function | ownership contract | result |
|----------|--------------------|--------|
| `Compiler.optimize(move_ast)` | consumes a `Program` | optimized `Program` |
| `Compiler.check(borrow_ast)` | snapshots the AST | `{ok, functions, globals}` |
| `Compiler.emit_c(borrow_ast)` | snapshots the AST | C11 source string |
| `Compiler.bootstrap(lexer, parser, source)` | consumes front-end handles | `{target, source_bytes, c_source, c_bytes}` |
| `Compiler.write_c(path, c_source)` | reads the C string | writes a file |
| `Compiler.verify_fixpoint(lexer, parser, source)` | clones handles for each run | `{equal, first, second}` |
| `Compiler.self_compile(lexer, parser, path)` | consumes handles and reads `path` | success/error descriptor |
| `Compiler.memory_layout()` | none | ABI/schema description |

`Compiler.Version = 2` and `Compiler.Targets = { c = "c11" }`.

### Static types

| Lusa annotation | C11 type |
|-----------------|----------|
| `u32` | `uint32_t` |
| `i32` | `int32_t` |
| `i128` | `lusa_i128` (`__int128`, or `long long` under MSVC) |
| `f32` | `float` |
| `bool` | `bool` (`<stdbool.h>`) |
| `string` | `LusaString *` |
| `[T]`, `[[T]]` | `LusaTable *` with a homogeneous element kind |

Untyped integer literals infer `i32`; untyped decimal literals infer `f32`.
The checker validates locals, assignments, function arguments/returns,
conditions, table elements, and operator operands. Errors use Cargo-style
`error[E....]` diagnostics with `--> lusa:line:column` and expected/found notes.
The checker annotates the optimized AST with `ctype`, `own`, `borrowed`, and
`moved` so the emitter never guesses.

### Unsafe blocks and raw pointers

`unsafe do ... end` is a lexical block that disables the checker's
borrow-assignment restriction locally while preserving ordinary RAII drops for
owned values declared inside it.

```lusa
unsafe do
    local reg: i128 = unsafe_ptr_cast(0x40000000)
    unsafe_write_mem(reg, 1)
    local value: u32 = unsafe_read_mem(reg)
    print(value)
end
```

Generated C:

```c
lusa_i128 v = ((uintptr_t)(0x40000000));
(*(volatile uint32_t *)(uintptr_t)(v)) = (uint32_t)(1);
uint32_t read = (*(volatile uint32_t *)(uintptr_t)(v));
```

Intrinsics (`unsafe_ptr_cast`, `unsafe_read_mem`, `unsafe_write_mem`) are
rejected outside an unsafe block with `E0316`. They perform no bounds checks or
memory sandboxing: the caller is responsible for using valid hardware or
process addresses.

### C runtime and heap ABI

`Compiler.memory_layout()` describes the generated runtime:

```c
typedef struct LusaObj {
    uint64_t type;      /* 1 = string, 2 = table */
    uint64_t length;    /* bytes or element count */
    uint64_t flags;     /* bit 0 = owned */
    uint64_t size;      /* allocation size */
} LusaObj;

typedef struct LusaString { LusaObj obj; char *data; } LusaString;
typedef struct LusaTable {
    LusaObj obj;
    uint64_t elem_kind;
    uint64_t capacity;
    uint64_t *items;
} LusaTable;
```

- Allocation uses `malloc`/`calloc`/`realloc`; release uses `free`.
- Table access is one-based in Lusa and lowered to zero-based C indices.
- `lusa_drop_string`/`lusa_drop_table` check the owned flag and recursively
  release string/table elements before `free`.
- Owned locals/globals are dropped in reverse declaration order at every
  lexical exit, before `return`/`break`, and before an assignment overwrites an
  owned value.
- A move stores the pointer in the destination, clears the source slot
  (`= NULL`), and cancels the source drop.
- `clone` deep-copies strings and tables (`lusa_string_clone`,
  `lusa_table_clone`); scalar clone is a copy.
- `borrow` emits an alias with no allocation and no drop.

### Diagnostics

The checker raises messages such as:

```text
error[E0308]: mismatched types in local `x`
  --> lusa:1:16
   = expected `u32`
   = found    `string`
```

`src/cbackend.rs` unwraps the interpreter's `E9000` wrapper so the user sees
the original Cargo-style diagnostic. Lexer/parser failures are normalized to
`E0001`/`E0101` with a source location.

### Host toolchain discovery

`src/cbackend.rs` searches, in order:

1. `--cc <path>`
2. `LUSA_CC`
3. `CC`
4. `cc`, `gcc`, `clang`, `tcc`, `cl` on `PATH`
5. Visual Studio installations found through `vswhere.exe` and
   `VC\Tools\MSVC\*\bin\Hostx64\x64\cl.exe`

GCC/Clang/TCC use `-std=c11 -O2`; MSVC uses `/nologo /TC /O2 /std:c11` and
falls back if that standard switch is not supported. When `cl.exe` is found
outside a Developer Command Prompt, the wrapper calls the matching
`vcvars64.bat`/`vcvarsall.bat x64` first so `INCLUDE` and `LIB` are available.
Linux GCC/Clang builds link `-lm`; TCC/Windows retries without it when
necessary.

### Tests

`tests/c_backend.rs` covers:

- typed locals and default inference;
- function prototypes/definitions and calls;
- strings, tables, clone, moves, and generated RAII drops;
- Cargo-style type diagnostics;
- unsafe volatile pointer emission and the E0316 guard;
- a native build-and-run test that skips when no host C compiler is available.

`tests/parser_compiler_self_hosted.rs` continues to validate the bytecode
compiler on the shared parser. `tests/lexer_self_hosted.rs` validates token
locations and literals.

### Current limits

The C emitter supports the typed subset above, not the full dynamic language:
keyed constructors, field/method access, assignment through an index/field,
heterogeneous tables, numeric/generic `for`, varargs, interpolated strings, and
closures/nested functions are rejected with diagnostics. The dynamic
interpreter and bytecode VM remain the complete-language engines.

## Extension points

The next steps are keyed/hash tables, field/index assignment, heap-table
mutation, closures/upvalues, numeric and generic `for`, and varargs in the C
emitter. The runtime already has deep clone/drop and volatile raw-memory
support; those features mainly need AST schema and lowering extensions.
