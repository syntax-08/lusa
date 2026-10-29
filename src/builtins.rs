//! Standard library: base functions plus the `math`, `string`, `table` and
//! `json` modules.

use std::rc::Rc;
use std::sync::Arc;
use std::path::{Path, PathBuf};

use crate::error::{
    LResult, LusaError, Span, E_ASSERT, E_CALL, E_CMP, E_ERROR, E_INDEX, E_IO, E_NUM,
};
use crate::interp::Interp;
use crate::pattern::{self, CaptureKind, Gmatch};
use crate::value::{format_number, with_reg, FuncBody, FuncObj, IterState, TableObj, Value};

type BF = Box<dyn Fn(&mut Interp, Vec<Value>, Span) -> LResult<Vec<Value>>>;

pub fn make_builtin(name: &str, f: BF) -> Value {
    let vid = with_reg(|r| r.alloc_vid());
    Value::Func(Rc::new(FuncObj {
        vid,
        name: Some(Arc::from(name)),
        env: Vec::new(),
        body: FuncBody::Builtin(f),
    }))
}

fn one(v: Value) -> LResult<Vec<Value>> {
    Ok(vec![v])
}

/// Marker key used by the dynamic engines to tag a table as a `Box<T>`
/// wrapper.  A leading control character keeps user tables from colliding
/// with it by accident.
pub const BOX_MARKER: &str = "\u{1}lusa_box";
const BOX_VALUE_KEY: &str = "value";

fn box_marker() -> Value {
    Value::Str(Arc::from(BOX_MARKER))
}

fn box_value_key() -> Value {
    Value::Str(Arc::from(BOX_VALUE_KEY))
}

/// Wraps `value` in a box table.  Wrapping an existing box is a no-op so
/// implicit annotation wrapping and an explicit `Box(x)` never nest.
pub fn box_value(value: Value) -> Value {
    if box_inner(&value).is_some() {
        return value;
    }
    let table = TableObj::new();
    table.set(box_marker(), Value::Bool(true)).expect("box marker key");
    table.set(box_value_key(), value).expect("box value key");
    Value::Table(Rc::new(table))
}

/// Returns the table behind a boxed value, or `None` for ordinary values.
pub fn box_table(value: &Value) -> Option<Rc<TableObj>> {
    let table = match value.base_value() {
        Value::Table(table) => table,
        _ => return None,
    };
    match table.get(&box_marker()) {
        Some(Value::Bool(true)) => Some(Rc::clone(table)),
        _ => None,
    }
}

/// Reads the pointee of a boxed value.  `Some(Value::Nil)` is a valid boxed
/// result, so callers must use `box_table` when they need to distinguish an
/// empty box from an ordinary value.
pub fn box_inner(value: &Value) -> Option<Value> {
    let table = box_table(value)?;
    Some(table.get(&box_value_key()).unwrap_or(Value::Nil))
}

/// Writes a new pointee into an already-resolved box table.
pub fn box_set_table(table: &TableObj, new_value: Value) {
    table
        .set(box_value_key(), new_value)
        .expect("box value key");
}

/// Writes a new pointee into an existing box.  Returns `false` when `value`
/// is not a box.
pub fn box_set_inner(value: &Value, new_value: Value) -> bool {
    let Some(table) = box_table(value) else {
        return false;
    };
    box_set_table(&table, new_value);
    true
}

// ===== built-in Result/Option sums (module 21) =====
//
// The dynamic engines represent `Result<T, E>` and `Option<T>` as ordinary
// tables tagged with private marker keys.  The C11 backend uses dedicated
// structs instead; both surfaces share the same methods.

pub const SUM_MARKER: &str = "__lusa_sum";
pub const SUM_TAG: &str = "__lusa_tag";
pub const SUM_VALUE: &str = "__lusa_value";

/// Builds a tagged sum table.  Constructor payloads are stored by value; the
/// ownership engine treats the wrapper as a heap value like any other table.
pub fn make_sum(kind: &str, tag: &str, value: Value) -> Value {
    let table = TableObj::new();
    table
        .set(Value::Str(Arc::from(SUM_MARKER)), Value::Str(Arc::from(kind)))
        .expect("sum marker key");
    table
        .set(Value::Str(Arc::from(SUM_TAG)), Value::Str(Arc::from(tag)))
        .expect("sum tag key");
    table
        .set(Value::Str(Arc::from(SUM_VALUE)), value)
        .expect("sum value key");
    Value::Table(Rc::new(table))
}

fn get<'a>(args: &'a [Value], i: usize, what: &str, span: Span) -> LResult<&'a Value> {
    args.get(i)
        .ok_or_else(|| LusaError::new(E_INDEX, Some(span), format!("missing argument `{}`", what)))
}

fn expect_table(v: &Value, span: Span) -> LResult<Rc<TableObj>> {
    match v.base_value() {
        Value::Table(t) => Ok(Rc::clone(t)),
        other => Err(LusaError::new(
            E_INDEX,
            Some(span),
            format!("expected a table, got {} ({})", other.type_name(), other.fmt_value()),
        )),
    }
}

pub fn need_num(v: &Value, what: &str, span: Span) -> LResult<f64> {
    match v {
        Value::Number(n) => Ok(*n),
        other => Err(LusaError::new(
            E_NUM,
            Some(span),
            format!("expected number for `{}`, got {} ({})", what, other.type_name(), other.fmt_value()),
        )),
    }
}

fn to_str(v: &Value) -> String {
    v.base_value().fmt_value()
}

fn pattern_error(err: pattern::PatternError, span: Span) -> LusaError {
    let help = if err.code == pattern::E_PATTERN_COMPLEX {
        "simplify nested quantifiers or match a shorter subject"
    } else {
        "patterns use Lua syntax: `%d`, `%a`, `%w`, `[...]`, `*`, `+`, `-`, `?`, captures, and `^`/`$`"
    };
    LusaError::new(err.code, Some(span), err.message).help(help)
}

/// Converts a Lua-style 1-based `init` argument to a zero-based search index.
/// Negative values count from the end of the subject; values beyond the last
/// valid insertion point produce no match.
fn pattern_init(value: Option<&Value>, len: usize) -> Option<usize> {
    let number = match value {
        Some(Value::Number(n)) if n.is_finite() => *n,
        _ => 1.0,
    };
    let mut pos = number.trunc() as i64;
    if pos < 0 {
        pos = len as i64 + pos + 1;
    }
    if pos < 1 {
        pos = 1;
    }
    if pos > len as i64 + 1 {
        return None;
    }
    Some((pos - 1) as usize)
}

fn capture_values(m: &pattern::Match, source: &[char]) -> Vec<Value> {
    m.captures
        .iter()
        .map(|cap| match cap.kind {
            CaptureKind::Position => Value::Number((cap.start + 1) as f64),
            CaptureKind::Substring => {
                let text: String = source[cap.start..cap.end].iter().collect();
                Value::Str(Arc::from(text.as_str()))
            }
        })
        .collect()
}

fn match_values(m: &pattern::Match, source: &[char]) -> Vec<Value> {
    if m.captures.is_empty() {
        vec![Value::Str(Arc::from(m.text(source).as_str()))]
    } else {
        capture_values(m, source)
    }
}

// ===== base functions =====

fn b_print(i: &mut Interp, args: Vec<Value>, _span: Span) -> LResult<Vec<Value>> {
    let line: Vec<String> = args.iter().map(to_str).collect();
    i.out.push(line.join(" "));
    Ok(Vec::new())
}

// ===== safe unsafe-block simulation =====

fn mem_addr(args: &[Value], span: Span) -> LResult<u64> {
    let n = need_num(get(args, 0, "address", span)?, "address", span)?;
    if !n.is_finite() || n < 0.0 || n.fract() != 0.0 {
        return Err(LusaError::new(
            E_NUM,
            Some(span),
            "unsafe memory address must be a non-negative integer",
        ));
    }
    Ok(n as u64)
}

