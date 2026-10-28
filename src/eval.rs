//! Expression evaluation — plain reads, moving reads, calls, operators.

use std::rc::Rc;
use std::sync::Arc;

use crate::ast::*;
use crate::error::{
    LResult, LusaError, Span, E_ARITH, E_CALL, E_CMP, E_CONCAT, E_INDEX, E_LEN,
};
use crate::interp::Interp;
use crate::value::{FuncBody, FuncObj, SlotV, TableObj, Value};

/// `true` when this expression may produce multiple values.
pub fn is_expansion(e: &Expr) -> bool {
    matches!(
        e.kind,
        ExprKind::Call { .. } | ExprKind::Method { .. } | ExprKind::Vararg
    )
}

// ===== loose key helpers (used by tables) =====

pub fn key_hash_loose(v: &Value) -> Option<u64> {
    match v {
        Value::Nil => None,
        Value::Bool(b) => Some(if *b { 0xB0001 } else { 0xB0002 }),
        Value::Number(n) => {
            if n.is_nan() {
                return None;
            }
            let m = if *n == 0.0 { 0.0 } else { *n };
            Some(m.to_bits())
        }
        Value::Str(s) => {
            let mut h: u64 = 0xcbf29ce484222325;
            for &b in s.as_bytes() {
                h = (h ^ b as u64).wrapping_mul(0x100000001b3);
            }
            Some(h)
        }
        Value::Table(t) => Some(t.vid.wrapping_mul(0x9E3779B97F4A7C15) | 1),
        Value::Func(f) => Some(f.vid.wrapping_mul(0x9E3779B97F4A7C15) | 2),
        Value::Iter(i) => Some((Rc::as_ptr(i) as *const () as u64).wrapping_mul(0x9E3779B97F4A7C15) | 3),
        Value::Coroutine(c) => Some((Rc::as_ptr(c) as *const () as u64).wrapping_mul(0x9E3779B97F4A7C15) | 4),
        Value::Task(t) => Some((Rc::as_ptr(t) as *const () as u64).wrapping_mul(0x9E3779B97F4A7C15) | 5),
        Value::Alias(a) => key_hash_loose(&a.base),
    }
}

pub fn key_eq_loose(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Nil, Value::Nil) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Number(x), Value::Number(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Table(x), Value::Table(y)) => Rc::ptr_eq(x, y),
        (Value::Func(x), Value::Func(y)) => Rc::ptr_eq(x, y),
        (Value::Iter(x), Value::Iter(y)) => Rc::ptr_eq(x, y),
        (Value::Coroutine(x), Value::Coroutine(y)) => Rc::ptr_eq(x, y),
        (Value::Alias(x), _) => key_eq_loose(&x.base, b),
        (_, Value::Alias(y)) => key_eq_loose(a, &y.base),
        _ => false,
    }
}

// ===== helpers =====

fn as_table(v: &Value) -> Option<Rc<TableObj>> {
    match v.base_value() {
        Value::Table(t) => Some(Rc::clone(t)),
        _ => None,
    }
}

fn index_err(v: &Value, span: Span) -> LusaError {
    LusaError::new(
        E_INDEX,
        Some(span),
        format!(
            "attempt to index a {} value ({})",
            v.type_name(),
            v.fmt_value()
        ),
    )
}

pub fn label_of(e: &Expr) -> String {
    match &e.kind {
        ExprKind::Var(n) => (**n).clone(),
        ExprKind::Field { obj, name } => format!("{}.{}", label_of(obj), (**name).clone()),
        ExprKind::Index { obj, .. } => format!("{}[…]", label_of(obj)),
        _ => "(expression)".into(),
    }
}

fn rename_owner(v: &Value, label: &str, span: Span) {
    if let Some(vid) = v.vid() {
        if !matches!(v, Value::Alias(_)) {
            crate::value::with_reg(|r| r.rename(vid, label, span));
        }
    }
}

// ===== plain evaluation =====

