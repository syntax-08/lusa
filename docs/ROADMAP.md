# Lusa Roadmap: Unification and Refinement

This document is the engineering plan that follows v0.6.1. It turns the next
three language milestones into concrete compiler, runtime, and release work.
The guiding rule is unchanged: every feature must have one documented
semantics and must pass the same tests in the interpreter, the bytecode VM,
and, where supported, the C11 backend.

## Baseline after v0.6.1

| Area | Interpreter | Bytecode VM | C11 backend |
| --- | --- | --- | --- |
| Keyed tables and nested fields | yes | yes | yes |
| Colon methods | yes | yes | yes (static dispatch) |
| `pairs` / `ipairs` | yes | yes | yes (native loops) |
| `pcall` | yes (`src/builtins.rs`) | yes | no |
| Coroutines | no | no | no |
| String patterns | literal `find`/`gsub` only | literal only | no string library |
| Ownership checks | runtime registry (`src/value.rs`) | runtime registry | static subset (`compiler_bootstrap.lusa`) |
| Explicit `move` / `copy` | no | no | no |

The C backend currently reports fatal runtime conditions through
`lusa_panic`, which flushes output and calls `exit(1)`. That is the main
blocker for native protected calls. The dynamic engines already model the
ownership rules at runtime, but the compiler does not yet prove them ahead of
time.

## Module 11 - Error Recovery and Coroutines (target v0.7.0)

### 11.1 Native protected calls (`pcall`)

The dynamic `pcall(f, ...)` contract is already stable: success returns `true`
followed by the callee's results; any error returns `false` and an
`"E-code: message"` string. Module 11 must make the same contract available to
native builds without leaking heap values or skipping RAII drops.

**Runtime design**

1. Replace the process-exiting `lusa_panic` path with a `LusaErrorFrame`
   stack. `lusa_throw(code, message)` records the error, unwinds registered
   drop frames down to the nearest `pcall` frame, and transfers control with
   C11 `setjmp`/`longjmp`.
2. Add a scope journal to the generated code. Each lexical scope pushes the
   addresses of its owned slots; a normal exit runs the same drops in reverse
   order, and an error unwind runs exactly the scopes above the protected
   frame. This preserves the v0.6.1 guarantee that every owned value is
   released once.
3. Keep OOM, bounds, division-by-zero, and table-index failures on the same
   error path; no runtime failure may bypass cleanup.
4. Convert the error value to an owned `LusaString` only at the `pcall`
   boundary, so intermediate frames do not allocate while unwinding.

**Code generation and type checking**

- Recognize `pcall` in `check_call` as a builtin with the signature
  `pcall(f: function, ...) -> (bool, T...)`. The first version can require a
  direct named function (the native subset has no first-class closures) and
  one result value; the dynamic engines keep the full Lua-style result list.
- Emit a protected region around the direct call, store the success flag and
  result/error in temporaries, and continue in the surrounding scope.
- Reject `pcall` with unsupported argument or result shapes using a normal
  `E03xx` diagnostic rather than an internal emitter error.
- Add `pcall` to `Compiler.memory_layout()` and the native compatibility
  table in `docs/LANGUAGE.md`.

**Coroutines**

- Introduce `coroutine.create`, `resume`, `yield`, `status`, and `close`.
- Interpreter and VM: represent a coroutine as a value owning a suspended
  frame, its registers, and its heap roots. Resume transfers arguments into
  the frame; yield transfers values out and leaves owned locals alive in the
  coroutine.
- Native: prefer stackless lowering to an explicit state machine over
  platform fibers. A coroutine function becomes a generated `switch` over
  suspension points, with locals promoted into a heap-allocated frame.
  `yield` returns `LUSA_CORO_YIELD`; `resume` re-enters at the saved state.
  This keeps the backend portable C11 and avoids `ucontext`/Windows-fiber
  dependencies.
- Closing or cancelling a coroutine runs the frame's drop journal, including
  values created after the last resume.
- Errors inside a coroutine propagate through `resume` and are catchable with
  `pcall`.

**Acceptance criteria**

- Native and dynamic `pcall` agree on success/failure values and E-codes.
- A native program that throws after allocating strings, tables, and nested
  tables is leak-free under the project's allocation counter/ASan job.
- `break`, `return`, and error unwinds drop loop-body locals exactly once.
- Coroutine tests cover yield/resume values, close/drop, nested coroutines,
  errors, and VM/interpreter parity.
- Release gate: full three-OS CI plus a native error-unwind stress test.

**Risks**

- `setjmp`/`longjmp` interacts badly with C++ destructors, but generated code
  is C11 and uses explicit journals, so the risk is contained.
- Stackless coroutine lowering increases emitter complexity; keep the first
  release to named coroutine functions with explicit `yield` sites.
- Multiple native return values may require a small result struct; do not
  silently truncate.

## Module 12 - String Utilities and Pattern Matching (target v0.8.0)

### 12.1 Pattern engine

`string.find` and `string.gsub` currently compare literal character slices in
`src/builtins.rs`. Module 12 adds a real pattern layer shared by all engines.

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

## Module 13 - Advanced Lifetimes (target v0.9.0)

### 13.1 Static borrow checker

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
| v0.7.0 | Native `pcall`, coroutines | three-OS CI, native unwind/leak suite, parity tests |
| v0.8.0 | Pattern matching and string utilities | shared matcher, fuzz corpus, native/VM/interpreter parity |
| v0.9.0 | Static borrow checker, explicit `move`/`copy` | golden diagnostics, migration report, full suite |
| v1.0.0 | Language stabilization | frozen semantics, no known memory-safety defects |

Each release uses the curated English template with `## Impact Introduction`,
`## Feature Highlights`, and `## How to Use`, and attaches `lusa-setup.zip`,
`lusa-linux-x86_64.tar.gz`, `lusa-linux-arm64.tar.gz`, and
`lusa-macos-arm64.tar.gz`.
