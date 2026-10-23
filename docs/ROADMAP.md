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

## Baseline after v0.8.0

| Area | Interpreter | Bytecode VM | C11 backend |
| --- | --- | --- | --- |
| Keyed tables and nested fields | yes | yes | yes |
| Colon methods | yes | yes | yes (static dispatch) |
| `pairs` / `ipairs` | yes | yes | yes (native loops) |
| Static structs | simulated buffers | simulated buffers | C typedef + member access |
| Raw pointers | simulated addresses | simulated addresses | volatile C pointers |
| `pcall` | yes (`src/builtins.rs`) | yes | yes (named-function subset) |
| Coroutines | yes | yes | yes (fibers/`ucontext`, restricted subset) |
| String patterns | literal `find`/`gsub` only | literal only | no string library |
| Ownership checks | runtime registry (`src/value.rs`) | runtime registry | static subset (`compiler_bootstrap.lusa`) |
| Explicit `move` / `copy` | no | no | no |

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

## Module 13 - String Utilities and Pattern Matching (target v0.9.0)

### 13.1 Pattern engine

`string.find` and `string.gsub` currently compare literal character slices in
`src/builtins.rs`. Module 13 adds a real pattern layer shared by all engines.

**Surface API**

- `string.find(s, pattern [, init [, plain]])` -> start, end, captures
- `string.match(s, pattern [, init])` -> captures
- `string.gmatch(s, pattern)` -> iterator
- `string.gsub(s, pattern, replacement [, limit])` -> result, count
- Lua-compatible classes and quantifiers: `%a %c %d %g %l %p %s %u %w %x`,
  `%bxy`, `%f[set]`, captures `()`, backreferences `%1`, `.`, `*`, `+`, `-`,
  `?`, anchors `^`/`$`, and escaped literals.

**Architecture**

1. Add `src/patterns.rs` with a compiled pattern representation (instruction
   list or small AST). Both the interpreter and VM call this module; no
   duplicated matching logic.
2. Define UTF-8 behavior explicitly. Lua patterns are byte-oriented; Lusa
   strings are UTF-8. The recommended contract is code-point matching for
   classes and `.`, with byte offsets still reported for compatibility. Add
   tests for multibyte input and document the choice.
3. Native runtime: add `lusa_pattern_*` functions to the generated C runtime.
   Constant patterns collected at compile time can be validated early; dynamic
   patterns use the same runtime matcher. Keep the matcher iterative with an
   explicit backtracking stack to avoid C stack overflow.
4. Enforce a pattern-length limit and a backtracking budget; report `E2008`
   instead of hanging on pathological patterns.
5. Replacement strings support `%0`..`%9`; dynamic engines may additionally
   accept a replacement function, while the native subset initially accepts
   string/number replacements only.
6. Captures produce owned strings; `gmatch` state owns the source string and
   matcher state and releases both when the iterator is dropped.
7. Add `string.split`, `string.trim`, and `string.starts_with`/`ends_with`
   helpers on top of the same scanner if they can share the ownership rules.

**Acceptance criteria**

- One shared conformance corpus runs on interpreter, VM, and native builds.
- `find`/`gsub` remain backward compatible when `plain = true`.
- Fuzz tests compare the native and Rust matchers on random ASCII/UTF-8 input.
- No pattern can overflow the native stack; the budget test is deterministic.
- `gmatch` iterators are leak-free across `break`, `return`, and `pcall`.

**Risks**

- Byte-versus-code-point semantics can break scripts that rely on Lua byte
  offsets; ship the rule in the release notes and provide `plain` as an
  escape hatch.
- Backtracking performance needs a budget from day one.

## Module 14 - Advanced Lifetimes (target v1.0.0)

### 14.1 Static borrow checker

The C backend currently tracks `own`, `borrowed`, and `mutable` flags in
`compiler_bootstrap.lusa`; the dynamic engines enforce rules in the runtime
`Ownership` registry. Module 13 replaces the ad-hoc checks with a shared
compile-time analysis and makes explicit transfer syntax part of the language.

**Analysis design**

1. Build a control-flow graph per function from the typed AST. Nodes are
   statements and branch joins; edges carry move/borrow events.
2. Track places, not just names: `x`, `x.field`, and `x[i]` are distinct move
   paths. A move of `x` invalidates all its subpaths; a move of a subpath
   leaves siblings valid.
3. Compute borrow regions with a last-use/NLL-style liveness pass. A borrow
   conflicts with a move or a mutable borrow only while it is live.
4. Integrate the analysis into the C backend front end and expose the same
   diagnostics to the interpreter/VM ahead of execution (warnings first).
5. Keep `unsafe do ... end` as the explicit escape hatch; it must be visible
   in diagnostics and documented as unchecked.

**Explicit move/copy labels**

- `move x` transfers ownership and leaves `x` unusable; the lexer already
  reserves `move`, and the bootstrap modules use a `move(...)` helper
  convention. Promote this to a real expression and make the implicit move
  explicit in consuming contexts.
- `copy x` (or the existing `clone x`) creates an independent value. Decide
  whether `copy` is a synonym or a shallow copy; the recommendation is a
  synonym for the current deep `clone`, with `copy` as the user-facing word.
- Type-level labels such as `move T` and `copy T` are a later extension; do
  not add them until the expression forms and diagnostics are stable.

**Diagnostics**

- Keep the stable codes `E1001` (use after move), `E1002` (borrow conflict),
  `E1004` (write through immutable borrow), and `E1005` (move out of a
  borrow), and add notes that point at the move/borrow site plus a `help`
  line suggesting `copy` or a shorter borrow.
- LSP quick fixes: rename the shadowed value, insert `copy`, or end the
  borrow earlier.
- Golden diagnostic tests for every code, including multiline spans.

**Acceptance criteria**

- The static checker accepts the full existing test suite and examples.
- Every runtime ownership test has a compile-time counterpart where the error
  is statically decidable.
- Native code generation no longer needs `own = false` escape hatches for
  field reads; the analysis proves when a copy is required.
- Migration report lists false positives and the planned fixes before the
  rules become errors.

**Risks**

- Field-sensitive tracking can explode; start with a bounded path depth and
  fall back to whole-object conservatism.
- Dynamic code that mutates tables through aliases may be valid in Lua but
  rejected by static rules; keep `unsafe` and clear migration notes.

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
| v0.9.0 | Pattern matching and string utilities | shared matcher, fuzz corpus, native/VM/interpreter parity |
| v1.0.0 | Static borrow checker, explicit `move`/`copy`, stabilization | golden diagnostics, migration report, frozen semantics |

Each release uses the curated English template with `## Impact Introduction`,
`## Feature Highlights`, and `## How to Use`, and attaches `lusa-setup.zip`,
`lusa-linux-x86_64.tar.gz`, `lusa-linux-arm64.tar.gz`, and
`lusa-macos-arm64.tar.gz`.
