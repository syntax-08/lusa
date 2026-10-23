# The Lusa language — reference

Lusa is a dynamic Lua-like language with an **optional ownership system for
heap values**. This document describes syntax, execution semantics and the
error-code table.

---

## 1. Values

| type     | semantics | example            |
|----------|-----------|--------------------|
| `nil`    | Copy      | `nil`              |
| boolean  | Copy      | `true`, `false`    |
| number   | Copy      | `1`, `3.14`, `0x10`, `1e-3` |
| string   | Copy      | `"abc"`, `'abc'`   |
| table    | **Move**  | `{1, 2, k = "v"}`  |
| function | **Move**  | closures and builtins |
| iterator | (internal)| result of `pairs`/`ipairs` |

Numbers are `f64` (like Lua). Truthiness: only `nil` and `false` are false —
`0` and `""` are **truthy** (Lua semantics).

---

## 2. String interpolation and escapes

```lusa
local name = "world"
print("hello \{name.upper()} \{1 + 2}")  -- => hello WORLD 3
print("\u{26A1}")                        -- ⚡
print("{thing}")                         -- {thing} literal (no \)
print("\\\{1}")                          -- \1 —— backslash + interpolation
```

Escapes: `\\ \n \t \r \0 \a \u{XXXX}`; literal braces are just `{` and `}`.
Interpolation requires a string/number result.

Comments: `-- line` and `--[[ block ]]`.

---

## 3. Declaration and assignment

```lusa
local a, b = 1, 2          -- locals (block scope)
c = 3                      -- global (implicitly created)
t = {x = 1}
t.x, t["y"] = 10, 20       -- fields/indices
local m1, m2 = f()         -- multiple return values adjusted (Lua-style)

-- Module 8.5 typed subset (C11 backend)
local count: u32 = 10
local ratio: f32 = 0.5
local grid: [[i32]] = {{1, 2}, {3, 4}}
```

An `undefined global` reads `nil` (Lua-forgiving). Moved slots follow the
rule in section 5. Type annotations are accepted by the interpreter and the
bytecode VM too, but ignored at runtime; `lusalang build` and
`lusalang check --typed` enforce them statically before emitting C11.

---

## 4. Control flow

```lusa
if x > 0 then print("+")
elseif x < 0 then print("-")
else print("0") end

while cond() do ... end
repeat ... until cond        -- cond sees the body's locals

for i = 1, 10, 2 do ... end               -- numeric
for k, v in pairs(t) do ... end           -- generic (1 iterator)

do ... end                                -- block (new scope)
function f(a, b, ...) return a, b, ... end
```

`break` exits the innermost loop. `return f()` returns **all** of `f`'s
results (same rule as Lua).

Operators (Lua precedence): `or < and < (== ~= < > <= >=) < .. (right) <
+ - < * / // % < unary (- not #) < ^ (right)`.

`and`/`or` short-circuit.

---

## 5. Ownership: move / clone / borrow

### Rules

1. Scalars (`nil bool number string`) are **Copy**: assigning, passing as
   an argument or returning copies the value.
2. Heap values (`table function`) **move by default** — a read in a
   *consuming context* (assignment, user-function argument, `return`,
   table literal) transfers ownership:
   ```lusa
   local t = {1, 2, 3}
   local u = t        -- move: t becomes moved-out
   print(t)           -- ⚠ error[E1001]: use of moved-out value `t`
   ```
   The moved slot keeps a history (`note: moved to `u` here`) for the
   diagnostic.
3. `clone x` — deep copy (recursive for tables), **independent**:
   ```lusa
   local t = {1}
   local u = clone t  -- both stay alive and separate
   ```
4. `borrow x` — immutable alias of the value (still owned by the original):
   ```lusa
   local v = borrow t
   print(v[1])        -- read ok
   -- v[1] = 9        -- ⚠ error[E1004]: write through an immutable borrow
   ```
5. Methods — `o:m()` passes the receiver as a **mutable borrow**, for
   compatibility with Lua:
   ```lusa
   function o:inc() self.n = self.n + 1 end
   o:inc()            -- implicit mutable borrow, mutates `o.n`
   ```
