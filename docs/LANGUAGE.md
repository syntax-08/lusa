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
| `Vec<T>` | **Move**  | `[1, 2, 3]`, `v:push(x)` |
| `Box<T>` | **Move**  | `Box(42)`, `b = 5`, `*b` |
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
local values: Vec<i32> = [4, 8, 15]   -- growable heap vector
local answer: Box<i32> = 41           -- owned pointer to a heap value
```

An `undefined global` reads `nil` (Lua-forgiving). Moved slots follow the
rule in section 5. Type annotations are accepted by the interpreter and the
bytecode VM too, but ignored at runtime; `lusalang build` and
`lusalang check --typed` enforce them statically before emitting C11.

### Modules, namespaces, and visibility (Module 23)

`mod name { ... }` declares an inline module; `mod name;` loads a file module
relative to the current file (or the entry directory):

```lusa
mod board;          -- board/mod.lusa first, then board.lusa
mod sensors {       -- inline module
    pub function read(): i32
        return 42
    end
}
```

A file loaded as `name/mod.lusa` resolves its own children inside `name/`. A
file loaded as `name.lusa` uses a sibling `name/` directory when it exists,
otherwise its own directory. The module directory therefore takes precedence
over a sibling `name.lusa` with the same logical name.

Items are private by default. Only `pub` items can be reached from outside the
defining module and its descendants:

```lusa
mod math {
    function secret(): i32
        return 1
    end

    pub function value(): i32
        return secret()          -- allowed: same module
    end
}

print(math::value())             -- 1
print(math::secret())            -- error[E0350]
```

`pub` is accepted on `mod`, `use`, `function`, `async function`, `struct`, and
top-level `local` declarations or assignments. Private items remain visible in
child modules (`super::`), so a parent can expose a narrow public API over
private helpers.

`use` imports a symbol and binds it to a local name:

```lusa
use board::Pin;
use sensors::temperature as temp;
use crate::protocol::Frame;
use super::util::clamp;
use self::local_mod::Item;

local p: Pin = Pin{port: 1, bit: 5}
print(temp::sample(72))
```

- `use path::item as alias` binds the item to `alias`; without `as`, the last
  path segment is used.
- `pub use` re-exports the imported name to the parent. Re-exporting a private
  symbol is `E0350`.
- `crate::` starts at the root, `self::` at the current module, and `super::`
  at the parent module. Plain paths start at the current module, so use one of
  the roots for cross-module access.
- `use path::*` imports every public item of a module.
- Grouped imports (`use a::{b, c}`) are not supported yet; write one `use` per
  name.

All module resolution happens before parsing and before either front end runs.
The resolver flattens the tree into one translation unit with collision-free
internal names, then hands the same text to the interpreter, bytecode VM, and
C11 backend. Legacy textual composition is unchanged:

```lusa
import "geo/point.lusa"   -- exact file, textual merge
mod counter               -- legacy textual include (no `;` or `{ ... }`)
```

Older `mod counter` (without a terminator) keeps merging one symbol table; use
`mod counter;` for the namespaced form. Module struct declarations are hoisted
ahead of the module bodies and child modules, so a module can export struct
types used by later declarations. Method sugar
(`function state:add(v) ... end`) targets an existing binding and also works
inside modules.

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
2. Heap values (`table function Vec<T> Box<T>`, and structs that own heap
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
   A `Vec<T>` owns its elements and a `Box<T>` owns its pointee; both release
   them at scope exit, on `Vec:clear`, and when the program ends.
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

### Tasks - `async function`, `await`, and `task`

Async functions are declared with `async function name(...) ... end` or the
compact `async fn name(...) ... end` alias. Calling one does **not** run the
body; it creates a lazy `task<T>` handle:

```lusa
async function add(a: i32, b: i32): i32
    task.yield_now()
    return a + b
end

local t = add(2, 3)
print(task.status(t))      -- pending
print(task.is_done(t))     -- false
print(await t)             -- 5
print(task.is_done(t))     -- true
```

`await expr` drives the task until it settles and evaluates to its first
result. `.await` and `::await` are postfix aliases, and `task.await(t)` is the
method form. Awaiting a completed task returns the stored result again without
re-running the body:

```lusa
print(add(7, 8).await)     -- 15
print(add(9, 1)::await)    -- 10
print(task.await(add(1, 2))) -- 3
```

Inside an async function, `await` suspends the parent cooperatively while the
child task runs. A failed child marks the parent failed and propagates the
error:

```lusa
async function parent(): i32
    local value: i32 = await add(20, 22)
    return value
end