fn b_unsafe_ptr_cast(_i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let addr = mem_addr(&args, span)?;
    one(Value::Number(addr as f64))
}

fn require_unsafe(interp: &Interp, what: &str, span: Span) -> LResult<()> {
    if interp.unsafe_depth == 0 {
        return Err(LusaError::new(
            crate::error::E_PTR_DEREF,
            Some(span),
            format!("{what} requires an `unsafe` block"),
        )
        .note("wrap the call in `unsafe do ... end`", None));
    }
    Ok(())
}

fn b_unsafe_write_mem(i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    require_unsafe(i, "raw memory access", span)?;
    let addr = mem_addr(&args, span)?;
    let value = need_num(get(&args, 1, "value", span)?, "value", span)?;
    i.unsafe_mem.insert(addr, value);
    Ok(Vec::new())
}

fn b_unsafe_read_mem(i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    require_unsafe(i, "raw memory access", span)?;
    let addr = mem_addr(&args, span)?;
    let value = i.unsafe_mem.get(&addr).copied().unwrap_or(0.0);
    one(Value::Number(value))
}

/// `Box(value)`: allocate an owned pointee in the dynamic engines.  The native
/// backend lowers this to a heap allocation; the sandbox uses a tagged table
/// so `*b` can read and write the pointee without the raw-pointer gate.
fn b_box(_i: &mut Interp, args: Vec<Value>, _span: Span) -> LResult<Vec<Value>> {
    one(box_value(args.first().cloned().unwrap_or(Value::Nil)))
}

fn sum_ctor(kind: &'static str, tag: &'static str, name: &'static str) -> BF {
    Box::new(move |_i: &mut Interp, args: Vec<Value>, span: Span| {
        if args.len() != 1 {
            return Err(LusaError::new(
                E_CALL,
                Some(span),
                format!("`{name}` takes exactly one value, found {}", args.len()),
            ));
        }
        Ok(vec![make_sum(kind, tag, args.into_iter().next().unwrap())])
    })
}

fn b_none(_i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    if !args.is_empty() {
        return Err(LusaError::new(
            E_CALL,
            Some(span),
            format!("`None` takes no arguments, found {}", args.len()),
        ));
    }
    one(make_sum("option", "none", Value::Nil))
}

/// Dynamic lowering of unary `*`: boxes dereference safely, while ordinary
/// numeric addresses keep the raw-memory sandbox rules.
fn b_box_read(i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let target = get(&args, 0, "value", span)?;
    if let Some(inner) = box_inner(target) {
        return one(inner);
    }
    require_unsafe(i, "raw pointer dereference", span)?;
    let addr = mem_addr(&args, span)?;
    let value = i.unsafe_mem.get(&addr).copied().unwrap_or(0.0);
    one(Value::Number(value))
}

/// Dynamic lowering of `*place = value`: box writes mutate the pointee;
/// numeric targets fall back to the simulated raw-memory space.
fn b_box_write(i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let target = get(&args, 0, "value", span)?;
    let value = get(&args, 1, "value", span)?;
    if box_set_inner(target, value.clone()) {
        return Ok(Vec::new());
    }
    require_unsafe(i, "raw pointer dereference", span)?;
    let addr = mem_addr(&args, span)?;
    let value = need_num(value, "value", span)?;
    i.unsafe_mem.insert(addr, value);
    Ok(Vec::new())
}

/// Accepts the scalar spellings supported by volatile hardware access.  The
/// sandbox stores plain numbers, so `bool` is represented as 1/0.
fn volatile_value(v: &Value, what: &str, span: Span) -> LResult<f64> {
    match v.base_value() {
        Value::Number(n) => Ok(*n),
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        other => Err(LusaError::new(
            E_NUM,
            Some(span),
            format!(
                "expected a scalar for `{what}`, got {} ({})",
                other.type_name(),
                other.fmt_value()
            ),
        )),
    }
}

/// `volatile_read(ptr)`: a typed, volatile load from the simulated address
/// space.  The native backend lowers the call to `*(volatile T *)(ptr)`, so
/// polling loops cannot be optimized away by the C compiler.
fn b_volatile_read(i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    require_unsafe(i, "volatile memory access", span)?;
    let addr = mem_addr(&args, span)?;
    let value = i.unsafe_mem.get(&addr).copied().unwrap_or(0.0);
    one(Value::Number(value))
}

/// `volatile_write(ptr, value)`: the write half of `volatile_read`.  The
/// native backend emits a volatile lvalue store using the pointer element
/// type inferred by the C backend.
fn b_volatile_write(i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    require_unsafe(i, "volatile memory access", span)?;
    let addr = mem_addr(&args, span)?;
    let value = volatile_value(get(&args, 1, "value", span)?, "value", span)?;
    i.unsafe_mem.insert(addr, value);
    Ok(Vec::new())
}

/// `unsafe_alloc(bytes)`: reserves a fresh region of the simulated address
/// space and returns its base address.  The native backend allocates a real
/// zeroed buffer; the sandbox hands out cell-aligned virtual addresses.
fn b_unsafe_alloc(i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    require_unsafe(i, "`unsafe_alloc`", span)?;
    let bytes = need_num(get(&args, 0, "bytes", span)?, "bytes", span)?;
    if !bytes.is_finite() || bytes < 0.0 {
        return Err(LusaError::new(E_NUM, Some(span), "unsafe_alloc size must be a non-negative number"));
    }
    let base = i.unsafe_alloc_next;
    // 16 cells per allocation keeps the sandbox deterministic and roomy.
    i.unsafe_alloc_next = i.unsafe_alloc_next.saturating_add(64).max(base + 64);
    one(Value::Number(base as f64))
}

/// `interrupts_disable()` - clears the global interrupt mask and returns 1
/// when interrupts were previously enabled.  The C backend lowers this to
/// `cpsid i`/PRIMASK on Arm and `csrci mstatus, 8` on RISC-V; the sandbox
/// tracks the mask so interpreted programs observe the same restore protocol.
fn b_interrupts_disable(i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    if !args.is_empty() {
        return Err(LusaError::new(
            E_CALL,
            Some(span),
            "`interrupts_disable` takes no arguments",
        ));
    }
    let previous = i.interrupts_enabled;
    i.interrupts_enabled = false;
    one(Value::Number(if previous { 1.0 } else { 0.0 }))
}

fn b_interrupts_enable(i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    if !args.is_empty() {
        return Err(LusaError::new(
            E_CALL,
            Some(span),
            "`interrupts_enable` takes no arguments",
        ));
    }
    i.interrupts_enabled = true;
    Ok(Vec::new())
}

fn b_interrupts_restore(i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let state = need_num(get(&args, 0, "state", span)?, "state", span)?;
    i.interrupts_enabled = state != 0.0;
    Ok(Vec::new())
}

/// `unsafe_nop()` - a portable no-op intrinsic.  The native backend lowers it
/// to a compiler barrier/nop; the sandbox treats it as a plain no-op.
fn b_unsafe_nop(i: &mut Interp, _args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    require_unsafe(i, "`unsafe_nop`", span)?;
    Ok(Vec::new())
}

/// `unsafe_asm("text")` - inline assembly hook.  The native backend forwards
/// the string to the host compiler (`__asm__ __volatile__` on GCC/Clang); the
/// dynamic sandbox ignores it and never executes real instructions.
fn b_unsafe_asm(i: &mut Interp, _args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    require_unsafe(i, "`unsafe_asm`", span)?;
    Ok(Vec::new())
}

/// `asm!("text")` - macro-style inline assembly.
///
/// Both front ends desugar the syntax to an `asm("text")` call, so the
/// interpreter and VM share this one unsafe gate.  Instructions are ignored by
/// the dynamic engines; the C11 backend emits `LUSA_ASM("text")`.
fn b_asm(i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    require_unsafe(i, "inline assembly (`asm!`)", span)?;
    if args.len() != 1 {
        return Err(LusaError::new(
            crate::error::E_INDEX,
            Some(span),
            format!("`asm!` takes one string argument, found {}", args.len()),
        ));
    }
    match args[0].base_value() {
        Value::Str(_) => Ok(Vec::new()),
        other => Err(LusaError::new(
            crate::error::E_INDEX,
            Some(span),
            format!("`asm!` expects a string, got {}", other.type_name()),
        )),
    }
}

