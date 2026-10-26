# Lusa Roadmap: Unification and Refinement

This document is the engineering plan that follows v0.6.1. It turns the next
three language milestones into concrete compiler, runtime, and release work.
The guiding rule is unchanged: every feature must have one documented
semantics and must pass the same tests in the interpreter, the bytecode VM,
and, where supported, the C11 backend.

## Shipped in v0.7.0 - static structs, raw pointers, kernel intrinsics

- **Static structs:** `struct Name { field: type }` lowers to a C typedef with
  natural alignment; field access compiles to direct member offsets.
- **Raw pointers:** `*mut u32`/`*const u32`/`*mut i32`/`*const i32` lower to
  volatile C pointers; `ptr + n` scales by the element size; `*ptr` reads and
  `*ptr = v` writes are volatile.
- **Sandbox emulation:** the interpreter and VM simulate structs as
  fixed-field buffers, pointers as indices into a virtual address space, and
  `unsafe_alloc` as virtual regions, so bare-metal logic debugs dynamically.
- **Intrinsics:** `unsafe_nop`/`unsafe_asm` emit `__asm__ __volatile__` hooks
  on GCC/Clang (barriers elsewhere); `unsafe_alloc` returns tracked buffers.
- **Safety diagnostics:** `E0320` (pointer arithmetic), `E0321` (dereference),
  and `E0322` (write through `*const`) with Cargo-style spans.

## Shipped in v0.8.0 - error recovery and coroutines

- **Native `pcall`:** checked error propagation replaces the fatal
  `lusa_panic` path for catchable runtime errors. Generated functions return
  through their normal RAII drops, and the protected boundary releases
  `unsafe_alloc` buffers created after the call began.
- **Coroutines:** `create`, `resume`, `yield`, `status`, `close`, and
  `isyieldable` run in the interpreter and VM with isolated stacks and shared
  captured state. The native backend uses Windows fibers and POSIX `ucontext`,
  with a hidden `LusaCoro *` parameter on every generated function.
- **Diagnostics and limits:** native `pcall` and `resume` require local
  initializer forms (`E0317`); native coroutine entries are named,
  zero-parameter `i32` functions (`E0319`); the native backend supports zero or
  one return value (`E0313`).

## Shipped in v1.0.0 - compile-time borrow checking and explicit move

- **Static borrow checker:** `compiler_bootstrap.lusa` tracks moved variables,
  multiple immutable aliases, mutable method receivers, and alias lifetimes
  without emitting runtime checks.
- **Stable diagnostics:** `E1001` use-after-move, `E1002` borrow conflicts,
  and `E1004` writes through immutable aliases include source spans and borrow
  notes.
- **Explicit transfer:** `move x` is parsed, evaluated, byte-compiled, and
  lowered by the C11 backend in all engines.
- **VM borrow support:** the bytecode VM creates immutable aliases, enforces
  borrow rules on field/index writes, and releases aliases at scope and loop
  exit.
- **Unsafe boundary:** `unsafe do ... end` bypasses borrow mutation checks for
  raw pointer and hardware access while move tracking remains active.

## Shipped in v0.9.0 - string patterns and native text processing

- **One bounded matcher:** `find`, `match`, `gmatch`, and `gsub` share the
  semantics of `src/pattern.rs`; native builds append the C11 implementation
  from `src/pattern_runtime.c`.
- **Lua syntax:** classes, sets, anchors, captures, position captures,
  backreferences, `%b`, `%f`, and `*`/`+`/`-`/`?`.
- **Static diagnostics:** literal malformed patterns and replacements report
  `E0323`; out-of-range replacement or `gmatch` captures report `E0324`.
- **Runtime safety:** dynamic failures raise `E2010`, budget exhaustion raises
  `E2011`, and both are catchable with `pcall`.
- **RAII text transforms:** native `gmatch` state, loop captures, and printed
  `match`/`gsub` temporaries are released by lexical scope drops.
- **UTF-8 native literals:** string literals are emitted as UTF-8 bytes so
  multibyte captures round-trip correctly.

## Baseline after v1.0.0

