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
| struct   | Copy when scalar-only, otherwise **Move** | `Point{x: 1, y: 2}` |
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
local active: bool = true
local label: string = "sensor"
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

match value with                          -- literal arms, then optional
| 0 => print("zero")                      -- binding or wildcard arm
| 1 => print("one")
| other => print("many", other)
end
```

`break` exits the innermost loop. `return f()` returns **all** of `f`'s
results (same rule as Lua). `match` is lowered to an ordinary `if` chain by the
parser and follows the ownership rules in section 5 per arm.

Operators (Lua precedence): `or < and < (== ~= < > <= >=) < .. (right) <
+ - < * / // % < unary (- not #) < ^ (right)`.

`and`/`or` short-circuit.

---

## 5. Ownership: move / clone / borrow

### Rules

1. Scalars (`nil bool number string`) are **Copy**: assigning, passing as
   an argument or returning copies the value.
2. Heap values (`table function`, and structs that own `string`/`table`
   fields) **move by default** — a read in a
   *consuming context* (assignment, user-function argument, `return`,
   table literal) transfers ownership:
   ```lusa
   local t = {1, 2, 3}
   local u = t        -- move: t becomes moved-out
   print(t)           -- ⚠ error[E1001]: use of moved-out value `t`
   ```
   The moved slot keeps a history (`note: moved to `u` here`) for the
   diagnostic. The C11 front end rejects the same use at compile time.
3. `move x` — explicit ownership transfer. It is accepted in every compiler
   stage and has the same effect as a consuming read, but makes the intent
   visible:
   ```lusa
   local t = {1}
   local u = move t   -- explicit move; t becomes moved-out
   ```
4. `clone x` — deep copy (recursive for tables), **independent**:
   ```lusa
   local t = {1}
   local u = clone t  -- both stay alive and separate
   ```
5. `borrow x` — immutable alias of the value (still owned by the original):
   ```lusa
   local v = borrow t
   print(v[1])        -- read ok
   -- v[1] = 9        -- ⚠ error[E1004]: write through an immutable borrow
   ```
   Multiple immutable aliases may coexist. They end at lexical scope exit
   (including loop-body exit and `break`).
6. Methods — `o:m()` passes the receiver as a **mutable borrow**, for
   compatibility with Lua:
   ```lusa
   function o:inc() self.n = self.n + 1 end
   o:inc()            -- implicit mutable borrow, mutates `o.n`
   ```
   No other read, borrow, or move of `o` may appear while the method call
   evaluates its arguments; the static checker reports `E1002`. A mutating
   method call through a `borrow` alias is rejected with `E1004`.
7. `and`, `or` evaluate their operands without consuming anything (plain
   reads are Copy in expression context).

Struct values follow the same model: a struct containing only scalar fields
(including nested scalar-only structs) is **Copy**, while a struct with a
`string`/`table` field, directly or through a nested struct, moves like a
table. Struct literals consume heap-valued field expressions unless they are
wrapped in `clone` or `move`.

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

### Compile-time borrow checking

The C11 backend performs the same ownership analysis before emitting C. The
checker is path-conservative at variable granularity and adds no runtime
overhead. It enforces:

- a moved or `move`-d heap value cannot be read or moved again (`E1001`);
- a value with a live alias cannot be moved or overwritten (`E1002`);
- multiple immutable `borrow` aliases are allowed;
- an immutable alias cannot be written through (`E1004`);
- a method receiver is treated as a mutable borrow while its arguments are
  checked; any simultaneous read or move of the receiver reports `E1002`.

Diagnostics include the source span and, for alias conflicts, the borrow site:

```text
error[E1002]: cannot move out of `t` because it is borrowed
  --> lusa:4:11
   = borrowed by `view` at lusa:2:14
```

`unsafe do ... end` bypasses borrow/mutation checks for raw pointer and hardware
access. Move tracking (`E1001`) still applies, because using a moved value would
be undefined even in native code. `clone` always remains a safe deep copy.

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

### Coroutines - `create`, `resume`, `yield`, `status`, `close`, `isyieldable`