/// Reads a field index-style: mistakes report E_INDEX; moved slots read nil.
pub(crate) fn read_field(obj: &Value, key: &Value, span: Span) -> LResult<Value> {
    let t = as_table(obj).ok_or_else(|| index_err(obj, span))?;
    let slot = t.get_slot(key);
    match slot {
        Some(SlotV::Set(v)) => Ok(v),
        _ => Ok(Value::Nil), // moved-out and missing keys read as nil
    }
}

pub fn eval_expr(interp: &mut Interp, frame: &mut crate::interp::Frame, e: &Expr) -> LResult<Value> {
    match &e.kind {
        ExprKind::Nil => Ok(Value::Nil),
        ExprKind::True => Ok(Value::Bool(true)),
        ExprKind::False => Ok(Value::Bool(false)),
        ExprKind::Num(n) => Ok(Value::Number(*n)),
        ExprKind::Int(text) => Ok(Value::Number(crate::value::int_literal_to_f64(text))),
        ExprKind::Str(segs) => {
            if segs.len() == 1 {
                if let StrAstSeg::Lit(s) = &segs[0] {
                    return Ok(Value::Str(Arc::from(s.as_str())));
                }
            }
            let mut out = String::new();
            for s in segs {
                match s {
                    StrAstSeg::Lit(l) => out.push_str(l),
                    StrAstSeg::Interp(i) => {
                        let v = eval_expr(interp, frame, i)?;
                        if matches!(v, Value::Table(_) | Value::Func(_) | Value::Nil) {
                            return Err(LusaError::new(
                                E_CONCAT,
                                Some(i.span),
                                format!(
                                    "string interpolation needs string or number values; found {} ({})",
                                    v.type_name(),
                                    v.fmt_value()
                                ),
                            ));
                        }
                        out.push_str(&v.fmt_value());
                    }
                }
            }
            Ok(Value::Str(Arc::from(out.as_str())))
        }
        ExprKind::Var(name) => interp.var_lookup(frame, name, e.span),
        ExprKind::Field { obj, name } => {
            let o = eval_expr(interp, frame, obj)?;
            let key = Value::Str(Arc::from(name.as_str()));
            read_field(&o, &key, e.span)
        }
        ExprKind::Index { obj, key } => {
            let o = eval_expr(interp, frame, obj)?;
            let k = eval_key(interp, frame, key)?;
            read_field(&o, &k, e.span)
        }
        ExprKind::Call { callee, args, span_args_end } => {
            let c = eval_expr(interp, frame, callee)?;
            let argv = eval_args(interp, frame, args, c.is_builtin())?;
            let results = interp.call_value(&c, argv, e.span.merge(*span_args_end))?;
            Ok(results.into_iter().next().unwrap_or(Value::Nil))
        }
        ExprKind::Method { obj, name, args, span_args_end } => {
            let results = eval_method(interp, frame, obj, name, args, e.span.merge(*span_args_end))?;
            Ok(results.into_iter().next().unwrap_or(Value::Nil))
        }
        ExprKind::Table(entries) => {
            let tbl = TableObj::new();
            build_table(interp, frame, &tbl, entries)?;
            Ok(Value::Table(Rc::new(tbl)))
        }
        ExprKind::ArrayLit(elems) => {
            // Fixed-size arrays use 0-based numeric keys so `arr[i]` reads the
            // i-th element directly in every engine.
            let tbl = TableObj::new();
            for (i, item) in elems.iter().enumerate() {
                let label = format!("item {i}");
                let value = eval_take_single(interp, frame, item, &label, item.span)?;
                tbl.set(Value::Number(i as f64), value)?;
            }
            Ok(Value::Table(Rc::new(tbl)))
        }
        ExprKind::AddrOf(inner) => {
            let value = eval_expr(interp, frame, inner)?;
            let value = match value.base_value() {
                Value::Number(n) => *n,
                other => {
                    return Err(LusaError::new(
                        crate::error::E_RUNTIME,
                        Some(e.span),
                        format!(
                            "the dynamic engines can take the address of numeric values only; found {} ({})",
                            other.type_name(),
                            other.fmt_value()
                        ),
                    ));
                }
            };
            let addr = interp.addr_next;
            interp.addr_next = interp.addr_next.saturating_add(8);
            interp.unsafe_mem.insert(addr, value);
            Ok(Value::Number(addr as f64))
        }
        ExprKind::StructLit { name, fields } => {
            let def = match interp.struct_defs.get(name) {
                Some(def) => def.clone(),
                None => {
                    return Err(LusaError::new(
                        E_INDEX,
                        Some(e.span),
                        format!("unknown struct `{name}`"),
                    )
                    .help("declare it with `struct Name ... end` before using a struct literal"));
                }
            };
            let base = interp
                .default_struct_value(name)
                .unwrap_or_else(|| Value::Table(Rc::new(TableObj::new())));
            let tbl = match base {
                Value::Table(tbl) => tbl,
                _ => Rc::new(TableObj::new()),
            };
            let mut seen: Vec<&str> = Vec::new();
            for field in fields {
                if !def.fields.iter().any(|(fname, _)| fname == &field.name) {
                    return Err(LusaError::new(
                        E_INDEX,
                        Some(field.span),
                        format!("struct `{name}` has no field `{}`", field.name),
                    ));
                }
                if seen.contains(&field.name.as_str()) {
                    return Err(LusaError::new(
                        E_INDEX,
                        Some(field.span),
                        format!("duplicate field `{}` in struct literal", field.name),
                    ));
                }
                seen.push(field.name.as_str());
                let label = format!("field `{}` of `{name}`", field.name);
                let mut value = eval_take_single(interp, frame, &field.value, &label, field.span)?;
                if let Some((_, ty)) = def.fields.iter().find(|(fname, _)| fname == &field.name) {
                    if box_annotation(ty).is_some() && !value.is_nil() {
                        value = crate::builtins::box_value(value);
                    }
                }
                let key = Value::Str(Arc::from(field.name.as_str()));
                tbl.set(key, value)?;
            }
            Ok(Value::Table(tbl))
        }
        ExprKind::Func(lit) => {
            let vid = crate::value::with_reg(|r| r.alloc_vid());
            // Keep the scope chain ordered outermost -> innermost: name lookup
            // walks it in reverse, so the closure's current scopes must come
            // last and shadow captured outer variables with the same name.
            let mut env = frame.env.clone();
            env.extend(frame.scopes.clone());
            let fobj = FuncObj {
                vid,
                name: Some("function".into()),
                env,
                body: FuncBody::Lusa(Rc::new(lit.clone())),
            };
            Ok(Value::Func(Rc::new(fobj)))
        }
        ExprKind::Bin { op, l, r } => eval_bin(interp, frame, *op, l, r, e.span),
        ExprKind::Log { op, l, r } => {
            let lv = eval_expr(interp, frame, l)?;
            match op {
                LogOp::And => {
                    if lv.truthy() {
                        eval_expr(interp, frame, r)
                    } else {
                        Ok(lv)
                    }
                }
                LogOp::Or => {
                    if lv.truthy() {
                        Ok(lv)
                    } else {
                        eval_expr(interp, frame, r)
                    }
                }
            }
        }
        ExprKind::Un { op, e } => {
            let v = eval_expr(interp, frame, e)?;
            eval_un(interp, *op, v, e.span)
        }
        ExprKind::Borrow { e, .. } => {
            let v = eval_expr(interp, frame, e)?;
            interp.make_alias(v, "<borrow>", crate::value::AliasKind::Imm, e.span)
        }
        ExprKind::Clone { e } => {
            let v = eval_expr(interp, frame, e)?;
            Ok(interp.deep_clone_value(&v))
        }
        ExprKind::Move { e } => {
            let label = label_of(e);
            eval_take_single(interp, frame, e, &label, e.span)
        }
        ExprKind::Paren(inner) => eval_expr(interp, frame, inner),
        ExprKind::Try { value } => {
            let v = eval_expr(interp, frame, value)?;            let Some((kind, tag, payload)) = v.base_value().sum_parts() else {
                return Err(LusaError::new(
                    E_CALL,
                    Some(e.span),
                    format!(
                        "the `?` operator needs a Result or Option value, found {} ({})",
                        v.type_name(),
                        v.fmt_value()
                    ),
                )
                .help("`?` unwraps `Ok`/`Some` and returns `Err`/`None` from the current function"));
            };
            let success =
                (kind == "result" && tag == "ok") || (kind == "option" && tag == "some");
            if success {
                Ok(interp.deep_clone_value(&payload))
            } else {
                if interp.depth == 0 {
                    return Err(LusaError::new(
                        E_CALL,
                        Some(e.span),
                        "the `?` operator can only be used inside a function returning Result or Option",
                    ));
                }
                interp.try_pending = Some(crate::interp::TryPending {
                    depth: interp.depth,
                    value: v.deep_clone(),
                });
                Err(LusaError::new(
                    crate::error::E_TRY,
                    Some(e.span),
                    "internal: `?` propagation",
                ))
            }
        }
        ExprKind::Await { value } => {
            let v = eval_expr(interp, frame, value)?;
            crate::task::await_value(interp, &v, e.span)
        }
        ExprKind::Vararg => {
            let first = frame.varargs.get(0).map(|v| v.clone()).unwrap_or(Value::Nil);
            Ok(first)
        }
    }
}

