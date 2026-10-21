//! Bytecode compiler: Lusa AST → `Chunk`s of `Instr`.
//!
//! Closures capture **shared slots** (`Rc<RefCell<SlotV>>` at runtime), which
//! gives the same observable alias/mutation behavior as the interpreter's
//! scope chains. Multi-value expansion (calls, `...`, the last position of
//! expression lists) follows the interpreter's rules via a runtime "last
//! expansion count" marker consumed by `CallExp`/`RetExp`/`TruncateToOne`.
//!
//! The compiler is index-based: a stack of per-function frames whose nesting
//! is expressed with parent *indexes* (no recursive borrows), with Lua-style
//! upvalue-chain synthesis when a nested function reads outer locals.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use crate::ast::*;
use crate::error::{LResult, LusaError, Span};
use crate::value::Value;

// ===== compiled units =====

/// A compiled function unit plus the upvalues it captures
/// (the payload of `MakeClosure`).
pub struct Compiled {
    pub proto: Rc<Chunk>,
    pub upvals: Vec<UpDef>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpDef {
    /// captures the parent *frame local slot*
    Local(u16),
    /// captures the parent *closure upvalue index*
    Up(u16),
}

/// A compiled function body.
pub struct Chunk {
    pub name: Option<Arc<str>>,
    pub nparams: usize,
    pub has_vararg: bool,
    pub nlocals: usize,
    pub consts: Vec<Value>,
    pub code: Vec<Instr>,
    pub prototypes: Vec<Rc<Compiled>>,
    /// slot → declared name (labels for ownership errors)
    pub slot_labels: Vec<Arc<str>>,
    /// upvalue → captured name (labels)
    pub upval_labels: Vec<Arc<str>>,
}

/// Stack-machine instructions. `i32` jump fields hold **absolute instruction
/// indexes** (patched after compilation).
#[derive(Clone, Debug)]
pub enum Instr {
    LoadConst(u16),
    LoadNil,
    GetLoc(u16),
    GetUp(u16),
    GetGlob(u16), // const index of the name
    // moving reads (assignment sources, call args, returns, table entries):
    // heap values leave `SlotV::Moved` behind; label = destination string
    TakeLoc(u16, u16),
    TakeUp(u16, u16),
    TakeGlob(u16, u16), // (name const, label const)
    TakeField(u16, u16, u16), // (key const, source-label const, dest-label const)
    TakeIndex(u16, u16),      // (source-label const, dest-label const)
    NewTable,
    GetField(u16), // TOS = obj
    GetIndex,      // TOS = key, under = obj
    SetGlob(u16),  // TOS = value
    SetFieldK(u16), // TOS = obj, below = value
    SetIndex,      // TOS = key, under1 = obj, under2 = value
    SetArr,        // TOS = value, under = table (appends via next_arr)
    SetArrExp,     // like SetArr but appends last_ret values in order
    // binary / unary ops
    Add, Sub, Mul, Div, FloorDiv, Mod, Pow, Concat,
    Eq, Ne, Lt, Le, Gt, Ge,
    Neg, Not, Len,
    CloneV,
    BorrowErr, // borrow is rejected in bytecode mode
    // calls — `nt` = number of journaled argument takes finalized by this call
    // (builtins restore them, matching the interpreter's `is_builtin()` rules)
    Call(usize, usize),
    CallExp(usize, usize), // nargs = base + last_ret
    MethodCall(u16, usize, usize),
    MethodCallExp(u16, usize, usize),
    // deferred call-argument takes (same moving reads; journaled per-frame
    // until the call decides: builtins restore, user functions commit)
    ArgLoc(u16, u16),
    ArgUp(u16, u16),
    ArgGlob(u16, u16),
    ArgField(u16, u16, u16), // (key const, source-label const, dest-label const)
    ArgIndex(u16, u16),      // (source-label const, dest-label const)
    // control flow
    Jump(i32),
    JumpIfFalse(i32),     // pops tested value
    JumpIfTrueKeep(i32),  // peeks: true keeps (or), false pops
    JumpIfFalseKeep(i32), // peeks: false keeps (and), true pops
    // stack-value fixups
    TruncateToOne, // keep the FIRST of the last_ret values (Lua rule)
    DropValues,    // statement calls: discard last_ret values
    Pop,           // discard one value
    // closures & returns
    MakeClosure(u16),
    RetFixed(usize),
    RetExp(usize), // nvals = base + last_ret
    // varargs
    VarargLoad,    // pushes all `...` (multi)
    VarargLoadOne, // first of `...` (single)
    // loops
    ForNumInit {
        var: u16,
        limit: u16,
        step: u16,
        end: i32,
    },
    ForNumStep {
        var: u16,
        limit: u16,
        step: u16,
        top: i32,
        end: i32,
    },
    ForInSetup {
        iter: u16,
        first: u16,
        nvars: usize,
        end: i32,
    },
    ForInStep {
        iter: u16,
        first: u16,
        nvars: usize,
        top: i32,
        end: i32,
    },
    // list segments (assignment / declaration value groups)
    SegStart,
    SegEnd,
    DeclFromSeg(Vec<(u16, u16)>), // (slot, name-const) per target, in order
    StoreLocSeg(u16),
    StoreUpSeg(u16),
    StoreGlobSeg(u16),  // name-const
    StoreFieldSeg(u16), // TOS = obj; name-const
    StoreIndexFromSeg,  // TOS = key, under = obj
}

// ===== compiler state =====

/// One function-in-progress. Lives in `Compiler.frames`; nesting is expressed
/// with the parent *index* (no recursive borrows).
struct Frame {
    name: String,
    parent: Option<usize>,
    /// shadow levels: name → slot
    scopes: Vec<HashMap<String, u16>>,
    /// slot → declared name (ownership labels)
    slots: Vec<String>,
    consts: Vec<ConstKey>,
    consts_idx: HashMap<ConstKey, u16>,
    protos: Vec<Rc<Compiled>>,
    upvals: Vec<UpDef>,
    upval_labels: Vec<String>,
    code: Vec<Instr>,
    /// pending break jumps of the innermost enclosing loops
    breaks: Vec<Vec<usize>>,
    nparams: usize,
    has_vararg: bool,
}

#[derive(Clone, Debug, PartialEq, Hash, Eq)]
enum ConstKey {
    Nil,
    Bool(bool),
    Num(u64),
    Str(String),
}

impl ConstKey {
    fn of(v: &Value) -> ConstKey {
        match v {
            Value::Nil => ConstKey::Nil,
            Value::Bool(b) => ConstKey::Bool(*b),
            Value::Number(n) => ConstKey::Num(n.to_bits()),
            Value::Str(s) => ConstKey::Str(s.to_string()),
            other => unreachable!("consts hold scalars only; got {}", other.type_name()),
        }
    }