Coroutines are cooperative, isolated call stacks. `coroutine.create(f)` stores
a function without running it. `coroutine.resume(co, ...)` starts or continues
the coroutine and returns `true` plus the values passed to `yield`; when the
body finishes it returns `true` plus the function's return values. A failure
returns `false` plus an `"E-code: message"` string. `coroutine.yield(...)`
suspends the current coroutine and later returns the values supplied to the
next `resume`:

```lusa
function worker()
    local incoming = coroutine.yield("ready")
    return incoming + 1
end

local co = coroutine.create(worker)
print(coroutine.status(co))       -- suspended
print(coroutine.resume(co))       -- true	ready
print(coroutine.status(co))       -- suspended
print(coroutine.resume(co, 41))   -- true	42
print(coroutine.status(co))       -- dead
```

`coroutine.status(co)` reports `suspended`, `running`, `normal`, or `dead`.
`normal` means the coroutine is waiting for another coroutine it resumed.
`coroutine.close(co)` closes a suspended coroutine and releases captured
state; closing an unstarted coroutine is allowed, and closing a running one
fails. `coroutine.isyieldable()` is `true` while executing inside a coroutine.
Captured upvalues are shared with their closures; each coroutine has its own
operand stack and local variables.

`string`: `len upper lower sub rep reverse char byte find match gmatch gsub
format`. `find`, `match`, `gmatch`, and `gsub` use the Lua-style pattern
language described below. `gsub` returns `(new text, count)` and keeps its
previous behavior when `plain = true`.

### String patterns - `find`, `match`, `gmatch`, `gsub`

Patterns are matched over Unicode scalar values. Indices count characters, not
UTF-8 bytes, and `.` matches one scalar value. The supported syntax is:

| syntax | meaning |
|---|---|
| `%a %c %d %g %l %p %s %u %w %x` | ASCII classes; uppercase letters negate the class |
| `[set]`, `[^set]`, `a-z` | character sets and ranges |
| `.` | any character |
| `*`, `+`, `-`, `?` | greedy, one-or-more, non-greedy, optional |
| `^`, `$` | start and end anchors |
| `(...)`, `()` | substring capture and position capture |
| `%1`..`%9` | back reference to an earlier capture |
| `%bxy`, `%f[set]` | balanced match and frontier match |
| `%%` and `%x` | escaped literal characters |

```lusa
local line = "release=7 build=42"
print(string.find(line, "%d+"))            -- 9	9
print(string.match(line, "build=(%d+)"))   -- 42
print(string.gsub(line, "(%a+)=(%d+)", "%1:<%2>"))
-- release:<7> build:<42>	2

for word in string.gmatch(line, "%a+") do
    print(word)                            -- release, build
end
```

`string.find(s, pattern [, init [, plain]])` returns `start, end` followed by
any captures, or `nil` when there is no match. A negative `init` counts from
the end. `plain = true` performs a literal substring search. `string.match`
returns the captures, or the whole match when the pattern has no captures.
`string.gsub(s, pattern, replacement [, limit])` supports `%0` (the whole
match), `%1`..`%9` (captures), and `%%`; `limit` caps the substitution count.
Position captures produce 1-based decimal numbers.

Matching is bounded by a nesting depth of 200 and a step budget of 1,000,000.
A malformed pattern or replacement raises `E2010`; exhausting the budget raises
`E2011`. Both are catchable with `pcall`, and both leave the process running.

