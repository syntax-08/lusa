//! The interpreter: frames, statement execution, calls, loops, and the
//! runtime ownership/borrow engine.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::cell::RefCell;

use crate::ast::*;
use crate::error::{LResult, LusaError, Span, E_BORROW_CONFLICT, E_MOVED, E_RUNTIME};
use crate::eval;
use crate::scope::{self, ScopeRef};
use crate::value;

use crate::value::{
    AliasKind, AliasBox, Borrower, FuncBody, MoveNote, SlotV, TableObj, Value,
};

pub const MAX_CALL_DEPTH: usize = 200;

/// A registered `struct` declaration, used by the safe emulation layer.
#[derive(Clone, Debug)]
pub struct StructDef {
    pub name: String,
    /// (field name, type annotation)
    pub fields: Vec<(String, String)>,
}

/// A storage location for a variable name.
pub struct SlotLoc {
    pub scope: ScopeRef,
    pub name: String,
}

pub struct Interp {
    pub globals: ScopeRef,
    pub out: Vec<String>,
    pub depth: usize,
    /// bytecode VM state (`--vm` runs compile + execute here)
    pub vm: crate::vm::Vm,
    /// Safe simulation of `unsafe_write_mem`/`unsafe_read_mem`.  Addresses are
    /// process-local keys and values are `f64` cells, so fractional register
    /// values round-trip without truncation.  The dynamic engines never
    /// dereference real memory.
    pub unsafe_mem: HashMap<u64, f64>,
    /// nesting depth of `unsafe do ... end` blocks (enforced in both engines)
    pub unsafe_depth: usize,
    /// `struct` declarations registered by the running program.
    pub struct_defs: HashMap<String, StructDef>,
    /// table ids created for struct instances, mapped to their struct name.
    /// The dynamic engines emulate structs with tables; this map lets move
    /// checks treat scalar-only structs as copyable values, matching the
    /// native backend.
    pub struct_tables: RefCell<HashMap<u64, String>>,
    /// next simulated base address handed out by `unsafe_alloc`
    pub unsafe_alloc_next: u64,
    /// next simulated address handed out by `&place`
    pub addr_next: u64,
    /// ids of coroutines currently executing (innermost last)
    pub active_coroutines: Vec<u64>,
    /// upvalue cells of the running coroutine, used when helper functions are
    /// compiled on demand so they share captures instead of taking snapshots
    pub coroutine_captures: Vec<value::CaptureMap>,
}

pub struct Frame {
    /// block scopes, top = last
    pub scopes: Vec<ScopeRef>,
    /// lexical chain captured by the closure (outer frames' scopes)
    pub env: Vec<ScopeRef>,
    /// pending `...` values
    pub varargs: Vec<Value>,
    /// return values
    pub ret: Vec<Value>,
    /// name for diagnostics (reserved for richer stack traces)
    #[allow(dead_code)]
    pub name: Option<Arc<str>>,
    /// a `break` is unwinding
    pub breaking: bool,
}

impl Frame {
    pub fn new(name: Option<Arc<str>>, env: Vec<ScopeRef>) -> Self {
        Frame {
            scopes: vec![scope::new_scope(false)],
            env,
            varargs: Vec::new(),
            ret: Vec::new(),
            name,
            breaking: false,
        }
    }
}

impl Interp {
    pub fn new() -> Self {
        Interp {
            globals: scope::new_scope(true),
            out: Vec::new(),
            depth: 0,
            vm: crate::vm::Vm::new(),
            unsafe_mem: HashMap::new(),
            unsafe_depth: 0,
            struct_defs: HashMap::new(),
            struct_tables: RefCell::new(HashMap::new()),
            unsafe_alloc_next: 0x1000,
            addr_next: 0x8000_0000,
            active_coroutines: Vec::new(),
            coroutine_captures: Vec::new(),
        }
    }

    /// Installs the standard library into globals.
    pub fn install_stdlib(&mut self) {
        for (name, f) in crate::builtins::all() {
            let func = crate::builtins::make_builtin(name, f);
            self.globals
                .borrow_mut()
                .vars
                .insert((*name).to_string(), SlotV::Set(func));
        }
        for (mod_name, members) in crate::builtins::modules() {
            let tbl = TableObj::new();
            for (mname, f) in members {
                let func = crate::builtins::make_builtin(&mname, f);
                tbl.set(Value::Str(Arc::from(mname.as_str())), func).unwrap();
            }
            if mod_name == "math" {
                // constants, not functions
                tbl.set(Value::Str(Arc::from("pi")), Value::Number(std::f64::consts::PI))
                    .unwrap();
                tbl.set(Value::Str(Arc::from("huge")), Value::Number(f64::INFINITY))
                    .unwrap();
            }
            self.globals
                .borrow_mut()
                .vars
                .insert((*mod_name).to_string(), SlotV::Set(Value::Table(Rc::new(tbl))));
        }
        let mut g = self.globals.borrow_mut();
        g.version = g.version.wrapping_add(1);
    }