6. `and`, `or` evaluate their operands without consuming anything (plain
   reads are Copy in expression context).

### Order of checks (runtime)

- moving out of a slot while it has a borrower → **E1002**;
- creating a *mutable borrow* while any borrow exists → E1002;
- creating an *immutable borrow* while a mutable borrow exists → E1002.

Aliases release their tag when they go out of scope (Drop), so:

```lusa
do
    local b = borrow t
    print(b[1])
end
local c = t         -- ok: the borrow already ended
```

---

## 6. stdlib

Base: `print msg...`, `type v`, `tostring v`, `tonumber s (base)`,
`error msg`, `assert ok, msg`, `pcall f, args...`, `pairs t`, `ipairs t`,
`io_read_file path`, `io_write_binary path, bytes`, `io_read_binary path`,
`require name`.

### File I/O - `io_read_file(path)`

Reads a UTF-8 text file and returns its contents as a Lusa string. The path
must be a string; missing, unreadable, or non-UTF-8 files raise `E2009`, which
can be caught with `pcall`:

```lusa
local source = io_read_file("src/main.lusa")
print(#source)

local ok, err = pcall(io_read_file, "missing.lusa")
-- ok = false, err starts with "E2009: io_read_file: cannot read ..."
```

The same native function is installed in the global environment used by both
the tree-walking interpreter and the bytecode VM.

### Binary I/O - `io_write_binary(path, bytes)` / `io_read_binary(path)`

`io_write_binary` writes raw bytes and returns `true`. `bytes` may be a dense
1-based table of integers in `0..255` (the form used by the C backend when it
serializes generated source) or a string, whose UTF-8 bytes are written
unchanged. Missing entries, non-number bytes, and out-of-range values raise
`E2006`; filesystem failures raise `E2009`.

`io_read_binary` reads any file and returns a dense 1-based table of byte
numbers, so generated C round-trips exactly:

```lusa
local c_source = Compiler.emit_c(borrow ast)
io_write_binary("program.c", c_source)

local check = io_read_binary("program.c")
assert(#check == #c_source)
assert(check[1] == 47 and check[2] == 42) -- "/*"
```

Both functions are available in the interpreter and the VM. They are ordinary
native builtins, so they do not need `require`.

### Protected calls — `pcall`

`pcall(f, ...)` calls `f` and never raises: on success it returns
`true` plus every value `f` returned; on any failure (user `error()`,
`assert`, type errors, ownership moves, even call-stack overflow) it
returns `false` plus one error string shaped `"CODE: message"` so
programs can inspect the E-code:

```lusa
local ok, err = pcall(load_file, "data.bin")
if not ok then
    print("could not load:", err)   -- e.g. E9999: call stack overflow
end
-- the program keeps running from here
```

Ownership applies to the callee: each `pcall(f, x)` consumes `f` the
way function calls do; to protect-call the same function twice, pass
`clone f`. Anonymous wrappers sidestep re-use entirely:

```lusa
local ok = pcall(function() return risky(a, b) end)
```

`string`: `len upper lower sub rep reverse char byte find gsub format`.
`find` and `gsub` are literal-only today (no Lua patterns) and `gsub`
returns `(new text, count)`.

`table`: `insert` (positional or append), `remove`, `concat`, `sort`
(optional comparator), `unpack`, `pack`.

`math`: `abs floor ceil round sqrt exp log sin cos tan pi huge hypot?`
`max min fmod modf tointeger random randomseed`.
`math.pi` and `math.huge` are values (not functions).

`json`: hand-rolled JSON (strict RFC 8259, no external deps).

- `json.encode(v)` — any value to JSON text. Scalars map directly
  (`nil`→`null`); tables with dense integer keys `1..n` become arrays,
  everything else becomes objects (numeric keys are stringified).
  Non-finite numbers, functions and iterators raise `E9000`; cyclic
  tables are detected and reported.
- `json.decode(s)` — JSON text back to a Lusa value. Arrays come back
  with keys `1..n` (so `#` and `ipairs` work), `null` becomes `nil`,
  `\uXXXX` escapes (including surrogate pairs) are resolved. Raw control
  characters inside a string and trailing garbage raise `E9000` with the
  offending position: `json.decode: expected '"', found … (at position 2)`.