The C11 backend accepts the same surface with a few static restrictions:
patterns and replacements must be strings, `gmatch` must appear directly in a
generic `for`, and a loop may bind one to nine values. A literal pattern with
no captures yields one loop value (the whole match); a literal pattern with
captures must not bind more values than it produces. Literal malformed
patterns report `E0323`; an out-of-range replacement capture or `gmatch`
binding reports `E0324`. Native `string.find` returns `0` (not `nil`) when the
search fails and supports `local start, finish = ...`; native `string.gsub`
supports `local text, count = ...`; native `string.match` returns the first
capture (or the whole match) as a nullable string.

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
| E1002  | borrow   | move, read, or write needing exclusive access while a borrow or method receiver borrow is active |
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
| E2010  | pattern  | malformed string pattern or replacement |
| E2011  | pattern  | pattern exceeded the bounded matching budget |
| E0301  | static   | C11 backend: missing/invalid function parameter or return type |
| E0302  | static   | C11 backend: duplicate struct/field definition or duplicate struct-literal field |
| E0308  | static   | C11 backend: mismatched types (assignment, return, call, condition) |
| E0309  | static   | C11 backend: struct/table has no such field |
| E0311  | static   | C11 backend: wrong argument/initializer count |
| E0312  | static   | C11 backend: unsupported construct (struct field type, whole-array value, invalid `&` place, array-by-value call, or non-scalar volatile element) |
| E0313  | static   | C11 backend: zero or one return value only; void/value return mismatch |
| E0316  | unsafe   | C11 backend: raw pointer or volatile intrinsic used outside `unsafe` |
| E0317  | static   | C11 backend: `pcall`, `coroutine.resume`, or `string.gmatch` used outside its required local/loop form |
| E0318  | static   | C11 backend: missing method or invalid method receiver |
| E0319  | static   | C11 backend: unsupported generic `for` iterator; invalid native coroutine entry function, handle, operation, or string method |
| E0320  | unsafe   | raw pointer arithmetic outside an `unsafe` block |
| E0321  | unsafe   | raw pointer dereference outside an `unsafe` block |
| E0322  | unsafe   | write through a `*const` pointer |
| E0323  | static   | C11 backend: statically evaluable malformed string pattern or replacement |
| E0324  | static   | C11 backend: replacement capture or `gmatch` binding exceeds the pattern's captures |
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
local active: bool = true
local label: string = "pin"

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
| `bool` | `bool` (`<stdbool.h>`) |
| `string` | `LusaString *` (length + NUL-terminated `.data`) |
| `[T]` / `[[T]]` | `LusaTable *` with a homogeneous element kind |
| `struct S` | native `S` typedef with fields in declaration order |
| untyped integer literal | inferred as `i32` |
| untyped decimal literal | inferred as `f32` |

Unannotated locals use this static native inference (`i32` for integers, `f32`
for decimals); there is no dynamic `double` fallback in generated C. Pure
integer literals keep their original decimal or hexadecimal spelling in the
token/AST stream. The C11 backend passes that text to
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
(void)(((*(volatile uint32_t *)(uintptr_t)(v)) = (uint32_t)(1)));
uint32_t read = (uint32_t)((*(volatile uint32_t *)(uintptr_t)(v)));
```

Both reads and writes use the same `volatile uint32_t` lvalue form, which is
valid C11 for GCC, Clang, TCC, and MSVC. For typed access, prefer
`volatile_read`/`volatile_write` or the `read_volatile`/`write_volatile` method
sugar described in section 9.10. `unsafe_ptr_cast(address) -> i128` (a
`uintptr_t` in C), `unsafe_read_mem(address) -> u32`,
`unsafe_write_mem(address, value)`, `volatile_read(ptr)`, and
`volatile_write(ptr, value)` are rejected outside an `unsafe` block with
`E0316` in the C backend. Use these only with addresses that are valid on the
target board/process; the native backend intentionally does not add bounds
checks or a memory sandbox.

The interpreter and `--vm` give the same source a **safe simulation**: the
intrinsics read/write a process-local `HashMap<address, f64>` and never
dereference real memory. Cells hold full `f64` values, so fractional register
values round-trip without truncation; unwritten addresses read as `0`. This
lets developers test register logic before compiling:

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
  an owned value. The drop list is compile-time metadata and is never consumed
  while emitting declarations or error paths, so normal nested-scope exits run
  exactly once.
- A `move` clears the source slot and cancels its drop; `clone` performs a deep
  copy for strings and tables; `borrow` emits an alias without allocation or
  drop.
- `LusaString` tracks its byte length in `LusaObj` (embedded NUL bytes are
  valid) and keeps `data` NUL-terminated for C/FFI interoperability.

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

### 9.5 Static structs, literals, and raw pointers (Modules 11 and 15)

`struct` declarations give the native backend rigid, compile-time layouts. Both
brace and bare forms are accepted:

```lusa
struct RegisterBank { control: u32, data: i32, status: u32 }

struct Frame
    origin: Point,
    label: string,
    payload: table<i32>
end
```

Fields may be scalars (`u32`, `i32`, `i128`, `f32`, `bool`), `string`,
`table`/`table<T>`, a raw pointer type, or a previously declared struct.
Forward references are rejected: declare `Point` before using it as a field
type. A declaration becomes a plain C typedef with fields in declaration order,
and field access compiles to direct member access with zero hashing or lookup
overhead:

```lusa
struct RegisterBank { control: u32, data: i32, status: u32 }

