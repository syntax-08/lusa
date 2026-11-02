//! `HashMap<K, V>` and `String` in the dynamic engines (Module 29), and the
//! `Vec<T>` methods added with them.
//!
//! Like `Box<T>`, both are tables tagged with a private marker key:
//!
//! * a `String` keeps its text under [`STRING_MARKER`]; its methods replace
//!   that text, so the buffer is mutable while every `string` stays
//!   immutable;
//! * a `HashMap` keeps its entries in an inner table under [`MAP_MARKER`].
//!   Tables iterate in insertion order, and `remove` keeps the order of the
//!   remaining entries, which is exactly what the native map does.
//!
//! `iter()` returns a builtin iterator over a snapshot taken when it is
//! called, so `for k, v in m:iter()` never observes later changes.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use crate::error::{LResult, LusaError, Span, E_BAD_KEY, E_CALL, E_INDEX};
use crate::interp::Interp;
use crate::value::{TableObj, Value};

/// Marker key holding the text of a `String`.
pub const STRING_MARKER: &str = "\u{1}lusa_string";
/// Marker key holding the entry table of a `HashMap`.
pub const MAP_MARKER: &str = "\u{1}lusa_map";

fn key(text: &str) -> Value {
    Value::Str(Arc::from(text))
}

fn tagged(value: &Value, marker: &str) -> Option<Rc<TableObj>> {
    match value.base_value() {
        Value::Table(table) if table.get(&key(marker)).is_some() => Some(Rc::clone(table)),
        _ => None,
    }
}

/// A new `String` holding `text`.
pub fn string_value(text: Arc<str>) -> Value {
    let table = TableObj::new();
    table.set(key(STRING_MARKER), Value::Str(text)).expect("string marker key");
    Value::Table(Rc::new(table))
}

/// The text of a `String`, or `None` for any other value.
pub fn string_text(value: &Value) -> Option<Arc<str>> {
    let table = tagged(value, STRING_MARKER)?;
    match table.get(&key(STRING_MARKER)) {
        Some(Value::Str(text)) => Some(text),
        _ => Some(Arc::from("")),
    }
}

/// A new, empty `HashMap`.
pub fn map_value() -> Value {
    let table = TableObj::new();
    table
        .set(key(MAP_MARKER), Value::Table(Rc::new(TableObj::new())))
        .expect("map marker key");
    Value::Table(Rc::new(table))
}

/// The entry table of a `HashMap`, or `None` for any other value.
pub fn map_entries(value: &Value) -> Option<Rc<TableObj>> {
    let table = tagged(value, MAP_MARKER)?;
    match table.get(&key(MAP_MARKER)) {
        Some(Value::Table(entries)) => Some(entries),
        _ => None,
    }
}

/// True when `table` is a `String` or a `HashMap`.
pub fn is_wrapper(table: &Rc<TableObj>) -> bool {
    table.get(&key(STRING_MARKER)).is_some() || table.get(&key(MAP_MARKER)).is_some()
}

/// Annotation-driven construction: kind 0 turns a `string` initializer into a
/// `String`, kind 1 turns a table constructor into a `HashMap`.  Values that
/// already have the target type pass through.
pub fn wrap(kind: u8, value: Value, span: Span) -> LResult<Value> {
    if kind == 0 {
        if string_text(&value).is_some() {
            return Ok(value);
        }
        return match value.base_value() {
            Value::Str(text) => Ok(string_value(Arc::clone(text))),
            other => Err(LusaError::new(
                E_CALL,
                Some(span),
                format!("a `String` is built from a string, got {} ({})", other.type_name(), other.fmt_value()),
            )),
        };
    }
    if map_entries(&value).is_some() {
        return Ok(value);
    }
    match value.base_value() {
        Value::Table(source) => {
            let map = map_value();
            let entries = map_entries(&map).expect("fresh map");
            let mut i = 0;
            while let Some((k, v)) = source.entry_at(i) {
                if let Some(v) = v {
                    entries.set(k, v)?;
                }
                i += 1;
            }
            Ok(map)
        }
        other => Err(LusaError::new(
            E_CALL,
            Some(span),
            format!("a `HashMap` is built from a table constructor, got {} ({})", other.type_name(), other.fmt_value()),
        )),
    }
}