```lusa
local cfg = json.decode('{ "window": { "width": 800, "widgets": ["panel", "log"] } }')
print(cfg.window.width, cfg.window.widgets[2])   -- 800	log
local out = json.encode({ name = "game", score = 12.5 })
-- {"name":"game","score":12.5}
```

---

## 7. Error-code table

Codes `E{category}{n}`, like rustc:
- **E0xxx** — lexer/parser;  **E1xxx** — ownership/mutability;
  **E2xxx** — runtime types;  **E9xxx** — program failures.

| code | kind     | meaning |
|------|----------|---------|
| E0001  | lex      | malformed character, unterminated string, invalid escape |
| E0101  | parser   | syntax (unexpected token, missing `end`) |
| E0102  | reserved | `break` outside a loop (reported as E0101 today) |
| E1001  | move     | `use of moved-out value` — a moved slot was reused |
| E1002  | borrow   | op. that needs exclusive access while a `borrow` exists |
| E1003  | reserved | (future) access through a borrow whose owner was moved |
| E1004  | borrow   | write through an immutable borrow |
| E1005  | reserved | (future) move out through a borrow |
| E2001  | type     | arithmetic with a non-number |
| E2002  | type     | concat (`..`) with a non-string/number |
| E2003  | type     | comparison of incomparable types |
| E2004  | type     | length `#` of a value without length |
| E2005  | type     | calling a non-function |
| E2006  | type     | indexing (`t.x`/`t[k]`) a non-table; invalid binary byte table |
| E2007  | key      | `nil`/NaN key in `t[k] = v` |
| E2008  | value    | invalid `tonumber`/`format` |
| E2009  | I/O      | file I/O cannot read/write (missing, unreadable, non-UTF-8, permissions) |
| E0301  | static   | C11 backend: missing/invalid function parameter or return type |
| E0308  | static   | C11 backend: mismatched types (assignment, return, call, condition) |
| E0311  | static   | C11 backend: wrong argument/initializer count |
| E0316  | unsafe   | C11 backend: raw pointer intrinsic used outside `unsafe` |
| E0318  | static   | C11 backend: missing method or invalid method receiver |
| E0319  | static   | C11 backend: unsupported generic `for` iterator |
| E0320  | unsafe   | raw pointer arithmetic outside an `unsafe` block |
| E0321  | unsafe   | raw pointer dereference outside an `unsafe` block |
| E0322  | unsafe   | write through a `*const` pointer |
| E9000  | runtime  | `error(msg)` |
| E9100  | runtime  | `assert(false, msg)` |
| E9999  | runtime  | internal error (umbrella; includes stack overflow for calls > 200 — catchable with `pcall`) |

### Report format

```text
error[E1001]: use of moved-out value `d`
 --> prog.lusa:5:9
  |
5 | print(s(d))
  |         ^
note: `d` moved to `(argument 1)` here
 --> prog.lusa:4:13
help: `d` no longer holds the value; use `clone d` (copy) or
      `borrow d` (keep the original) when you still need it
```

---

## 8. Implementation notes

- Default engine: tree-walking interpreter; optional bytecode VM (`--vm`) with
  compile-time register allocation (see `compiler.rs` / `vm.rs`).
- Tables: `Vec<(key, SlotV)>` in insertion order + a hash index
  (`by_hash`) keyed "loose" (number/str/ptr for table×function).
  `#t` = the largest `n` such that keys 1..=n all exist (Lua heuristic).
- Thread-local ownership registry (`Ownership`): per-heap-object `vid`
  + current owner + list of `Borrower`s (tag/idx, released on Drop).
- Strings are `Arc<str>` (cheap Clone). Single `RefCell` re-entrancy:
  cloning an alias takes `base` **outside** `with_reg` (documented fix).
- Max call depth: 200 (E9999 above that).

---

## 9. Native C11 backend (Module 8.5)

`lusalang build` and `lusalang run --native` lower the typed subset below to
portable C11 and invoke the host C compiler. The generated `.c` file is an
inspectable intermediate; it contains no PE/ELF headers and no hand-written
x86_64 instructions. `lusalang build file.lusa --emit-c` writes the C file
without compiling it.