| Area | Interpreter | Bytecode VM | C11 backend |
| --- | --- | --- | --- |
| Keyed tables and nested fields | yes | yes | yes |
| Colon methods | yes | yes | yes (static dispatch) |
| `pairs` / `ipairs` | yes | yes | yes (native loops) |
| Static structs | simulated buffers | simulated buffers | C typedef + member access |
| Raw pointers | simulated addresses | simulated addresses | volatile C pointers |
| `pcall` | yes (`src/builtins.rs`) | yes | yes (named-function subset) |
| Coroutines | yes | yes | yes (fibers/`ucontext`, restricted subset) |
| String patterns | full `find`/`match`/`gmatch`/`gsub` | full | direct C runtime calls (restricted surface) |
| Ownership checks | runtime registry (`src/value.rs`) | runtime registry | static subset (`compiler_bootstrap.lusa`) |
| Explicit `move` / `clone` | yes | yes | yes (static move analysis) |

## Module 12 - Error Recovery and Coroutines (shipped in v0.8.0)

### 12.1 Native protected calls (`pcall`)

The dynamic `pcall(f, ...)` contract stays unchanged: success returns `true`
followed by the callee's results; any error returns `false` and an
`"E-code: message"` string. Native builds now expose a statically checked,
local-initializer variant with the same error string shape and no skipped RAII
drops.

**Runtime design**

1. Catchable runtime errors use **checked error propagation**. Helpers record
   the first error in a flag; generated functions test the flag after each
   statement, run their normal drops, and return a default value. No
   `setjmp`/`longjmp` frame stack is used.
2. The protected boundary clears the flag before the call, records an
   `unsafe_alloc` mark, and inspects the flag afterwards. A failed call frees
   buffers allocated after the mark and converts the recorded error into an
   owned `LusaString`.
3. OOM, bounds, division-by-zero, and table-index failures all use the same
   checked path. Ordinary string/table locals are released by generated RAII
   drops as the callee returns.
4. Uncaught errors still print `lusa runtime: <E-code>: <message>` to stderr
   and exit with status `1`; no error bypasses cleanup.

**Code generation and type checking**

- Native `pcall` requires a direct named function and a local initializer:
  `local ok, err = pcall(f, ...)` or
  `local ok, result, err = pcall(f, ...)`. Wrong shapes report `E0317`;
  arguments are checked against the target signature.
- The emitter wraps the direct call, stores the success flag and
  result/error in temporaries, releases post-mark allocations on failure, and
  continues in the surrounding scope.
- The native backend keeps the zero-or-one return rule (`E0313`); the dynamic
  engines keep the full Lua-style result list.

**Coroutines**

- `coroutine.create`, `resume`, `yield`, `status`, `close`, and
  `isyieldable` are implemented in the interpreter and the VM. A coroutine
  owns an isolated stack and shares captured cells with its closure.
- Native coroutines use Windows fibers and POSIX `ucontext`. Every generated
  function carries a hidden `LusaCoro *` parameter, so helper calls can yield
  through ordinary frames without converting the program into a state machine.
- Closing a suspended coroutine resumes it with a closing flag; each statement
  boundary unwinds the frame through the normal drops. Closing an unstarted
  coroutine is allowed.
- Errors inside a coroutine mark it dead, drop its locals, and are returned
  through `resume` as `false` plus the error string.

**Acceptance criteria**

- Native and dynamic `pcall` agree on success/failure booleans and E-codes for
  shared error cases.
- A native program that throws after allocating strings, tables, and
  `unsafe_alloc` buffers cleans up without terminating the process.
- `break`, `return`, close, and error paths drop loop-body and coroutine locals
  exactly once.
- Coroutine tests cover yield/resume values, close/drop, nested coroutines,
  errors, and VM/interpreter parity.
- Release gate: full three-OS CI plus native error-unwind and coroutine tests.

**Risks and limits**

- Native coroutine stacks are platform resources (fibers/`ucontext`); keep the
  first release to named zero-parameter `i32` entry functions and one `i32`
  payload per resume/yield.
- POSIX `ucontext` is deprecated on some platforms but remains available on
  the supported Linux/macOS targets; CI covers both architectures.
- Multiple native return values still require a result struct and remain out
  of scope (`E0313`).

## Module 13 - String Utilities and Pattern Matching (shipped in v0.9.0)

### 13.1 One bounded pattern engine

`string.find`, `string.match`, `string.gmatch`, and `string.gsub` now share one
matcher instead of literal-only comparisons. The interpreter and VM call
`src/pattern.rs`; native builds append the dependency-free C11 implementation
from `src/pattern_runtime.c` to every generated translation unit.

