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
```

An `undefined global` reads `nil` (Lua-forgiving). Moved slots follow the
rule in section 5.

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
1-based table of integers in `0..255` (the form used by the native backend) or
a string, whose UTF-8 bytes are written unchanged. Missing entries, non-number
bytes, and out-of-range values raise `E2006`; filesystem failures raise
`E2009`.

`io_read_binary` reads any file and returns a dense 1-based table of byte
numbers, so images round-trip exactly:

```lusa
local compiled = Compiler.emit_binary(borrow ast, "windows")
io_write_binary("program.exe", compiled)

local check = io_read_binary("program.exe")
assert(#check == #compiled)
assert(check[1] == 0x4D and check[2] == 0x5A) -- "MZ"
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