// ===== loose key helpers =====

pub fn eval_key(interp: &mut Interp, frame: &mut crate::interp::Frame, e: &Expr) -> LResult<Value> {
    let k = eval_expr(interp, frame, e)?;
    if k.is_nil() {
        return Err(LusaError::new(
            crate::error::E_BAD_KEY,
            Some(e.span),
            "table index is nil",
        ));
    }
    if let Value::Number(n) = k {
        if n.is_nan() {
            return Err(LusaError::new(crate::error::E_BAD_KEY, Some(e.span), "table index is NaN"));
        }
    }
    if let Value::Str(_) = &k {
    } else if !matches!(&k, Value::Number(_) | Value::Bool(_) | Value::Table(_) | Value::Func(_) | Value::Alias(_)) {
        return Err(LusaError::new(
            crate::error::E_BAD_KEY,
            Some(e.span),
            format!("unsupported table key type {} ({})", k.type_name(), k.fmt_value()),
        )
        .help("usable keys: nil is disallowed; use numbers, strings, booleans, or table/function identities"));
    }
    Ok(k)
}

fn eval_un(interp: &mut Interp, op: UnOp, v: Value, span: Span) -> LResult<Value> {
    match op {
        UnOp::Not => Ok(Value::Bool(!v.truthy())),
        UnOp::Neg => match v {
            Value::Number(n) => Ok(Value::Number(-n)),
            other => Err(LusaError::new(
                E_ARITH,
                Some(span),
                format!(
                    "attempt to negate a {} value ({}) — numbers only",
                    other.type_name(),
                    other.fmt_value()
                ),
            )),
        },
        UnOp::Len => match v {
            Value::Str(s) => Ok(Value::Number(s.chars().count() as f64)),
            Value::Table(t) => Ok(Value::Number(t.len() as f64)),
            other => Err(LusaError::new(
                E_LEN,
                Some(span),
                format!(
                    "attempt to get length of a {} value ({})",
                    other.type_name(),
                    other.fmt_value()
                ),
            )),
        },
        UnOp::Deref => {
            // Boxes are owned pointers: dereferencing one is always safe and
            // yields the pointee directly.  Numeric addresses keep the raw
            // pointer sandbox rules below.
            if let Some(inner) = crate::builtins::box_inner(&v) {
                return Ok(inner);
            }
            if interp.unsafe_depth == 0 {
                return Err(LusaError::new(
                    crate::error::E_PTR_DEREF,
                    Some(span),
                    "raw pointer dereference requires an `unsafe` block",
                )
                .note("wrap the read in `unsafe do ... end`", None));
            }
            match v.base_value() {
                Value::Number(n) => {
                    let cell = interp.unsafe_mem.get(&(*n as u64)).copied().unwrap_or(0.0);
                    Ok(Value::Number(cell))
                }
                other => Err(LusaError::new(
                    crate::error::E_RUNTIME,
                    Some(span),
                    format!(
                        "cannot dereference a {} value ({}) - raw pointers are simulated addresses",
                        other.type_name(),
                        other.fmt_value()
                    ),
                )),
            }
        }
    }
}