    pub fn run(&mut self, ast: &Block, progname: &str) -> LResult<Vec<Value>> {
        let mut frame = Frame::new(Some(Arc::from(progname)), Vec::new());
        let keep = self.exec_block(&mut frame, ast)?;
        let _ = keep;
        Ok(std::mem::take(&mut frame.ret))
    }

    // ===== name resolution =====

    pub fn var_lookup(&self, frame: &Frame, name: &str, span: Span) -> LResult<Value> {
        if scope::find_in(&frame.scopes, name).is_some() {
            return scope::read_slot(&frame.scopes, name, span);
        }
        if scope::find_in(&frame.env, name).is_some() {
            return scope::read_slot(&frame.env, name, span);
        }
        scope::read_slot(std::slice::from_ref(&self.globals), name, span)
    }

    pub fn var_set(&self, frame: &Frame, name: &str, slot: SlotV) {
        if scope::find_in(&frame.scopes, name).is_some() {
            scope::set_slot(&frame.scopes, &self.globals, name, slot);
        } else if scope::find_in(&frame.env, name).is_some() {
            scope::set_slot(&frame.env, &self.globals, name, slot);
        } else {
            let mut g = self.globals.borrow_mut();
            g.vars.insert(name.to_string(), slot);
            g.version = g.version.wrapping_add(1);
        }
    }

    pub fn var_declare(&self, frame: &Frame, name: &str, slot: SlotV) {
        scope::declare(&frame.scopes, &self.globals, name, slot);
    }

    pub fn slot_loc(&self, frame: &Frame, name: &str) -> Option<SlotLoc> {
        if let Some(s) = scope::find_in(&frame.scopes, name) {
            return Some(SlotLoc { scope: s, name: name.to_string() });
        }
        if let Some(s) = scope::find_in(&frame.env, name) {
            return Some(SlotLoc { scope: s, name: name.to_string() });
        }
        if self.globals.borrow().vars.contains_key(name) {
            return Some(SlotLoc { scope: self.globals.clone(), name: name.to_string() });
        }
        None
    }

    // ===== ownership helpers =====

    /// Creates a borrow (alias) around a value.
    pub fn make_alias(&self, base: Value, by: &str, kind: AliasKind, span: Span) -> LResult<Value> {
        if let Some(vid) = base.vid() {
            let existing = self.borrowed_by(vid);
            let reborrows = matches!(&base, Value::Alias(a) if a.kind == AliasKind::Mut);
            match kind {
                AliasKind::Imm => {
                    if existing.iter().any(|b| b.kind == AliasKind::Mut) {
                        return Err(self.conflict_conf(
                            &format!("cannot borrow `{}` while it is mutably borrowed", by),
                            by,
                            &existing,
                            span,
                        ));
                    }
                }
                AliasKind::Mut => {
                    // a Mut reborrowed from an existing Mut alias is fine;
                    // anything else conflicts with any outstanding borrow
                    if !existing.is_empty() && !reborrows {
                        let msg = if existing.iter().any(|b| b.kind == AliasKind::Imm) {
                            format!("cannot borrow `{}` mutably while it is (immutably) borrowed", by)
                        } else {
                            format!("cannot borrow `{}` mutably again", by)
                        };
                        return Err(self.conflict_conf(&msg, by, &existing, span));
                    }
                }
            }
            let tag = value::with_reg(|r| r.next_tag());
            value::with_reg(|r| {
                r.add_borrower(vid, Borrower { tag, by: by.to_string(), span, kind })
            });
            return Ok(Value::Alias(Box::new(AliasBox {
                base,
                tag,
                kind,
                by: by.to_string(),
                span,
            })));
        }
        // scalars: borrow is a no-op (they are Copy)
        Ok(base)
    }

    fn conflict_conf(
        &self,
        msg: &str,
        var: &str,
        borrowers: &[Borrower],
        span: Span,
    ) -> LusaError {
        let mut err = LusaError::new(E_BORROW_CONFLICT, Some(span), msg.to_string());
        for b in borrowers {
            err = err.note(format!("`{}` is borrowed by `{}` here", var, b.by), Some(b.span));
        }
        err.help(format!(
            "moving and mutating need exclusive access; finish using `{}` first, drop the alias, or use `clone {}` for an independent copy",
            var, var
        ))
    }

    pub fn borrowed_by(&self, vid: u64) -> Vec<Borrower> {
        value::with_reg(|r| r.borrowers(vid))
    }

    pub fn move_err(&self, var: &str, notes: &[MoveNote], span: Span) -> LusaError {
        let mut err = LusaError::new(
            E_MOVED,
            Some(span),
            format!("use of moved-out value `{}`", var),
        );
        for n in notes {
            err = err.note(format!("`{}` moved to `{}` here", var, n.to), Some(n.span));
        }
        err.help(format!(
            "`{}` no longer holds the value; use `clone {}` (copy) or `borrow {}` (keep the original) when you still need it",
            var, var, var
        ))
    }

