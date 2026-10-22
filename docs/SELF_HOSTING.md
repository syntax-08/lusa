# Self-hosted front end

The `lexer.lusa`, `parser.lusa` and `compiler.lusa` files at the repository root
implement the first three stages of the toolchain in Lusa itself.  They are
reference implementations: small, readable, portable across the tree-walking
interpreter and `--vm`, and intentionally limited to a core language subset.

```text
source text
    │
    ▼
lexer.lusa ──► flat token table
    │
    ▼
parser.lusa ─► nested-table AST
    │
    ▼
compiler.lusa ─► numeric bytecode image
```

Each module exposes one table:

```lua
local Lexer = require("lexer")
local Parser = require("parser")
local Compiler = require("compiler")

local tokens = Lexer.lex(source)
local ast = Parser.parse(tokens)
local image = Compiler.compile(clone ast)
```

The parser consumes the token table by value.  The compiler consumes the AST by
value, so `clone ast` is required when the AST must remain available.  Neither
module uses `borrow`, because both are exercised on the bytecode VM as well as
the interpreter.

## Supported subset

Statements:

- `local` declarations with optional initializers
- name assignment, including multiple targets/values
- call statements
- `if` / `elseif` / `else`
- `while`, `repeat`, `do`
- `return`, `break`

Expressions:

- `nil`, booleans, numbers (decimal and hex), strings (decoded escapes)
- names, calls, parentheses
- unary `-`, `not`, `#`
- binary arithmetic, concatenation, comparison and equality operators
- `and` / `or`
- `clone` and `borrow`
- array table constructors (`{}`, `{1, 2}`, trailing comma allowed)
- integer index reads (`t[1]`, including nested `t[1][2]`)

Intentionally unsupported in this reference stage: keyed table constructors
(`{k = v}`), field/method access (`.`, `:`), assignment through a field/index,
function definitions, numeric/generic `for`, varargs, and compilation of
interpolated strings. The parser reports a `parser:line:column:` error instead
of silently building a partial AST. The compiler reports unsupported AST nodes
with a `compiler:line:column:` error. `borrow` is parsed and compiled to the
bytecode `BorrowErr` opcode, matching the native compiler's VM behavior. Calls
are compiled as single-value `CallOne` expressions; multi-return expansion from
calls and vararg propagation are not part of this reference subset. A
declaration or assignment with more initializers than targets is rejected
rather than silently dropping the extras.

## AST schema

Every node is a fresh table with `tag`, `line` and `column` fields.

| node | fields |
|------|--------|
| `Program` | `body = { stmt... }` |
| `Local` | `names = { string... }`, `values = { expr... }` |
| `Assign` | `targets = { Name... }`, `values = { expr... }` |
| `CallStmt` | `value = Call` |
| `If` | `branches = { { cond = expr, body = { stmt... } }... }`, `else_body = { stmt... } or nil` |
| `While` | `cond = expr`, `body = { stmt... }` |
| `Repeat` | `body = { stmt... }`, `cond = expr` |
| `Do` | `body = { stmt... }` |
| `Return` | `values = { expr... }` |
| `Break` | no extra fields |
| `Nil` | no extra fields |
| `Bool` | `value = true or false` |
| `Number` | `value = number` |
| `String` | `value = decoded text`, `raw = raw text`, `interpolated = bool` |
| `Name` | `name = string` |
| `Paren` | `value = expr` |
| `Unary` | `op = "-" or "not" or "#"`, `value = expr` |
| `Binary` | `op = string`, `lhs = expr`, `rhs = expr` |
| `Logical` | `op = "and" or "or"`, `lhs = expr`, `rhs = expr` |
| `Call` | `callee = expr`, `args = { expr... }` |
| `Table` | `items = { expr... }` (array entries only) |
| `Index` | `table = expr`, `index = expr` |
| `Clone` | `value = expr` |
| `Borrow` | `value = expr` |

`Assign` targets are restricted to `Name` nodes. The parser accepts `t[i]` as
an expression but rejects `a.b = ...`, `a[i] = ...`, and call targets with an
explicit error.