local bank: RegisterBank
bank.control = 7
bank.data = -3
print(bank.control)          -- (bank).control in C
```

Struct literals initialize any subset of fields; omitted fields use the same
defaults as an uninitialized typed local:

```lusa
struct Point { x: i32, y: i32 }
struct Label
    origin: Point,
    text: string
end

local p = Point{x: 10}                 -- y = 0
local tag = Label{origin: p, text: "corner"}
print(tag.origin.x, tag.origin.y)      -- 10	0
```

Scalar fields default to zero/`false` and heap fields to `nil` (`NULL` in C).
Both `:` and `=` are accepted between a literal field name and its value. The
checker rejects unknown fields (`E0309`), duplicate fields (`E0302`), and
mismatched values (`E0308`).

In the dynamic engines, a struct whose fields are all scalar (including nested
scalar-only structs) is **Copy**: reading it returns an independent value.
Structs that own a `string`/`table` field, directly or through a nested struct,
move like tables and must be `clone`d to keep both values. The native checker
enforces the same ownership model. Generated C carries a
`lusa_drop_<Name>`/`lusa_clone_<Name>` pair; drops recurse through nested
struct, string, and table fields, and clone deep-copies them. Moved struct
values are tracked in the emitter's static move table because C has no null
representation for a struct value.

Raw pointer types follow the Rust spelling:

```lusa
local regs: *mut u32 = 0x1000   -- volatile uint32_t *
local rom:  *const u32 = 0x2000 -- const volatile uint32_t *
```

The element may be any supported scalar, `string`, table, or previously
declared struct; numeric elements remain the common case for hardware access.
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
hooks: `unsafe_nop()` becomes `nop`/`__nop()`, and `unsafe_asm("text")` becomes
`LUSA_ASM("text")`, which expands to `({ __asm__ volatile (text : : ); })` on
GCC/Clang (a no-op on toolchains without inline assembly). The assembly text
must be a plain string literal without quotes, backslashes, or newlines:

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

### 9.6 Native error recovery and coroutines (Module 12)

Native protected calls use **checked error propagation**, not `setjmp`/
`longjmp`. Runtime helpers record the first error in a runtime flag;
generated functions return normally through their RAII drop paths; `pcall`
inspects and clears the flag. A failed native `pcall` therefore keeps the
process alive, releases every local declared above the failure point, and
frees `unsafe_alloc` buffers created after the protected call began:

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

Native `pcall` is compiler-lowered and has stricter syntax than the dynamic
form:

- It must be a local initializer. Use
  `local ok, err = pcall(f, ...)` when the result is not needed, or
  `local ok, result, err = pcall(f, ...)` to bind one return value. A void
  callee requires the two-name form; a value-returning callee can use either.
  On success the two-name error is the empty string. Using `pcall` as an
  expression, in a return, or with one name reports `E0317`.
- `f` must be a direct named user function, not a method, anonymous function,
  or builtin such as `assert`. Wrap builtins in a named function. The target
  function's parameter count and types are checked at compile time.
- The native backend supports zero or one return value per function (`E0313`);
  the `pcall` protocol itself adds the status and error slots.
- An uncaught native runtime error prints `lusa runtime: <E-code>: <message>`
  to stderr and exits with status `1`. Allocation failure and exhaustion of
  the host stack or fiber stack remain process-level failures.

Native coroutines use platform-supported stacks: Windows fibers
(`CreateFiber`/`SwitchToFiber`) and POSIX `ucontext`. Every generated function
receives a hidden `LusaCoro *lusa_co` parameter, so an ordinary helper can
yield without special annotations; the normal drop checks unwind the helper
chain when a coroutine is closed:

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

The native coroutine subset is intentionally small:

- `coroutine.create(f)` accepts only a named function with no parameters that
  returns `i32`. Methods, parameters, anonymous functions, and other return
  types report `E0319`. The result is a `coroutine` value that must remain in a
  named local while it is resumed, inspected, or closed.
- `coroutine.resume(co, value)` must also be a local initializer with two or
  three names. The payload is one `i32`; varargs and multiple yield/resume
  values are not part of the native subset. The two-name form binds
  `ok, value` and discards the error. The three-name form binds
  `ok, value, err`, with `err` an empty string on success and an
  `"E-code: message"` string on failure.
- `coroutine.yield(value)` returns the `i32` supplied to the next resume (or
  `0` during close). It reports `E9999` if called from the program's main
  flow, which is not a coroutine. A dedicated coroutine entry function may be
  called directly, but then any yield is reported as an error.
- `coroutine.status(co)`, `coroutine.close(co)`, and
  `coroutine.isyieldable()` mirror the dynamic names. `close` returns a
  `bool`; closing an unstarted or suspended coroutine succeeds, while closing
  the currently running coroutine returns `false`. Closing resumes a suspended
  fiber only to run its drop paths, then frees its stack.
- An error inside a coroutine unwinds its locals, marks it `dead`, and is
  returned by `resume` as `false` plus the error string. A later resume of a
  dead coroutine reports `E9999`; the native message wording is
  `cannot resume a dead coroutine`.

The native 2-name `resume` result and 3-name error slot are deliberately
different from the dynamic engines, where a failed 2-name `resume` binds the
error string in the second slot and a failed 3-name `resume` puts the error in
the second slot with `nil` third. Portable examples should print the status
boolean and inspect the error only through the form appropriate to the chosen
engine.

### 9.7 Native string patterns (Module 13)

The C11 backend recognizes `string.find`, `string.match`, `string.gmatch`, and
`string.gsub`, including colon forms such as `s:find("%d+")`. Constant patterns
are checked during type checking; runtime calls lower directly to the
dependency-free matcher appended from `src/pattern_runtime.c`.

```lusa
local s: string = "a=1 b=2"
local start, finish = string.find(s, "%d+")
print(start, finish)                         -- 3	3