print(await parent()) -- 42
```

The executor surface is:

| call | behavior |
|---|---|
| `task.spawn(t)` | enqueue a task (or a plain function wrapped as one) and return it |
| `task.run()` | drain the queue in rounds; returns how many distinct tasks received their first poll |
| `task.yield_now()` | suspend the current task at an instruction boundary |
| `task.is_done(t)` | true after success or failure |
| `task.status(t)` | `pending`, `running`, `done`, or `failed` (hosted only) |
| `task.await(t)` | drive to completion and return the result |

```lusa
local a = task.spawn(add(1, 2))
local b = task.spawn(add(3, 4))
print(task.run())       -- 2
print(await a, await b) -- 3 7
```

Tasks are cooperative and single-threaded: they never create OS threads, and
`task.yield_now()` is the only preemption point. Calling an async function
creates a fresh task whose arguments are captured at the call site. A task
cannot await itself (`E9999`). Awaiting a non-task value is `E2005`, and
`task.yield_now()` outside an async function is `E0312`/`E9999`.

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
| E0350  | module   | module visibility violation: private item/module accessed outside its subtree, or `pub use` of a private symbol |
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
| E0312  | static   | C11 backend: unsupported construct (struct field type, whole-array or `Vec<T>`-by-value call/return, invalid `&` place, non-scalar volatile element, invalid inline-assembly text, invalid async result/local, or `task.status` in `#[no_std]`) |
| E0313  | static   | C11 backend: zero or one return value only; void/value return mismatch |
| E0316  | unsafe   | C11 backend: raw pointer, volatile, or inline-assembly intrinsic used outside `unsafe` |
| E0317  | static   | C11 backend: `pcall`, `coroutine.resume`, or `string.gmatch` used outside its required local/loop form |
| E0318  | static   | C11 backend: missing method or invalid method receiver |
| E0319  | static   | C11 backend: unsupported generic `for` iterator; invalid native coroutine entry function, handle, operation, or string method |
| E0320  | unsafe   | raw pointer arithmetic outside an `unsafe` block |
| E0321  | unsafe   | raw pointer dereference or inline assembly outside an `unsafe` block |
| E0322  | unsafe   | write through a `*const` pointer |
| E0323  | static   | C11 backend: statically evaluable malformed string pattern or replacement |
| E0324  | static   | C11 backend: replacement capture or `gmatch` binding exceeds the pattern's captures |
| E0325  | static   | C11 backend: invalid `#[no_std]`/`#[entry]`/`#[global_allocator]`/`#[link_section]` placement, duplicate entry/allocator/`#[no_std]`, invalid allocator signature, async entry/allocator, sectioned existing global, or incomplete promoted-local initialization |
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
hooks: `unsafe_nop()` becomes `nop`/`__nop()`, and inline assembly becomes
`LUSA_ASM("text")`, which expands to
`({ __asm__ __volatile__ (text : : ); })` on GCC/Clang (a no-op on toolchains
without inline assembly). The assembly text must be a plain string literal
without quotes, backslashes, or newlines:

```lusa
unsafe do
    unsafe_nop()
    asm!("wfe")          -- preferred spelling since v1.4.0
    unsafe_asm("wfe")    -- backward-compatible alias
end
```

The interpreter and `--vm` emulate the same source safely: structs become
fixed-field buffers, raw pointers are indices into a simulated address space,
and `unsafe_alloc` hands out virtual regions. `unsafe_nop` and inline assembly
are sandboxed no-ops, and dereferencing outside `unsafe` still reports
`E0321`, so bare-metal logic can be debugged before native compilation. See
section 9.11 for the complete `asm!("...")` surface.

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

The legacy forms above stay flat: every file shares one symbol table and
nothing is namespaced. Namespaced modules (`mod name;`,
`mod name { ... }`, `use`, and `pub`) are the Module 23 layer described in
section 3. They run after this textual expansion and before parsing, so the
same expanded unit reaches the interpreter, VM, and C11 backend. The two
layers compose: an imported file may declare namespaced modules, and a module
file may use legacy `import`.

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

### 9.11 Inline assembly and hardware controls (Module 18)

`asm!("...")` emits a raw assembly statement. It is available only inside
`unsafe` and accepts one plain string literal:

```lusa
unsafe do
    asm!("cli")           -- disable maskable interrupts (ring 0)
    asm!("nop; nop")      -- several instructions per string
    asm!("sei")           -- enable maskable interrupts
end
```

The macro syntax is parsed by both front ends and desugars to an ordinary
`asm("...")` call, so the interpreter, bytecode VM, and C11 backend share one
argument check and one safety gate. Native output is:

```c
(void)(LUSA_ASM("nop; nop"));
```

`LUSA_ASM` is defined once in the generated preamble:

```c
#if defined(__GNUC__) || defined(__clang__)
#define LUSA_ASM(text) ({ __asm__ __volatile__ (text : : ); })
#else
#define LUSA_ASM(text) ((void)0)
#endif
```

- GCC and Clang emit the raw instruction sequence as a volatile statement.
  MSVC and other compilers use the no-op definition, so the same program still
  builds; it simply does not execute the instructions.
- The interpreter and `--vm` treat `asm!` as a safe no-op. Loops, branches,
  and simulated register values keep working, which makes hardware sequencing
  testable without privilege.
- Used outside `unsafe`, the C backend reports `E0316` and the dynamic engines
  report `E0321`.
- The argument must be a non-empty string literal. Interpolation and non-string
  arguments are parse errors (`E0101`); the native checker also rejects empty
  or unsafe text with `E0312`. Quotes, backslashes, `\n`, `\r`, and NUL are
  rejected because the text is embedded in a C string literal.
- `unsafe_asm("...")` remains supported as a backward-compatible alias and
  lowers through the same emitter.
- `asm!` is statement-like and returns no value. Use `unsafe_nop()` for a
  portable single no-op and `asm!` for target-specific instruction sequences.

### 9.12 Bare-metal entry points and linker sections (Module 19)

`#[no_std]` is a standalone file-level statement and must be the first
statement in the file. It selects a freestanding C11 translation unit with no
hosted runtime, no `int main`, no coroutine runtime, no pattern matcher, and no
allocator. The preamble keeps only `<stdint.h>`, `<stdbool.h>`, `<stddef.h>`,
the assembly/packing/section macros, checked integer helpers, and a minimal
error flag. Duplicate or non-leading `#[no_std]` and nested use are `E0325`.

In `#[no_std]` mode every top-level `local` is promoted to a file-scope global,
so helpers and the entry function can share it. Promotion rewrites the
declaration into an assignment and zero-initializes typed globals that have no
initializer. If a promoted declaration initializes only some of its names, the
backend reports `E0325`.

`#[entry]` marks one top-level, parameterless function as the exported startup
symbol:

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

The wrapper exports the function's own name (`_start` here) and normally
returns `void`. A function named `main` is special-cased to `int main(void)`
returning `0`, because C and macOS Clang require `main` to return `int`;
top-level statements run in `static int lusa_program_body(void)` before the
entry call. In hosted programs the wrapper then drops globals and calls
`lusa_free_allocs()`. Duplicate `#[entry]` markers, entry functions with
parameters, and `#[entry]` on a method are `E0325`. An entry may be used with or
without `#[no_std]`; when present, no separate `int main` is emitted (a `main`
entry is itself the process entry).

`#[link_section = "..."]` is accepted on top-level functions, top-level
`local` declarations, and top-level implicit globals:

```lusa
#[link_section = ".text.boot"]
function boot() : void
end

#[link_section = ".data.state"]
local counter: u32 = 7

#[link_section = ".bss.ready"]
ready = false
```

- Section names are 1–255 ASCII bytes from `[A-Za-z0-9_.$-]`; empty names,
  invalid characters, and oversized names are parser errors (`E0101`).
- A sectioned top-level `local` is promoted to a file-scope global. The
  attribute may only annotate a newly declared plain global; placing it on an
  existing global is `E0325`.
- Nested locals and assignments, struct declarations, and other statements
  cannot carry `#[link_section]` (`E0325` or `E0101` depending on placement).
- The emitter writes a comment showing the GNU lowering and calls a portable
  macro:

  ```c
  /* #[link_section = ".text.boot"] -> __attribute__((section(".text.boot"))) */
  LUSA_LINK_SECTION(".text.boot")
  static void f1(LusaCoro *lusa_co) { ... }
  ```

  GCC/Clang define `LUSA_LINK_SECTION(name)` as
  `__attribute__((section(name)))`; MSVC expands it to nothing because it has
  no GNU code-section attribute.

The dynamic engines accept `#[no_std]`, `#[entry]`, and `#[link_section]` and
ignore their native effects. `#[no_std]` runs `#[entry]` as an ordinary
function, so programs remain testable with `run` and `--vm`. The freestanding
subset supports scalars, structs, raw pointers, `asm!`, and control flow;
strings, tables, `print`, coroutine APIs, and `unsafe_alloc` still require the
hosted runtime.

### 9.13 Heap allocation and dynamic collections (Module 20)