    fn to_value(&self) -> Value {
        match self {
            ConstKey::Nil => Value::Nil,
            ConstKey::Bool(b) => Value::Bool(*b),
            ConstKey::Num(bits) => Value::Number(f64::from_bits(*bits)),
            ConstKey::Str(s) => Value::Str(Arc::from(s.as_str())),
        }
    }
}

struct Compiler {
    frames: Vec<Frame>,
    _dummy: (),
}

impl Frame {
    fn new(name: impl Into<String>, parent: Option<usize>) -> Frame {
        Frame {
            name: name.into(),
            parent,
            scopes: vec![HashMap::new()],
            slots: Vec::new(),
            consts: Vec::new(),
            consts_idx: HashMap::new(),
            protos: Vec::new(),
            upvals: Vec::new(),
            upval_labels: Vec::new(),
            code: Vec::new(),
            breaks: Vec::new(),
            nparams: 0,
            has_vararg: false,
        }
    }

    fn find_local(&self, name: &str) -> Option<u16> {
        for m in self.scopes.iter().rev() {
            if let Some(&slot) = m.get(name) {
                return Some(slot);
            }
        }
        None
    }
}

impl Compiler {
    fn cur(&mut self) -> &mut Frame {
        self.frames.last_mut().expect("a frame is always compiling")
    }

    fn top(&self) -> usize {
        self.frames.len() - 1
    }

    fn em(&mut self, i: Instr) -> usize {
        self.cur().code.push(i);
        self.cur().code.len() - 1
    }

    fn pos(&self) -> usize {
        self.frames.last().map(|f| f.code.len()).unwrap_or(0)
    }

    fn konst(&mut self, k: &ConstKey) -> u16 {
        let f = self.cur();
        if let Some(&i) = f.consts_idx.get(k) {
            return i;
        }
        let i = f.consts.len() as u16;
        f.consts.push(k.clone());
        f.consts_idx.insert(k.clone(), i);
        i
    }

    fn konst_str(&mut self, s: &str) -> u16 {
        self.konst(&ConstKey::Str(s.to_string()))
    }

    /// Declares a fresh local slot in the current shadow level.
    fn declare(&mut self, name: &str) -> u16 {
        let f = self.cur();
        let slot = f.slots.len() as u16;
        f.slots.push(name.to_string());
        f.scopes
            .last_mut()
            .expect("scope levels are never empty")
            .insert(name.to_string(), slot);
        slot
    }

    fn push_shadow(&mut self) {
        self.cur().scopes.push(HashMap::new());
    }

    fn pop_shadow(&mut self) {
        self.cur().scopes.pop();
    }