/// `io_read_file(path)`: reads a UTF-8 text file and returns its contents.
///
/// This is the native I/O primitive used by the self-hosted compiler pipeline.
/// It is installed in the global environment, so both the tree-walking
/// interpreter and the bytecode VM see the same function.
fn b_io_read_file(_i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let path = match args.first().map(Value::base_value) {
        Some(Value::Str(s)) => s,
        Some(other) => {
            return Err(LusaError::new(
                E_INDEX,
                Some(span),
                format!(
                    "io_read_file: expected a string path, got {} ({})",
                    other.type_name(),
                    other.fmt_value()
                ),
            ))
        }
        None => {
            return Err(LusaError::new(
                E_INDEX,
                Some(span),
                "io_read_file: missing argument `path`",
            ))
        }
    };

    let contents = std::fs::read_to_string(path.as_ref()).map_err(|e| {
        LusaError::new(
            E_IO,
            Some(span),
            format!("io_read_file: cannot read `{}`: {}", path, e),
        )
        .help("check that the file exists, is readable, and contains valid UTF-8")
    })?;
    Ok(vec![Value::Str(Arc::from(contents.as_str()))])
}

/// `io_write_binary(path, bytes)`: writes a dense byte table (or a string) to
/// a file.  This is the output side of the self-hosted native bootstrap.
fn b_io_write_binary(_i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let path = match args.first().map(Value::base_value) {
        Some(Value::Str(s)) => s,
        Some(other) => {
            return Err(LusaError::new(
                E_INDEX,
                Some(span),
                format!(
                    "io_write_binary: expected a string path, got {} ({})",
                    other.type_name(),
                    other.fmt_value()
                ),
            ))
        }
        None => {
            return Err(LusaError::new(
                E_INDEX,
                Some(span),
                "io_write_binary: missing argument `path`",
            ))
        }
    };

    let bytes: Vec<u8> = match args.get(1).map(Value::base_value) {
        Some(Value::Str(s)) => s.as_bytes().to_vec(),
        Some(Value::Table(t)) => {
            let len = t.len();
            let mut out = Vec::with_capacity(len);
            for i in 1..=len {
                let key = Value::Number(i as f64);
                let value = t.get(&key).ok_or_else(|| {
                    LusaError::new(
                        E_INDEX,
                        Some(span),
                        format!("io_write_binary: missing byte at index {}", i),
                    )
                })?;
                let n = match value {
                    Value::Number(n) => n,
                    other => {
                        return Err(LusaError::new(
                            E_INDEX,
                            Some(span),
                            format!(
                                "io_write_binary: byte {} must be a number, got {} ({})",
                                i,
                                other.type_name(),
                                other.fmt_value()
                            ),
                        ))
                    }
                };
                if !n.is_finite() || n.fract() != 0.0 || !(0.0..=255.0).contains(&n) {
                    return Err(LusaError::new(
                        E_INDEX,
                        Some(span),
                        format!("io_write_binary: byte {} is out of range: {}", i, n),
                    ));
                }
                out.push(n as u8);
            }
            out
        }
        Some(other) => {
            return Err(LusaError::new(
                E_INDEX,
                Some(span),
                format!(
                    "io_write_binary: expected a byte table or string, got {} ({})",
                    other.type_name(),
                    other.fmt_value()
                ),
            ))
        }
        None => {
            return Err(LusaError::new(
                E_INDEX,
                Some(span),
                "io_write_binary: missing argument `bytes`",
            ))
        }
    };

    std::fs::write(path.as_ref(), &bytes).map_err(|e| {
        LusaError::new(
            E_IO,
            Some(span),
            format!("io_write_binary: cannot write `{}`: {}", path, e),
        )
    })?;
    Ok(vec![Value::Bool(true)])
}

/// `io_read_binary(path)`: reads raw bytes into a dense 1-based table.
fn b_io_read_binary(_i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let path = match args.first().map(Value::base_value) {
        Some(Value::Str(s)) => s,
        Some(other) => {
            return Err(LusaError::new(
                E_INDEX,
                Some(span),
                format!(
                    "io_read_binary: expected a string path, got {} ({})",
                    other.type_name(),
                    other.fmt_value()
                ),
            ))
        }
        None => {
            return Err(LusaError::new(
                E_INDEX,
                Some(span),
                "io_read_binary: missing argument `path`",
            ))
        }
    };
    let raw = std::fs::read(path.as_ref()).map_err(|e| {
        LusaError::new(
            E_IO,
            Some(span),
            format!("io_read_binary: cannot read `{}`: {}", path, e),
        )
    })?;
    let table = Rc::new(TableObj::new());
    for (i, b) in raw.into_iter().enumerate() {
        table
            .set(Value::Number((i + 1) as f64), Value::Number(b as f64))
            .map_err(|e| LusaError::new(e.code, Some(span), e.msg))?;
    }
    Ok(vec![Value::Table(table)])
}

fn b_type(_i: &mut Interp, args: Vec<Value>, _span: Span) -> LResult<Vec<Value>> {
    let t = args.first().map(|v| v.type_name().to_string()).unwrap_or("no value".into());
    one(Value::Str(Arc::from(t.as_str())))
}

fn b_tostring(_i: &mut Interp, args: Vec<Value>, _span: Span) -> LResult<Vec<Value>> {
    let s = args.first().map(to_str).unwrap_or_else(|| "nil".into());
    one(Value::Str(Arc::from(s.as_str())))
}

fn b_tonumber(_i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let v = match args.first() {
        Some(Value::Number(n)) => return one(Value::Number(*n)),
        Some(Value::Str(s)) => s.trim().to_string(),
        Some(Value::Nil) | None => return one(Value::Nil),
        Some(other) => {
            return Err(LusaError::new(
                E_NUM,
                Some(span),
                format!("expected string or number to convert, got {}", other.type_name()),
            ))
        }
    };
    match v.parse::<f64>() {
        Ok(n) => one(Value::Number(n)),
        Err(_) => one(Value::Nil),
    }
}

fn b_error(_i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let msg = args.first().map(to_str).unwrap_or_else(|| "unspecified error".into());
    // no extra prefix here: diagnostics already render `error[E9000]: msg`
    Err(LusaError::new(E_ERROR, Some(span), msg))
}

/// `pcall(f, ...)`: protected call — never raises. On success returns
/// `true` followed by the function's return values; on any error (user-raised,
/// type, ownership, or stack overflow) returns `false` plus an error string
/// shaped like `"E9000: boom"` so programs can inspect the E-code.
fn b_pcall(i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let callee = match args.first() {
        Some(v) => v.clone(),
        None => {
            return Err(LusaError::new(
                E_ERROR,
                Some(span),
                "missing argument `f` for pcall(f, ...)".to_string(),
            ))
        }
    };
    let rest: Vec<Value> = args.into_iter().skip(1).collect();
    match i.call_value(&callee, rest, span) {
        Ok(mut rets) => {
            let mut out = Vec::with_capacity(rets.len() + 1);
            out.push(Value::Bool(true));
            out.append(&mut rets);
            Ok(out)
        }
        Err(e) => Ok(vec![
            Value::Bool(false),
            Value::Str(Arc::from(format!("{}: {}", e.code, e.msg).as_str())),
        ]),
    }
}

fn b_assert(_i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let ok = args.first().map(|v| v.truthy()).unwrap_or(false);
    if ok {
        Ok(args)
    } else {
        let msg = args.get(1).map(to_str).unwrap_or_else(|| "assertion failed!".into());
        Err(LusaError::new(E_ASSERT, Some(span), msg))
    }
}