The C11 backend routes every collection allocation through one heap contract.
`#[global_allocator]` marks either half of the pair; a program may provide one
or both halves:

```lusa
#[global_allocator]
function my_alloc(size: u32, align: u32): *mut u32
    unsafe do
        return unsafe_alloc(size)
    end
end

#[global_allocator]
function my_dealloc(ptr: *mut u32, size: u32, align: u32): void
end
```

- The allocator signature is `(size: u32, align: u32): *mut u32`; the
  deallocator signature is `(ptr: *mut u32, size: u32, align: u32): void`.
- Duplicate allocators or deallocators, a wrong signature, a method receiver,
  or combining `#[global_allocator]` with `#[entry]` is `E0325`.
- With no user allocator, hosted programs call `malloc`/`free`. In
  `#[no_std]` mode the default is a static 64 KiB bump arena
  (`LusaHeapArena`) with a no-op deallocator, so vectors and boxes work
  without libc.
- The dispatch is emitted after function prototypes, so a custom allocator may
  call other user functions.

`Vec<T>` is a growable, heap-backed, 0-based sequence. Array-literal syntax
constructs a vector when the target type is `Vec<T>`:

```lusa
local values: Vec<i32> = [4, 8, 15]
values:push(16)
print(values:len(), values:capacity())   -- 4 8
print(values[0], values:pop())           -- 4 16
values:reserve(32)
print(values:capacity())                 -- 32
values:clear()
print(values:len(), values:is_empty())   -- 0 true
```

- `push(value)` appends and transfers ownership of heap elements.
- `pop()` returns the last element (the element type's default on an empty
  vector) and decrements the length.
- `len()`, `capacity()`, and `is_empty()` report vector state; `#values` is
  also the length.
- Capacity starts at 0 and grows geometrically from 4 (`4, 8, 16, ...`).
  `reserve(n)` grows to at least `n`; `clear()` drops elements but keeps the
  current allocation.
- Indexing and index assignment use `values[i]` with `0 <= i < len`.
- `Vec<T>` values cannot be passed or returned by value yet (`E0312`); pass a
  raw pointer instead.
- Owned elements (`Vec<string>`, structs, nested vectors) are released by the
  generated per-element drop helper on scope exit, on `clear`, and at program
  end.

`Box<T>` is an owned pointer to one heap value. It is initialized implicitly
from a value or explicitly with `Box(value)`; `nil` clears it without
allocating:

```lusa
local answer: Box<i32> = 41
print(*answer)              -- 41
*answer = *answer + 1
print(*answer)              -- 42

local message = Box("lusa")
print(*message)             -- lusa
```

Dereferencing or assigning through a Box does **not** require `unsafe`; the
Box is the owner. Native `Box<T>` lowers to `T *`, so it stays layout-
transparent and can appear in struct fields. Boxed strings and nested structs
are dropped through `lusa_drop_box_<tag>`.

The interpreter and bytecode VM simulate both collections without a real
allocator. A `Vec<T>` is a 0-based table with fallback methods (`push`, `pop`,
`len`, `capacity`, `is_empty`, `reserve`, `clear`); real fields shadow the
fallback. A `Box<T>` is a tagged table, and annotation-driven construction wraps
plain initializers so `local b: Box<i32> = 5; *b` behaves identically in all
engines. Raw numeric pointers keep the `unsafe` dereference gate.

```sh
lusalang run examples/dynamic_heap.lusa
lusalang run examples/dynamic_heap.lusa --vm
lusalang run examples/dynamic_heap.lusa --native
lusalang build examples/dynamic_heap.lusa --emit-c
```

`tests/module20.rs` covers vector growth, capacity, pop/clear/reserve, Box
dereference and writes, `Vec<string>`/`Box<string>` RAII, custom allocator
dispatch, freestanding arena emission, allocator diagnostics, and native +
dynamic parity.

### 9.14 Safe error handling (Module 21)

`Result<T, E>` and `Option<T>` are built-in sum types shared by the interpreter,
bytecode VM, and C11 backend. `Result` carries either `Ok(value)` or
`Err(error)`; `Option` carries either `Some(value)` or `None`:

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

- Annotations are canonicalized to lowercase `result<T,E>` and `option<T>`;
  both `Result<...>`/`Option<...>` and the lowercase spellings are accepted.
- Constructors infer their payload types: `Ok(5)` is `result<i32,nil>`,
  `Err("x")` is `result<nil,string>`, `Some(v)` is `option<T>`, and `None` is
  `option<nil>`. A source-side `nil` payload is a wildcard when stored into a
  fully typed `Result`/`Option`.