    /// Moves the value out of `loc`, enforcing borrows and recording history.
    /// `label` describes where it went.
    pub fn take_from_slot(
        &self,
        loc: &SlotLoc,
        src_desc: &str,
        label: &str,
        span: Span,
    ) -> LResult<Value> {
        let (found, history) = {
            let b = loc.scope.borrow();
            match b.vars.get(&loc.name) {
                Some(SlotV::Set(v)) => (Some(v.clone()), Vec::new()),
                Some(SlotV::Moved(n)) => (None, n.clone()),
                None => (None, Vec::new()),
            }
        };
        let v = match found {
            Some(v) => v,
            None => {
                return Err(self.move_err(src_desc, &history, span));
            }
        };
        if let Some(copy) = self.copy_struct_value(&v) {
            return Ok(copy);
        }
        match &v {
            Value::Table(_) | Value::Func(_) => {
                let vid = v.vid().expect("heap value has vid");
                let borrowers = self.borrowed_by(vid);
                if !borrowers.is_empty() {
                    return Err(self.conflict_conf(
                        &format!("cannot move out of `{}` because it is borrowed", src_desc),
                        src_desc,
                        &borrowers,
                        span,
                    ));
                }
                let note = MoveNote { to: label.to_string(), span };
                loc.scope
                    .borrow_mut()
                    .vars
                    .insert(loc.name.clone(), SlotV::Moved(vec![note]));
                value::with_reg(|r| r.rename(vid, label, span));
                Ok(v)
            }
            // aliases are Copy, scalars are Copy
            _ => Ok(v.clone()),
        }
    }

    /// Moves a table slot out (for field chains), same rules.
    pub fn take_field_from_table(
        &self,
        table: &TableObj,
        key: &Value,
        src_desc: &str,
        label: &str,
        span: Span,
    ) -> LResult<Value> {
        let slot = table.get_slot(key);
        let v = match slot {
            Some(SlotV::Set(v)) => v,
            Some(SlotV::Moved(notes)) => {
                return Err(self.move_err(src_desc, &notes, span));
            }
            None => return Ok(Value::Nil), // missing key: nil, nothing moved
        };
        if let Some(copy) = self.copy_struct_value(&v) {
            return Ok(copy);
        }
        match &v {
            Value::Table(_) | Value::Func(_) => {
                let vid = v.vid().expect("heap value has vid");
                let borrowers = self.borrowed_by(vid);
                if !borrowers.is_empty() {
                    return Err(self.conflict_conf(
                        &format!("cannot move out of `{}` because it is borrowed", src_desc),
                        src_desc,
                        &borrowers,
                        span,
                    ));
                }
                let note = MoveNote { to: label.to_string(), span };
                table.move_out(key, note);
                value::with_reg(|r| r.rename(vid, label, span));
                Ok(v)
            }
            _ => Ok(v.clone()),
        }
    }

    // ===== calls =====

    pub fn call_value(
        &mut self,
        callee: &Value,
        args: Vec<Value>,
        span: Span,
    ) -> LResult<Vec<Value>> {
        match callee {
            Value::Func(f) => {
                if self.depth >= MAX_CALL_DEPTH {
                    return Err(LusaError::new(
                        E_RUNTIME,
                        Some(span),
                        "call stack overflow (too many nested calls)",
                    )
                    .help(format!("Lusa allows {} nested calls", MAX_CALL_DEPTH)));
                }
                match &f.body {
                    FuncBody::Lusa(lit) => {
                        self.depth += 1;
                        let mut f2 = Frame::new(f.name.clone(), f.env.clone());
                        // bind params (ownership: args were already moved at call site)
                        for (i, p) in lit.params.iter().enumerate() {
                            let v = match args.get(i) {
                                Some(v) => v.clone(),
                                None => Value::Nil,
                            };
                            if let Some(vid) = v.vid() {
                                value::with_reg(|r| r.rename(vid, p.as_str(), span));
                            }
                            f2.scopes[0]
                                .borrow_mut()
                                .vars
                                .insert(p.clone(), SlotV::Set(v));
                        }
                        // extras become varargs
                        for extra in args.iter().skip(lit.params.len()) {
                            f2.varargs.push(extra.clone());
                        }
                        if !lit.has_vararg && args.len() > lit.params.len() {
                            // extra args dropped silently (Lua-style adjustment)
                            let _ = &mut f2.varargs;
                            f2.varargs.clear();
                        }
                        let result = self.exec_block(&mut f2, &lit.body);
                        self.depth -= 1;
                        result?;
                        Ok(std::mem::take(&mut f2.ret))
                    }
                    FuncBody::Builtin(b) => b(self, args, span),
                    FuncBody::Vm(compiled, upvals) => {
                        self.depth += 1;
                        let res = crate::vm::invoke(self, compiled, upvals.clone(), args, span);
                        self.depth -= 1;
                        res
                    }
                }
            }
            other => Err(LusaError::new(
                crate::error::E_CALL,
                Some(span),
                format!(
                    "attempt to call a {} value ({})",
                    other.type_name(),
                    other.fmt_value()
                ),
            )),
        }
    }