fn b_pairs(_i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let t = expect_table(args.first().unwrap_or(&Value::Nil), span)?;
    one(Value::Iter(Rc::new(RefCell::new(IterState { table: t, idx: 0 }))))
}

fn b_ipairs(_i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    b_pairs(_i, args, span)
}

use std::cell::RefCell;

// ===== package system (`require`) =====

thread_local! {
    /// Loaded modules by canonical path (survives nested module Interps).
    static MODULE_CACHE: RefCell<std::collections::HashMap<PathBuf, Value>> =
        RefCell::new(std::collections::HashMap::new());
    /// Currently-loading modules, for circular-require detection.
    static MODULE_STACK: RefCell<Vec<PathBuf>> = RefCell::new(Vec::new());
}

/// Directories searched by `require`, in priority order.
fn packages_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(base) = std::env::var("LUSA_PACKAGES") {
        if !base.is_empty() {
            roots.push(PathBuf::from(base));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd.join("packages"));
    }
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        roots.push(PathBuf::from(local).join("Lusa").join("packages"));
    } else if let Ok(home) = std::env::var("HOME") {
        roots.push(PathBuf::from(&home).join(".lusa").join("packages"));
    }
    roots
}

/// Reads `entry = "file.lusa"` from a package's `lusa.toml`; defaults to
/// `init.lusa` when the manifest or key is missing.
fn package_entry(pkg_dir: &Path) -> String {
    let txt = match std::fs::read_to_string(pkg_dir.join("lusa.toml")) {
        Ok(t) => t,
        Err(_) => return "init.lusa".into(),
    };
    for line in txt.lines() {
        let t = line.trim();
        if let Some(eq) = t.find('=') {
            if t[..eq].trim() == "entry" {
                let val = t[eq + 1..].trim().trim_matches(|c| c == '"' || c == '\'');
                if !val.is_empty() {
                    return val.replace('\\', "/");
                }
            }
        }
    }
    "init.lusa".into()
}