/// A resolved `*place` assignment target: either a boxed pointee or a
/// simulated raw address.
pub enum DerefPlace {
    Address(u64),
    Box(Rc<TableObj>),
}

/// Resolves `*ptr = value` / `*box = value`, enforcing the `unsafe` rule only
/// for raw addresses.
pub fn eval_deref_place(
    interp: &mut Interp,
    frame: &mut crate::interp::Frame,
    e: &Expr,
    span: Span,
) -> LResult<DerefPlace> {
    let v = eval_expr(interp, frame, e)?;
    if let Some(table) = crate::builtins::box_table(&v) {
        return Ok(DerefPlace::Box(table));
    }
    if interp.unsafe_depth == 0 {
        return Err(LusaError::new(
            crate::error::E_PTR_DEREF,
            Some(span),
            "raw pointer dereference requires an `unsafe` block",
        )
        .note("wrap the write in `unsafe do ... end`", None));
    }
    match v.base_value() {
        Value::Number(n) if !n.is_nan() && *n >= 0.0 => Ok(DerefPlace::Address(*n as u64)),
        other => Err(LusaError::new(
            crate::error::E_RUNTIME,
            Some(span),
            format!(
                "raw pointer address must be a non-negative number, found {} ({})",
                other.type_name(),
                other.fmt_value()
            ),
        )),
    }
}