    // ===== statements =====

    /// Executes a block with its own scope (fresh locals per invocation).
    pub fn exec_block(&mut self, frame: &mut Frame, block: &Block) -> LResult<bool> {
        frame.scopes.push(scope::new_scope(false));
        for st in block {
            let keep = self.exec_st(frame, st)?;
            if !keep {
                frame.scopes.truncate(frame.scopes.len().saturating_sub(1));
                return Ok(false);
            }
        }
        frame.scopes.truncate(frame.scopes.len().saturating_sub(1));
        Ok(true)
    }

    fn exec_st(&mut self, frame: &mut Frame, st: &St) -> LResult<bool> {
        match &st.kind {
            StKind::Local { targets, values, annotations } => {
                self.exec_local(frame, targets, values, annotations, st.span)
            }
            StKind::Assign { targets, values } => self.exec_assign(frame, targets, values),
            StKind::Struct { name, fields, .. } => {
                let def = StructDef {
                    name: name.clone(),
                    fields: fields.iter().map(|f| (f.name.clone(), f.ty.clone())).collect(),
                };
                self.struct_defs.insert(name.clone(), def);
                Ok(true)
            }
            StKind::Unsafe { body } => {
                self.unsafe_depth += 1;
                let result = self.exec_block(frame, body);
                self.unsafe_depth -= 1;
                result
            }
            StKind::ExprSt(e) => {
                eval::eval_discard(self, frame, e, st.span)?;
                Ok(true)
            }
            StKind::If { branches, else_block } => {
                for (cond, body) in branches {
                    let c = eval::eval_expr(self, frame, cond)?;
                    if c.truthy() {
                        return self.exec_block(frame, body);
                    }
                }
                if let Some(e) = else_block {
                    return self.exec_block(frame, e);
                }
                Ok(true)
            }
            StKind::While { cond, body } => {
                loop {
                    let c = eval::eval_expr(self, frame, cond)?;
                    if !c.truthy() {
                        break;
                    }
                    let keep = self.exec_block(frame, body)?;
                    if !keep {
                        if frame.breaking {
                            frame.breaking = false;
                            break;
                        }
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            StKind::Repeat { body, cond } => {
                loop {
                    // the condition can see body's locals (Lua semantics):
                    // push a scope, run the body leaving it, eval cond, pop
                    frame.scopes.push(scope::new_scope(false));
                    let mut unwound = false;
                    for st in body {
                        let keep = self.exec_st(frame, st)?;
                        if !keep {
                            unwound = true;
                            break;
                        }
                    }
                    if unwound {
                        if frame.breaking {
                            frame.breaking = false;
                            frame.scopes.pop();
                            break;
                        }
                        frame.scopes.pop();
                        return Ok(false);
                    }
                    let c = eval::eval_expr(self, frame, cond)?;
                    frame.scopes.pop();
                    if c.truthy() {
                        break;
                    }
                }
                Ok(true)
            }
            StKind::ForNum { var, start, limit, step, body } => {
                self.exec_for_num(frame, var, start, limit, step.as_ref(), body)
            }
            StKind::ForIter { vars, values, body } => self.exec_for_iter(frame, vars, values, body),
            StKind::Do { body } => self.exec_block(frame, body),
            StKind::FuncSt { target, func } => {
                let fexpr = Expr {
                    kind: ExprKind::Func(func.clone()),
                    span: st.span,
                };
                self.exec_assign(frame, &[target.clone()], &[fexpr])?;
                Ok(true)
            }
            StKind::Return { values, span } => {
                let n = values.len();
                for (i, e) in values.iter().enumerate() {
                    let is_last = i + 1 == n;
                    let label = if is_last {
                        "(the last return value)".to_string()
                    } else {
                        format!("(return value {})", i + 1)
                    };
                    if is_last && eval::is_expansion(e) {
                        // `return f()` / `return ...` yields ALL results (Lua)
                        let vs = eval::eval_expand(self, frame, e)?;
                        frame.ret.extend(vs);
                    } else {
                        let v = eval::eval_take_single(self, frame, e, &label, *span)?;
                        frame.ret.push(v);
                    }
                }
                Ok(false)
            }
            StKind::Break => {
                frame.breaking = true;
                Ok(false)
            }
        }
    }

    fn exec_for_num(
        &mut self,
        frame: &mut Frame,
        var: &str,
        start: &Expr,
        limit: &Expr,
        step: Option<&Expr>,
        body: &Block,
    ) -> LResult<bool> {
        let span = start.span;
        let s = eval::eval_expr(self, frame, start)?;
        let l = eval::eval_expr(self, frame, limit)?;
        let st = match step {
            Some(e) => eval::eval_expr(self, frame, e)?,
            None => Value::Number(1.0),
        };
        let (sv, lv, stv) = match (&s, &l, &st) {
            (Value::Number(a), Value::Number(b), Value::Number(c)) => (*a, *b, *c),
            _ => {
                return Err(LusaError::new(
                    crate::error::E_ARITH,
                    Some(span),
                    "`for` bounds must be numbers",
                ));
            }
        };
        if stv == 0.0 {
            return Err(LusaError::new(
                crate::error::E_ARITH,
                Some(span),
                "`for` step cannot be zero",
            ));
        }
        let mut i = sv;
        loop {
            let more = if stv > 0.0 { i <= lv } else { i >= lv };
            if !more {
                break;
            }
            frame.scopes.push(scope::new_scope(false));
            frame
                .scopes
                .last_mut()
                .unwrap()
                .borrow_mut()
                .vars
                .insert(var.to_string(), SlotV::Set(Value::Number(i)));
            let keep = self.exec_block(frame, body)?;
            frame.scopes.truncate(frame.scopes.len().saturating_sub(1));
            if !keep {
                if frame.breaking {
                    frame.breaking = false;
                    break;
                }
                return Ok(false);
            }
            i += stv;
        }
        Ok(true)
    }

    fn exec_for_iter(
        &mut self,
        frame: &mut Frame,
        vars: &Vec<String>,
        values: &[Expr],
        body: &Block,
    ) -> LResult<bool> {
        if values.len() != 1 {
            return Err(LusaError::new(
                crate::error::E_PARSE,
                Some(values.first().map(|e| e.span).unwrap_or_else(crate::value::span0)),
                "generic `for` takes exactly one iterator expression",
            )
            .help("use `for k, v in pairs(t) do ... end` or `for v in ipairs(t) do ... end`"));
        }
        let span = values[0].span;
        let it = eval::eval_expr(self, frame, &values[0])?;
        loop {
            let vs = self.next_iter(&it, &vars, span)?;
            let done = vs.first().map(|v| v.is_nil()).unwrap_or(true);
            if done {
                break;
            }
            frame.scopes.push(scope::new_scope(false));
            for (i, var) in vars.iter().enumerate() {
                let v = vs.get(i).cloned().unwrap_or(Value::Nil);
                let v = if v.vid().is_some() {
                    // loop variables alias heap values so methods work
                    let vid = v.vid().expect("checked");
                    let existing = self.borrowed_by(vid);
                    if existing.is_empty() {
                        self.make_alias(v, var, AliasKind::Mut, span)?
                    } else {
                        v
                    }
                } else {
                    v
                };
                frame
                    .scopes
                    .last_mut()
                    .unwrap()
                    .borrow_mut()
                    .vars
                    .insert(var.clone(), SlotV::Set(v));
            }
            let keep = self.exec_block(frame, body)?;
            frame.scopes.truncate(frame.scopes.len().saturating_sub(1));
            if !keep {
                if frame.breaking {
                    frame.breaking = false;
                    break;
                }
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Advances an iterator one step, producing the loop values.
    fn next_iter(
        &mut self,
        it: &Value,
        vars: &[String],
        span: Span,
    ) -> LResult<Vec<Value>> {
        match it {
            Value::Iter(iter) => {
                let b = iter.borrow();
                let tbl = b.table.clone();
                let mut idx = b.idx;
                drop(b);
                loop {
                    if idx >= tbl.entry_count() {
                        iter.borrow_mut().idx = idx;
                        return Ok(Vec::new());
                    }
                    let (k, v) = tbl.entry_at(idx).expect("idx < count");
                    idx += 1;
                    if v.is_none() {
                        continue; // skipped moved/nil slots
                    }
                    iter.borrow_mut().idx = idx;
                    let pair = vec![k, v.expect("checked")];
                    let n = vars.len();
                    return Ok(pair.into_iter().take(n).collect());
                }
            }
            Value::Func(_) => {
                let results = self.call_value(it, Vec::new(), span)?;
                if results.is_empty() {
                    return Ok(Vec::new());
                }
                let n = vars.len().max(1);
                let mut out = results.into_iter().take(n).collect::<Vec<_>>();
                while out.len() < n {
                    out.push(Value::Nil);
                }
                Ok(out)
            }
            other => Err(LusaError::new(
                crate::error::E_INDEX,
                Some(span),
                format!(
                    "generic `for` needs an iterator (got {} from pairs/ipairs or a function); found {}",
                    "`pairs(t)`-style iterator",
                    other.type_name()
                ),
            )
            .help("pairs(t)/ipairs(t) return iterators in Lusa")),
        }
    }
}

// ===== declarations & assignment =====

impl Interp {
    /// Evaluates an l-value list, aligning values to targets (Lua semantics:
    /// only the last expression expands).
    fn eval_aligned_values(
        &mut self,
        frame: &mut Frame,
        desc: &[String],
        values: &[Expr],
    ) -> LResult<Vec<Value>> {
        let mut vals: Vec<Value> = Vec::new();
        let nv = values.len();
        for (i, e) in values.iter().enumerate() {
            let is_last = i + 1 == nv;
            if is_last && crate::eval::is_expansion(e) {
                let vs = crate::eval::eval_expand(self, frame, e)?;
                vals.extend(vs);
            } else {
                let label = desc.get(i).cloned().unwrap_or_else(|| "(value)".to_string());
                let v = crate::eval::eval_take_single(self, frame, e, &label, e.span)?;
                vals.push(v);
            }
        }
        vals.truncate(desc.len());
        while vals.len() < desc.len() {
            vals.push(Value::Nil);
        }
        Ok(vals)
    }

    fn rename_to_owner(&self, v: &Value, label: &str, span: Span) {
        if let Some(vid) = v.vid() {
            if !matches!(v, Value::Alias(_)) {
                value::with_reg(|r| r.rename(vid, label, span));
            }
        }
    }

    fn exec_local(
        &mut self,
        frame: &mut Frame,
        targets: &[String],
        values: &[Expr],
        annotations: &[Option<String>],
        span: Span,
    ) -> LResult<bool> {
        let mut vals = self.eval_aligned_values(frame, targets, values)?;
        for i in 0..targets.len() {
            let is_nil = matches!(vals[i], Value::Nil);
            if !is_nil {
                continue;
            }
            if let Some(ann) = annotations.get(i).and_then(|a| a.as_deref()) {
                if ann.starts_with('*') {
                    // raw pointer: a cell index into the simulated address space
                    vals[i] = Value::Number(0.0);
                } else if let Some((element, length)) = array_annotation(ann) {
                    // fixed array: a 0-based table, matching the native C buffer
                    let tbl = TableObj::new();
                    let zero = if element == "bool" {
                        Value::Bool(false)
                    } else if element == "string"
                        || element == "table"
                        || element.starts_with("table<")
                        || self.struct_defs.contains_key(element)
                    {
                        Value::Nil
                    } else {
                        Value::Number(0.0)
                    };
                    for index in 0..length {
                        tbl.set(Value::Number(index as f64), zero.clone()).ok();
                    }
                    vals[i] = Value::Table(Rc::new(tbl));
                } else if self.struct_defs.contains_key(ann) {
                    // static struct: a fixed-field simulated buffer
                    if let Some(v) = self.default_struct_value(ann) {
                        vals[i] = v;
                    }
                }
            }
        }
        for (name, v) in targets.iter().zip(vals.into_iter()) {
            self.rename_to_owner(&v, name, span);
            self.var_declare(frame, name, SlotV::Set(v));
        }
        Ok(true)
    }

    /// Builds a zero-initialized struct instance for the dynamic engines.
    pub(crate) fn default_struct_value(&self, name: &str) -> Option<Value> {
        self.default_struct_value_at(name, 0)
    }

    fn struct_table_name(&self, value: &Value) -> Option<String> {
        let vid = value.vid()?;
        let name = self.struct_tables.borrow().get(&vid).cloned();
        name
    }

    /// True when a struct owns heap values through any field, directly or via
    /// a nested struct.  Such values follow move semantics; scalar-only
    /// structs are ordinary copyable values.
    fn struct_owns_heap(&self, name: &str) -> bool {
        let mut pending = vec![name.to_string()];
        let mut seen: HashSet<String> = HashSet::new();
        while let Some(current) = pending.pop() {
            if !seen.insert(current.clone()) {
                continue;
            }
            let Some(def) = self.struct_defs.get(&current) else {
                continue;
            };
            for (_, ty) in &def.fields {
                if ty == "string" || ty == "table" || ty.starts_with("table<") {
                    return true;
                }
                if self.struct_defs.contains_key(ty) {
                    pending.push(ty.clone());
                }
            }
        }
        false
    }

    /// Returns an independent copy when `value` is a scalar-only struct
    /// instance, matching the native backend's value semantics.  Heap-bearing
    /// structs keep move semantics and return `None`.
    pub(crate) fn copy_struct_value(&self, value: &Value) -> Option<Value> {
        let name = self.struct_table_name(value)?;
        if self.struct_owns_heap(&name) {
            return None;
        }
        Some(self.deep_clone_value(value))
    }

    /// Deep-clones a value while carrying struct identity tags over to the
    /// clone, including nested struct tables.  Cloned scalar-only structs must
    /// keep copy semantics, otherwise the next field read would move them.
    pub(crate) fn deep_clone_value(&self, value: &Value) -> Value {
        let copy = value.deep_clone();
        let mut visited = HashSet::new();
        self.tag_cloned_structs(value, &copy, &mut visited);
        copy
    }

    fn tag_cloned_structs(&self, src: &Value, copy: &Value, visited: &mut HashSet<u64>) {
        if let (Value::Alias(src_alias), Value::Alias(copy_alias)) = (src, copy) {
            self.tag_cloned_structs(&src_alias.base, &copy_alias.base, visited);
            return;
        }
        let (Value::Table(src_t), Value::Table(copy_t)) = (src, copy) else {
            return;
        };
        if !visited.insert(src_t.vid) {
            return;
        }
        let tag = self.struct_tables.borrow().get(&src_t.vid).cloned();
        if let Some(name) = tag {
            self.struct_tables.borrow_mut().insert(copy_t.vid, name);
        }
        let pairs: Vec<(Value, Value)> = {
            let source = src_t.cell.borrow();
            let cloned = copy_t.cell.borrow();
            source
                .entries
                .iter()
                .zip(cloned.entries.iter())
                .filter_map(|((_, source), (_, cloned))| match (source, cloned) {
                    (SlotV::Set(source), SlotV::Set(cloned)) => {
                        Some((source.clone(), cloned.clone()))
                    }
                    _ => None,
                })
                .collect()
        };
        for (source, cloned) in pairs {
            self.tag_cloned_structs(&source, &cloned, visited);
        }
    }

    fn default_struct_value_at(&self, name: &str, depth: usize) -> Option<Value> {
        let def = self.struct_defs.get(name)?;
        let table = TableObj::new();
        self.struct_tables
            .borrow_mut()
            .insert(table.vid, name.to_string());
        for (fname, ty) in &def.fields {
            let zero = if ty == "bool" {
                Value::Bool(false)
            } else if ty == "string" || ty == "table" || ty.starts_with("table<")
                || ty.starts_with('*') || ty == "nil"
            {
                Value::Nil
            } else if depth < 8 && self.struct_defs.contains_key(ty) {
                self.default_struct_value_at(ty, depth + 1)
                    .unwrap_or(Value::Nil)
            } else if let Some((element, length)) = array_annotation(ty) {
                let tbl = TableObj::new();
                let zero = if element == "bool" {
                    Value::Bool(false)
                } else if element == "string" || element == "table"
                    || element.starts_with("table<")
                {
                    Value::Nil
                } else if depth < 8 && self.struct_defs.contains_key(element) {
                    self.default_struct_value_at(element, depth + 1)
                        .unwrap_or(Value::Nil)
                } else {
                    Value::Number(0.0)
                };
                for index in 0..length {
                    tbl.set(Value::Number(index as f64), zero.clone()).ok();
                }
                Value::Table(Rc::new(tbl))
            } else {
                Value::Number(0.0)
            };
            let _ = table.set(Value::Str(Arc::from(fname.as_str())), zero);
        }
        Some(Value::Table(Rc::new(table)))
    }

    fn exec_assign(
        &mut self,
        frame: &mut Frame,
        targets: &[Expr],
        values: &[Expr],
    ) -> LResult<bool> {
        use crate::eval::label_of;

        // resolve target locations first (Lua order: targets before values)
        enum Dst {
            Var(String),
            Field { obj: Value, key: Value, desc: String, span: Span },
            Deref { addr: u64, desc: String, span: Span },
        }
        let mut dsts: Vec<Dst> = Vec::with_capacity(targets.len());
        for t in targets {
            match &t.kind {
                ExprKind::Var(n) => dsts.push(Dst::Var((**n).clone())),
                ExprKind::Field { obj, name } => {
                    let o = crate::eval::eval_expr(self, frame, obj)?;
                    let key = Value::Str(Arc::from(name.as_str()));
                    dsts.push(Dst::Field { obj: o, key, desc: label_of(t), span: t.span });
                }
                ExprKind::Index { obj, key } => {
                    let o = crate::eval::eval_expr(self, frame, obj)?;
                    let k = crate::eval::eval_key(self, frame, key)?;
                    dsts.push(Dst::Field { obj: o, key: k, desc: label_of(t), span: t.span });
                }
                ExprKind::Un { op: UnOp::Deref, e } => {
                    let addr = crate::eval::eval_deref_addr(self, frame, e, t.span)?;
                    dsts.push(Dst::Deref { addr, desc: label_of(t), span: t.span });
                }
                _ => unreachable!("parser validates assignment targets"),
            }
        }
        let desc: Vec<String> = dsts
            .iter()
            .map(|d| match d {
                Dst::Var(n) => n.clone(),
                Dst::Field { desc, .. } => desc.clone(),
                Dst::Deref { desc, .. } => desc.clone(),
            })
            .collect();
        let vals = self.eval_aligned_values(frame, &desc, values)?;

        for (dst, v) in dsts.into_iter().zip(vals.into_iter()) {
            match dst {
                Dst::Var(name) => {
                    match self.slot_loc(frame, &name) {
                        Some(loc) => {
                            self.check_slot_overwrite(&loc, &name)?;
                            // aliases flow as-is; ownership goes to the name
                            self.var_set(frame, &name, SlotV::Set(v.clone()));
                            self.rename_to_owner(&v, &name, Span::new(0, 0));
                        }
                        None => {
                            // assignment auto-declares a global (Lua-like)
                            let mut g = self.globals.borrow_mut();
                            g.vars.insert(name.clone(), SlotV::Set(v.clone()));
                            g.version = g.version.wrapping_add(1);
                            drop(g);
                            self.rename_to_owner(&v, &name, Span::new(0, 0));
                        }
                    }
                }
                Dst::Field { obj, key, desc, span } => {
                    self.write_table_slot(obj, key, v, &desc, span)?;
                }
                Dst::Deref { addr, desc, span } => {
                    if self.unsafe_depth == 0 {
                        return Err(LusaError::new(
                            crate::error::E_PTR_DEREF,
                            Some(span),
                            "raw pointer dereference requires an `unsafe` block",
                        )
                        .note(
                            format!("wrap the write to `{desc}` in `unsafe do ... end`"),
                            None,
                        ));
                    }
                    let value = match v.base_value() {
                        Value::Number(n) => *n,
                        other => {
                            return Err(LusaError::new(
                                crate::error::E_RUNTIME,
                                Some(span),
                                format!(
                                    "cannot store a {} value through a raw pointer",
                                    other.type_name()
                                ),
                            ))
                        }
                    };
                    self.unsafe_mem.insert(addr, value);
                }
            }
        }
        Ok(true)
    }

    /// Refuse assignments over slots whose current heap value is borrowed.
    fn check_slot_overwrite(&self, loc: &SlotLoc, desc: &str) -> LResult<()> {
        let cur = loc.scope.borrow().vars.get(&loc.name).map(|s| match s {
            SlotV::Set(v) => Some(v.clone()),
            SlotV::Moved(_) => None,
        });
        match cur.flatten() {
            Some(ref v) if matches!(v, Value::Table(_) | Value::Func(_)) => {
                let vid = v.vid().expect("heap value");
                let borrowers = self.borrowed_by(vid);
                if !borrowers.is_empty() {
                    return Err(self.conflict_conf(
                        &format!("cannot assign to `{}` because it is borrowed", desc),
                        desc,
                        &borrowers,
                        Span::new(0, 0),
                    ));
                }
                Ok(())
            }
            _ => Ok(()), // aliases / scalars are replaced freely
        }
    }

    /// `t.k = v` / `t[k] = v` through slot semantics with borrow checks.
    pub(crate) fn write_table_slot(
        &self,
        obj: Value,
        key: Value,
        newv: Value,
        desc: &str,
        span: Span,
    ) -> LResult<()> {
        let t: Rc<TableObj> = match obj {
            Value::Table(t) => {
                if self.unsafe_depth == 0 {
                    let borrowers = self.borrowed_by(t.vid);
                    if !borrowers.is_empty() {
                        return Err(self.conflict_conf(
                            &format!("cannot assign to `{}` because its table is borrowed", desc),
                            desc,
                            &borrowers,
                            span,
                        ));
                    }
                }
                t
            }
            Value::Alias(a) => {
                if self.unsafe_depth == 0 && a.kind != AliasKind::Mut {
                    return Err(LusaError::new(
                        crate::error::E_MUT_IMMUT,
                        Some(span),
                        format!(
                            "cannot assign to `{}` through a read-only borrow (`{}`)",
                            desc, a.by
                        ),
                    )
                    .help("only method receivers (`o:m()`, a mutable borrow) can mutate through"));
                }
                match a.base.base_value() {
                    Value::Table(t) => Rc::clone(t),
                    other => {
                        return Err(LusaError::new(
                            crate::error::E_INDEX,
                            Some(span),
                            format!("attempt to index a {} value", other.type_name()),
                        ))
                    }
                }
            }
            other => {
                return Err(LusaError::new(
                    crate::error::E_INDEX,
                    Some(span),
                    format!(
                        "attempt to index a {} value ({})",
                        other.type_name(),
                        other.fmt_value()
                    ),
                ))
            }
        };
        // existing slot borrowed? => conflict
        if let Some(SlotV::Set(cur)) = t.get_slot(&key) {
            if matches!(cur, Value::Table(_) | Value::Func(_)) {
                let vid = cur.vid().expect("heap value");
                let borrowers = self.borrowed_by(vid);
                if !borrowers.is_empty() {
                    return Err(self.conflict_conf(
                        &format!("cannot overwrite `{}` because it is borrowed", desc),
                        desc,
                        &borrowers,
                        span,
                    ));
                }
            }
        }
        t.set(key, newv.clone())?;
        self.rename_to_owner(&newv, desc, span);
        Ok(())
    }
}

/// Helper used by scope.rs for error notes.
pub fn move_from_history(err: LusaError, name: &str, notes: &[MoveNote]) -> LusaError {
    let mut e = err;
    for n in notes {
        e = e.note(format!("`{}` moved to `{}` here", name, n.to), Some(n.span));
    }
    e
}