/// Resolves a `require("...")` name to a module file. Accepts `"pkg"` (uses the
/// package entry point) and `"pkg/sub/file.lusa"` (specific file).
fn resolve_module(name: &str) -> Option<PathBuf> {
    let name = name.replace('\\', "/");
    let (pkg, file) = match name.split_once('/') {
        Some((p, f)) if !f.is_empty() && !p.is_empty() => (p, f),
        _ => (name.as_str(), ""),
    };
    for root in packages_roots() {
        let pkg_dir = root.join(pkg);
        if !pkg_dir.is_dir() {
            continue;
        }
        let rel = if file.is_empty() {
            package_entry(&pkg_dir)
        } else {
            file.to_string()
        };
        let path = pkg_dir.join(rel);
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

fn b_require(parent: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let raw = args.first().map(to_str).ok_or_else(|| {
        LusaError::new(
            E_ERROR,
            Some(span),
            "missing argument `name` for require(\"package\")".to_string(),
        )
    })?;
    let name = raw.replace('\\', "/");
    let path = resolve_module(&name).ok_or_else(|| {
        let searched = packages_roots()
            .iter()
            .map(|r| format!("{}/<package>{}", r.display(), "/init.lusa"))
            .collect::<Vec<_>>()
            .join("; ");
        LusaError::new(
            E_ERROR,
            Some(span),
            format!(
                "package \"{}\" not found — install it with `lusalang install <user>/<repo>` (searched: {})",
                name, searched
            ),
        )
    })?;
    let canon = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());

    if let Some(v) = MODULE_CACHE.with(|c| c.borrow().get(&canon).cloned()) {
        return one(v);
    }
    if MODULE_STACK.with(|s| s.borrow().contains(&canon)) {
        return Err(LusaError::new(
            E_ERROR,
            Some(span),
            format!("circular require of \"{}\"", name),
        ));
    }

    let src = std::fs::read_to_string(&path).map_err(|e| {
        LusaError::new(
            E_ERROR,
            Some(span),
            format!("cannot read module {}: {}", path.display(), e),
        )
    })?;
    let block = crate::parser::parse(&src)?;

    MODULE_STACK.with(|s| s.borrow_mut().push(canon.clone()));
    let mut module_interp = Interp::new();
    module_interp.install_stdlib();
    let result = module_interp.run(&block, &path.display().to_string());
    MODULE_STACK.with(|s| {
        s.borrow_mut().pop();
    });
    let rets = result?;
    parent.out.append(&mut module_interp.out);

    let exports = rets.into_iter().next().unwrap_or(Value::Nil);
    let cached = exports.clone();
    MODULE_CACHE.with(|c| c.borrow_mut().insert(canon, exports));
    one(cached)
}

// ===== modules =====

pub fn all() -> Vec<(&'static str, BF)> {
    vec![
        ("print", Box::new(b_print)),
        ("type", Box::new(b_type)),
        ("tostring", Box::new(b_tostring)),
        ("tonumber", Box::new(b_tonumber)),
        ("error", Box::new(b_error)),
        ("assert", Box::new(b_assert)),
        ("pcall", Box::new(b_pcall)),
        ("pairs", Box::new(b_pairs)),
        ("ipairs", Box::new(b_ipairs)),
        ("Box", Box::new(b_box)),
        ("Ok", sum_ctor("result", "ok", "Ok")),
        ("Err", sum_ctor("result", "err", "Err")),
        ("Some", sum_ctor("option", "some", "Some")),
        ("None", Box::new(b_none)),
        ("box_read", Box::new(b_box_read)),
        ("box_write", Box::new(b_box_write)),
        ("unsafe_ptr_cast", Box::new(b_unsafe_ptr_cast)),
        ("unsafe_write_mem", Box::new(b_unsafe_write_mem)),
        ("unsafe_read_mem", Box::new(b_unsafe_read_mem)),
        ("unsafe_alloc", Box::new(b_unsafe_alloc)),
        ("volatile_read", Box::new(b_volatile_read)),
        ("volatile_write", Box::new(b_volatile_write)),
        ("unsafe_nop", Box::new(b_unsafe_nop)),
        ("interrupts_disable", Box::new(b_interrupts_disable)),
        ("interrupts_enable", Box::new(b_interrupts_enable)),
        ("interrupts_restore", Box::new(b_interrupts_restore)),
        ("unsafe_asm", Box::new(b_unsafe_asm)),
        ("asm", Box::new(b_asm)),
        ("io_read_file", Box::new(b_io_read_file)),
        ("io_write_binary", Box::new(b_io_write_binary)),
        ("io_read_binary", Box::new(b_io_read_binary)),
        ("require", Box::new(b_require)),
    ]
}

pub fn modules() -> Vec<(&'static str, Vec<(String, BF)>)> {
    let map = |v: Vec<(&'static str, BF)>| v.into_iter().map(|(k, f)| (String::from(k), f)).collect();
    vec![
        ("math", math_mod()),
        ("string", map(string_mod())),
        ("table", map(table_mod())),
        ("json", map(json_mod())),
        ("coroutine", crate::coroutine::module()),
        ("task", crate::task::module()),
    ]
}

fn math_mod() -> Vec<(String, BF)> {
    // xorshift64* — deterministic seed for repeatable tests
    static RNG_SEED: std::sync::Mutex<u64> = std::sync::Mutex::new(0x123456789ABCDEF0);

    fn rng_uniform() -> f64 {
        let mut s = RNG_SEED.lock().unwrap();
        let mut x = *s;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        *s = x;
        let bits = x.wrapping_mul(0x2545F4914F6CDD1D);
        (bits >> 11) as f64 / (1u64 << 53) as f64
    }

    fn m1(f: impl Fn(f64) -> f64 + 'static) -> BF {
        Box::new(move |_: &mut Interp, a: Vec<Value>, s: Span| {
            let n = need_num(get(&a, 0, "x", s)?, "x", s)?;
            one(Value::Number(f(n)))
        })
    }
    fn m2(f: impl Fn(f64, f64) -> f64 + 'static) -> BF {
        Box::new(move |_: &mut Interp, a: Vec<Value>, s: Span| {
            let x = need_num(get(&a, 0, "x", s)?, "x", s)?;
            let y = need_num(get(&a, 1, "y", s)?, "y", s)?;
            one(Value::Number(f(x, y)))
        })
    }
    vec![
        ("abs", m1(|x| x.abs())),
        ("floor", m1(|x| x.floor())),
        ("ceil", m1(|x| x.ceil())),
        ("round", m1(|x| (x + if x >= 0.0 { 0.5 } else { -0.5 }).floor())),
        ("sqrt", m1(|x| x.sqrt())),
        ("exp", m1(|x| x.exp())),
        ("log", m1(|x| x.ln())),
        ("sin", m1(|x| x.sin())),
        ("cos", m1(|x| x.cos())),
        ("tan", m1(|x| x.tan())),
        ("max", m2(|x, y| x.max(y))),
        ("min", m2(|x, y| x.min(y))),
        ("fmod", m2(|x, y| x % y)),
        ("modf", {
            Box::new(move |_: &mut Interp, a: Vec<Value>, s: Span| {
                let n = need_num(get(&a, 0, "x", s)?, "x", s)?;
                let frac = n - n.floor();
                let int = n.floor();
                Ok(vec![Value::Number(int), Value::Number(frac)])
            })
        }),
        ("tointeger", {
            Box::new(move |_: &mut Interp, a: Vec<Value>, s: Span| {
                let v = get(&a, 0, "x", s)?;
                match v {
                    Value::Number(n) if n.fract() == 0.0 && n.is_finite() => {
                        one(Value::Number(*n))
                    }
                    Value::Number(_) => one(Value::Nil),
                    other => Err(LusaError::new(
                        E_NUM,
                        Some(s),
                        format!("math.tointeger needs a number, got {}", other.type_name()),
                    )),
                }
            })
        }),
        ("random", {
            Box::new(move |_: &mut Interp, a: Vec<Value>, s: Span| {
                match a.len() {
                    0 => one(Value::Number(rng_uniform())),
                    1 => {
                        let m = need_num(&a[0], "m", s)? as i64;
                        if m <= 0 {
                            return Err(LusaError::new(
                                E_NUM,
                                Some(s),
                                "math.random needs m > 0",
                            ));
                        }
                        one(Value::Number(
                            (rng_uniform() * (m as f64)).floor().min((m as f64) - 1.0) + 1.0,
                        ))
                    }
                    _ => {
                        let lo = need_num(&a[0], "m", s)? as i64;
                        let hi = need_num(&a[1], "n", s)? as i64;
                        if hi < lo {
                            return Err(LusaError::new(
                                E_NUM,
                                Some(s),
                                "intervalo vazio",
                            ));
                        }
                        let span = (hi - lo + 1) as f64;
                        one(Value::Number(
                            (rng_uniform() * span).floor().min(span - 1.0) + lo as f64,
                        ))
                    }
                }
            })
        }),
        ("randomseed", {
            Box::new(move |_: &mut Interp, a: Vec<Value>, s: Span| {
                let v = get(&a, 0, "x", s)?;
                *RNG_SEED.lock().unwrap() = match v {
                    Value::Number(n) => n.to_bits(),
                    _ => 0x9E3779B97F4A7C15,
                };
                Ok(Vec::new())
            })
        }),
    ]
    .into_iter()
    .map(|(k, f)| (String::from(k), f))
    .collect()
}

fn string_mod() -> Vec<(&'static str, BF)> {
    vec![
        (
            "len",
            Box::new(|_: &mut Interp, a: Vec<Value>, s: Span| {
                let v = to_str(get(&a, 0, "s", s)?);
                one(Value::Number(v.chars().count() as f64))
            }),
        ),
        (
            "upper",
            Box::new(|_: &mut Interp, a: Vec<Value>, s: Span| {
                let v = to_str(get(&a, 0, "s", s)?);
                one(Value::Str(Arc::from(v.to_uppercase().as_str())))
            }),
        ),
        (
            "lower",
            Box::new(|_: &mut Interp, a: Vec<Value>, s: Span| {
                let v = to_str(get(&a, 0, "s", s)?);
                one(Value::Str(Arc::from(v.to_lowercase().as_str())))
            }),
        ),
        (
            "sub",
            Box::new(|_: &mut Interp, a: Vec<Value>, s: Span| {
                let src = to_str(get(&a, 0, "s", s)?);
                let len = src.chars().count() as f64;
                let norm = |x: f64, dflt: f64| -> usize {
                    if x.is_nan() {
                        dflt as usize
                    } else {
                        let i = x.clamp(-len, len).rem_euclid(len + 1.0) as usize;
                        i
                    }
                };
                let i0 = norm(need_num(get(&a, 1, "i", s)?, "i", s)?, 1.0);
                let j0 = match a.get(2) {
                    Some(Value::Number(n)) => norm(*n, len),
                    _ => len as usize,
                };
                let empty: Vec<char> = src.chars().collect();
                let (b, e) = if i0 == 0 { (0usize, j0.saturating_sub(1)) } else { (i0 - 1, j0) };
                let b = b.min(e + 1).min(empty.len());
                let e = e.min(empty.len());
                one(Value::Str(Arc::from(empty[b..e].iter().collect::<String>().as_str())))
            }),
        ),
        (
            "rep",
            Box::new(|_: &mut Interp, a: Vec<Value>, s: Span| {
                let src = to_str(get(&a, 0, "s", s)?);
                let n = need_num(get(&a, 1, "n", s)?, "n", s)? as usize;
                one(Value::Str(Arc::from(src.repeat(n).as_str())))
            }),
        ),
        (
            "reverse",
            Box::new(|_: &mut Interp, a: Vec<Value>, s: Span| {
                let src = to_str(get(&a, 0, "s", s)?);
                one(Value::Str(Arc::from(src.chars().rev().collect::<String>().as_str())))
            }),
        ),
        (
            "char",
            Box::new(|_: &mut Interp, a: Vec<Value>, s: Span| {
                let mut out = String::new();
                for v in &a {
                    let n = need_num(v, "code", s)?;
                    let c = char::from_u32(n as u32).ok_or_else(|| {
                        LusaError::new(E_NUM, Some(s), format!("invalid character code ({})", n))
                    })?;
                    out.push(c);
                }
                one(Value::Str(Arc::from(out.as_str())))
            }),
        ),
        (
            "byte",
            Box::new(|_: &mut Interp, a: Vec<Value>, s: Span| {
                let src = to_str(get(&a, 0, "s", s)?);
                let chars: Vec<char> = src.chars().collect();
                let len = chars.len() as f64;
                let i0 = match a.get(1) {
                    Some(Value::Number(n)) => *n,
                    _ => 1.0,
                };
                let j0 = match a.get(2) {
                    Some(Value::Number(n)) => *n,
                    _ => i0,
                };
                let lo = (i0.max(1.0) as usize).min(len as usize);
                let hi = (j0.max(lo as f64) as usize).min(len as usize);
                let mut out = Vec::new();
                if lo >= 1 {
                    for k in lo..=hi {
                        out.push(Value::Number(chars[k - 1] as u32 as f64));
                    }
                }
                Ok(out)
            }),
        ),
        (
            "find",
            Box::new(|_: &mut Interp, a: Vec<Value>, s: Span| {
                let src = to_str(get(&a, 0, "s", s)?);
                let pat = to_str(get(&a, 1, "pat", s)?);
                let hay: Vec<char> = src.chars().collect();
                let Some(start) = pattern_init(a.get(2), hay.len()) else {
                    return one(Value::Nil);
                };
                let plain = a.get(3).map(Value::truthy).unwrap_or(false);
                let Some(found) = pattern::find(&src, &pat, start, plain)
                    .map_err(|err| pattern_error(err, s))?
                else {
                    return one(Value::Nil);
                };
                let mut out = vec![
                    Value::Number((found.start + 1) as f64),
                    Value::Number(found.end as f64),
                ];
                out.extend(capture_values(&found, &hay));
                Ok(out)
            }),
        ),
        (
            "match",
            Box::new(|_: &mut Interp, a: Vec<Value>, s: Span| {
                let src = to_str(get(&a, 0, "s", s)?);
                let pat = to_str(get(&a, 1, "pat", s)?);
                let hay: Vec<char> = src.chars().collect();
                let Some(start) = pattern_init(a.get(2), hay.len()) else {
                    return one(Value::Nil);
                };
                let Some(found) = pattern::find(&src, &pat, start, false)
                    .map_err(|err| pattern_error(err, s))?
                else {
                    return one(Value::Nil);
                };
                Ok(match_values(&found, &hay))
            }),
        ),
        (
            "gmatch",
            Box::new(|_: &mut Interp, a: Vec<Value>, s: Span| {
                let src = to_str(get(&a, 0, "s", s)?);
                let pat = to_str(get(&a, 1, "pat", s)?);
                let state = Rc::new(std::cell::RefCell::new(
                    Gmatch::new(&src, &pat).map_err(|err| pattern_error(err, s))?,
                ));
                Ok(vec![make_builtin(
                    "string.gmatch iterator",
                    Box::new(move |_: &mut Interp, _args: Vec<Value>, call_span: Span| {
                        let mut state = state.borrow_mut();
                        let Some(found) = state
                            .next_match()
                            .map_err(|err| pattern_error(err, call_span))?
                        else {
                            return Ok(Vec::new());
                        };
                        let source = state.source_chars();
                        Ok(match_values(&found, source))
                    }),
                )])
            }),
        ),
        (
            "gsub",
            Box::new(|_: &mut Interp, a: Vec<Value>, s: Span| {
                let src = to_str(get(&a, 0, "s", s)?);
                let pat = to_str(get(&a, 1, "pat", s)?);
                let repl = get(&a, 2, "repl", s)?;
                match repl.base_value() {
                    Value::Str(_) | Value::Number(_) => {}
                    other => {
                        return Err(LusaError::new(
                            E_NUM,
                            Some(s),
                            format!(
                                "string.gsub replacement must be a string or number; found {}",
                                other.type_name()
                            ),
                        ))
                    }
                }
                let rep = to_str(repl);
                let limit = match a.get(3) {
                    Some(Value::Number(n)) if n.is_finite() => {
                        Some((*n).max(0.0).trunc() as usize)
                    }
                    _ => None,
                };
                let (out, count) = pattern::gsub(&src, &pat, &rep, limit)
                    .map_err(|err| pattern_error(err, s))?;
                Ok(vec![
                    Value::Str(Arc::from(out.as_str())),
                    Value::Number(count as f64),
                ])
            }),
        ),
        (
            "format",
            Box::new(|_: &mut Interp, a: Vec<Value>, s: Span| {
                let f = to_str(get(&a, 0, "fmt", s)?);
                let mut ai = 1usize;
                let mut out = String::new();
                let mut ci = 0usize;
                let cs: Vec<char> = f.chars().collect();
                while ci < cs.len() {
                    let c = cs[ci];
                    ci += 1;
                    if c != '%' {
                        out.push(c);
                        continue;
                    }
                    if ci >= cs.len() {
                        return Err(LusaError::new(E_NUM, Some(s), "invalid format: `%` at end of string"));
                    }
                    // optional flags/width/precision (minimal): `-.2f`
                    let mut prec: Option<usize> = None;
                    while ci < cs.len() && matches!(cs[ci], '-' | '0'..='9') {
                        ci += 1;
                    }
                    if ci < cs.len() && cs[ci] == '.' {
                        ci += 1;
                        let d0 = ci;
                        while ci < cs.len() && cs[ci].is_ascii_digit() {
                            ci += 1;
                        }
                        let digits: String = cs[d0..ci].iter().collect();
                        prec = Some(digits.parse::<usize>().unwrap_or(6));
                    }
                    if ci >= cs.len() {
                        return Err(LusaError::new(E_NUM, Some(s), "invalid format: `%` at end of string"));
                    }
                    let spec = cs[ci];
                    ci += 1;
                    if spec == '%' {
                        out.push('%');
                        continue;
                    }
                    let arg = get(&a, ai, "value", s)?;
                    ai += 1;
                    match spec {
                        'd' | 'i' => {
                            let n = need_num(arg, "value", s)?;
                            out.push_str(&(n as i64).to_string());
                        }
                        'f' => {
                            let n = need_num(arg, "value", s)?;
                            out.push_str(&format!("{:.*}", prec.unwrap_or(6), n));
                        }
                        'g' | 's' => {
                            let mut v = to_str(arg);
                            if let Some(p) = prec {
                                v = v.chars().take(p).collect();
                            }
                            out.push_str(&v);
                        }
                        'x' => {
                            let n = need_num(arg, "value", s)?;
                            out.push_str(&format!("{:x}", n as i64));
                        }
                        'X' => {
                            let n = need_num(arg, "value", s)?;
                            out.push_str(&format!("{:X}", n as i64));
                        }
                        'q' => {
                            let v = to_str(arg);
                            out.push('"');
                            for ch in v.chars() {
                                if ch == '"' || ch == '\\' {
                                    out.push('\\');
                                    out.push(ch);
                                } else if ch == '\n' {
                                    out.push_str("\\n");
                                } else {
                                    out.push(ch);
                                }
                            }
                            out.push('"');
                        }
                        other => {
                            return Err(LusaError::new(
                                E_NUM,
                                Some(s),
                                format!("invalid format specifier `%{}`", other),
                            ))
                        }
                    }
                }
                one(Value::Str(Arc::from(out.as_str())))
            }),
        ),
    ]
}

fn table_mod() -> Vec<(&'static str, BF)> {    vec![
        (
            "insert",
            Box::new(|_: &mut Interp, a: Vec<Value>, s: Span| {
                let t = expect_table(get(&a, 0, "t", s)?, s)?;
                if a.len() == 2 {
                    let idx = t.next_arr.get();
                    t.next_arr.set(idx + 1);
                    t.set(Value::Number(idx as f64), a[1].clone()).unwrap_or(());
                } else if a.len() == 3 {
                    // positional insert: shift integer keys >= pos up by 1
                    let pos = need_num(&a[1], "pos", s)? as usize;
                    {
                        let mut cell = t.cell.borrow_mut();
                        for i in 0..cell.entries.len() {
                            if let Value::Number(n) = &cell.entries[i].0 {
                                let v_pos = *n as usize;
                                if v_pos >= pos && (v_pos as f64).fract() == 0.0 && v_pos >= 1 {
                                    cell.entries[i].0 = Value::Number((v_pos + 1) as f64);
                                }
                            }
                        }
                        // rebuild hash buckets
                        cell.by_hash.clear();
                        for i in 0..cell.entries.len() {
                            if let Some(h) = crate::eval::key_hash_loose(&cell.entries[i].0) {
                                cell.by_hash.entry(h).or_default().push(i);
                            }
                        }
                    }
                    t.set(Value::Number(pos as f64), a[2].clone()).unwrap_or(());
                    let maxk = t.next_arr.get();
                    if t.len() >= maxk {
                        t.next_arr.set(t.len() + 1);
                    }
                } else {
                    return Err(LusaError::new(
                        E_INDEX,
                        Some(s),
                        "table.insert needs a table plus a value (or a position and a value)",
                    ));
                }
                Ok(Vec::new())
            }),
        ),
        (
            "remove",
            Box::new(|_: &mut Interp, a: Vec<Value>, s: Span| {
                let t = expect_table(get(&a, 0, "t", s)?, s)?;
                let pos = match a.get(1) {
                    Some(Value::Number(n)) => *n as usize,
                    _ => t.len(),
                };
                let removed = {
                    let mut cell = t.cell.borrow_mut();
                    let key = Value::Number(pos as f64);
                    let mut found: Option<usize> = None;
                    for i in 0..cell.entries.len() {
                        if crate::eval::key_eq_loose(&cell.entries[i].0, &key) {
                            found = Some(i);
                            break;
                        }
                    }
                    let out = if let Some(i) = found {
                        let (_, slot) = cell.entries.remove(i);
                        // rebuild buckets+simplify
                        cell.by_hash.clear();
                        for k in 0..cell.entries.len() {
                            if let Some(h) = crate::eval::key_hash_loose(&cell.entries[k].0) {
                                cell.by_hash.entry(h).or_default().push(k);
                            }
                        }
                        // shift integer keys > pos down
                        for k in 0..cell.entries.len() {
                            if let Value::Number(n) = &cell.entries[k].0 {
                                let v_pos = *n as usize;
                                if v_pos > pos && (v_pos as f64).fract() == 0.0 {
                                    cell.entries[k].0 = Value::Number((v_pos - 1) as f64);
                                }
                            }
                        }
                        cell.by_hash.clear();
                        for k in 0..cell.entries.len() {
                            if let Some(h) = crate::eval::key_hash_loose(&cell.entries[k].0) {
                                cell.by_hash.entry(h).or_default().push(k);
                            }
                        }
                        match slot {
                            crate::value::SlotV::Set(v) => Some(v),
                            _ => None,
                        }
                    } else {
                        None
                    };
                    out
                };
                if let Some(len) = Some(t.len()) {
                    t.next_arr.set(len + 1);
                }
                match removed {
                    Some(v) => one(v),
                    None => one(Value::Nil),
                }
            }),
        ),
        (
            "concat",
            Box::new(|_: &mut Interp, a: Vec<Value>, s: Span| {
                let t = expect_table(get(&a, 0, "t", s)?, s)?;
                let sep = match a.get(1) {
                    Some(v) => to_str(v),
                    None => String::new(),
                };
                let mut out = String::new();
                for k in 1..=t.len() {
                    let v = t.get(&Value::Number(k as f64)).unwrap_or(Value::Nil);
                    // numbers and strings go through fine; `nil`/other need an error
                    let piece = match v.base_value() {
                        Value::Str(_) | Value::Number(_) => to_str(&v),
                        other => {
                            return Err(LusaError::new(
                                E_INDEX,
                                Some(s),
                                format!(
                                    "table.concat needs numbers or strings, found {} at position {}",
                                    other.type_name(),
                                    k
                                ),
                            ))
                        }
                    };
                    if k > 1 {
                        out.push_str(&sep);
                    }
                    out.push_str(&piece);
                }
                one(Value::Str(Arc::from(out.as_str())))
            }),
        ),
        (
            "unpack",
            Box::new(|_: &mut Interp, a: Vec<Value>, s: Span| {
                let t = expect_table(get(&a, 0, "t", s)?, s)?;
                let i0 = match a.get(1) {
                    Some(Value::Number(n)) => (*n).ceil().max(1.0) as usize,
                    _ => 1,
                };
                let j0 = match a.get(2) {
                    Some(Value::Number(n)) => (*n).ceil() as usize,
                    _ => t.len(),
                };
                let mut out = Vec::new();
                for k in i0..=j0 {
                    out.push(t.get(&Value::Number(k as f64)).unwrap_or(Value::Nil));
                }
                Ok(out)
            }),
        ),
        (
            "pack",
            Box::new(|_: &mut Interp, a: Vec<Value>, _: Span| {
                let t = Rc::new(TableObj::new());
                for (k, v) in a.iter().enumerate() {
                    t.set(Value::Number((k + 1) as f64), v.clone()).unwrap_or(());
                }
                t.set(Value::Str(Arc::from("n")), Value::Number(a.len() as f64))
                    .unwrap_or(());
                one(Value::Table(t))
            }),
        ),
        (
            "sort",
            Box::new(|i: &mut Interp, a: Vec<Value>, s: Span| {
                let t = expect_table(get(&a, 0, "t", s)?, s)?;
                let cmp = a.get(1).cloned();
                let mut items: Vec<Value> = Vec::with_capacity(t.len());
                for k in 1..=t.len() {
                    items.push(t.get(&Value::Number(k as f64)).unwrap_or(Value::Nil));
                }
                // insertion sort (stable, simple); comparisons: callable or `<`
                let mut i0 = 1usize;
                while i0 < items.len() {
                    let mut j0 = i0;
                    while j0 > 0 {
                        let less = if let Some(c) = &cmp {
                            let r = i.call_value(c, vec![items[j0].clone(), items[j0 - 1].clone()], s)?;
                            r.first().map(|v| v.truthy()).unwrap_or(false)
                        } else {
                            lua_lt(&items[j0], &items[j0 - 1], s)?
                        };
                        if less {
                            items.swap(j0, j0 - 1);
                            j0 -= 1;
                        } else {
                            break;
                        }
                    }
                    i0 += 1;
                }
                for (k, v) in items.iter().enumerate() {
                    t.set(Value::Number((k + 1) as f64), v.clone()).unwrap_or(());
                }
                Ok(Vec::new())
            }),
        ),
    ]
}

/// Lua `<` over numbers and strings (used by table.sort); errors otherwise.
fn lua_lt(x: &Value, y: &Value, span: Span) -> LResult<bool> {
    match (x.base_value(), y.base_value()) {
        (Value::Number(a), Value::Number(b)) => Ok(a < b),
        (Value::Str(a), Value::Str(b)) => Ok(a.as_ref() < b.as_ref()),
        (a, b) => Err(
            LusaError::new(
                E_CMP,
                Some(span),
                "table.sort needs numbers or strings (or a comparator function)",
            )
            .help(format!("found {} and {}", a.type_name(), b.type_name())),
        ),
    }
}

// (end of builtins)

// ===== json module =====

/// Quotes a string as a JSON string literal (control chars escaped, UTF-8 raw).
fn json_quote(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Encodes one value; `seen` holds table identities for cycle detection.
fn json_encode_value(
    v: &Value,
    span: Span,
    seen: &mut Vec<usize>,
    depth: usize,
    out: &mut String,
) -> LResult<()> {
    if depth > 128 {
        return Err(LusaError::new(E_ERROR, Some(span), "json.encode: nesting too deep"));
    }
    match v.base_value() {
        Value::Nil => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            if !n.is_finite() {
                return Err(LusaError::new(
                    E_ERROR,
                    Some(span),
                    format!("json.encode: cannot encode non-finite number ({})", n),
                ));
            }
            out.push_str(&format_number(*n));
        }
        Value::Str(s) => json_quote(s, out),
        Value::Table(t) => {
            let id = Rc::as_ptr(t) as usize;
            if seen.contains(&id) {
                return Err(LusaError::new(E_ERROR, Some(span), "json.encode: cyclic table"));
            }
            seen.push(id);
            // tables with dense integer keys 1..=n become arrays; others objects
            let mut entries: Vec<(Value, Value)> = Vec::new();
            for i in 0..t.entry_count() {
                if let Some((k, Some(val))) = t.entry_at(i) {
                    entries.push((k, val));
                }
            }
            let mut is_array = true;
            let mut maxk: usize = 0;
            for (k, _) in &entries {
                match k.base_value() {
                    Value::Number(n) if n.fract() == 0.0 && *n >= 1.0 => {
                        let idx = *n as usize;
                        if idx > maxk {
                            maxk = idx;
                        }
                    }
                    _ => is_array = false,
                }
            }
            if is_array && entries.len() != maxk {
                is_array = false; // holes: {1, 3} is not a JSON array
            }
            if is_array {
                entries.sort_by_key(|(k, _)| match k.base_value() {
                    Value::Number(n) => *n as usize,
                    _ => 0,
                });
                out.push('[');
                for (i, (_, val)) in entries.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    json_encode_value(val, span, seen, depth + 1, out)?;
                }
                out.push(']');
            } else {
                out.push('{');
                let mut first = true;
                for (k, val) in &entries {
                    let key_str = match k.base_value() {
                        Value::Str(s) => s.to_string(),
                        Value::Number(n) => format_number(*n),
                        other => {
                            return Err(LusaError::new(
                                E_ERROR,
                                Some(span),
                                format!("json.encode: cannot encode key of type {}", other.type_name()),
                            ))
                        }
                    };
                    if !first {
                        out.push(',');
                    }
                    first = false;
                    json_quote(&key_str, out);
                    out.push(':');
                    json_encode_value(val, span, seen, depth + 1, out)?;
                }
                out.push('}');
            }
            seen.pop();
        }
        Value::Func(_) | Value::Iter(_) | Value::Coroutine(_) | Value::Task(_) | Value::Alias(_) => {
            return Err(LusaError::new(
                E_ERROR,
                Some(span),
                format!("json.encode: cannot encode a value of type {}", v.type_name()),
            ))
        }
    }
    Ok(())
}