local text, count = string.gsub(s, "(%a+)=(%d+)", "%2:%1")
print(text, count)                           -- 1:a 2:b	2

for key, value in string.gmatch(s, "(%a+)=(%d+)") do
    print(key, value)                        -- a 1, then b 2
end
```

Native surface rules:

- Subject, pattern, and replacement expressions must have type `string`.
  `init` and `limit` are `i32`/`u32`; `plain` is `bool`.
- `string.find` returns `0` when there is no match. Bind one name for the start
  position or use `local start, finish = ...` for both positions. The native
  backend does not expose the dynamic capture list from `find`.
- `string.match` returns the first capture, or the whole match when there are no
  captures, as an owned nullable string. Bind it with one local.
- `string.gsub` supports a string replacement using `%0`..`%9` and `%%`.
  Function replacements are dynamic-only. Bind one name for the text or
  `local text, count = ...` for both values.
- `string.gmatch` must be the direct iterator expression of a generic `for`.
  The loop owns the iterator and every capture string; `break`, `return`, and
  normal loop exit release them through RAII. Literal patterns with no captures
  produce the whole match; literal patterns with captures produce one string per
  capture (position captures become decimal strings). With a dynamic pattern,
  bind only one loop variable.
- Literal malformed patterns and invalid replacement escapes report `E0323`.
  Replacement indices beyond a literal pattern's capture count, or too many
  `gmatch` loop variables, report `E0324`. Dynamic failures raise `E2010`,
  catchable through the native `pcall` subset. Budget exhaustion raises
  `E2011`.

The matcher mirrors `src/pattern.rs`: both decode UTF-8 to scalar values, enforce
the same depth (200) and step (1,000,000) limits, and restore captures when a
backtracking branch fails so sibling branches observe consistent state. Native
string literals are emitted as UTF-8 bytes, so pattern results containing
multibyte characters print and compare correctly.

### 9.8 `match`, imports, and modules (Module 15)

`match` evaluates its value once and compares it with literal patterns:

```lusa
match status with
| 0 => print("idle")
| 1 => print("busy")
| code => print("unknown", code)
end
```

- Patterns are numbers (including negative literals), strings, booleans, `nil`,
  a single binding name, or `_`.
- Literal arms compare with `==` and do not consume the value.
- A binding arm (`| value =>`) moves the matched value into a new local. `_`
  is the wildcard and performs no binding.
- A binding or wildcard arm must be last; another arm after it is a parse
  error.
- `match` is syntactic sugar. Both parsers lower it to a `do` block, one
  hidden local, and an `if`/`elseif` chain. There is no match bytecode
  instruction and no C dispatcher. Ownership is checked independently in each
  arm, so a move in one arm cannot leak into another:

```lusa
function pick(flag: bool, p: Point): Point
    match flag with
    | true => return p
    | _ => return clone p
    end
