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
`if`/`elseif`/`else`, `while`, `repeat`, generic `for ... in ... do`,
`do`, `unsafe do`, `struct` (brace or bare form), `match`, `function`
(including colon methods), `return`, and `break`.

Expressions: `nil`, booleans, decimal and hexadecimal numbers, strings,
names, calls, colon method calls, field reads (`t.name`), parentheses, unary
`-`/`not`/`#`, arithmetic/concatenation/comparison operators, `and`/`or`,
`clone`, `borrow`, `move`, array/keyed table constructors, struct literals
(`Point{x: 1}`), and one-based index reads.

`match` is parser sugar: it becomes a `Do` block containing one hidden `Local`
and an `If`/`elseif` chain. Literal arms become `==` conditions; a binding arm
becomes a leading `local name = move __match_N`; `_` becomes the `else` body.
The checker therefore sees ordinary scopes and applies ownership per arm.
Imports are expanded before parsing, so `import` and `mod` never reach the AST.

The reference parser intentionally rejects numeric `for`, dynamic key
constructors (`{[expr] = value}` except string literals), assignment through a
dynamic index, and varargs.

### AST schema

Every node carries `tag`, `line`, and `column`.

| node | fields |
|------|--------|
| `Program` | `body = { stmt... }` |
| `Local` | `names`, `types`, `values` |
| `Assign` | `targets = { Name | Field... }`, `values` |
| `Function` | `name`, `receiver` (method table, or nil), `params = { {name,type}... }`, `return_type`, `body` |
| `CallStmt` | `value = Call | MethodCall` |
| `If` | `branches = { {cond, body}... }`, `else_body` |
| `While` | `cond`, `body` |
| `Repeat` | `body`, `cond` |
| `ForIn` | `vars`, `iter = pairs(table) | ipairs(table)`, `body` |
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
| `MethodCall` | `obj`, `name`, `args` |
| `Field` | `table`, `field_name` |
| `Table` | `items`, `keys` (`string | nil` per entry) |
| `Index` | `table`, `index` |
| `Struct` | `name`, `fields = { {name, type}... }` (declaration) |
| `StructLit` | `name`, `fields = { {name, value}... }` (literal) |
| `Clone` / `Borrow` / `Move` | `value` |

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
  │ import expansion (src/imports.rs: recursive, deduplicated)
  ▼
merged source
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

`import "file.lusa"` and `mod name` are resolved by the Rust front end
(`src/imports.rs`) before the merged source is parsed. Expansion is recursive and
relative to the importing file, and canonical paths are included once. The
expander tokenizes each file to locate import statements; the merged source then
goes through the normal lexer/parser pipeline, so the self-hosted parser never
emits `Import` or `Mod` AST nodes.

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
| `S` (declared `struct`) | native `S` typedef with declaration-order fields |

Untyped integer literals infer `i32`; untyped decimal literals infer `f32`.
The checker validates locals, assignments, function arguments/returns,
conditions, table elements, and operator operands. Errors use Cargo-style
`error[E....]` diagnostics with `--> lusa:line:column` and expected/found notes.
The checker annotates the optimized AST with `ctype`, `own`, `borrowed`, and
`moved` so the emitter never guesses.

### Exact integer literals

The self-hosted lexer stores a separate `int` field on pure integer tokens with
the original decimal or `0x` spelling. The parser carries it on `Number` nodes,
and the emitter lowers large i128 values through a runtime parser:

```c
static lusa_i128 lusa_i128_parse(const char *text);
```

```lusa
local big: i128 = 123456789012345678901234567890
```

```c
static const char lusa_int_1[] = "123456789012345678901234567890";
lusa_i128 init = lusa_i128_parse(lusa_int_1);
```