fn need_num(a: &Value, b: &Value, opname: &str, span: Span) -> LResult<(f64, f64)> {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => Ok((*x, *y)),
        _ => {
            let (bad, desc) = if !matches!(a, Value::Number(_)) {
                (a, a.fmt_value())
            } else {
                (b, b.fmt_value())
            };
            Err(LusaError::new(
                E_ARITH,
                Some(span),
                format!(
                    "attempt to perform arithmetic `{}` on a {} value ({}) — numbers only",
                    opname,
                    bad.type_name(),
                    desc
                ),
            ))
        }
    }
}

fn eval_bin(
    interp: &mut Interp,
    frame: &mut crate::interp::Frame,
    op: BinOp,
    l: &Expr,
    r: &Expr,
    span: Span,
) -> LResult<Value> {
    use BinOp::*;
    let lv = eval_expr(interp, frame, l)?;
    let rv = eval_expr(interp, frame, r)?;
    let out = match op {
        Add => {
            let (a, b) = need_num(&lv, &rv, "+", span)?;
            Value::Number(a + b)
        }
        Sub => {
            let (a, b) = need_num(&lv, &rv, "-", span)?;
            Value::Number(a - b)
        }
        Mul => {
            let (a, b) = need_num(&lv, &rv, "*", span)?;
            Value::Number(a * b)
        }
        Div => {
            let (a, b) = need_num(&lv, &rv, "/", span)?;
            Value::Number(a / b)
        }
        FloorDiv => {
            let (a, b) = need_num(&lv, &rv, "//", span)?;
            Value::Number((a / b).floor())
        }
        Mod => {
            let (a, b) = need_num(&lv, &rv, "%", span)?;
            if b == 0.0 {
                Value::Number(f64::NAN)
            } else {
                Value::Number(a - (a / b).floor() * b)
            }
        }
        Pow => {
            let (a, b) = need_num(&lv, &rv, "^", span)?;
            Value::Number(a.powf(b))
        }
        Concat => {
            let (x, y) = match (&lv, &rv) {
                (Value::Str(_) | Value::Number(_), Value::Str(_) | Value::Number(_)) => {
                    (lv.clone(), rv.clone())
                }
                _ => {
                    let (bad, d) = if matches!(lv, Value::Str(_) | Value::Number(_)) {
                        (rv.clone(), rv.fmt_value())
                    } else {
                        (lv.clone(), lv.fmt_value())
                    };
                    return Err(LusaError::new(
                        E_CONCAT,
                        Some(span),
                        format!(
                            "attempt to concatenate with a {} value ({})",
                            bad.type_name(),
                            d
                        ),
                    ));
                }
            };
            Value::Str(Arc::from(format!("{}{}", x.fmt_value(), y.fmt_value()).as_str()))
        }
        Eq => Value::Bool(key_eq_loose_discard_aliases(&lv, &rv)),
        Ne => Value::Bool(!key_eq_loose_discard_aliases(&lv, &rv)),
        Lt | Gt | Le | Ge => {
            let ok = match (&lv, &rv) {
                (Value::Number(x), Value::Number(y)) => match op {
                    Lt => x < y,
                    Gt => x > y,
                    Le => x <= y,
                    Ge => x >= y,
                    _ => false,
                },
                (Value::Str(x), Value::Str(y)) => {
                    let (a, b) = (x.to_string(), y.to_string());
                    match op {
                        Lt => a < b,
                        Gt => a > b,
                        Le => a <= b,
                        Ge => a >= b,
                        _ => false,
                    }
                }
                _ => {
                    return Err(LusaError::new(
                        E_CMP,
                        Some(span),
                        format!(
                            "attempt to compare {} ({}) with {} ({})",
                            lv.type_name(),
                            lv.fmt_value(),
                            rv.type_name(),
                            rv.fmt_value()
                        ),
                    )
                    .help("comparisons < <= > >= need two numbers or two strings (use ~= / == for identity)"));
                }
            };
            Value::Bool(ok)
        }
    };
    Ok(out)
}