## Ownership strategy

- `Parser.parse(input)` moves the token table into module state, reads only
  scalar token fields, and returns a freshly built `Program` with `move`.
  Token sub-tables are never moved, written, or aliased into the AST.
- Parser nodes are assembled bottom-up.  Fields that are still needed (for
  example a target's source location) are copied to scalars before the table is
  moved into its parent.
- `Compiler.compile(ast)` takes ownership of `ast`.  Traversal moves fields out
  of the owned tree, but helper functions receive scalar locations or owned
  subtrees; they never receive a shared AST node merely to inspect it.
- The compiler's scope stack and loop/break stack use explicit depth counters.
  Lusa's `#table` does not shrink when a trailing slot is assigned `nil`, so
  `scopes[#scopes] = nil` cannot be used to pop a stack safely.
- Callers that need the AST after compilation pass `clone ast`.  Tests assert
  that the original tree is unchanged after compiling a clone.

## Bytecode image

`Compiler.compile` returns:

```lua
{
    version = 1,
    code = { byte... },            -- opcodes and operands, 0..255
    constants = { {kind=..., value=...}... },
    lines = { line... },           -- one entry per byte in `code`
    nlocals = number
}
```

Encoding rules:

- Opcodes are one byte and follow the native `Instr` discriminants in
  `src/compiler.rs`.
- Operands are big-endian `u16` values.
- Constant operands are **zero-based** indexes into `constants`; the Lusa array
  itself is one-based, so operand `k` refers to `constants[k + 1]`.
- Local-slot operands are zero-based.  `nlocals` is the high-water mark of
  unique slots allocated across all lexical scopes.
- Jump operands are absolute **one-based byte offsets** into `code`.  This is a
  property of the portable image; the native enum stores instruction indexes.
- `lines[i]` is the source line for `code[i]`, including operand bytes.
- The compiler appends an implicit `ReturnFixed(0)` after the program body, so
  top-level fallthrough returns no values.

### Opcode subset

| byte | opcode | operands | meaning |
|------|--------|----------|---------|
| 0 | `LoadConst` | constant | push a constant |
| 1 | `LoadNil` | — | push `nil` |
| 2 | `GetLocal` | slot | push local |
| 4 | `GetGlobal` | name constant | push global |
| 13 | `SetGlobal` | name constant | pop into global |
| 14 | `SetLocal` | slot | pop into local |
| 21..28 | `Add`…`Concat` | — | binary arithmetic/concat |
| 29..34 | `Eq`…`Ge` | — | comparison |
| 35..37 | `Neg`, `Not`, `Len` | — | unary |
| 38 | `Clone` | — | deep clone top |
| 39 | `BorrowErr` | — | always errors in bytecode mode |
| 42 | `CallOne` | nargs, ntakes | fixed-arity call; this compiler emits `ntakes = 0` |
| 53 | `Jump` | target | unconditional jump |
| 54 | `JumpIfFalse` | target | pop and branch when false |
| 55 | `JumpIfTrueKeep` | target | `or`: keep truthy and jump, otherwise pop |
| 56 | `JumpIfFalseKeep` | target | `and`: keep falsey and jump, otherwise pop |
| 59 | `Pop` | — | discard top |
| 61 | `ReturnFixed` | count | return top `count` values |

The keep-jumps already pop the value when evaluation continues, so a logical
expression must not emit an extra `Pop` after them.  The native VM compiler had
such an extra instruction; it was removed and is covered by the
`vm_parity_logical_short_circuit` regression test.

### Example

For:

```lua
local a = 1 + 2
return a
```

the image has `version = 1`, `nlocals = 1`, constants `1` and `2`, and these
19 bytes (hexadecimal; the test asserts the same values as decimals):

```text
00 00 00    LoadConst 0      -- 1
00 00 01    LoadConst 1      -- 2
15          Add
0E 00 00    SetLocal 0
02 00 00    GetLocal 0
3D 00 01    ReturnFixed 1
3D 00 00    ReturnFixed 0    -- implicit
```

## Errors and tests

Both modules raise `E9000` with their stage in the message:

```text
parser:1:10: expected an expression, found end of input
compiler:1:11: string interpolation is not supported by this reference compiler
```

`tests/parser_compiler_self_hosted.rs` composes all three modules inside
separate `do ... end` blocks, checks AST shape, verifies the exact 19-byte image
above, decodes instruction boundaries and jump targets, asserts clone-based AST
preservation, and exercises parser/compiler error paths on both engines.

## Modules 7–8: native backend, heap objects and RAII

`compiler_bootstrap.lusa` is the first native-code stage. It does not emit VM
bytecode: it builds complete x86_64 executable images with hand-written PE32+
and ELF64 headers, so an emitted program needs neither the Rust runtime nor an
external linker. Module 7 established the scalar backend; Module 8 adds native
heap strings/tables, OS-backed allocation, deep scope drops, and move/borrow
tracking.

### Public API

| function | ownership contract | result |
|----------|--------------------|--------|
| `Compiler.optimize(move_ast)` | consumes a `Program` AST | optimized `Program` |
| `Compiler.emit_binary(borrow_ast, target_os)` | reads the AST; a `borrow ast` argument is valid | byte table (`PE` or `ELF`) |
| `Compiler.bootstrap(lexer, parser, source, target_os)` | consumes the front-end tables | `{target, source_bytes, image_bytes, image}` |
| `Compiler.verify_fixpoint(lexer, parser, source, target_os)` | clones the front-end tables for each run | `{equal, first, second}` |
| `Compiler.self_compile(lexer, parser, target_os, path)` | consumes the front-end tables and reads `path` | success descriptor or an explicit `{ok=false, stage=...}` report |
| `Compiler.write_image(path, image)` | reads `image` | `io_write_binary(path, image)` |
| `Compiler.memory_layout()` | none | heap/RAII ABI description |

`emit_binary` snapshots only `borrow_ast.body`, then optimizes a private copy.
The caller's AST and body remain readable after emission. Top-level fields are
copied as scalars; the emitter never mutates the borrowed tree. The scratch
tables used while lowering are cleared after the image is built.

The front-end APIs consume their lexer/parser arguments because the Module-5/6
modules use module-level state. Pass `clone Lexer, clone Parser` when the same
handles are needed again; `verify_fixpoint` does this internally so the caller's
handles remain valid.

### Optimizer

The optimizer is ownership-consuming: each helper takes a node and returns the
replacement, so nodes are never aliased after being folded or dropped. It
performs:

- constant folding for arithmetic, comparisons, unary operators, and static
  string concatenation;
- `and`/`or` pruning when the left side is a known boolean;
- constant-`if` elimination while preserving a later dynamic `elseif` head;
- removal of statements after `return`/`break`;
- removal of `while false` bodies and unreachable branches.

### Native subset

Accepted core:

- integer/boolean/`nil`/static-string literals, plus dynamic strings produced
  by `..`;
- locals, globals, assignment, arithmetic and comparison operators, `and`/`or`,
  unary `-`/`not`/`#`;
- `if`/`elseif`/`else`, `while`, `repeat`, `do`, `return`, `break`;
- homogeneous array tables (`{}`, `{1, 2}`, `{"a", "b"}`, nested tables),
  one-based integer index reads (`t[1]`), and `#t`;
- `move` by plain assignment and immutable `borrow` aliases;
- `print` with multiple arguments; Windows and ELF both format strings, tables
  (`<table>`), integers, booleans, and `nil`.

Owned locals and globals are dropped automatically at lexical scope exits,
including `if`/`while` blocks and the early `return`/`break` paths. Moving a
value clears its source slot and cancels that slot's drop; borrowing keeps a
non-owning alias alive and blocks moving or overwriting the owner until the
alias leaves scope. Reassigning a heap global or local drops the previous value
first.

Rejected nodes produce a `bootstrap:line:column:` error rather than a partial
image: keyed or heterogeneous tables, field access, assignment through an index
or field, heap `clone`, closures/functions, methods, numeric and generic `for`,
varargs, interpolation, floating-point arithmetic, and borrowed/moved
heap-valued table elements. Table values are homogeneous and integer-indexed;
the reserved hash-capacity field is not yet used. The backend represents raw
scalars as signed 64-bit integers; `false`/`nil` are `0` and `true` is `1`, while
strings and tables are payload pointers.

### PE32+ layout (Windows)

- DOS/PE headers occupy the first `0x200` bytes.
- `.text` is at RVA `0x1000`, file offset `0x200`; its first `0x1000` bytes are
  the static data area (NUL-terminated strings and writable global slots).
- Generated code begins at section offset `0x1000`, so the entry point is RVA
  `0x2000`.
- `.rdata` starts at RVA `0x3000` and contains the import descriptor, ILT/IAT,
  and hint/name entries.
- Imports are `ExitProcess`, `VirtualAlloc`, and `VirtualFree` from
  `kernel32.dll`, plus `printf` from `msvcrt.dll`. Calls use RIP-relative
  `FF 15` through the IAT.
- Because globals currently share the data/code section, `.text` is mapped
  read/write/execute (`0xE0000020`). A future layout can split globals into a
  dedicated writable `.data` section and keep code read/execute-only.
- Every Windows API call reserves the required 32-byte shadow space and keeps
  `rsp` 16-byte aligned. The prologue aligns the stack, establishes `rbp`, and
  reserves `align16(64 + 8*slots) + 8` bytes. Slot `i` lives at
  `[rbp - 40 - 8*i]`.
- Top-level `return` reaches `ExitProcess`, so the process exit code is the
  returned scalar. `print` uses `%s` for strings, booleans, `nil`, and tables
  (`<table>`), and `%lld` for integers.

### ELF64 layout (Linux)

- Entry is `0x402000` (`0x400000` load base + `0x2000` code offset).
- Static data starts at image offset `0x1000`; code starts at image offset
  `0x2000`.
- One `PT_LOAD` segment carries `R|W|X` flags; there is no dynamic loader,
  interpreter, relocation, or symbol table.
- `print` uses the `write(2)` syscall and supports static/dynamic strings,
  tables (`<table>`), integers, booleans, and `nil`; exit uses `exit(2)`
  directly.

### Heap lifecycle and RAII ABI

`Compiler.memory_layout()` describes the live Module-8 representation:

```lua
{
    scalar = "i64 in a register/stack slot",
    value_header = {
        size = 32,
        type = "u64 at object-32 (1 = string, 2 = table)",
        length = "u64 at object-24",
        flags = "u64 at object-16 (bit 0 = owned)",
        allocation = "u64 at object-8",
        payload = "object itself (header + 32)"
    },
    table = {
        element_kind = "u64 at payload+0",
        hash_capacity = "u64 at payload+8 (reserved)",
        entries = "u64 at payload+16, one-based array values"
    },
    allocator = {
        windows = "VirtualAlloc / VirtualFree",
        linux = "mmap(2) / munmap(2)"
    }
}
```

The 32-byte header immediately precedes the value pointer stored in a native
slot. Static strings embedded in the image carry the same header with
`flags = 0`; dynamic strings and tables carry `flags = 1` and are returned to
the OS by `rt_drop`.

- String payload: UTF-8 bytes followed by a NUL. Dynamic concatenation computes
  the combined byte length, allocates `header + length + 1`, copies both
  payloads, and returns the new payload pointer.
- Table payload: `elem_kind` (0 unknown, 1 number, 2 bool, 3 nil, 4 string,
  5 table), a reserved hash-capacity slot, then the one-based entries.
  `#t` reads the header length; `t[i]` performs a bounds check and loads entry
  `i`. The native subset currently supports homogeneous array tables only.
- Deep drop: `rt_drop` checks the owned flag, recursively drops table entries
  whose element kind is string or table, then releases the object. Strings are
  released directly.
- RAII lowering: owned locals are dropped in reverse declaration order at every
  lexical scope exit, including `if`/`while` blocks, `return`, and `break`.
  Heap globals are dropped at program exit; assigning over an owned local or
  global drops the old value first.
- Move: the source slot/global is cleared and its pending drop is cancelled.
  Borrow: the compiler records a non-owning alias, suppresses the owner's drop
  while the alias lives, and rejects moving or overwriting the owner. The alias
  itself is never dropped.
- `clone` of a heap value is intentionally rejected with a
  `bootstrap:line:column:` diagnostic; scalar `clone` is a copy. A deep-copying
  heap clone remains future work.

This is the RAII analogue of Rust: the scope owns its heap values and the
compiler inserts release calls rather than relying on a garbage collector.

### Self-compilation status

`self_compile` reads a source path (default `compiler_bootstrap.lusa`) and runs
it through the self-hosted lexer/parser and native pipeline. Full self-hosting
is deliberately not claimed yet:

- the Module-6 parser now accepts array tables and index reads, but still
  rejects this file's function definitions, keyed/field access, and `for`
  loops;
- the native backend would also reject closures/functions and `for` even if
  the parser accepted them;
- the Module-5 lexer is character-by-character and is very slow on the full
  ~70 KiB source.

On success it returns
`{ok=true, target, source_bytes, image_bytes, image}`. For this file the API
therefore returns `{ok=false, stage="frontend", error=...}`, with the parser
diagnostic. If lexing fails first the stage is `"lexer"`; if a future parser
accepts the source but the native emitter cannot lower it, the stage is
`"backend"`. The integration test exercises both sides of the contract: a tiny
supported source must produce a PE through `self_compile`, while a tiny
unsupported file must report the front-end gap without spending minutes in the
lexer. `verify_fixpoint` and the AST-to-image comparison validate deterministic
emission on the supported subset.

### Tests

`tests/bootstrap_native.rs` composes all four modules, then:

- asserts that `optimize` consumes only its argument, that `emit_binary`
  leaves a borrowed AST unchanged, and that a top-level borrow can be cloned
  without re-entering the ownership registry;
- checks constant folding, false-head `elseif` preservation, constant
  `if`/`while` pruning, dead-code removal, and the memory ABI;
- rejects an invalid target with the documented diagnostic, and verifies that
  an array `Table` AST node now emits a valid PE;
- compiles the same seed through `bootstrap` and the AST path and requires the
  same bytes; checks `verify_fixpoint`;
- exercises `self_compile` on a supported file and on an unsupported file that
  reports `stage="frontend"`;
- runs the pipeline on both the interpreter and VM and requires byte-identical
  PE/ELF output;
- executes the generated PE (exit value, static strings, while/repeat/break
  loops, assignments, bool/nil printing);
- validates ELF class/type/machine/entry/program-header and the single RWX
  `PT_LOAD` segment.

`tests/bootstrap_heap.rs` compiles a dedicated seed and executes its generated
PE. It covers dynamic concatenation, string and table lifetimes, array tables
and one-based indexing, nested tables, `if`/`while` scope drops, moves,
borrows, assignment drops, global heap values, and a nonzero process exit. The
same seed is emitted as ELF64 and structurally validated (there is no Linux
runtime in the current test environment).

`tests/parser_compiler_self_hosted.rs` additionally asserts the new `Table`
and `Index` AST shapes and retains explicit errors for field access and index
assignment.

## Extension points

The next stage can add keyed/hash tables, field/index assignment, heap `clone`,
function prototypes/upvalues, numeric and generic `for`, and varargs by
extending the AST schema, the parser precedence/suffix handling, and the
compiler's lowering. The heap drop/borrow machinery is already in place for
owned strings and homogeneous array tables; keyed entries still need hashing,
key lookup, and the corresponding drop traversal. Full-file self-parse also
requires functions and `for` before this module can replace the remaining Rust
stages. The module-level state design is single-threaded and non-reentrant by
choice; a future version can thread an explicit parser/compiler context table
instead.