Floating literals infer `f32` in the native backend, while the dynamic engines
keep their runtime numbers as `f64`. The Rust lexer uses the same split
(`TokenKind::Int(String)` vs `TokenKind::Num(f64)`), so the dynamic engines
still run, while the C11 pipeline preserves every bit up to `__int128`.

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
(void)(((*(volatile uint32_t *)(uintptr_t)(v)) = (uint32_t)(1)));
uint32_t read = (uint32_t)((*(volatile uint32_t *)(uintptr_t)(v)));
```

Intrinsics (`unsafe_ptr_cast`, `unsafe_read_mem`, `unsafe_write_mem`) are
rejected outside an unsafe block with `E0316` by the C backend. Both directions
use the same `volatile uint32_t` lvalue, valid C11 for GCC, Clang, TCC, and
MSVC. They perform no bounds checks or memory sandboxing: the caller is
responsible for using valid hardware or process addresses.

`unsafe_nop()` emits `LUSA_NOP()`, and `asm!("wfe")`/`unsafe_asm("wfe")` emit
`LUSA_ASM("wfe")`. GCC/Clang define `LUSA_ASM(text)` as
`({ __asm__ __volatile__ (text : : ); })`; toolchains without inline assembly
use a no-op definition. The text must be a plain string literal without quotes,
backslashes, or newlines. `asm!` is available only inside `unsafe`; the
self-hosted parser desugars it to an `asm("...")` call before the compiler
checks the argument and safety gate.

The interpreter and bytecode VM implement the same names as safe builtins over
a process-local `HashMap<u64, f64>`. `lusalang run` and `lusalang run --vm`
therefore simulate register writes/reads without dereferencing memory; the
`build`/`run --native` path emits the volatile pointer access shown above.

### Fixed arrays, address-of, and pointer arithmetic

`[T; N]` is lowered to a C declarator with contiguous storage. Locals,
globals, and struct fields keep the bound next to the name:

```lusa
local samples: [i32; 4] = [10, 20, 30, 40]
local names: [string; 2] = ["alpha", "beta"]

struct Frame
    samples: [i32; 4],
    label: string
end
```

```c
int32_t v1[4] = {10, 20, 30, 40};
LusaString *v2[2] = { ... };
typedef struct { int32_t samples[4]; LusaString *label; } Frame;
```

Indexing is zero-based and lowers directly to `(buffer)[i]`. `#array` folds to
the declared bound. Arrays whose element type owns heap storage are tracking
values: the emitter inserts a drop loop

```c
for (int32_t lusa_i = 0; lusa_i < 2; lusa_i++) {
    lusa_drop_string(&v2[lusa_i]);
}
```

on every scope exit, and `lusa_drop_<Struct>`/`lusa_clone_<Struct>` apply the
same loop to array fields. C initializes omitted elements to zero, so a partial
initializer is still safe to drop.

`&place` emits C `&(place)` and produces `*mut T`:

```lusa
local x: u32 = 5
local p: *mut u32 = &x
unsafe do
    *p = 9
    local q: *mut u32 = p + 2
    print(x, *q)
end
```

```c
volatile uint32_t *init = (volatile uint32_t *)(uintptr_t)(&(x));
(*(init)) = (uint32_t)9;
volatile uint32_t *q = init + 2;
```