fn key_eq_loose_discard_aliases(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Table(x), Value::Table(y)) => Rc::ptr_eq(x, y),
        (Value::Func(x), Value::Func(y)) => Rc::ptr_eq(x, y),
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Number(x), Value::Number(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Nil, Value::Nil) => true,
        (Value::Alias(x), _) => key_eq_loose_discard_aliases(&x.base, b),
        (_, Value::Alias(y)) => key_eq_loose_discard_aliases(a, &y.base),
        _ => false,
    }
}

// ===== calls =====

/// Evaluates an argument list.
/// `no_moves` (used for builtins): heap leaves are *borrowed*, the caller keeps
/// ownership. Otherwise heap leaves are *moved* into the call and
/// `borrow x` / `clone x` are honoured.
pub fn eval_args(
    interp: &mut Interp,
    frame: &mut crate::interp::Frame,
    args: &[Expr],
    no_moves: bool,
) -> LResult<Vec<Value>> {
    let mut out = Vec::new();
    let n = args.len();
    for (i, e) in args.iter().enumerate() {
        let is_last = i + 1 == n;
        if is_last && is_expansion(e) {
            let vs = eval_expand(interp, frame, e)?;
            out.extend(vs);
        } else if no_moves {
            // plain (non-moving) read: builtins never take ownership
            let v = eval_expr(interp, frame, e)?;
            out.push(v);
        } else {
            let label = format!("(argument {})", i + 1);
            let v = eval_take_single(interp, frame, e, &label, e.span)?;
            out.push(v);
        }
    }
    Ok(out)
}

/// Evaluates what produces multiple values (call, method, `...`).
pub fn eval_expand(interp: &mut Interp, frame: &mut crate::interp::Frame, e: &Expr) -> LResult<Vec<Value>> {
    match &e.kind {
        ExprKind::Call { callee, args, span_args_end } => {
            let c = eval_expr(interp, frame, callee)?;
            let argv = eval_args(interp, frame, args, c.is_builtin())?;
            interp.call_value(&c, argv, e.span.merge(*span_args_end))
        }
        ExprKind::Method { obj, name, args, .. } => {
            eval_method(interp, frame, obj, name, args, e.span)
        }
        ExprKind::Vararg => {
            let taken: Vec<Value> = frame
                .varargs
                .iter()
                .map(|v| v.clone())
                .collect();
            frame.varargs.clear();
            Ok(taken)
        }
        _ => {
            let label = "(value)".to_string();
            let v = eval_take_single(interp, frame, e, &label, e.span)?;
            Ok(vec![v])
        }
    }
}