- Bare `None` lowers to a zero-argument `None()` call in both parsers unless it
  is followed by `(`, so the value is freshly constructed.
- `nil` cannot be used to infer a payload (`E0308`); annotate the destination or
  pass a typed value.

The `?` operator short-circuits a function. On `Ok(value)`/`Some(value)` it
evaluates to the payload; on `Err(error)`/`None` it returns from the enclosing
function with that variant:

```lusa
function double(n: i32): Result<i32,string>
    local value: i32 = parse(n)?
    return Ok(value + 1)
end

function plus_ten(n: i32): Option<i32>
    local value: i32 = maybe(n)?
    return Some(value + 10)
end
```

- `?` requires an enclosing function that returns the same sum kind
  (`Result` with `Result`, `Option` with `Option`); otherwise `E0312`.
- Only the error side must be compatible. A `Result<Point,string>` can be
  propagated by a function returning `Result<i32,string>`; the success payload
  of the operand becomes the expression value.
- `Ok(...)?` with a `nil` error payload is allowed; the failure branch is
  emitted but unreachable.
- In the dynamic engines `?` clones the payload and leaves the operand usable.
  In C11 the operand is cloned when it is a place or non-owning expression and
  moved when it is owned; the success payload is taken and the sum is dropped
  before the value is used.
- In the VM, `?` lowers to `Instr::Try`, which truncates the shared stacks to
  the frame base, pushes the cloned propagated sum, and returns from the frame.

`match` supports the constructors directly. The scrutinee is not moved, and
each arm binds the payload of its variant:

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

Patterns are `Ok(binding)`, `Ok(_)`, `Err(binding)`, `Err(_)`,
`Some(binding)`, `Some(_)`, and `None`. Both parsers lower each arm to a hidden
temporary check (`is_ok`/`is_err`/`is_some`/`is_none`) plus an `unwrap` or
`unwrap_err` binding, so duplicate or unreachable arms follow the existing
`match` rules.

Combinators are methods on the sums. Calling one on the wrong variant raises
the catchable runtime error `E9999` (for example
`called \`Result::unwrap()\` on an \`Err\` value: boom`):

| type     | methods |
|----------|---------|
| `Result` | `is_ok`, `is_err`, `unwrap`, `unwrap_err`, `unwrap_or`, `expect` |
| `Option` | `is_some`, `is_none`, `unwrap`, `unwrap_or`, `expect` |

```lusa
print(Ok(5):unwrap_or(9), Err("bad"):unwrap_or(9))  -- 5 9
print(Some(2):unwrap_or(0), None:unwrap_or(0))      -- 2 0
print(Ok(7):expect("ok"), Some(8):expect("some"))   -- 7 8
```

- `unwrap_or(fallback)` checks the fallback against the success payload; a
  one-sided constructor receiver (`Err("bad")`, `None`) specializes the payload
  from the fallback.
- `expect(message)` behaves like `unwrap` but raises `E9999` with the supplied
  message on the wrong variant.
- Printing formats sums as `Ok(value)`, `Err(error)`, `Some(value)`, and
  `None`; payloads must themselves be printable.

In the C11 backend, a `Result<T,E>` lowers to
`LusaResult { bool is_ok; void *ok; void *err; }` and an `Option<T>` to
`LusaOption { bool is_some; void *value; }`. Payloads are heap-boxed through the
module 20 heap contract, and every instantiation receives
`lusa_result_new_ok_<tag>`, `lusa_result_new_err_<tag>`, `lusa_result_take_*`,
`lusa_result_get_*`, `lusa_result_clone_<tag>`, `lusa_result_drop_<tag>`, and
the matching `lusa_option_*` helpers. Sum parameters are owned by the callee and
dropped there; call sites clone non-owning arguments and move owned ones.
Supported payloads are scalars, `string`, structs, `Vec<T>`, `Box<T>`, and
nested sums. Fixed arrays are rejected with `E0301`.

In `#[no_std]` builds, sums use the static bump arena and scalar or struct
payloads; strings are unavailable, and the `lusa_print_result_*`/
`lusa_print_option_*` helpers are omitted. `?` still lowers to the same
early-return checks.

```sh
lusalang run examples/error_handling.lusa
lusalang run examples/error_handling.lusa --vm
lusalang run examples/error_handling.lusa --native
lusalang build examples/error_handling.lusa --emit-c
```

`tests/module21.rs` covers constructors and combinators, `?` through branches
and loops, struct/`Vec`/`Box`/nested payloads, sum parameters and fields,
`match` binding and returns, freestanding scalar sums, typed diagnostics,
runtime wrong-variant errors, and native + dynamic parity.