### 9.1 Typed locals and functions

```lusa
local count: u32 = 10
local signed: i32 = -5
local wide: i128 = 1234567890123
local sensor: f32 = 2.5

function ler_sensor(pino: u32): f32
    return 1.5
end
```

| Lusa | C11 |
|------|-----|
| `u32` | `uint32_t` |
| `i32` | `int32_t` |
| `i128` | `lusa_i128` (`__int128`, or `long long` on MSVC) |
| `f32` | `float` |
| `[T]` / `[[T]]` | `LusaTable *` with a homogeneous element kind |
| untyped integer literal | inferred as `i32` |
| untyped decimal literal | inferred as `f32` |

Pure integer literals keep their original decimal or hexadecimal spelling in
the token/AST stream. The C11 backend passes that text to
`lusa_i128_parse("...")`, so constants above 2^53 retain all bits up to
`__int128`. The dynamic interpreter/VM still expose numbers as `f64` for
Lua-compatible arithmetic; use `build`/`run --native` when exact 128-bit
literals matter.

The checker runs before emission. Incompatible assignments, returns, calls,
conditions and table elements stop compilation with Cargo-style diagnostics:

```text
error[E0308]: mismatched types in local `x`
  --> lusa:1:16
   = expected `u32`
   = found    `string`
```

### 9.2 Unsafe blocks and raw hardware access

The C backend recognizes `unsafe do ... end`. Inside it, direct assignment
through a borrowed alias is permitted and the raw-memory intrinsics are
available:

```lusa
unsafe do
    local reg: i128 = unsafe_ptr_cast(0x40000000)
    unsafe_write_mem(reg, 1)
    local value: u32 = unsafe_read_mem(reg)
    print(value)
end
```

The intrinsics lower to volatile C accesses:

```c
lusa_i128 v = ((uintptr_t)(0x40000000));
(*(volatile uint32_t *)(uintptr_t)(v)) = (uint32_t)(1);
uint32_t read = (*(volatile uint32_t *)(uintptr_t)(v));
```

`unsafe_ptr_cast(address) -> i128` (a `uintptr_t` in C),
`unsafe_read_mem(address) -> u32`, and `unsafe_write_mem(address, value)` are
rejected outside an `unsafe` block with `E0316` in the C backend. Use these
only with addresses that are valid on the target board/process; the native
backend intentionally does not add bounds checks or a memory sandbox.

The interpreter and `--vm` give the same source a **safe simulation**: the
intrinsics read/write a process-local `HashMap<address, u32>` and never
dereference real memory. This lets developers test register logic before
compiling:

```sh
lusalang run hardware_test.lusa
lusalang run hardware_test.lusa --vm
lusalang build hardware_test.lusa   # real volatile access
```

### 9.3 Heap ABI and RAII

`Compiler.memory_layout()` is the source of truth for the generated runtime:

- `LusaString` and `LusaTable` begin with a 32-byte `LusaObj` header
  (`type`, `length`, `flags`, `size`).
- `LusaTable` includes the 1-based array part plus a string-keyed
  open-addressing map (`LusaEntry`), so keyed and array values share one
  object and one deep-drop traversal.
- Allocation uses `malloc`, `calloc`, `realloc`, and `free`; there is no
  `VirtualAlloc`/`mmap` dependency.
- Owned locals and globals are deep-dropped in reverse declaration order at
  lexical exits, before `return`/`break`, and before an assignment overwrites
  an owned value.
- A `move` clears the source slot and cancels its drop; `clone` performs a deep
  copy for strings and tables; `borrow` emits an alias without allocation or
  drop.

### 9.4 Native keyed tables, methods, and generic iterators

Keyed constructors and field access are lowered to the generated hash-map
runtime:

```lusa
local user = { id = 1, name = "Lusa", profile = { city = "Oslo" } }
print(user.id, user.name, user.profile.city)
user.name = "New"
user.profile.city = "Bergen"
```

The C runtime uses a string-keyed open-addressing map (`LusaEntry` +
FNV-1a/linear probing) alongside the array part. `lusa_table_set_string`,
`lusa_table_get_string`, and typed keyed getters are emitted directly; field
assignment drops the previous owned value.