pub fn eval_method(
    interp: &mut Interp,
    frame: &mut crate::interp::Frame,
    obj: &Expr,
    name: &str,
    args: &[Expr],
    span: Span,
) -> LResult<Vec<Value>> {
    let o = eval_expr(interp, frame, obj)?;
    let key = Value::Str(Arc::from(name));

    // Reading the method: scalars (strings) get pseudo-methods from `string`;
    // tables may be real methods or fallback to the table library.
    let method: Value = match o.base_value() {
        Value::Str(_) => {
            let mod_table = match interp.var_lookup(frame, "string", span) {
                Ok(v) => v,
                Err(_) => {
                    return Err(LusaError::new(E_CALL, Some(span), "string methods need the `string` module")
                    )
                }
            };
            let t = as_table(&mod_table)
                .ok_or_else(|| LusaError::new(E_CALL, Some(span), "`string` is not a table"))?;
            let m = t
                .get(&key)
                .ok_or_else(|| LusaError::new(E_CALL, Some(span), format!("string has no method `:{}`", name)))?;
            m
        }
        _ => {
            let t = as_table(&o).ok_or_else(|| index_err(&o, span))?;
            if let Some(method) = t.get(&key) {
                method
            } else if Interp::is_sum_method(name)
                && Value::Table(Rc::clone(&t)).sum_parts().is_some()
            {
                let mut argv = Vec::new();
                let n = args.len();
                for (i, e) in args.iter().enumerate() {
                    let is_last = i + 1 == n;
                    if is_last && is_expansion(e) {
                        argv.extend(eval_expand(interp, frame, e)?);
                    } else {
                        let label = format!("(argument {} of :{})", i + 1, name);
                        let v = eval_take_single(interp, frame, e, &label, e.span)?;
                        argv.push(v);
                    }
                }
                return interp.sum_method_call(&t, name, argv, span);
            } else if Interp::is_vec_method(name) {
                // `Vec<T>` is a 0-based table in the dynamic engines; the
                // vector surface is a fallback when no real field shadows it.
                let mut argv = Vec::new();
                let n = args.len();
                for (i, e) in args.iter().enumerate() {
                    let is_last = i + 1 == n;
                    if is_last && is_expansion(e) {
                        argv.extend(eval_expand(interp, frame, e)?);
                    } else {
                        let label = format!("(argument {} of :{})", i + 1, name);
                        let v = eval_take_single(interp, frame, e, &label, e.span)?;
                        argv.push(v);
                    }
                }
                return interp.vec_method_call(&t, name, argv, span);
            } else {
                return Err(LusaError::new(
                    E_CALL,
                    Some(span),
                    format!(
                        "attempt to call method `:{}` (no such method on {})",
                        name,
                        o.fmt_value()
                    ),
                ));
            }
        }
    };

    let receiver = interp.make_alias(o, "self", crate::value::AliasKind::Mut, span)?;
    // note: aliases are Copy — we need the underlying value to stay usable
    let mut argv = vec![receiver];
    let n = args.len();
    for (i, e) in args.iter().enumerate() {
        let is_last = i + 1 == n;
        if is_last && is_expansion(e) {
            let vs = eval_expand(interp, frame, e)?;
            argv.extend(vs);
        } else {
            let label = format!("(argument {} of :{})", i + 1, name);
            let v = eval_take_single(interp, frame, e, &label, e.span)?;
            argv.push(v);
        }
    }
    interp.call_value(&method, argv, span)
}

// ===== moving reads (for assignment/args/returns) =====