### 9.15 Async tasks and stackless coroutines (Module 22)

Both parsers accept `async function` and the compact `async fn` alias. Inside
the checker an async signature returns `task<T>` (or `task<void>`), and every
call site sees that handle type. The C11 backend lowers it without threads:

- A constructor `static LusaTask *f(...)` allocates a
  `LusaTaskState_f` struct through `lusa_task_new`, copies the arguments into
  the hoisted parameter fields, and returns `&self->base` **without running the
  body**.
- Every local in the async body is hoisted into the task struct. A generated
  `lusa_task_step_f(LusaTask *base)` enters through `switch (base->state)` and
  jumps back to saved `case N:` labels on resume.
- `await e` evaluates the child constructor once, stores it in
  `base->awaiting`, and returns `LUSA_TASK_WAIT` until the child reports done.
  The resumed case reads the typed result through `lusa_task_result_*`, and a
  failed child copies its error and returns `LUSA_TASK_FAIL`.
- `task.yield_now()` stores the next state and returns `LUSA_TASK_YIELD`.
  `return value` fills the `base->value` union and returns `LUSA_TASK_DONE`.
- `task.spawn(t)` enqueues a task, `task.run()` drains the queue in rounds, and
  `task.is_done(t)`/`task.status(t)` query the state. `task.status` is omitted
  in `#[no_std]` because it returns a string.
- `task.await(t)` and top-level/synchronous `await` use `lusa_task_drive`,
  which runs a task to completion inline. Parent tasks awaiting children are
  driven cooperatively, so no call blocks an OS thread.

Statement-level await forms supported by the emitter are:

```lusa
local value: i32 = await child()   -- Local initializer
value = await child()              -- assignment to a name
return await child()               -- return value
await child()                      -- standalone call statement
```

Repeated await in `while` loops works; the state machine keeps all hoisted
locals across suspensions. Async functions may return scalars, `string`,
tables, and pointers (the result union stores them in `ptr`). The current
limits are:

- async methods, async `#[entry]` functions, and async
  `#[global_allocator]` functions report `E0312`/`E0325`;
- struct-by-value async results report `E0312`;
- fixed-array locals inside async functions report `E0312`;
- `await` and `task.yield_now()` inside `for ... in` loops report `E0312`
  (use `while`), because the native iterator state is a stack temporary;
- the state machine does not emit drops for hoisted owned locals or parameters
  yet, so async bodies should prefer scalars and pointers for long-lived work.

In hosted builds the task runtime uses the module 20 heap contract
(`lusa_heap_alloc`/`lusa_heap_dealloc`) and the string helpers.
`#[no_std]` builds instead include `LUSA_TASK_ARENA_BYTES` (16 KiB by default),
a static bump arena, and the same cooperative queue. No `malloc`, libc,
`pthread`, fibers, or `CreateThread` calls are emitted.

```sh
lusalang run examples/async_cooperative.lusa
lusalang run examples/async_cooperative.lusa --vm
lusalang run examples/async_cooperative.lusa --native
lusalang build examples/async_cooperative.lusa --emit-c
```

`tests/module22.rs` checks the generated `LusaTaskState_*` structs and
`switch (base->state)` step functions, absence of thread primitives, lazy
execution, awaited children, `.await`/`::await`/`task.await`, `task.run` polling
counts, freestanding arena emission, typed diagnostics, and native + dynamic
parity.

### 9.16 Cross-compilation and Renode targets (Module 24)

`lusalang build` selects an output machine with `--target`. Without the flag it
builds the host executable exactly as before:

```sh
lusalang build examples/baremetal_renode_uart.lusa --target riscv32-virt
lusalang build examples/baremetal_renode_uart.lusa --target riscv64-virt
lusalang build examples/baremetal_renode_uart.lusa --target=riscv32-unknown-elf
lusalang build examples/baremetal_renode_uart.lusa --target host
lusalang build --help
```

| target | accepted spellings | triple | ISA/ABI | default output |
|--------|--------------------|--------|---------|----------------|
| host | `host`, `native`, omitted | host | host compiler | `.exe` on Windows, no extension elsewhere |
| 32-bit Renode `virt` | `riscv32-virt`, `riscv32-unknown-elf` | `riscv32-unknown-elf` | `rv32imac` / `ilp32` | `.elf` |
| 64-bit Renode `virt` | `riscv64-virt`, `riscv64-unknown-elf` | `riscv64-unknown-elf` | `rv64imac` / `lp64` | `.elf` |

Names and triples are matched case-insensitively. An unknown target exits with
status 2 and prints the available names.

#### Entry and freestanding requirements