**Surface API**

- `string.find(s, pattern [, init [, plain]])` -> start, end, captures
- `string.match(s, pattern [, init])` -> captures (or the whole match)
- `string.gmatch(s, pattern)` -> stateful iterator
- `string.gsub(s, pattern, replacement [, limit])` -> result, count
- Lua-compatible classes and quantifiers: `%a %c %d %g %l %p %s %u %w %x`,
  `%bxy`, `%f[set]`, captures `()`, backreferences `%1`, `.`, `*`, `+`, `-`,
  `?`, anchors `^`/`$`, and escaped literals.

**Semantics**

1. Matching operates on Unicode scalar values. Pattern indices and `find`
   results count characters, not UTF-8 bytes; `plain = true` preserves the
   literal-search escape hatch.
2. Both implementations enforce the same depth (200) and step (1,000,000)
   budgets. A failed branch restores its capture state, so backtracking cannot
   leak captures into a sibling branch.
3. Malformed patterns and replacements raise `E2010`; budget exhaustion raises
   `E2011`. Dynamic failures are catchable with `pcall`.
4. Captures produce owned strings. `gmatch` state owns the source and compiled
   pattern; native loop captures and iterators are released through RAII on
   `break`, `return`, scope exit, and `pcall` unwinding.
5. Native constant patterns are validated before C emission (`E0323`), and
   statically visible replacement or `gmatch` capture overflows report
   `E0324`. Dynamic patterns use the same runtime matcher as the dynamic
   engines.