end
```

Imports are compile-time composition:

```lusa
import "geo/point.lusa"   -- exact path, relative to the importing file
mod counter               -- counter.lusa, counter/mod.lusa, or counter/main.lusa
```

- `import` expects a plain string literal (no interpolation) and resolves it
  relative to the importing file, or as an absolute path.
- `mod a.b` resolves to `a/b.lusa`, `a/b/mod.lusa`, or `a/b/main.lusa`.
- Expansion is recursive; imported files may import or `mod` further files.
- Every canonical path is included at most once, so shared dependencies
  contribute their definitions once even when reached through several modules.
- Expansion happens before parsing, and all files share one symbol table: there
  is no namespace or export filter. Imported definitions are merged at the
  textual include point, so a `struct` still has to be declared before the
  literals or field types that use it.
- The same expansion runs in the interpreter, bytecode VM, `build`,
  `run --native`, `check`, and `--emit-c`. Missing files report `E0101` with a
  help line describing the two forms.

Inline assembly is covered in section 9.5.

### 9.9 Fixed-size arrays and address-of (Module 16)

`[T; N]` is a fixed-size array with a compile-time length. It is distinct from
`[T]`, which remains the existing `table<T>` spelling:

```lusa
local samples: [i32; 4] = [10, 20, 30, 40]
samples[1] = samples[0] + samples[2]
print(samples[0], samples[3], #samples)   -- 10	40	4
```

- Array literals use `[elem1, elem2, ...]`; an empty literal cannot infer its
  element type and is rejected.
- The length must be a positive integer literal. The literal length must match
  the annotation.
- Indexing is zero-based. Reads return a view of the element; writes update the
  buffer in place.
- `#a` is the declared length in native code and the number of zero-based keys
  in the dynamic engines.
- Whole-array assignment, passing an array by value, returning one, and taking
  `&array` are rejected. Index the array or pass a raw pointer instead.

Native locals, globals, and struct fields are real C declarators:

```lusa
struct Frame { samples: [i32; 4], label: string }
local frame: Frame
frame.samples[0] = 10
```

```c
typedef struct { int32_t samples[4]; LusaString *label; } Frame;
```

Arrays of heap-owning elements get a generated RAII drop loop on every exit,
and struct drop/clone helpers loop over array fields. C aggregate
initialization zero-fills omitted elements, so every slot is safe to drop.

`&place` takes the address of a variable, field, index, or dereference and
produces `*mut T`. Pointer reads and writes are still restricted to `unsafe`:

```lusa
local x: u32 = 5
local p: *mut u32 = &x
unsafe do
    *p = 9
    print(x)                    -- 9 in native code
    local q: *mut u32 = p + 2   -- scales by sizeof(u32)
    local r: *mut u32 = q - 1
    print(*r)
end
```

`ptr + n` and `ptr - n` lower to ordinary C pointer offsets. Dereferencing
outside `unsafe` reports `E0321`, pointer arithmetic outside it reports
`E0320`, and writes through `*const` report `E0322`. In the interpreter and VM,
`&place` allocates a simulated cell and dereferences read/write that cell; they
never touch host memory, and writes do not alias the originating local.

### 9.10 Volatile MMIO and struct representation (Module 17)

Hardware polling needs loads that the optimizer cannot remove, and register
blocks need layouts that match the datasheet. Volatile access has three
equivalent spellings; all of them require `unsafe`:

```lusa
unsafe do
    local status: *mut u32 = unsafe_alloc(16)

    status.write_volatile(0)        -- dot method
    status:write_volatile(0)        -- colon method
    volatile_write(status, 0)       -- builtin

    local a: u32 = status.read_volatile()
    local b: u32 = volatile_read(status)
    print(a, b)
end
```

- `read_volatile`/`write_volatile` are lowered by both parsers into
  `volatile_read(ptr)`/`volatile_write(ptr, value)`, so the interpreter, VM,
  and C11 backend share one call shape.
- The element type is taken from the pointer: `*mut u32`, `*mut i32`,
  `*mut i128`, `*mut f32`, or `*mut bool`. A bare integer address defaults to
  `u32`. Non-scalar elements are rejected with `E0312` so a volatile access
  cannot bypass heap RAII.
- `write_volatile` checks the value against the element type (`E0308`) and the
  intrinsic arity is checked with `E0311`.
- Native code emits a typed volatile lvalue load or store:

  ```c
  uint32_t value = (*(volatile uint32_t *)(uintptr_t)(ptr));
  (*(volatile uint32_t *)(uintptr_t)(ptr)) = (uint32_t)(value);
  ```

- The interpreter and `--vm` route both builtins through the same simulated
  address map as `unsafe_read_mem`/`unsafe_write_mem`; booleans are stored as
  `1`/`0`. Outside `unsafe` the dynamic engines report `E0321`; the native
  checker reports `E0316`.

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

- `#[packed]` removes inter-field padding. The C11 backend wraps the typedef in
  `LUSA_PACK_BEGIN`/`LUSA_PACK_END` and marks it `LUSA_PACKED`.
- `#[repr(align(N))]` raises the struct's alignment; `N` must be a power of two
  from `1` to `4096`. The emitter places `LUSA_ALIGNAS(N)` on the first member,
  which is valid C11 and also raises the enclosing struct's alignment.
- The preamble defines the macros once for MSVC (`__pragma(pack(...))`,
  `__declspec(align(n))`), GCC/Clang (`__attribute__((packed))`,
  `__attribute__((aligned(n)))`), and other C11 compilers (`_Alignas(n)`).
- Duplicate attributes, unsupported attributes, and invalid alignments are
  rejected by both parsers before code generation with `E0101`.
- The dynamic engines accept and ignore the layout attributes; they keep their
  fixed-field struct simulation, so programs remain portable across engines.

`examples/baremetal_gpio.lusa` exercises a packed configuration, an aligned DMA
descriptor, and a volatile polling loop against a simulated peripheral block:

```sh
lusalang run examples/baremetal_gpio.lusa
lusalang run examples/baremetal_gpio.lusa --vm
lusalang run examples/baremetal_gpio.lusa --native
```

### 9.11 Native subset limits

Supported: typed and inferred locals, named typed functions, calls, string and
table literals, fixed-size `[T; N]` arrays and array literals, nested keyed
tables, field reads/writes, zero-based array indexing and element assignment,
colon methods, `pairs`/`ipairs` generic `for` over tables, `#` on strings,
tables, and arrays, `if`, `while`, `repeat`, `do`, `return`, `break`,
assignments, globals, `print`, `clone`, `borrow`, `move`, the compile-time
borrow checker, `unsafe` blocks, static `struct` declarations in brace and bare
form, `#[packed]`/`#[repr(align(N))]` representation attributes, struct
literals with defaults (scalar, `string`, `table`, pointer, and previously
declared struct fields), `match`, compile-time `import`/`mod`, address-of
(`&place`), raw pointer types (`*mut T`, `*const T`), pointer arithmetic and
dereferences inside `unsafe`, the raw intrinsics above (`unsafe_ptr_cast`,
`unsafe_read_mem`, `unsafe_write_mem`, `unsafe_alloc`, `volatile_read`,
`volatile_write`, `unsafe_nop`, `unsafe_asm`), native `pcall`, the
string-pattern surface in
section 9.7, and the restricted
`coroutine.create/resume/yield/status/close/isyieldable` surface described in
section 9.6.

Not supported by the C emitter: numeric `for`, arbitrary dynamic key
construction in table literals, assignment through a dynamic index, varargs,
interpolated strings, closures/nested functions, forward struct references,
whole-array assignment or arrays passed/returned by value, `&array`,
namespaced or selectively exported imports, the rest of the dynamic string
library, multiple function return values (`E0313`), dynamic `pcall`
expressions, and general first-class coroutine operations. Native colon methods are static functions
and are not stored as table fields, so `pairs(user)` does not enumerate method
names; keep enumerable data in a separate table when parity with the dynamic
engines matters. The normal `lusalang run` interpreter and `--vm` engine
continue to implement the full dynamic language in the preceding sections.