fn arity(args: &[Value], wanted: usize, method: &str, span: Span) -> LResult<()> {
    if args.len() == wanted {
        return Ok(());
    }
    Err(LusaError::new(
        E_CALL,
        Some(span),
        format!("`{method}` takes {wanted} argument(s), found {}", args.len()),
    ))
}

fn text_arg(value: &Value, method: &str, span: Span) -> LResult<Arc<str>> {
    if let Some(text) = string_text(value) {
        return Ok(text);
    }
    match value.base_value() {
        Value::Str(text) => Ok(Arc::clone(text)),
        other => Err(LusaError::new(
            E_CALL,
            Some(span),
            format!("`{method}` expects a string, got {} ({})", other.type_name(), other.fmt_value()),
        )),
    }
}

fn index_arg(value: &Value, method: &str, span: Span) -> LResult<i64> {
    match value.base_value() {
        Value::Number(n) if n.fract() == 0.0 => Ok(*n as i64),
        other => Err(LusaError::new(
            E_CALL,
            Some(span),
            format!("`{method}` expects an integer, got {} ({})", other.type_name(), other.fmt_value()),
        )),
    }
}

fn some(value: Value) -> Value {
    crate::builtins::make_sum("option", "some", value)
}

fn none() -> Value {
    crate::builtins::make_sum("option", "none", Value::Nil)
}

/// A builtin iterator over `items`: each call returns the next pair, then no
/// values once the snapshot is exhausted (the end of a generic `for`).
fn iterator(items: Vec<(Value, Value)>) -> Value {
    let items = Rc::new(items);
    let next = Cell::new(0usize);
    crate::builtins::make_builtin(
        "iter",
        Box::new(move |_interp: &mut Interp, _args: Vec<Value>, _span: Span| {
            let at = next.get();
            match items.get(at) {
                Some((k, v)) => {
                    next.set(at + 1);
                    Ok(vec![k.clone(), v.clone()])
                }
                None => Ok(Vec::new()),
            }
        }),
    )
}

/// Runs a `String` or `HashMap` method on a tagged table.
pub fn method_call(
    interp: &mut Interp,
    table: &Rc<TableObj>,
    name: &str,
    args: Vec<Value>,
    span: Span,
) -> LResult<Vec<Value>> {
    let value = Value::Table(Rc::clone(table));
    if let Some(text) = string_text(&value) {
        return string_method(table, &text, name, args, span);
    }
    let entries = map_entries(&value).expect("caller checked the tag");
    map_method(interp, &entries, name, args, span)
}

fn string_method(
    table: &Rc<TableObj>,
    text: &Arc<str>,
    name: &str,
    args: Vec<Value>,
    span: Span,
) -> LResult<Vec<Value>> {
    let store = |next: String| {
        table
            .set(key(STRING_MARKER), Value::Str(Arc::from(next.as_str())))
            .expect("string marker key");
    };
    match name {
        "push_str" => {
            arity(&args, 1, "String:push_str", span)?;
            let piece = text_arg(&args[0], "String:push_str", span)?;
            store(format!("{text}{piece}"));
            Ok(Vec::new())
        }
        "push" => {
            arity(&args, 1, "String:push", span)?;
            let code = index_arg(&args[0], "String:push", span)?;
            if !(0..128).contains(&code) {
                return Err(LusaError::new(
                    E_CALL,
                    Some(span),
                    format!("`String:push` takes an ASCII code (0 to 127), got {code}"),
                ));
            }
            let mut next = text.to_string();
            next.push(code as u8 as char);
            store(next);
            Ok(Vec::new())
        }
        "pop" => {
            arity(&args, 0, "String:pop", span)?;
            let mut next = text.to_string();
            let code = next.pop().map(|c| c as u32).unwrap_or(0);
            store(next);
            Ok(vec![Value::Number(code as f64)])
        }
        "len" => Ok(vec![Value::Number(text.len() as f64)]),
        "is_empty" => Ok(vec![Value::Bool(text.is_empty())]),
        "clear" => {
            store(String::new());
            Ok(Vec::new())
        }
        "as_str" => Ok(vec![Value::Str(Arc::clone(text))]),
        "contains" | "starts_with" | "ends_with" => {
            arity(&args, 1, &format!("String:{name}"), span)?;
            let needle = text_arg(&args[0], name, span)?;
            let found = match name {
                "contains" => text.contains(&*needle),
                "starts_with" => text.starts_with(&*needle),
                _ => text.ends_with(&*needle),
            };
            Ok(vec![Value::Bool(found)])
        }
        other => Err(LusaError::new(
            E_CALL,
            Some(span),
            format!("unknown String method `:{other}`"),
        )
        .help("String methods: push_str, push, pop, len, is_empty, clear, as_str, contains, starts_with, ends_with")),
    }
}