The native surface is intentionally smaller than the dynamic one: `find`
returns `0` on failure, `match` returns the first capture, `gsub` accepts string
replacements only, and `gmatch` is restricted to direct generic-`for`
iterators. These differences are documented in
[`LANGUAGE.md`](LANGUAGE.md#97-native-string-patterns-module-13).

**Acceptance criteria**

- One conformance corpus runs on the interpreter, VM, and native builds.
- `find`/`gsub` remain backward compatible when `plain = true`.
- Native and dynamic engines agree on captures, anchors, backreferences,
  balanced/frontier matches, and backtracking results.
- No pattern can overflow the native stack; budget tests are deterministic.
- `gmatch` iterators and `gsub` temporaries are leak-free across `break`,
  `return`, and `pcall`.

**Risks**

- Byte-versus-code-point semantics differ from stock Lua; the rule is published
  in the release notes and `plain` remains available for literal byte-oriented
  workflows.
- Backtracking performance depends on the budget; nested quantifiers should be
  simplified when `E2011` appears.

## Module 14 - Advanced Lifetimes (shipped in v1.0.0)

### 14.1 Static borrow checker

`compiler_bootstrap.lusa` now performs ownership analysis on every typed
function before C emission. It tracks moved variables and live aliases while
the dynamic engines enforce the same rules through the runtime `Ownership`
registry.

**Analysis design**

1. Each binding receives an analysis state with `moved`, immutable-borrow
   sites, and an optional mutable method-receiver borrow.
2. Consuming contexts (locals, assignments, user-function/method arguments,
   returns, table literals, and explicit `move`) mark heap values moved.
   `clone` and `borrow` are non-consuming.
3. Multiple immutable aliases are recorded together and released at lexical
   scope exit. Moving or overwriting an aliased owner reports `E1002`; writing
   through an immutable alias reports `E1004`.
4. Method calls begin a mutable receiver borrow before checking arguments.
   Reads, moves, and borrows of the receiver during that window report
   `E1002`.
5. `unsafe do ... end` bypasses borrow/mutation checks for raw pointer and
   hardware access; move tracking still reports `E1001`.

**Explicit move**

- `move x` is a real expression in `parser.lusa`, the Rust front end, the
  reference compiler, the interpreter, the bytecode VM, and the C11 emitter.
- Assignment and explicit `move` both transfer ownership for heap values;
  `clone x` remains the explicit deep-copy form.
- The lexer reserves `move`; internal self-hosted helpers were renamed to
  `transfer` so the keyword is unambiguous.

**Diagnostics**

- Stable codes: `E1001` use after move, `E1002` move/read/write while
  borrowed, and `E1004` write through an immutable alias.
- Diagnostics include the offending source span and a note pointing at the
  borrow site.

**Acceptance criteria**

- The static checker accepts the full existing test suite and examples.
- Interpreter, VM, and native builds agree on `move`, `clone`, borrow
  lifetimes, catchable failures, and unsafe escape behavior.
- Native code generation adds no runtime borrow tracking.
- Borrows end at block/loop exit, including `break`, in the VM.

**Risks**

- The static analysis is variable-focused; nested field-path precision and
  branch-sensitive NLL-style liveness remain future work.
- Dynamic code that mutates tables through aliases may be rejected by static
  rules; keep `unsafe` and clear migration notes.

## Module 15 - Struct literals, match, and compile-time modules (shipped in v1.1.0)

### 15.1 Struct literals and deep RAII

- Both `struct Point { x: i32, y: i32 }` and
  `struct Point x: i32, y: i32 end` declare the same layout.
- `Point{x: 10}` fills omitted scalar fields with zero/`false` and omitted
  `string`/table fields with `nil`. Unknown and duplicate fields report
  `E0309`/`E0302`.
- Fields may be scalars, `string`, `table`/`table<T>`, raw pointers, or
  previously declared structs. Native builds emit a C typedef in declaration
  order; nested structs are embedded by value.
- Each declaration emits `lusa_drop_<Name>` and `lusa_clone_<Name>` helpers
  that recurse through nested struct, string, and table fields.
- Scalar-only structs are Copy in the interpreter and VM; structs that own heap
  fields move. Clones preserve struct identity tags on nested field tables.

### 15.2 Zero-overhead `match`

- `match value with | literal => ... | name => ... | _ => ... end` evaluates
  the scrutinee once.
- Literal arms become `==` branches; a binding arm moves the value into a new
  local; `_` becomes the `else` body. A binding or wildcard arm must be last.
- Both parsers emit a hidden local and an ordinary `if`/`elseif` chain. No
  bytecode instruction, runtime dispatcher, or C arm metadata is generated.
- Ownership is checked independently per arm, so moves cannot leak across
  branches.

### 15.3 Compile-time imports

- `import "file.lusa"` resolves relative to the importing file; `mod a.b`
  resolves `a/b.lusa`, `a/b/mod.lusa`, or `a/b/main.lusa`.
- Expansion is recursive and deduplicated by canonical path, so shared modules
  contribute their definitions once.
- Imports are expanded before parsing by `src/imports.rs`; all files share one
  symbol table. There are no namespaces or export filters.

### 15.4 Inline assembly

- `unsafe_asm("text")` lowers to `LUSA_ASM("text")`, defined as
  `({ __asm__ __volatile__ (text : : ); })` on GCC/Clang and as a no-op
  elsewhere. Module 18 adds the preferred `asm!("text")` spelling.
- The text must be a plain string literal without quotes, backslashes, or
  newlines.

**Acceptance criteria**

- `tests/module15.rs` covers both struct forms, literals/defaults, nested deep
  clone/drop, scalar-only copy semantics, match arms and per-arm `E1001`,
  recursive imports/modules with deduplication, and inline assembly.
- Interpreter, VM, and native builds produce identical output for the shared
  example programs.

**Risks**

- Imports are flat textual includes; large projects may want namespaces,
  selective exports, and explicit module manifests in a future release.
- Forward struct references remain unsupported because the native checker is
  single-pass.

## Module 16 - Fixed arrays, address-of, and pointer arithmetic (shipped in v1.2.0)

### 16.1 Syntax and C11 layout

- `[T; N]` declares a fixed-size array; it remains distinct from `[T]`
  (`table<T>`) and needs a positive literal length.
- `[a, b, c]` builds an array literal. Its type is inferred from the first
  element and must match the annotation exactly in length.
- Access is zero-based: `a[0]` is the first element and `a[i] = v` writes an
  element. `#a` folds to the declared length in native code.
- Native locals, globals, and struct fields use real C declarators such as
  `int32_t samples[4]` and `LusaString *items[2]`; the array is contiguous
  storage, not a heap table.
- Arrays are not passed or returned by value and cannot be assigned as a whole.
  Use element writes, an indexed loop, or a raw pointer.

### 16.2 RAII drop loops

- A fixed array whose element type owns heap storage (`string`, `table`, or a
  heap-bearing struct) is an owning value. The emitter generates a drop loop on
  every normal and error exit:
  `for (int32_t lusa_i = 0; lusa_i < N; lusa_i++) lusa_drop_...`.
- Struct drop and clone helpers apply the same loop to array fields, so nested
  arrays release and deep-copy their elements exactly once.
- Partially initialized arrays are zero-filled by C aggregate initialization,
  and `lusa_drop_*` tolerate `NULL`, so the drop loop is safe.

### 16.3 Address-of and pointer arithmetic

- `&place` accepts a variable, field, index, or dereference and yields
  `*mut T`; `&a[i]` and `&obj.field` lower to C `&(...)` expressions.
- `*ptr` reads and `*ptr = value` writes through the pointer. Both remain gated
  by `unsafe do ... end` with `E0321`; writes through `*const` report `E0322`.
- `ptr + n` and `ptr - n` lower to ordinary C pointer offsets, which scale by
  `sizeof(element)`. Pointer arithmetic outside `unsafe` reports `E0320`.
- The interpreter and VM keep simulating pointers as cells in a process-local
  address map. `&place` allocates a simulated cell, and dereferences read/write
  that cell; they never touch host memory.

**Acceptance criteria**

- `tests/module16.rs` covers fixed arrays, zero-based reads/writes, struct
  array fields, generated drop loops, `&place`, dereference gating, pointer
  arithmetic lowering, and a native build-and-run program.
- Interpreter, VM, and native builds agree on the shared example
  `examples/arrays_pointers.lusa`.

**Risks**

- Dynamic pointer writes do not alias the originating local; only native code
  observes `*p = v` through `&x`. Document this simulation boundary.
- C arrays decay to pointers in function position. Passing fixed arrays by
  value or returning them needs an explicit pointer/out-parameter design in a
  future module.
- Fixed-size arrays are intentionally not iterable with `pairs`/`ipairs`;
  index them with a numeric loop.

## Module 17 - Volatile MMIO and struct representation (shipped in v1.3.0)

### 17.1 Typed volatile access

- `ptr.read_volatile()` and `ptr.write_volatile(value)` add Rust-style typed
  volatile access; `ptr:read_volatile()`/`ptr:write_volatile(value)` are
  accepted for symmetry with colon methods.
- Both parsers desugar the method forms into `volatile_read(ptr)` and
  `volatile_write(ptr, value)`, so the interpreter, VM, and native backend see
  one call shape and share argument checking.
- The element type comes from the pointer (`*mut u32`, `*mut i32`, `*mut i128`,
  `*mut f32`, `*mut bool`); a bare integer address defaults to `u32`. Heap
  elements (`string`, tables, structs) are rejected with `E0312` so volatile
  access cannot bypass RAII.
- Native lowering is a typed lvalue access,
  `(*(volatile T *)(uintptr_t)(ptr))`, which guarantees that polling loops and
  MMIO stores remain observable at `-O2`.
- The intrinsic is `unsafe`-gated: the C backend reports `E0316`, while the
  interpreter and VM report `E0321`. The dynamic engines continue to use the
  process-local simulated address map.

### 17.2 Packed and aligned structs

- `#[packed]` is accepted before a `struct` declaration and removes inter-field
  padding. `#[repr(align(N))]` raises the struct's alignment, with `N` a power
  of two in `1..=4096`.
- The AST records `packed: bool` and `align: Option<u32>` on struct nodes. Both
  parsers validate duplicate attributes, unsupported attributes, and invalid
  alignments before either backend runs.
- The C11 preamble defines `LUSA_PACKED`, `LUSA_PACK_BEGIN`/`LUSA_PACK_END`,
  and `LUSA_ALIGNAS(n)` for MSVC, GCC/Clang, and other C11 compilers.
  Alignment is attached to the first member, which raises the enclosing
  struct's alignment without relying on a non-portable type-attribute position.
- Dynamic engines accept the attributes but keep their fixed-field simulation;
  only native layout changes.

**Acceptance criteria**

- `tests/module17.rs` covers dot/colon/builtin volatile spellings, typed C
  loads and stores, scalar-only elements, value type checking, `unsafe` gating
  in all three engines, packed/aligned emission, attribute and alignment
  diagnostics, dynamic parity, and a native build-and-run.
- `examples/baremetal_gpio.lusa` prints identical output in `run`, `--vm`, and
  `--native`, using `unsafe_alloc` as a simulated peripheral block.

**Risks**

- Native volatile access is only as safe as the address supplied; the backend
  deliberately performs no bounds or validity checking inside `unsafe`.
- `#[packed]` fields may be unaligned, and taking their address is target-
  dependent. Keep packed structs for on-wire/register layouts and copy values
  out before doing aligned work.
- Layout attributes are advisory in the dynamic engines; parity tests compare
  values, not `sizeof` or field offsets.
- The C emitter remains a typed subset; volatile access is limited to scalar
  elements and does not yet support volatile struct copies or array bulk access.

## Module 18 - Inline assembly and hardware controls (shipped in v1.4.0)

### 18.1 `asm!("...")`

- `asm!("text")` is accepted as a statement/expression only inside `unsafe`.
  Both parsers consume the macro syntax and desugar it to `asm("text")`, so
  the interpreter, VM, and C11 backend share one argument check and one gate.
- The C11 emitter lowers the call to `LUSA_ASM("text")`. GCC/Clang define the
  macro as `({ __asm__ __volatile__ (text : : ); })`; MSVC and other
  toolchains keep a no-op definition so the same program still builds.
- The dynamic engines treat assembly as a sandboxed no-op, preserving control
  flow and simulated register state.
- Text validation rejects empty strings, interpolation, quotes, backslashes,
  `\n`, `\r`, and NUL. Parse diagnostics are `E0101`; the native intrinsic
  check uses `E0312`; the unsafe gates are `E0316` (native) and `E0321`
  (dynamic).
- `unsafe_asm("...")` is retained as a backward-compatible alias with
  identical lowering.

**Acceptance criteria**

- `tests/module18.rs` covers macro emission, multiple instructions, `unsafe`
  gating in all engines, malformed input and arity, shared lowering with
  `unsafe_asm`, dynamic control flow, and a native build/run.
- `examples/baremetal_asm.lusa` prints identical output in `run`, `--vm`, and
  `--native`.

**Risks**

- Privileged instructions (`cli`, `sei`, `hlt`) fault or trap outside ring 0.
  They are covered by emission tests; runnable examples use `nop`.
- The text is passed through verbatim to the host compiler. Lusa validates
  string safety, not assembly syntax or target support; the host assembler
  reports those errors.
- MSVC and toolchains without GNU-style inline assembly compile the call as a
  no-op, so behavior differs from GCC/Clang by design.
- Operand constraints and clobbers are not modeled yet; the current surface is
  raw instruction text without input/output operands.

## Module 19 - Entry points, freestanding mode, and linker sections (shipped in v1.5.0)

### 19.1 Custom entry points

- `#[entry]` marks one top-level, parameterless function as the exported
  startup symbol. The wrapper keeps the Lusa function name (for example
  `void _start(void)`), normally returns `void`, and calls the internal `f<N>`
  function. A function literally named `main` is emitted as `int main(void)`
  returning `0`, as the C standard requires.
- When an entry exists, the backend emits no `int main`. Top-level statements
  run in `static int lusa_program_body(void)` before the entry call. Hosted
  entries then run global drops and `lusa_free_allocs()`; `#[no_std]` entries
  skip both.
- Duplicate entries, entry functions with parameters, and `#[entry]` on a
  method are `E0325`.

### 19.2 `#[no_std]`

- `#[no_std]` is a standalone first statement. It selects a minimal
  freestanding preamble (`<stdint.h>`, `<stdbool.h>`, `<stddef.h>`, asm and
  layout macros, checked integer helpers, and an error flag) and omits the
  hosted runtime, `main`, coroutines, the pattern matcher, and the allocator.
- All top-level `local` declarations become file-scope globals in this mode.
  Typed globals without initializers are zero-initialized; partial
  initialization of a multi-name declaration is `E0325`.
- Duplicate, non-leading, and nested `#[no_std]` statements are `E0325`.
- File-scope structs use `{0}` initializers so MSVC accepts them as constant
  expressions. Functions receive `NULL` for the hidden coroutine argument, and
  struct field keys/`asm!` text are not interned as `LusaString` values.

### 19.3 `#[link_section = "..."]`

- The attribute applies to top-level functions, top-level `local`
  declarations, and top-level implicit globals.
- Section names are validated in both parsers: 1–255 ASCII bytes from
  `[A-Za-z0-9_.$-]`, otherwise `E0101`.
- Emission writes a comment with the GNU lowering and calls
  `LUSA_LINK_SECTION(name)`. GCC/Clang expand it to
  `__attribute__((section(name)))`; MSVC expands it to nothing.
- The dynamic engines accept and ignore the attribute.

### 19.4 Promotion and placement diagnostics

- A sectioned top-level `local` is rewritten as a file-scope global
  declaration plus assignment. Annotated types are pre-declared, and an
  attribute on an already-declared global is `E0325`.
- Nested locals/assignments, struct declarations, and other statements reject
  `#[link_section]`/`#[entry]` with `E0325` or `E0101` as appropriate.
- The check runs in both the Rust parser and the self-hosted parser, so
  `run`, `--vm`, `build`, and `--emit-c` agree on section validation.

**Acceptance criteria**

- `tests/module19.rs` covers the freestanding preamble, entry ordering and
  `void` return, section lowering for functions/globals/promoted locals,
  promotion initializer checks, `E0101` name validation, `E0325` placement and
  duplicate diagnostics, dynamic parity, and native hosted/no_std builds.
- `examples/baremetal_boot.lusa` runs in `run` and `--vm` and emits a
  freestanding C11 translation unit with `--emit-c` that compiles without the
  hosted runtime.
- The full suite passes on Windows, Linux, and macOS.

**Risks**

- MSVC cannot place functions in a named code section; the macro is empty
  there and only the comment records the intent. GCC/Clang are the supported
  section-placement compilers.
- `#[no_std]` covers a deliberately small subset. Strings, tables, `print`,
  coroutines, and `unsafe_alloc` still require the hosted runtime and are
  documented as unsupported rather than silently emulated.
- The generated freestanding code still relies on the host C compiler for
  scalar types and the linker script for section addresses; Lusa does not
  provide a linker script or startup vector table.
- Section names are ASCII-only and never escaped; the validation exists to
  keep the emitted C string literal and attribute well-formed.

## Module 20 - Heap allocator and dynamic collections (shipped in v1.6.0)

### 20.1 The global allocator contract

- `#[global_allocator]` marks one allocator and/or one deallocator function.
  The allocator signature is `(size: u32, align: u32): *mut u32`; the
  deallocator signature is `(ptr: *mut u32, size: u32, align: u32): void`.
- Hosted programs without a user allocator dispatch to `malloc`/`free`.
  `#[no_std]` programs without one use a static 64 KiB `LusaHeapArena` bump
  allocator with a no-op deallocator, so collections work without libc.
- Allocator dispatch is emitted after the function prototypes, so a custom
  allocator may call other user functions (for example a bump arena helper).
- Duplicate allocators/deallocators, a wrong signature, a method receiver, or
  combining `#[global_allocator]` with `#[entry]` is `E0325`.

### 20.2 `Vec<T>`

- `Vec<T>` lowers to `LusaVec { void *data; u32 len; u32 capacity; }` with
  per-element-type push/pop helpers and a deep drop helper.
- Capacity starts at 0, grows from 4, and doubles as needed. `reserve(n)`
  grows to at least `n`; `clear()` drops elements but keeps the allocation.
- Methods: `push`, `pop`, `len`, `capacity`, `is_empty`, `reserve`, `clear`.
  Indexing (`v[i]`) and index assignment are 0-based and bounds-checked.
- Elements of `string`, struct, and nested collection types are owned and
  release through the generated `lusa_drop_vec_<tag>` helper.
- `Vec<T>` cannot yet be passed or returned by value; that boundary is
  `E0312` with a raw-pointer hint.
- The interpreter and bytecode VM simulate a vector as a 0-based table with
  the same method fallbacks; fields with the same names shadow the fallback.
  Capacity is tracked out of band so `pop`/`clear` match native behavior.

### 20.3 `Box<T>`

- Native `Box<T>` lowers to `T *` and allocates through `lusa_heap_alloc`.
- Implicit initialization (`local b: Box<i32> = 5`) and explicit
  `Box(value)` both allocate; `nil` clears the box without allocating.
- `*b` reads and `*b = value` writes the pointee without the raw-pointer
  `unsafe` gate, because the box owns it.
- Boxed strings and structs release through `lusa_drop_box_<tag>`.
- The dynamic engines use a tagged table (marker `\u{1}lusa_box`) so implicit
  annotation wrapping, explicit `Box(...)`, and `MakeBox` are idempotent;
  numeric raw pointers still require `unsafe`.

**Acceptance criteria**

- `tests/module20.rs` covers vector growth/capacity/pop/clear/reserve,
  `Box<T>` dereference and writes, `Vec<string>`/`Box<string>` RAII, custom
  allocator dispatch, freestanding arena emission, allocator diagnostics, and
  native + interpreter + VM parity.
- `examples/dynamic_heap.lusa` runs in `run`, `--vm`, and `--native`.
- Hosted native builds use `malloc`/`free` by default and freestanding builds
  emit no libc includes, `malloc`, or `free`.
- The full suite passes on Windows, Linux, and macOS.

**Risks**

- `Vec<T>` remains a value-type restriction: no by-value parameter/return
  boundary yet, and no growth-failure recovery beyond process abort.
- The freestanding arena is a fixed 64 KiB bump allocator with no reuse; it is
  intended for firmware-scale programs, not general allocation workloads.
- MSVC accepts the generated helpers but GCC/Clang remain the reference
  compilers for section placement and inline assembly.
- The dynamic simulations are behavioral, not byte-for-byte: capacity is
  tracked out of band and raw pointer dereferences remain sandboxed.

## Cross-cutting work

- **Single semantics source:** extend `docs/LANGUAGE.md` first, then implement
  interpreter, VM, and C backend against the same examples.
- **Parity harness:** every new builtin gets an interpreter/VM/native test in
  `tests/` with identical expected stdout and E-codes.
- **Memory gates:** add an allocation-counter mode to the generated runtime
  for tests, so leaks and double frees fail CI rather than relying on crashes.
- **Diagnostics registry:** keep `docs/LANGUAGE.md` error-code table current;
  every new `E03xx`/`E10xx` code needs a golden test.
- **Performance:** keep the keyed-table and iterator benchmarks in
  `bench.lusa`; reject changes that regress the native hot path by more than
  the agreed budget.
- **English-only policy:** code comments, diagnostics, release notes, and docs
  remain entirely in English.

## Release plan

| Version | Theme | Required before tagging |
| --- | --- | --- |
| v0.7.0 | Static structs, raw pointers, kernel intrinsics (shipped) | three-OS CI, sandbox parity, native volatile access |
| v0.8.0 | Native `pcall`, coroutines (shipped) | three-OS CI, native unwind/leak suite, parity tests |
| v0.9.0 | Pattern matching and string utilities (shipped) | shared matcher, native/VM/interpreter parity, RAII lifetime tests |
| v1.0.0 | Static borrow checker, explicit `move`, stabilization (shipped) | golden diagnostics, native/VM/interpreter parity, frozen semantics |
| v1.1.0 | Struct literals, `match`, compile-time modules, inline asm (shipped) | deep struct RAII, per-arm ownership, import deduplication, three-engine parity |
| v1.2.0 | Fixed arrays, address-of, pointer arithmetic (shipped) | zero-based C11 arrays, heap-element drop loops, `unsafe` dereference gate, three-engine parity |
| v1.3.0 | Volatile MMIO and struct representation (shipped) | typed volatile lowering, scalar-element gate, packed/align portability macros, three-engine parity |
| v1.4.0 | Inline assembly and hardware controls (shipped) | `asm!` parsing in both front ends, GNU volatile lowering, strict `unsafe` gate, three-engine parity |
| v1.5.0 | Custom entries, `#[no_std]`, and linker sections (shipped) | freestanding preamble, entry-wrapper ordering, section lowering and promotion, `E0101`/`E0325` diagnostics, three-engine parity, native builds |
| v1.6.0 | Heap allocator, `Vec<T>`, and `Box<T>` (shipped) | allocator dispatch and validation, freestanding bump arena, geometric vector growth, Box deref safety, owned-element RAII, three-engine parity, native builds |

Each release uses the curated English template with `## Impact Introduction`,
`## Feature Highlights`, and `## How to Use`, and attaches `lusa-setup.zip`,
`lusa-linux-x86_64.tar.gz`, `lusa-linux-arm64.tar.gz`, and
`lusa-macos-arm64.tar.gz`.