    /// Patches an already-emitted jump to an absolute instruction index.
    fn patch(&mut self, at: usize, target: usize) {
        let code = &mut self.cur().code;
        match &mut code[at] {
            Instr::Jump(t)
            | Instr::JumpIfFalse(t)
            | Instr::JumpIfTrueKeep(t)
            | Instr::JumpIfFalseKeep(t)
            | Instr::ForNumInit { end: t, .. }
            | Instr::ForNumStep { end: t, .. }
            | Instr::ForInSetup { end: t, .. }
            | Instr::ForInStep { end: t, .. } => *t = target as i32,
            _ => unreachable!("patching a non-jump instruction"),
        }
    }
}

// ===== name resolution =====

enum Resolved {
    Local(u16),
    Upval(u16),
    Global,
}

/// Resolves `name` within the frame stack: current frame locals first, then a
/// synthesized upvalue chain through ancestor frames, then a global.
fn resolve(c: &mut Compiler, name: &str) -> Resolved {
    let top = c.top();
    if let Some(slot) = c.frames[top].find_local(name) {
        return Resolved::Local(slot);
    }
    // find the ancestor level holding the name
    let mut level = top;
    while let Some(p) = c.frames[level].parent {
        if c.frames[p].find_local(name).is_some() {
            level = p;
            // synthesize upvalue entries from level+1 up to top; each frame
            // points at the entry registered in its inner neighbor
            let mut cur_idx = 0u16;
            let mut j = level + 1;
            while j <= top {
                let def = if j == level + 1 {
                    UpDef::Local(c.frames[level].find_local(name).expect("checked above"))
                } else {
                    UpDef::Up(cur_idx)
                };
                let already = c.frames[j].upvals.iter().position(|u| *u == def);
                cur_idx = match already {
                    Some(i) => i as u16,
                    None => {
                        c.frames[j].upvals.push(def);
                        c.frames[j].upval_labels.push(name.to_string());
                        (c.frames[j].upvals.len() - 1) as u16
                    }
                };
                j += 1;
            }
            return Resolved::Upval(cur_idx);
        }
        level = p;
    }
    Resolved::Global
}

/// Emits the load for a variable read.
fn emit_var_load(c: &mut Compiler, name: &str) {
    match resolve(c, name) {
        Resolved::Local(slot) => {
            c.em(Instr::GetLoc(slot));
        }
        Resolved::Upval(i) => {
            c.em(Instr::GetUp(i));
        }
        Resolved::Global => {
            let k = c.konst_str(name);
            c.em(Instr::GetGlob(k));
        }
    }
}

// ===== moving reads =====

/// Compiles a value in a *moving* position (assignment sources, call args,
/// returns, table entries, for-in groups). Var/Field/Index sources consume the
/// heap value out of its slot (parity with the interpreter's `eval_take_single`);
/// anything else compiles as a plain read.
///
/// When `arg` is true the take is DEFERRED: the `Arg*` ops push the value and
/// journal the source in the current frame; the call instruction finalizes them
/// (builtins restore — they never take ownership; user functions commit).
/// Returns true when a take op was emitted (call sites count them).
fn compile_take_value(c: &mut Compiler, e: &Expr, dest: &str, arg: bool) -> LResult<bool> {
    match &e.kind {
        ExprKind::Var(name) => {
            let lk = c.konst_str(dest);
            match resolve(c, name) {
                Resolved::Local(slot) => {
                    if arg {
                        c.em(Instr::ArgLoc(slot, lk));
                    } else {
                        c.em(Instr::TakeLoc(slot, lk));
                    }
                    return Ok(true);
                }
                Resolved::Upval(i) => {
                    if arg {
                        c.em(Instr::ArgUp(i, lk));
                    } else {
                        c.em(Instr::TakeUp(i, lk));
                    }
                    return Ok(true);
                }
                Resolved::Global => {
                    let k = c.konst_str(name);
                    if arg {
                        c.em(Instr::ArgGlob(k, lk));
                    } else {
                        c.em(Instr::TakeGlob(k, lk));
                    }
                    return Ok(true);
                }
            }
        }
        ExprKind::Field { obj, name } => {
            compile_expr(c, obj)?;
            truncate_if_expansion(c, obj);
            let k = c.konst_str(name);
            let src = match &obj.kind {
                ExprKind::Var(vn) => format!("{}.{}", vn, name),
                _ => format!("<value>.{}", name),
            };
            let sk = c.konst_str(&src);
            let lk = c.konst_str(dest);
            if arg {
                c.em(Instr::ArgField(k, sk, lk));
            } else {
                c.em(Instr::TakeField(k, sk, lk));
            }
            return Ok(true);
        }
        ExprKind::Index { obj, key } => {
            compile_expr(c, obj)?;
            truncate_if_expansion(c, obj);
            compile_expr(c, key)?;
            truncate_if_expansion(c, key);
            let src = match &obj.kind {
                ExprKind::Var(vn) => format!("{}[]", vn),
                _ => format!("<value>[]"),
            };
            let sk = c.konst_str(&src);
            let lk = c.konst_str(dest);
            if arg {
                c.em(Instr::ArgIndex(sk, lk));
            } else {
                c.em(Instr::TakeIndex(sk, lk));
            }
            return Ok(true);
        }
        _ => {}
    }
    // expansion nodes keep their multi-value shape; the caller truncates
    // mid-list positions (`i + 1 < n` rule mirrors the interpreter)
    if is_expansion(e) {
        compile_expand_value(c, e)?;
    } else {
        compile_expr(c, e)?;
    }
    Ok(false)
}

/// Compiles a value whose results may be multi (calls already push all of
/// theirs through `CallExp`; `...` must push the whole rest here).
fn compile_expand_value(c: &mut Compiler, e: &Expr) -> LResult<()> {
    match &e.kind {
        ExprKind::Vararg => {
            c.em(Instr::VarargLoad);
        }
        _ => compile_expr(c, e)?,
    }
    Ok(())
}

// ===== expressions =====

/// scalar constant of simple literal nodes
fn scalar_of(e: &Expr) -> Value {
    match &e.kind {
        ExprKind::Nil => Value::Nil,
        ExprKind::True => Value::Bool(true),
        ExprKind::False => Value::Bool(false),
        ExprKind::Num(n) => Value::Number(*n),
        _ => unreachable!("scalar_of on non-literal"),
    }
}

/// Compiles an expression in *single-value* position: pushes exactly one value.
fn compile_expr(c: &mut Compiler, e: &Expr) -> LResult<()> {
    match &e.kind {
        ExprKind::Nil | ExprKind::True | ExprKind::False | ExprKind::Num(_) => {
            let k = c.konst(&ConstKey::of(&scalar_of(e)));
            c.em(Instr::LoadConst(k));
        }
        ExprKind::Str(segs) => compile_interp_str(c, segs)?,
        ExprKind::Var(name) => emit_var_load(c, name),
        ExprKind::Field { obj, name } => {
            compile_expr(c, obj)?;
            let k = c.konst_str(name);
            c.em(Instr::GetField(k));
        }
        ExprKind::Index { obj, key } => {
            compile_expr(c, obj)?;
            compile_expr(c, key)?;
            c.em(Instr::GetIndex);
        }
        ExprKind::Paren(inner) => {
            compile_expr(c, inner)?;
            c.em(Instr::TruncateToOne);
        }
        ExprKind::Vararg => {
            c.em(Instr::VarargLoadOne);
        }
        ExprKind::Call { callee, args, span_args_end } => {
            compile_call(c, callee, args, *span_args_end)?;
        }
        ExprKind::Method { obj, name, args, span_args_end } => {
            compile_method(c, obj, name, args, *span_args_end)?;
        }
        ExprKind::Table(entries) => compile_table(c, entries)?,
        ExprKind::Func(lit) => {
            compile_nested_fn(c, lit, "anonymous")?;
        }
        ExprKind::Bin { op, l, r } => {
            compile_expr(c, l)?;
            truncate_if_expansion(c, l);
            compile_expr(c, r)?;
            truncate_if_expansion(c, r);
            c.em(match op {
                BinOp::Add => Instr::Add,
                BinOp::Sub => Instr::Sub,
                BinOp::Mul => Instr::Mul,
                BinOp::Div => Instr::Div,
                BinOp::FloorDiv => Instr::FloorDiv,
                BinOp::Mod => Instr::Mod,
                BinOp::Pow => Instr::Pow,
                BinOp::Concat => Instr::Concat,
                BinOp::Eq => Instr::Eq,
                BinOp::Ne => Instr::Ne,
                BinOp::Lt => Instr::Lt,
                BinOp::Gt => Instr::Gt,
                BinOp::Le => Instr::Le,
                BinOp::Ge => Instr::Ge,
            });
        }
        ExprKind::Log { op, l, r } => {
            compile_expr(c, l)?;
            truncate_if_expansion(c, l);
            let skip = match op {
                LogOp::And => c.em(Instr::JumpIfFalseKeep(0)),
                LogOp::Or => c.em(Instr::JumpIfTrueKeep(0)),
            };
            c.em(Instr::Pop);
            compile_expr(c, r)?;
            truncate_if_expansion(c, r);
            let end = c.pos();
            c.patch(skip, end);
        }
        ExprKind::Un { op, e: inner } => {
            compile_expr(c, inner)?;
            truncate_if_expansion(c, inner);
            c.em(match op {
                UnOp::Neg => Instr::Neg,
                UnOp::Not => Instr::Not,
                UnOp::Len => Instr::Len,
            });
        }
        ExprKind::Borrow { .. } => {
            c.em(Instr::BorrowErr);
        }
        ExprKind::Clone { e: inner } => {
            compile_expr(c, inner)?;
            truncate_if_expansion(c, inner);
            c.em(Instr::CloneV);
        }
    }
    Ok(())
}

/// Non-last positions truncate expansions to their first value.
fn truncate_if_expansion(c: &mut Compiler, e: &Expr) {
    if is_expansion(e) {
        c.em(Instr::TruncateToOne);
    }
}

/// Is this expression a known multi-return producer?
fn is_expansion(e: &Expr) -> bool {
    matches!(
        e.kind,
        ExprKind::Call { .. } | ExprKind::Method { .. } | ExprKind::Vararg
    )
}

/// Compiles an interpolated string: literal + `\{expr}` segments.
/// Pieces are pushed in order and joined with a `Concat` step each
/// (left-associative, mirroring the interpreter).
fn compile_interp_str(c: &mut Compiler, segs: &[StrAstSeg]) -> LResult<()> {
    if segs.len() == 1 {
        if let StrAstSeg::Lit(text) = &segs[0] {
            let k = c.konst_str(text);
            c.em(Instr::LoadConst(k));
            return Ok(());
        }
    }
    for (i, seg) in segs.iter().enumerate() {
        match seg {
            StrAstSeg::Lit(text) => {
                let k = c.konst_str(text);
                c.em(Instr::LoadConst(k));
            }
            StrAstSeg::Interp(ex) => {
                compile_expr(c, ex)?;
                truncate_if_expansion(c, ex);
            }
        }
        if i > 0 {
            c.em(Instr::Concat);
        }
    }
    Ok(())
}

// ===== calls =====

/// Compiles a call: callee first, then args; only the LAST arg keeps expansion.
fn compile_call(c: &mut Compiler, callee: &Expr, args: &[Expr], _end: Span) -> LResult<()> {
    compile_expr(c, callee)?;
    truncate_if_expansion(c, callee);
    let n = args.len();
    let mut nt = 0usize;
    for (i, arg) in args.iter().enumerate() {
        let dest = format!("(argument {})", i + 1);
        if compile_take_value(c, arg, &dest, true)? {
            nt += 1;
        }
        if i + 1 < n {
            truncate_if_expansion(c, arg);
        }
    }
    if n > 0 && is_expansion(&args[n - 1]) {
        c.em(Instr::CallExp(n - 1, nt));
    } else {
        c.em(Instr::Call(n, nt));
    }
    Ok(())
}

/// Compiles `obj:name(args...)`.
fn compile_method(c: &mut Compiler, obj: &Expr, name: &str, args: &[Expr], _end: Span) -> LResult<()> {
    compile_expr(c, obj)?;
    truncate_if_expansion(c, obj);
    let k = c.konst_str(name);
    let n = args.len();
    let mut nt = 0usize;
    for (i, arg) in args.iter().enumerate() {
        let dest = format!("(argument {} of :{})", i + 1, name);
        if compile_take_value(c, arg, &dest, true)? {
            nt += 1;
        }
        if i + 1 < n {
            truncate_if_expansion(c, arg);
        }
    }
    if n > 0 && is_expansion(&args[n - 1]) {
        c.em(Instr::MethodCallExp(k, n - 1, nt));
    } else {
        c.em(Instr::MethodCall(k, n, nt));
    }
    Ok(())
}

/// Compiles a table constructor; the fresh table stays under the entries.
fn compile_table(c: &mut Compiler, entries: &[TableEntry]) -> LResult<()> {
    c.em(Instr::NewTable);
    let n = entries.len();
    for (i, entry) in entries.iter().enumerate() {
        let is_last = i + 1 == n;
        match &entry.key {
            Some(key) => {
                compile_expr(c, key)?;
                truncate_if_expansion(c, key);
                let _ = compile_take_value(c, &entry.value, "(into table)", false)?;
                truncate_if_expansion(c, &entry.value);
                c.em(Instr::SetIndex);
            }
            None => {
                if is_last && is_expansion(&entry.value) {
                    compile_expand_value(c, &entry.value)?;
                    c.em(Instr::SetArrExp);
                } else {
                    let dest = format!("item {}", c.pos());
                    let _ = compile_take_value(c, &entry.value, &dest, false)?;
                    truncate_if_expansion(c, &entry.value);
                    c.em(Instr::SetArr);
                }
            }
        }
    }
    Ok(())
}

/// Compiles a nested function literal as a prototype + MakeClosure.
fn compile_nested_fn(c: &mut Compiler, lit: &FuncLit, name: &str) -> LResult<()> {
    let parent = c.top();
    let has_vararg = lit.has_vararg;
    c.frames.push(Frame::new(format!("{}::{}", c.frames[parent].name, name), Some(parent)));
    {
        let f = c.cur();
        f.has_vararg = has_vararg;
        f.nparams = lit.params.len();
    }
    for p in &lit.params {
        c.declare(p);
    }
    compile_block(c, &lit.body)?;
    // implicit return nil
    c.em(Instr::LoadNil);
    c.em(Instr::RetFixed(1));
    let frame = c.frames.pop().expect("popped the child frame");
    let compiled = Rc::new(Compiled {
        proto: Rc::new(Chunk {
            name: Some(Arc::from(frame.name.as_str())),
            nparams: frame.nparams,
            has_vararg: frame.has_vararg,
            nlocals: frame.slots.len(),
            consts: frame.consts.iter().map(|k| k.to_value()).collect(),
            code: frame.code,
            prototypes: frame.protos,
            slot_labels: frame.slots.iter().map(|s| Arc::from(s.as_str())).collect(),
            upval_labels: frame.upval_labels.iter().map(|s| Arc::from(s.as_str())).collect(),
        }),
        upvals: frame.upvals,
    });
    let idx = c.frames.last().expect("parent frame").protos.len() as u16;
    c.frames.last_mut().expect("parent frame").protos.push(compiled);
    c.em(Instr::MakeClosure(idx));
    Ok(())
}
// ===== statements =====

fn compile_block(c: &mut Compiler, block: &Block) -> LResult<()> {
    c.push_shadow();
    for st in block {
        compile_stmt(c, st)?;
    }
    c.pop_shadow();
    Ok(())
}

fn compile_stmt(c: &mut Compiler, st: &St) -> LResult<()> {
    match &st.kind {
        StKind::Local { targets, values } => compile_local(c, targets, values)?,
        StKind::Assign { targets, values } => compile_assign(c, targets, values)?,
        StKind::ExprSt(e) => {
            compile_expr(c, e)?;
            if is_expansion(e) {
                c.em(Instr::DropValues);
            } else {
                c.em(Instr::Pop);
            }
        }
        StKind::If { branches, else_block } => {
            let eb = else_block.as_ref().map(|b| b);
            compile_if(c, branches, eb)?
        }
        StKind::While { cond, body } => compile_while(c, cond, body)?,
        StKind::Repeat { body, cond } => compile_repeat(c, body, cond)?,
        StKind::ForNum { var, start, limit, step, body } => {
            let sp = step.as_ref();
            compile_for_num(c, var, start, limit, sp, body)?
        }
        StKind::ForIter { vars, values, body } => compile_for_iter(c, vars, values, body)?,
        StKind::FuncSt { target, func } => compile_func_st(c, target, func)?,
        StKind::Do { body } => compile_block(c, body)?,
        StKind::Return { values, .. } => compile_return(c, values)?,
        StKind::Break => {
            let at = c.em(Instr::Jump(0));
            // record under the innermost loop
            if let Some(loops) = c.cur().breaks.last_mut() {
                loops.push(at);
            } else {
                return Err(LusaError::new(
                    crate::error::E_BREAK,
                    Some(st.span),
                    "`break` outside of a loop".to_string(),
                ));
            }
        }
    }
    Ok(())
}

/// `local a, b = v1, v2`: values first, then fresh slots (Lua rule: the value
/// expressions see enclosing variables of the same name).
fn compile_local(c: &mut Compiler, targets: &[String], values: &[Expr]) -> LResult<()> {
    c.em(Instr::SegStart);
    // slots pre-declared before their value (`local function f()` recursion)
    let mut pre_slots: Vec<Option<u16>> = vec![None; targets.len()];
    let n = values.len();
    for (i, v) in values.iter().enumerate() {
        let dest = targets.get(i).map(|s| s.as_str()).unwrap_or("(value)");
        // `local function f() ... end` recursion: pre-bind the target so the
        // body captures its own slot (matches the interpreter's name lookup)
        if let ExprKind::Func(_) = &v.kind {
            if let Some(name) = targets.get(i) {
                let slot = c.declare(name);
                pre_slots[i] = Some(slot);
            }
        }
        let _ = compile_take_value(c, v, dest, false)?;
        if i + 1 < n {
            truncate_if_expansion(c, v);
        }
    }
    // bind targets to slots — fresh for plain locals, reusing the pre-declared
    // slot when the value was a function (recursion capture) — map names to consts
    let mut slot_name: Vec<(u16, u16)> = Vec::new();
    for (i, t) in targets.iter().enumerate() {
        let slot = match pre_slots[i] {
            Some(s) => s,
            None => c.declare(t),
        };
        let k = c.konst_str(t);
        slot_name.push((slot, k));
    }
    c.em(Instr::DeclFromSeg(slot_name));
    c.em(Instr::SegEnd);
    Ok(())
}

fn compile_assign(c: &mut Compiler, targets: &[Expr], values: &[Expr]) -> LResult<()> {
    c.em(Instr::SegStart);
    let n = values.len();
    for (i, v) in values.iter().enumerate() {
        let dest = match targets.get(i).map(|t| &t.kind) {
            Some(ExprKind::Var(name)) => format!("`{}`", name),
            Some(ExprKind::Field { name, .. }) => format!("field `{}`", name),
            Some(ExprKind::Index { .. }) => "(indexed)".to_string(),
            _ => "(value)".to_string(),
        };
        let _ = compile_take_value(c, v, &dest, false)?;
        if i + 1 < n {
            truncate_if_expansion(c, v);
        }
    }
    for t in targets {
        match &t.kind {
            ExprKind::Var(name) => match resolve(c, name) {
                Resolved::Local(slot) => {
                    c.em(Instr::StoreLocSeg(slot));
                }
                Resolved::Upval(i) => {
                    c.em(Instr::StoreUpSeg(i));
                }
                Resolved::Global => {
                    let k = c.konst_str(name);
                    c.em(Instr::StoreGlobSeg(k));
                }
            },
            ExprKind::Field { obj, name } => {
                compile_expr(c, obj)?;
                let k = c.konst_str(name);
                c.em(Instr::StoreFieldSeg(k));
            }
            ExprKind::Index { obj, key } => {
                compile_expr(c, obj)?;
                compile_expr(c, key)?;
                c.em(Instr::StoreIndexFromSeg);
            }
            _ => {
                return Err(LusaError::new(
                    crate::error::E_PARSE,
                    Some(t.span),
                    "cannot assign to this expression",
                ))
            }
        }
    }
    c.em(Instr::SegEnd);
    Ok(())
}

fn compile_if(c: &mut Compiler, branches: &[(Expr, Block)], else_block: Option<&Block>) -> LResult<()> {
    let mut end_jumps: Vec<usize> = Vec::new();
    let n = branches.len();
    for (i, (cond, body)) in branches.iter().enumerate() {
        compile_expr(c, cond)?;
        truncate_if_expansion(c, cond);
        let jump = c.em(Instr::JumpIfFalse(0));
        compile_block(c, body)?;
        let span = cond.span;
        if i + 1 < n || else_block.is_some() {
            end_jumps.push(c.em(Instr::Jump(0)));
        }
        let _ = span;
        let next = c.pos();
        c.patch(jump, next);
    }
    if let Some(els) = else_block {
        compile_block(c, els)?;
    }
    let end = c.pos();
    for j in end_jumps {
        c.patch(j, end);
    }
    Ok(())
}

fn compile_while(c: &mut Compiler, cond: &Expr, body: &Block) -> LResult<()> {
    let start = c.pos();
    compile_expr(c, cond)?;
    truncate_if_expansion(c, cond);
    let exit = c.em(Instr::JumpIfFalse(0));
    c.cur().breaks.push(Vec::new());
    compile_block(c, body)?;
    let breaks = c.cur().breaks.pop().expect("loop breaks pushed");
    let back = start;
    c.em(Instr::Jump(back as i32));
    let end = c.pos();
    for b in breaks {
        c.patch(b, end);
    }
    c.patch(exit, end);
    Ok(())
}

fn compile_repeat(c: &mut Compiler, body: &Block, cond: &Expr) -> LResult<()> {
    let start = c.pos();
    c.cur().breaks.push(Vec::new());
    // repeat condition sees body locals
    c.push_shadow();
    for s in body {
        compile_stmt(c, s)?;
    }
    compile_expr(c, cond)?;
    c.pop_shadow();
    let breaks = c.cur().breaks.pop().expect("loop breaks pushed");
    let _ = c.em(Instr::JumpIfFalse(start as i32));
    let end = c.pos();
    for b in breaks {
        c.patch(b, end);
    }
    Ok(())
}

fn compile_for_num(c: &mut Compiler, var: &str, start: &Expr, limit: &Expr, step: Option<&Expr>, body: &Block) -> LResult<()> {
    // start, limit, step are evaluated BEFORE the loop variable is declared
    compile_expr(c, start)?;
    truncate_if_expansion(c, start);
    compile_expr(c, limit)?;
    truncate_if_expansion(c, limit);
    match step {
        Some(s) => {
            compile_expr(c, s)?;
            truncate_if_expansion(c, s);
        }
        None => {
            let k = c.konst(&ConstKey::Num(1.0f64.to_bits()));
            c.em(Instr::LoadConst(k));
        }
    }
    // hidden slot layout: [var, limit, step]
    c.push_shadow();
    let var_slot = c.declare(var);
    let limit_slot = c.declare("(for limit)");
    let step_slot = c.declare("(for step)");
    c.cur().breaks.push(Vec::new());
    let init = c.em(Instr::ForNumInit { var: var_slot, limit: limit_slot, step: step_slot, end: 0 });
    compile_block(c, body)?;
    let step_at = c.em(Instr::ForNumStep { var: var_slot, limit: limit_slot, step: step_slot, top: (init + 1) as i32, end: 0 });
    let breaks = c.cur().breaks.pop().expect("loop breaks pushed");
    let end = c.pos();
    c.patch(step_at, end);
    for b in breaks {
        c.patch(b, end);
    }
    c.pop_shadow();
    Ok(())
}

fn compile_for_iter(c: &mut Compiler, vars: &[String], values: &[Expr], body: &Block) -> LResult<()> {
    c.em(Instr::SegStart);
    let n = values.len();
    for (i, v) in values.iter().enumerate() {
        let dest = format!("(for control {})", i + 1);
        let _ = compile_take_value(c, v, &dest, false)?;
        if i + 1 < n {
            truncate_if_expansion(c, v);
        }
    }
    c.push_shadow();
    // hidden iterator slot first, then the loop variables
    let iter_slot = c.declare("(iter)");
    let mut first = iter_slot + 1;
    for v in vars {
        let s = c.declare(v);
        first = first.min(s);
    }
    let nvars = vars.len();
    c.cur().breaks.push(Vec::new());
    let setup = c.em(Instr::ForInSetup { iter: iter_slot, first, nvars, end: 0 });
    compile_block(c, body)?;
    let step_at = c.em(Instr::ForInStep { iter: iter_slot, first, nvars, top: (setup + 1) as i32, end: 0 });
    let breaks = c.cur().breaks.pop().expect("loop breaks pushed");
    let end = c.pos();
    c.patch(step_at, end);
    for b in breaks {
        c.patch(b, end);
    }
    c.pop_shadow();
    c.em(Instr::SegEnd);
    Ok(())
}

/// `function name(...) ... end` sugar — binds before the body so that plain
/// recursion works (`local function f` sees `f` inside its own body).
fn compile_func_st(c: &mut Compiler, target: &Expr, func: &FuncLit) -> LResult<()> {
    match &target.kind {
        ExprKind::Var(name) => {
            match resolve(c, name) {
                Resolved::Local(slot) => {
                    // slot exists already (reuse) or declare fresh then store
                    compile_expr(c, &Expr { kind: ExprKind::Func(func.clone()), span: target.span })?;
                    let fall = c.em(Instr::StoreLocSeg(slot));
                    let _ = fall;
                }
                Resolved::Upval(i) => {
                    compile_expr(c, &Expr { kind: ExprKind::Func(func.clone()), span: target.span })?;
                    c.em(Instr::StoreUpSeg(i));
                }
                Resolved::Global => {
                    compile_expr(c, &Expr { kind: ExprKind::Func(func.clone()), span: target.span })?;
                    let k = c.konst_str(name);
                    c.em(Instr::StoreGlobSeg(k));
                }
            };
        }
        ExprKind::Field { .. } | ExprKind::Index { .. } => {
            compile_assign(c, &[target.clone()], &[Expr { kind: ExprKind::Func(func.clone()), span: target.span }])?;
        }
        _ => {
            return Err(LusaError::new(
                crate::error::E_PARSE,
                Some(target.span),
                "cannot define a function here",
            ))
        }
    }
    Ok(())
}

fn compile_return(c: &mut Compiler, values: &[Expr]) -> LResult<()> {
    let n = values.len();
    for (i, v) in values.iter().enumerate() {
        let _ = compile_take_value(c, v, "(return)", false)?;
        if i + 1 < n {
            truncate_if_expansion(c, v);
        }
    }
    if n > 0 && is_expansion(&values[n - 1]) {
        c.em(Instr::RetExp(n - 1));
    } else {
        c.em(Instr::RetFixed(n));
    }
    Ok(())
}
// ===== top-level entry =====

/// Compiles a whole source program into a runnable main chunk.
/// Top-level statements compile as the body of a chunk named "main".
pub fn compile_program(src: &str) -> LResult<Rc<Compiled>> {
    let ast = crate::parser::parse(src)?;
    let c = &mut Compiler { frames: vec![Frame::new("main", None)], _dummy: () };
    {
        let f = c.cur();
        f.has_vararg = true; // top level may use `...` like the REPL needs
    }
    let r = compile_block(c, &ast);
    match r {
        Ok(()) => {}
        Err(e) => return Err(e),
    }
    // top level: implicit return with no values
    c.em(Instr::RetFixed(0));
    let f = c.frames.pop().expect("main frame");
    Ok(Rc::new(Compiled {
        proto: Rc::new(Chunk {
            name: Some(Arc::from(f.name.as_str())),
            nparams: f.nparams,
            has_vararg: f.has_vararg,
            nlocals: f.slots.len(),
            consts: f.consts.iter().map(|k| k.to_value()).collect(),
            code: f.code,
            prototypes: f.protos,
            slot_labels: f.slots.iter().map(|s| Arc::from(s.as_str())).collect(),
            upval_labels: f.upval_labels.iter().map(|s| Arc::from(s.as_str())).collect(),
        }),
        upvals: f.upvals,
    }))
}