Pointer offsets use ordinary C arithmetic and therefore scale by the element
size. The checker still requires `unsafe` for dereferences (`E0321`) and
pointer offset expressions (`E0320`), and rejects writes through `*const`
(`E0322`). Whole-array assignment and arrays as function parameters/results are
rejected with `E0312`; use element access or a raw pointer.

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
typedef struct LusaValue { uint64_t kind; uint64_t payload; } LusaValue;
typedef struct LusaEntry {
    LusaString *key;
    uint64_t value;
    uint64_t kind;
} LusaEntry;
typedef struct LusaTable {
    LusaObj obj;
    uint64_t elem_kind;
    uint64_t capacity;
    uint64_t *items;
    uint64_t key_count;
    uint64_t map_capacity;
    LusaEntry *entries;
} LusaTable;
```

Struct declarations add one native typedef per declaration plus generated
drop/clone helpers:

```c
typedef struct { int32_t x; int32_t y; } Point;
static void lusa_drop_Point(Point *value);
static Point lusa_clone_Point(Point value);
```

Fields are emitted in declaration order; `string` and table fields become
pointers, and nested structs are embedded by value. Drop helpers release nested
struct, string, and table fields; clone helpers deep-copy them. The emitter also
records moved struct locals in a static move table because assigning `NULL` to a
struct lvalue is invalid C.

- Allocation uses `malloc`/`calloc`/`realloc`; release uses `free`.
- The array part is one-based in Lusa and lowered to zero-based C indices.
- The keyed part uses FNV-1a hashing with linear probing; string keys are
  cloned into the table and released during deep drop.
- `lusa_table_set_string`, `lusa_table_get_string`, and typed keyed getters
  implement field access. `LusaValue` carries heterogeneous `pairs` values.
- Colon methods compile to static C functions whose first parameter is a
  non-owning `LusaTable *self`; field writes mutate the receiver in place.
- `ipairs` walks the array part; `pairs` walks the keyed map. Each iteration
  enters a fresh RAII scope, so temporary heap values are dropped per loop.
- `lusa_drop_string`/`lusa_drop_table` check the owned flag and recursively
  release string/table elements before `free`.
- Owned locals/globals are dropped in reverse declaration order at every
  lexical exit, before `return`/`break`, and before an assignment overwrites an
  owned value. Drop lists are compile-time metadata; emitting declarations or
  error-path cleanup does not consume them, so normal nested exits still run
  exactly once.
- A move stores the pointer in the destination, clears the source slot
  (`= NULL`), and cancels the source drop.
- `clone` deep-copies strings and tables (`lusa_string_clone`,
  `lusa_table_clone`); scalar clone is a copy.
- `borrow` emits an alias with no allocation and no drop.
- `LusaString` keeps its byte length in `LusaObj` (embedded NUL bytes are
  valid) and a NUL-terminated `data` pointer for C/FFI calls.

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
locations, exact integer fields, and literals. `tests/module9.rs` checks typed
annotations and unsafe simulation on both dynamic engines, plus exact integer
AST tokens. `tests/module10.rs` covers native keyed tables, nested field
writes, colon methods, `pairs`, `ipairs`, and per-iteration RAII.
`tests/module15.rs` covers both struct declaration forms, literal defaults,
nested deep clone/drop, `match` arms and per-arm ownership, recursive
`import`/`mod` deduplication, and the GNU inline-assembly form.
`tests/module16.rs` covers fixed arrays, zero-based indexing, C declarators and
array drop loops, struct array fields, address-of, `unsafe` dereference gating,
pointer arithmetic lowering, dynamic-engine parity, and a native build/run.
`tests/module17.rs` covers typed volatile lowering for dot/colon/builtin
spellings, scalar-element and value-type diagnostics, `unsafe` gating in all
engines, packed/aligned C macros and diagnostics, dynamic parity, and a native
volatile/packed/aligned build/run. `tests/module18.rs` covers `asm!("...")`
emission, multiple instructions, `unsafe` gating in all three engines,
malformed input and arity, shared lowering with `unsafe_asm`, dynamic control
flow, and a native build/run.

### Current limits

The C emitter supports the typed subset above, not the full dynamic language:
numeric `for`, dynamic key construction in table literals, assignment through a
dynamic index, varargs, interpolated strings, and closures/nested functions are
rejected with diagnostics. Fixed arrays cannot be assigned as a whole, passed
or returned by value, or addressed with `&array`; index them or pass a raw
pointer. Struct fields must name a scalar, `string`, table, raw pointer, fixed
array, or previously declared struct (no forward references). `match`
patterns are literals, one binding name, or `_`; there are no destructuring,
range, or guard arms. Imports are flat textual includes with one shared symbol
table, not namespaces with selective exports. Native colon methods are static
functions rather than table fields, so `pairs` does not enumerate method names;
use a separate data table when enumerating. The dynamic interpreter and
bytecode VM remain the complete-language engines; they accept typed/`unsafe`
source but use f64 arithmetic and simulated memory, so exact 128-bit arithmetic
and aliasing through `&local` are C11-backend features. Volatile access is
limited to scalar pointer elements; volatile struct copies and bulk array
access are not part of the native subset. `#[packed]` and
`#[repr(align(N))]` change native layout only; the dynamic engines accept the
same declarations and keep their fixed-field simulation. `asm!("...")` lowers
raw instruction text without operand constraints or clobber modeling;
privileged instructions such as `cli`/`sei` require ring 0 and are exercised by
emission tests rather than native runs.

## Extension points

The next steps are dynamic key construction, assignment through dynamic
indices, closures/upvalues, numeric `for`, varargs, and interpolated strings in
the C emitter. The runtime already has keyed tables, deep clone/drop, methods,
generic iterators, typed volatile raw-memory access, and packed/aligned struct
layout; those features mainly need AST and lowering extensions.