fn map_key(value: &Value, span: Span) -> LResult<Value> {
    match value.base_value() {
        Value::Str(_) => Ok(value.base_value().clone()),
        Value::Number(n) if n.fract() == 0.0 => Ok(Value::Number(*n)),
        other => Err(LusaError::new(
            E_BAD_KEY,
            Some(span),
            format!("a HashMap key must be a string or an integer, got {} ({})", other.type_name(), other.fmt_value()),
        )),
    }
}

fn map_method(
    interp: &mut Interp,
    entries: &Rc<TableObj>,
    name: &str,
    args: Vec<Value>,
    span: Span,
) -> LResult<Vec<Value>> {
    match name {
        "insert" => {
            arity(&args, 2, "HashMap:insert", span)?;
            let k = map_key(&args[0], span)?;
            entries.set(k, args[1].clone())?;
            Ok(Vec::new())
        }
        "get" => {
            arity(&args, 1, "HashMap:get", span)?;
            let k = map_key(&args[0], span)?;
            Ok(vec![match entries.get(&k) {
                Some(v) => some(interp.deep_clone_value(&v)),
                None => none(),
            }])
        }
        "get_or" => {
            arity(&args, 2, "HashMap:get_or", span)?;
            let k = map_key(&args[0], span)?;
            Ok(vec![match entries.get(&k) {
                Some(v) => interp.deep_clone_value(&v),
                None => args[1].clone(),
            }])
        }
        "contains_key" => {
            arity(&args, 1, "HashMap:contains_key", span)?;
            let k = map_key(&args[0], span)?;
            Ok(vec![Value::Bool(entries.get(&k).is_some())])
        }
        "remove" => {
            arity(&args, 1, "HashMap:remove", span)?;
            let k = map_key(&args[0], span)?;
            Ok(vec![Value::Bool(entries.remove(&k).is_some())])
        }
        "len" => Ok(vec![Value::Number(entries.entry_count() as f64)]),
        "is_empty" => Ok(vec![Value::Bool(entries.entry_count() == 0)]),
        "clear" => {
            entries.clear();
            Ok(Vec::new())
        }
        "iter" => {
            let mut items = Vec::new();
            let mut i = 0;
            while let Some((k, v)) = entries.entry_at(i) {
                if let Some(v) = v {
                    items.push((k, interp.deep_clone_value(&v)));
                }
                i += 1;
            }
            Ok(vec![iterator(items)])
        }
        other => Err(LusaError::new(
            E_CALL,
            Some(span),
            format!("unknown HashMap method `:{other}`"),
        )
        .help("HashMap methods: insert, get, get_or, contains_key, remove, len, is_empty, clear, iter")),
    }
}

/// The `Vec<T>` methods added in Module 29.
pub fn is_vec_extra(name: &str) -> bool {
    matches!(name, "get" | "insert" | "remove" | "contains" | "iter")
}