struct JsonParser<'a> {
    chars: &'a [char],
    pos: usize,
}

impl<'a> JsonParser<'a> {
    fn err(&self, msg: impl Into<String>) -> LusaError {
        LusaError::new(
            E_ERROR,
            None,
            format!("json.decode: {} (at position {})", msg.into(), self.pos),
        )
    }

    fn skip_ws(&mut self) {
        while self.pos < self.chars.len() && self.chars[self.pos].is_whitespace() {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn expect(&mut self, c: char) -> LResult<()> {
        match self.bump() {
            Some(x) if x == c => Ok(()),
            other => Err(self.err(format!("expected '{}', found {:?}", c, other))),
        }
    }

    fn parse_value(&mut self, depth: usize) -> LResult<Value> {
        if depth > 128 {
            return Err(self.err("nesting too deep"));
        }
        self.skip_ws();
        match self.peek() {
            None => Err(self.err("unexpected end of input")),
            Some('n') => self.parse_lit("null", Value::Nil),
            Some('t') => self.parse_lit("true", Value::Bool(true)),
            Some('f') => self.parse_lit("false", Value::Bool(false)),
            Some('"') => self.parse_string().map(|s| Value::Str(Arc::from(s.as_str()))),
            Some('[') => self.parse_array(depth),
            Some('{') => self.parse_object(depth),
            Some(c) if c == '-' || c.is_ascii_digit() => self.parse_number(),
            Some(c) => Err(self.err(format!("unexpected character '{}'", c))),
        }
    }

    fn parse_lit(&mut self, lit: &str, v: Value) -> LResult<Value> {
        for c in lit.chars() {
            match self.bump() {
                Some(x) if x == c => {}
                _ => return Err(self.err(format!("invalid literal, expected '{}'", lit))),
            }
        }
        Ok(v)
    }

    fn parse_number(&mut self) -> LResult<Value> {
        let start = self.pos;
        if self.peek() == Some('-') {
            self.pos += 1;
        }
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.peek() == Some('.') {
            self.pos += 1;
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some('e') | Some('E')) {
            self.pos += 1;
            if matches!(self.peek(), Some('+') | Some('-')) {
                self.pos += 1;
            }
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        let text: String = self.chars[start..self.pos].iter().collect();
        text.parse::<f64>()
            .map(Value::Number)
            .map_err(|_| self.err(format!("invalid number '{}'", text)))
    }

    fn parse_string(&mut self) -> LResult<String> {
        self.expect('"')?;
        let mut out = String::new();
        loop {
            match self.bump() {
                None => return Err(self.err("unterminated string")),
                Some('"') => return Ok(out),
                Some('\\') => match self.bump() {
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some('/') => out.push('/'),
                    Some('b') => out.push('\u{08}'),
                    Some('f') => out.push('\u{0C}'),
                    Some('n') => out.push('\n'),
                    Some('r') => out.push('\r'),
                    Some('t') => out.push('\t'),
                    Some('u') => {
                        let hi = self.parse_hex4()?;
                        let cp = if (0xD800..0xDC00).contains(&hi) {
                            if self.peek() == Some('\\') {
                                self.pos += 1;
                                self.expect('u')?;
                                let lo = self.parse_hex4()?;
                                if !(0xDC00..0xE000).contains(&lo) {
                                    return Err(self.err("invalid low surrogate"));
                                }
                                0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                            } else {
                                return Err(self.err("lone high surrogate"));
                            }
                        } else if (0xDC00..0xE000).contains(&hi) {
                            return Err(self.err("lone low surrogate"));
                        } else {
                            hi
                        };
                        out.push(
                            char::from_u32(cp).ok_or_else(|| self.err("invalid unicode escape"))?,
                        );
                    }
                    other => return Err(self.err(format!("invalid escape '\\{:?}'", other))),
                },
                Some(c) if (c as u32) < 0x20 => {
                    return Err(self.err("raw control character inside a JSON string"))
                }
                Some(c) => out.push(c),
            }
        }
    }

    fn parse_hex4(&mut self) -> LResult<u32> {
        let mut v = 0u32;
        for _ in 0..4 {
            let c = self
                .bump()
                .ok_or_else(|| self.err("unexpected end in \\u escape"))?;
            let d = c
                .to_digit(16)
                .ok_or_else(|| self.err(format!("invalid hex digit '{}'", c)))?;
            v = v * 16 + d;
        }
        Ok(v)
    }

    fn parse_array(&mut self, depth: usize) -> LResult<Value> {
        self.expect('[')?;
        let t = Rc::new(TableObj::new());
        let mut n = 0usize;
        self.skip_ws();
        if self.peek() == Some(']') {
            self.pos += 1;
            t.next_arr.set(1);
            return Ok(Value::Table(t));
        }
        loop {
            let v = self.parse_value(depth + 1)?;
            n += 1;
            let _ = t.set(Value::Number(n as f64), v);
            self.skip_ws();
            match self.bump() {
                Some(',') => continue,
                Some(']') => break,
                other => return Err(self.err(format!("expected ',' or ']', found {:?}", other))),
            }
        }
        t.next_arr.set(n + 1);
        Ok(Value::Table(t))
    }

    fn parse_object(&mut self, depth: usize) -> LResult<Value> {
        self.expect('{')?;
        let t = Rc::new(TableObj::new());
        self.skip_ws();
        if self.peek() == Some('}') {
            self.pos += 1;
            return Ok(Value::Table(t));
        }
        loop {
            self.skip_ws();
            let key = self.parse_string()?;
            self.skip_ws();
            self.expect(':')?;
            let v = self.parse_value(depth + 1)?;
            let _ = t.set(Value::Str(Arc::from(key.as_str())), v);
            self.skip_ws();
            match self.bump() {
                Some(',') => continue,
                Some('}') => break,
                other => return Err(self.err(format!("expected ',' or '}}', found {:?}", other))),
            }
        }
        Ok(Value::Table(t))
    }
}

fn json_mod() -> Vec<(&'static str, BF)> {
    vec![
        (
            "encode",
            Box::new(|_: &mut Interp, a: Vec<Value>, s: Span| {
                let v = get(&a, 0, "value", s)?;
                let mut out = String::new();
                let mut seen = Vec::new();
                json_encode_value(v, s, &mut seen, 0, &mut out)?;
                one(Value::Str(Arc::from(out.as_str())))
            }),
        ),
        (
            "decode",
            Box::new(|_: &mut Interp, a: Vec<Value>, s: Span| {
                let v = get(&a, 0, "text", s)?;
                let text = match v.base_value() {
                    Value::Str(t) => t.to_string(),
                    other => {
                        return Err(LusaError::new(
                            E_ERROR,
                            Some(s),
                            format!("json.decode: expected a string, got {}", other.type_name()),
                        ))
                    }
                };
                let chars: Vec<char> = text.chars().collect();
                let mut p = JsonParser { chars: &chars, pos: 0 };
                let val = p.parse_value(0)?;
                p.skip_ws();
                if p.pos < p.chars.len() {
                    return Err(p.err("trailing characters after JSON value"));
                }
                one(val)
            }),
        ),
    ]
}