Cross targets accept only programs that declare `#[no_std]` and an `#[entry]`
function. Both conditions are checked before compiler discovery:

- A hosted program fails with `target ... requires a #[no_std] program;
  hosted programs need the host C runtime`.
- A `#[no_std]` program without `#[entry]` fails with `target ... requires an
  #[entry] function so the reset stub has an entry point`.

The generated C exports the entry wrapper under the user's chosen name. The
cross build passes `-D<entry>=lusa_entry` (unless the name is already
`lusa_entry`), so any `#[entry]` name links against the fixed startup stub.
The rest of the image is compiled with `-std=c11 -O2 -ffreestanding
-fno-builtin -fno-stack-protector -mcmodel=medany` plus the target ISA/ABI
flags.

`renode/startup_riscv.S` is assembled into the image and linked first at
`0x80000000` by `renode/linker_riscv.ld`:

```asm
_reset:
    la sp, _stack_top          # 64 KiB stack placed after .bss
    la t0, _bss_start
    la t1, _bss_end
1:  bgeu t0, t1, 2f
    sd zero, 0(t0)             # sw on RV32
    addi t0, t0, 8
    j 1b
2:  call lusa_entry
3:  j 3b
```

The final link uses `-nostdlib -nostartfiles -Wl,-T,linker.ld
-Wl,--gc-sections -Wl,--build-id=none ... -lgcc`. `-lgcc` supplies compiler
helpers only; no libc, `crt0`, `malloc`, `printf`, or hosted startup code is
linked. The linker script places `.text.init` first, then `.text`, `.rodata`,
`.data`, and `.bss`, and reserves the stack after BSS.

#### Cross-compiler discovery

Cross builds use their own chain and never fall back to the host `LUSA_CC`/`CC`
variables:

1. `--cc <path>`
2. `LUSA_RISCV_CC`
3. `LUSA_CROSS_CC`
4. `riscv32-unknown-elf-gcc`, `riscv-none-elf-gcc`,
   `riscv64-unknown-elf-gcc`, or `riscv64-elf-gcc` on `PATH` (target-specific
   order)

MSVC is rejected for cross targets. If no compiler is found, the build prints
the target, the candidate names, the environment variables, and the `--cc`
option. If compilation or linking fails, the generated C is saved next to the
requested output as `<output>.c` for inspection.

#### Renode platform and UART

The shipped platforms mirror Renode's `riscv_virt` memory map:

| region | address | notes |
|--------|---------|-------|
| RAM | `0x80000000`, 128 MiB | code, data, BSS, stack |
| NS16550 UART | `0x10000000` | `uart0`, IRQ to PLIC source 3 |
| PLIC | `0x0C000000` | 52 sources, one context |
| CLINT | `0x02000000` | 10 MHz time provider |

`renode/lusa_riscv32_virt.repl` uses `CPU.RiscV32` with `cpuType: "rv32imac"`;
`renode/lusa_riscv64_virt.repl` uses `CPU.RiscV64` with `cpuType: "rv64imac"`.
Both enable the RISC-V privileged architecture 1.10 and hart 0.

Lusa does not have a `u8` scalar yet, so the example performs 32-bit volatile
stores to the NS16550 transmit register. Renode accepts the 32-bit access:

```lusa
#[no_std]

function uart_putc(byte: u32): void
    unsafe do
        local uart: *mut u32 = 0x10000000
        uart.write_volatile(byte)
    end
end

#[entry]
function _start(): void
    local message: [u32; 38] = [
        76, 85, 83, 65, 32, 82, 69, 78, 79, 68, 69, 32, 85, 65, 82, 84,
        32, 79, 75, 10,
        108, 117, 115, 97, 32, 118, 105, 114, 116, 32, 118, 50, 46, 48,
        46, 48, 10, 10
    ]

    local i: u32 = 0
    while i < 38 do
        uart_putc(message[i])
        i = i + 1
    end

    while true do
    end
end
```

On real hardware, poll the line-status register (bit 5, THRE) before each
write or use the vendor UART driver. The example intentionally omits that poll
because Renode's model accepts back-to-back stores.

#### Running Renode

The monitor templates live in `renode/` and use three substitutions:

| placeholder | replacement |
|-------------|-------------|
| `@PLATFORM@` | `@<absolute forward-slash path to the .repl>` |
| `@ELF@` | `@<absolute path to the .elf>` |
| `@UART_LOG@` | plain absolute path to the UART log file (quoted by the template) |

The leading `@` on the platform and ELF substitutions is Renode's file
reference marker; dropping it produces `Could not tokenize here`. The rendered
script is:

```text
using sysbus

mach create "lusa-riscv32-virt"
machine LoadPlatformDescription @<platform>

sysbus LoadELF @<elf>
sysbus.uart0 CreateFileBackend "<uart.log>" true

emulation RunFor "00:00:01"
quit
```

Run it headlessly with:

```sh
renode --disable-gui --console --plain --hide-log run.resc
```

`emulation RunFor` advances virtual time directly; do not call `start` first in
a script that captures a fixed window and then quits.

#### Tests and CI

```sh
cargo test --test module24_renode -- --test-threads=1 --nocapture
```

The static tests (platform/UART definitions, freestanding C, hosted/no-entry
rejections) always run. The compile-and-boot tests skip with a printed reason
when no RISC-V compiler or Renode executable is found. To execute them locally:

```powershell
$env:LUSA_RISCV_CC = "C:\tools\riscv-none-elf-gcc\bin\riscv-none-elf-gcc.exe"
$env:LUSA_RENODE = "C:\tools\renode\renode.exe"
cargo test --test module24_renode -- --test-threads=1 --nocapture
```

Set `LUSA_KEEP_RENODE_ARTIFACTS=1` to keep the generated platform, script,
ELF, and UART log. `.github/workflows/renode.yml` downloads Renode and xPack
RISC-V GCC on Ubuntu, sets both environment variables, and runs the suite.

### 9.17 Native subset limits

Supported: typed and inferred locals, named typed functions, calls, string and
table literals, fixed-size `[T; N]` arrays and local array literals, nested keyed
tables, field reads/writes, zero-based array indexing and element assignment,
colon methods, `pairs`/`ipairs` generic `for` over tables, `#` on strings,
tables, and arrays, `if`, `while`, `repeat`, `do`, `return`, `break`,
assignments, globals, `print`, `clone`, `borrow`, `move`, the compile-time
borrow checker, `unsafe` blocks, static `struct` declarations in brace and bare
form, `#[packed]`/`#[repr(align(N))]` representation attributes,
`#[no_std]`/`#[entry]`/`#[global_allocator]`/`#[link_section = "..."]`
(sections 9.12 and 9.13), struct
literals with defaults (scalar, `string`, `table`, pointer, and previously
declared struct fields), `match`, compile-time `import`/`mod`, namespaced
modules with `mod name;`/`mod name { ... }`, `use` aliases and globs, and
`pub` visibility/re-exports, address-of
(`&place`), raw pointer types (`*mut T`, `*const T`), pointer arithmetic and
dereferences inside `unsafe`, the raw intrinsics above (`unsafe_ptr_cast`,
`unsafe_read_mem`, `unsafe_write_mem`, `unsafe_alloc`, `volatile_read`,
`volatile_write`, `unsafe_nop`, `unsafe_asm`, `asm!("...")`), native `pcall`,
`Vec<T>` (push/pop/len/capacity/is_empty/reserve/clear, indexing, and
element assignment) and `Box<T>` (implicit and `Box(value)` construction,
safe `*b` reads/writes, `nil` clearing) with their RAII drop helpers,
`Result<T,E>`/`Option<T>` (`Ok`/`Err`/`Some`/`None`, combinators, constructor
`match` arms, and `?`) with their RAII helpers, `async function`/`async fn`
with lazy `task<T>` handles, `await`/`.await`/`::await`, and the
`task.spawn`/`run`/`yield_now`/`is_done`/`status` executor surface, the
string-pattern surface in
section 9.7, and the restricted
`coroutine.create/resume/yield/status/close/isyieldable` surface described in
section 9.6.

Not supported by the C emitter: numeric `for`, arbitrary dynamic key
construction in table literals, assignment through a dynamic index, varargs,
interpolated strings, closures/nested functions, forward struct references,
whole-array assignment or arrays passed/returned by value, `&array`, `Vec<T>`
values passed or returned by value (`E0312`), fixed arrays as
`Result`/`Option` payloads (`E0301`), the rest of the dynamic string
library, multiple function return values (`E0313`), dynamic `pcall`
expressions, and general first-class coroutine operations.

A local declaration such as `local message: [u32; 38] = [...]` is supported and
lowers to a C aggregate initializer. A top-level array initializer is still a
whole-array assignment (`E0312`); declare the table inside the function that
uses it, or fill a promoted global element by element.

Native colon methods are static functions
and are not stored as table fields, so `pairs(user)` does not enumerate method
names; keep enumerable data in a separate table when parity with the dynamic
engines matters. The normal `lusalang run` interpreter and `--vm` engine
continue to implement the full dynamic language in the preceding sections.
