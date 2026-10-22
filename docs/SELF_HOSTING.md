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

Intentionally unsupported in this reference stage: table constructors,
field/index/method access, function definitions, numeric/generic `for`,
varargs, and compilation of interpolated strings.  The parser reports a
`parser:line:column:` error instead of silently building a partial AST.  The
compiler reports unsupported AST nodes with a `compiler:line:column:` error.
`borrow` is parsed and compiled to the bytecode `BorrowErr` opcode, matching
the native compiler's VM behavior.  Calls are compiled as single-value
`CallOne` expressions; multi-return expansion from calls and vararg
propagation are not part of this reference subset.  A declaration or
assignment with more initializers than targets is rejected rather than
silently dropping the extras.

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
| `Clone` | `value = expr` |
| `Borrow` | `value = expr` |

`Assign` targets are restricted to `Name` nodes.  The parser rejects
`a.b = ...`, `a[i] = ...` and call targets with an explicit error.

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

## Extension points

The next stage can add table/field nodes, function prototypes/upvalues, numeric
and generic `for`, and varargs by extending the AST schema, the parser
precedence/suffix handling, and the compiler's instruction emission.  The
module-level state design is single-threaded and non-reentrant by choice; a
future version can thread an explicit parser/compiler context table instead.