/// "Moving read": consumes the value for assignments/calls/returns.
/// `label` describes where it goes — the rename and error notes use it.
pub fn eval_take_single(
    interp: &mut Interp,
    frame: &mut crate::interp::Frame,
    e: &Expr,
    label: &str,
    span: Span,
) -> LResult<Value> {
    match &e.kind {
        ExprKind::Var(name) => match interp.slot_loc(frame, name) {
            Some(loc) => interp.take_from_slot(&loc, name, label, span),
            None => Ok(Value::Nil),
        },
        ExprKind::Field { obj, name } => {
            let o = eval_expr(interp, frame, obj)?;
            let key = Value::Str(Arc::from(name.as_str()));
            take_table_slot(interp, o, &key, &label_of_field(obj, name), label, span)
        }
        ExprKind::Index { obj, key } => {
            let o = eval_expr(interp, frame, obj)?;
            let k = eval_key(interp, frame, key)?;
            take_table_slot(interp, o, &k, &label_of(obj), label, span)
        }
        ExprKind::Paren(inner) => eval_take_single(interp, frame, inner, label, span),
        ExprKind::Call { callee, args, span_args_end } => {
            let c = eval_expr(interp, frame, callee)?;
            let argv = eval_args(interp, frame, args, c.is_builtin())?;
            let results = interp.call_value(&c, argv, e.span.merge(*span_args_end))?;
            let v = results.into_iter().next().unwrap_or(Value::Nil);
            rename_owner(&v, label, span);
            Ok(v)
        }
        ExprKind::Method { obj, name, args, .. } => {
            let results = eval_method(interp, frame, obj, name, args, e.span)?;
            let v = results.into_iter().next().unwrap_or(Value::Nil);
            rename_owner(&v, label, span);
            Ok(v)
        }
        ExprKind::Vararg => {
            let v = frame.varargs.first().cloned().unwrap_or(Value::Nil);
            rename_owner(&v, label, span);
            Ok(v)
        }
        ExprKind::Borrow { e, is_mut } => {
            let v = eval_expr(interp, frame, e)?;
            let kind = if *is_mut {
                crate::value::AliasKind::Mut
            } else {
                crate::value::AliasKind::Imm
            };
            interp.make_alias(v, label, kind, span)
        }
        ExprKind::Clone { e } => {
            let v = eval_expr(interp, frame, e)?;
            let c = interp.deep_clone_value(&v);
            rename_owner(&c, label, span);
            Ok(c)
        }
        ExprKind::Move { e } => eval_take_single(interp, frame, e, label, span),
        other => {
            let v = eval_expr(
                interp,
                frame,
                &Expr {
                    kind: other.clone(),
                    span: e.span,
                },
            )?;
            rename_owner(&v, label, span);
            Ok(v)
        }
    }
}

fn label_of_field(obj: &Expr, name: &str) -> String {
    format!("field `{}` of {}", name, label_of(obj))
}

/// Moves a slot out of a table value (enforcing aliveness/borrows).
fn take_table_slot(
    interp: &mut Interp,
    obj: Value,
    key: &Value,
    src_desc: &str,
    label: &str,
    span: Span,
) -> LResult<Value> {
    let t = as_table(&obj).ok_or_else(|| index_err(&obj, span))?;
    interp.take_field_from_table(&t, key, src_desc, label, span)
}

pub fn eval_discard(
    interp: &mut Interp,
    frame: &mut crate::interp::Frame,
    e: &Expr,
    span: Span,
) -> LResult<()> {
    if is_expansion(e) {
        eval_expand(interp, frame, e)?;
    } else {
        eval_take_single(interp, frame, e, "(discarded)", span)?;
    }
    Ok(())
}

// ===== tables =====

/// Builds a table literal. Values are *moving* reads; keys are plain reads.
pub fn build_table(
    interp: &mut Interp,
    frame: &mut crate::interp::Frame,
    tbl: &TableObj,
    entries: &[TableEntry],
) -> LResult<()> {
    for (i, entry) in entries.iter().enumerate() {
        let is_last = i + 1 == entries.len();
        match &entry.key {
            Some(kexpr) => {
                let k = eval_key(interp, frame, kexpr)?;
                let label = format!("(into {})", label_of(kexpr));
                let v = eval_take_single(interp, frame, &entry.value, &format!("field `{}`", k.fmt_value()), entry.span)?;
                let _ = label;
                tbl.set(k, v).map_err(|e| e)?;
            }
            None => {
                if is_last && is_expansion(&entry.value) {
                    // `f(...)` in the last position: append all results
                    let vs = eval_expand(interp, frame, &entry.value)?;
                    for v in vs {
                        let idx = tbl.next_arr.get();
                        tbl.next_arr.set(idx + 1);
                        tbl.set(Value::Number(idx as f64), v).unwrap_or(());
                    }
                } else {
                    let idx = tbl.next_arr.get();
                    let label = format!("item {}", idx);
                    let v = eval_take_single(interp, frame, &entry.value, &label, entry.span)?;
                    tbl.next_arr.set(idx + 1);
                    tbl.set(Value::Number(idx as f64), v).unwrap_or(());
                }
            }
        }
    }
    Ok(())
}