fn vec_bounds(index: i64, limit: usize, method: &str, span: Span) -> LResult<usize> {
    if index < 0 || index as usize > limit {
        return Err(LusaError::new(
            E_INDEX,
            Some(span),
            format!("`Vec:{method}` index {index} is out of bounds (length {limit})"),
        ));
    }
    Ok(index as usize)
}

/// Runs one of [`is_vec_extra`]'s methods on a 0-based vector table.
pub fn vec_extra_call(
    interp: &mut Interp,
    table: &Rc<TableObj>,
    name: &str,
    args: Vec<Value>,
    span: Span,
) -> LResult<Vec<Value>> {
    let len = table.len();
    let at = |i: usize| Value::Number(i as f64);
    match name {
        "get" => {
            arity(&args, 1, "Vec:get", span)?;
            let index = index_arg(&args[0], "Vec:get", span)?;
            if index < 0 || index as usize >= len {
                return Ok(vec![none()]);
            }
            let value = table.get(&at(index as usize)).unwrap_or(Value::Nil);
            Ok(vec![some(interp.deep_clone_value(&value))])
        }
        "insert" => {
            arity(&args, 2, "Vec:insert", span)?;
            let index = vec_bounds(index_arg(&args[0], "Vec:insert", span)?, len, "insert", span)?;
            let mut i = len;
            while i > index {
                let moved = table.get(&at(i - 1)).unwrap_or(Value::Nil);
                table.set(at(i), moved)?;
                i -= 1;
            }
            table.set(at(index), args[1].clone())?;
            interp.vec_reserve_capacity(table, len + 1);
            Ok(Vec::new())
        }
        "remove" => {
            arity(&args, 1, "Vec:remove", span)?;
            let index = index_arg(&args[0], "Vec:remove", span)?;
            if len == 0 || index < 0 || index as usize >= len {
                return Err(LusaError::new(
                    E_INDEX,
                    Some(span),
                    format!("`Vec:remove` index {index} is out of bounds (length {len})"),
                ));
            }
            let index = index as usize;
            let removed = table.get(&at(index)).unwrap_or(Value::Nil);
            for i in index..len - 1 {
                let moved = table.get(&at(i + 1)).unwrap_or(Value::Nil);
                table.set(at(i), moved)?;
            }
            table.remove(&at(len - 1));
            Ok(vec![removed])
        }
        "contains" => {
            arity(&args, 1, "Vec:contains", span)?;
            let needle = args[0].base_value().clone();
            let found = (0..len).any(|i| {
                table
                    .get(&at(i))
                    .map(|v| crate::eval::key_eq_loose(v.base_value(), &needle))
                    .unwrap_or(false)
            });
            Ok(vec![Value::Bool(found)])
        }
        "iter" => {
            let items = (0..len)
                .map(|i| {
                    let value = table.get(&at(i)).unwrap_or(Value::Nil);
                    (at(i), interp.deep_clone_value(&value))
                })
                .collect();
            Ok(vec![iterator(items)])
        }
        other => Err(LusaError::new(E_CALL, Some(span), format!("unknown Vec method `:{other}`"))),
    }
}

/// True when a built-in surface answers `:name` on a table that has no real
/// field of that name: a `String`/`HashMap` wrapper, or a `Vec<T>` method.
pub fn handles(table: &Rc<TableObj>, name: &str) -> bool {
    is_wrapper(table) || Interp::is_vec_method(name) || is_vec_extra(name)
}

/// Dispatches a method that [`handles`] accepted.
pub fn call(
    interp: &mut Interp,
    table: &Rc<TableObj>,
    name: &str,
    args: Vec<Value>,
    span: Span,
) -> LResult<Vec<Value>> {
    if is_wrapper(table) {
        method_call(interp, table, name, args, span)
    } else if is_vec_extra(name) {
        vec_extra_call(interp, table, name, args, span)
    } else {
        interp.vec_method_call(table, name, args, span)
    }
}