Colon methods are compiled as static C functions with `self` as a non-owning
`LusaTable *` receiver:

```lusa
function user:rename(next_name: string): void
    self.name = next_name
end
user:rename("New")
```

Generic iterators support `pairs` for keyed tables and `ipairs` for array
tables:

```lusa
for key, value in pairs(user) do
    print(key, value)
end

for index, value in ipairs({10, 20, 30}) do
    print(index, value)
end
```

`pairs` values use a tagged `LusaValue` when the table is heterogeneous;
`ipairs` values retain their array element type. Per-iteration locals are
dropped by the normal RAII scope machinery.

### 9.5 Static structs and raw pointers (Module 11)

`struct` declarations give the native backend rigid, compile-time layouts.
Each field is a scalar (`u32`, `i32`, `f32`, `bool`) or a raw pointer type;
the declaration becomes a plain C typedef, and field access compiles to a
direct member access with zero hashing or lookup overhead:

```lusa
struct RegisterBank { control: u32, data: i32, status: u32 }

local bank: RegisterBank
bank.control = 7
bank.data = -3
print(bank.control)          -- (bank).control in C
```

Raw pointer types follow the Rust spelling:

```lusa
local regs: *mut u32 = 0x1000   -- volatile uint32_t *
local rom:  *const u32 = 0x2000 -- const volatile uint32_t *
```

Pointer arithmetic and dereferences are only legal inside `unsafe do ... end`;
outside it they report `E0320` (arithmetic) and `E0321` (dereference):

```lusa
unsafe do
    local p: *mut u32 = unsafe_alloc(64)
    *(p + 1) = 9                 -- volatile write, scales by sizeof(u32)
    print(*(p + 1))              -- volatile read
end
```

Writes through a `*const` pointer are rejected with `E0322`. `unsafe_alloc(n)`
returns a zeroed `n`-byte buffer tracked by the generated runtime and freed
before the program exits. The kernel-level intrinsics lower to compiler
hooks: `unsafe_nop()` becomes a `nop`/`__nop()` and `unsafe_asm("text")`
becomes `__asm__ __volatile__(text)` on GCC/Clang (a compiler barrier on
toolchains without inline assembly):

```lusa
unsafe do
    unsafe_nop()
    unsafe_asm("wfe")
end
```

The interpreter and `--vm` emulate the same source safely: structs become
fixed-field buffers, raw pointers are indices into a simulated address space,
and `unsafe_alloc` hands out virtual regions. `unsafe_nop`/`unsafe_asm` are
sandboxed no-ops, and dereferencing outside `unsafe` still reports `E0321`,
so bare-metal logic can be debugged before native compilation.

### 9.6 Native subset limits

Supported: typed and inferred locals, named typed functions, calls, string and
array/keyed table literals, nested keyed tables, field reads/writes, colon
methods, `pairs`/`ipairs` generic `for`, one-based array indexing, `#`, `if`,
`while`, `repeat`, `do`, `return`, `break`, assignments, globals, `print`,
`clone`, `borrow`, `unsafe` blocks, static `struct` declarations, raw pointer
types (`*mut u32`, `*const u32`, `*mut i32`, `*const i32`), pointer arithmetic
and volatile dereferences inside `unsafe`, and the raw intrinsics above
(`unsafe_ptr_cast`, `unsafe_read_mem`, `unsafe_write_mem`, `unsafe_alloc`,
`unsafe_nop`, `unsafe_asm`).

Not supported by the C emitter: numeric `for`, arbitrary dynamic key
construction in table literals, assignment through a dynamic index, varargs,
interpolated strings, closures/nested functions, the string library, `pcall`,
and coroutines. Native colon methods are
static functions and are not stored as table fields, so `pairs(user)` does not
enumerate method names; keep enumerable data in a separate table when parity
with the dynamic engines matters. The normal `lusalang run` interpreter and
`--vm` engine continue to implement the full dynamic language in the preceding
sections. Native `pcall` and coroutines are the first milestone in
[docs/ROADMAP.md](ROADMAP.md).
