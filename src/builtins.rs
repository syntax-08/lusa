//! Standard library: base functions plus the `math`, `string` and `table`
//! modules.

use std::rc::Rc;
use std::sync::Arc;
use std::path::{Path, PathBuf};

use crate::error::{LResult, LusaError, Span, E_ASSERT, E_CMP, E_ERROR, E_INDEX, E_NUM};
use crate::interp::Interp;
use crate::value::{with_reg, FuncBody, FuncObj, IterState, TableObj, Value};

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

// ===== base functions =====

fn b_print(i: &mut Interp, args: Vec<Value>, _span: Span) -> LResult<Vec<Value>> {
    let line: Vec<String> = args.iter().map(to_str).collect();
    i.out.push(line.join(" "));
    Ok(Vec::new())
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
        ("require", Box::new(b_require)),
    ]
}

pub fn modules() -> Vec<(&'static str, Vec<(String, BF)>)> {
    let map = |v: Vec<(&'static str, BF)>| v.into_iter().map(|(k, f)| (String::from(k), f)).collect();
    vec![
        ("math", math_mod()),
        ("string", map(string_mod())),
        ("table", map(table_mod())),
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
                let init = match a.get(2) {
                    Some(Value::Number(n)) if !n.is_nan() => (*n).max(1.0) as usize,
                    _ => 1,
                };
                let hay: Vec<char> = src.chars().collect();
                let nee: Vec<char> = pat.chars().collect();
                if nee.is_empty() {
                    let p = init.min(hay.len() + 1) as f64;
                    return Ok(vec![Value::Number(p), Value::Number(p - 1.0)]);
                }
                let start = init.saturating_sub(1).min(hay.len());
                for i in start..=hay.len().saturating_sub(nee.len()) {
                    if hay[i..i + nee.len()] == nee[..] {
                        return Ok(vec![
                            Value::Number((i + 1) as f64),
                            Value::Number((i + nee.len()) as f64),
                        ]);
                    }
                }
                one(Value::Nil)
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
                let mut out = String::new();
                let hay: Vec<char> = src.chars().collect();
                let nee: Vec<char> = pat.chars().collect();
                let mut count = 0usize;
                if !nee.is_empty() {
                    let mut i = 0usize;
                    while i + nee.len() <= hay.len() {
                        if hay[i..i + nee.len()] == nee[..] {
                            out.push_str(&rep);
                            count += 1;
                            i += nee.len();
                            continue;
                        }
                        out.push(hay[i]);
                        i += 1;
                    }
                    out.extend(hay[i..].iter());
                } else {
                    out = rep;
                }
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
