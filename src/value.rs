//! Runtime values, tables, and the ownership/borrow engine.
//!
//! Copy kinds — nil, bool, number, string — are copied on assignment and call.
//! Heap kinds — tables and functions — are moved by default (the original
//! variable/field becomes moved-out). `borrow x` creates an alias; `clone x`
//! makes an independent deep copy.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use crate::error::{LResult, LusaError, Span, E_BAD_KEY};
use crate::eval::{key_eq_loose, key_hash_loose};

// ===== ownership registry =====

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AliasKind {
    /// read-only borrow — `borrow x`
    Imm,
    /// mutable borrow — method receivers (`o:m()`)
    Mut,
}

#[derive(Clone, Debug)]
pub struct MoveNote {
    /// where the value went, e.g. `y` or "(argument)"
    pub to: String,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Borrower {
    pub tag: u64,
    pub by: String,
    pub span: Span,
    pub kind: AliasKind,
}

#[derive(Default)]
pub struct OwnerState {
    /// what currently owns the value (latest rename); None = fresh/unowned
    pub owner: Option<(String, Span)>,
}

#[derive(Default)]
pub struct Ownership {
    state: HashMap<u64, OwnerState>,
    borrowers: HashMap<u64, Vec<Borrower>>,
    next_tag: u64,
    next_vid: u64,
}

impl Ownership {
    pub fn new() -> Self {
        Default::default()
    }
    pub fn alloc_vid(&mut self) -> u64 {
        self.next_vid += 1;
        self.next_vid
    }
    pub fn state(&mut self, vid: u64) -> &mut OwnerState {
        self.state.entry(vid).or_default()
    }
    pub fn rename(&mut self, vid: u64, name: impl Into<String>, span: Span) {
        let st = self.state(vid);
        st.owner = Some((name.into(), span));
    }
    pub fn next_tag(&mut self) -> u64 {
        self.next_tag += 1;
        self.next_tag
    }
    pub fn borrowers(&mut self, vid: u64) -> Vec<Borrower> {
        self.borrowers.get(&vid).cloned().unwrap_or_default()
    }
    pub fn add_borrower(&mut self, vid: u64, b: Borrower) {
        self.borrowers.entry(vid).or_default().push(b);
    }
    pub fn release_tag(&mut self, vid: u64, tag: u64) {
        if let Some(list) = self.borrowers.get_mut(&vid) {
            list.retain(|b| b.tag != tag);
            if list.is_empty() {
                self.borrowers.remove(&vid);
            }
        }
    }
}

thread_local! {
    static OWNERSHIP: RefCell<Ownership> = RefCell::new(Ownership::new());
}

pub fn with_reg<T>(f: impl FnOnce(&mut Ownership) -> T) -> T {
    OWNERSHIP.with(|r| f(&mut r.borrow_mut()))
}

pub fn span0() -> Span {
    Span { start: 0, end: 0 }
}

// ===== slots =====

/// A storage slot (a variable, or a table field).
pub enum SlotV {
    Set(Value),
    Moved(Vec<MoveNote>),
}

impl SlotV {
    pub fn as_value(&self) -> Option<&Value> {
        match self {
            SlotV::Set(v) => Some(v),
            SlotV::Moved(_) => None,
        }
    }
}

// ===== values =====

pub enum Value {
    Nil,
    Bool(bool),
    Number(f64),
    Str(Arc<str>),
    Table(Rc<TableObj>),
    Func(Rc<FuncObj>),
    Iter(Rc<RefCell<IterState>>),
    /// cooperative coroutine: a suspended bytecode VM plus its entry data
    Coroutine(Rc<RefCell<CoroutineObj>>),
    /// result of `borrow x` — an alias to a heap value
    Alias(Box<AliasBox>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoroState {
    Suspended,
    Running,
    Dead,
}

/// Captured coroutine locals keyed by name. Every entry keeps the cell shared
/// by the coroutine's compiled frames and the interpreter scope it came from.
pub type CaptureMap = std::collections::HashMap<
    String,
    (crate::scope::ScopeRef, Rc<RefCell<SlotV>>),
>;

/// A coroutine owns a private bytecode VM. The VM is swapped into
/// `Interp::vm` only while the coroutine is running, so suspended stacks
/// (registers, upvalues, and pending expression values) stay alive between
/// resumes.
pub struct CoroutineObj {
    pub id: u64,
    pub state: CoroState,
    pub vm: crate::vm::Vm,
    /// compiled entry function, consumed on the first resume
    pub entry: Option<(
        Rc<crate::compiler::Compiled>,
        Rc<[Rc<RefCell<SlotV>>]>,
    )>,
    /// entry upvalues stay reachable after `entry` is consumed so their
    /// captured values can be synchronized with interpreter scopes.
    pub upvals: Option<Rc<[Rc<RefCell<SlotV>>]>>,
    /// cells shared by entry and helper functions compiled on demand
    pub capture_env: CaptureMap,
    /// the pending yield expression truncates its resume result to one value
    pub yield_one: bool,
    /// `unsafe` depth saved while the coroutine is suspended. Each coroutine
    /// owns its sandbox capability; it must not leak into the host.
    pub unsafe_depth: usize,
}

impl CoroutineObj {
    pub fn new(
        id: u64,
        entry: Option<(
            Rc<crate::compiler::Compiled>,
            Rc<[Rc<RefCell<SlotV>>]>,
        )>,
        upvals: Option<Rc<[Rc<RefCell<SlotV>>]>>,
        captures: Vec<(usize, crate::scope::ScopeRef, Arc<str>)>,
    ) -> CoroutineObj {
        let mut capture_env = CaptureMap::new();
        if let Some(cells) = &upvals {
            for (index, scope, name) in captures {
                if let Some(cell) = cells.get(index) {
                    capture_env.insert(name.to_string(), (scope, cell.clone()));
                }
            }
        }
        CoroutineObj {
            id,
            state: CoroState::Suspended,
            vm: crate::vm::Vm::new(),
            entry,
            upvals,
            capture_env,
            yield_one: false,
            unsafe_depth: 0,
        }
    }
}

pub struct AliasBox {
    pub base: Value,
    pub tag: u64,
    pub kind: AliasKind,
    pub by: String,
    pub span: Span,
}

pub struct TableObj {
    pub vid: u64,
    pub next_arr: Cell<usize>,
    pub cell: RefCell<TableData>,
}

#[derive(Default)]
pub struct TableData {
    /// insertion ordered
    pub entries: Vec<(Value, SlotV)>,
    /// hash -> entry indexes (for O(1) lookup; collisions fall back to a scan)
    pub by_hash: HashMap<u64, Vec<usize>>,
}

pub struct IterState {
    pub table: Rc<TableObj>,
    pub idx: usize,
}

impl Drop for AliasBox {
    fn drop(&mut self) {
        if let Some(vid) = self.base.vid() {
            with_reg(|r| r.release_tag(vid, self.tag));
        }
    }
}

impl Clone for Value {
    /// Copy semantics — scalars/strings copy; heap handles duplicate the Rc
    /// (a *read*, not a move); aliases get a fresh tag (they are Copy too,
    /// like Rust's shared references).
    ///
    /// The alias branch is cold and outlined; the common scalar/Rc copy stays
    /// small enough to inline into the VM's slot reads.
    #[inline]
    fn clone(&self) -> Self {
        match self {
            Value::Alias(a) => clone_alias(a),
            Value::Nil => Value::Nil,
            Value::Bool(b) => Value::Bool(*b),
            Value::Number(n) => Value::Number(*n),
            Value::Str(s) => Value::Str(Arc::clone(s)),
            Value::Table(t) => Value::Table(Rc::clone(t)),
            Value::Func(f) => Value::Func(Rc::clone(f)),
            Value::Iter(i) => Value::Iter(Rc::clone(i)),
            Value::Coroutine(c) => Value::Coroutine(Rc::clone(c)),
        }
    }
}

#[cold]
#[inline(never)]
fn clone_alias(a: &AliasBox) -> Value {
    // clone the base first: cloning a nested alias uses the registry too, so
    // it must NOT happen inside `with_reg`
    let base = a.base.clone();
    let (tag, by, span) = with_reg(|r| {
        let tag = r.next_tag();
        if let Some(vid) = base.vid() {
            r.add_borrower(
                vid,
                Borrower { tag, by: a.by.clone(), span: a.span, kind: a.kind },
            );
        }
        (tag, a.by.clone(), a.span)
    });
    Value::Alias(Box::new(AliasBox { base, tag, kind: a.kind, by, span }))
}

impl Value {
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Nil => "nil",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::Str(_) => "string",
            Value::Table(_) => "table",
            Value::Func(_) => "function",
            Value::Iter(_) => "iterator",
            Value::Coroutine(_) => "coroutine",
            Value::Alias(a) => a.base.type_name(),
        }
    }

    pub fn vid(&self) -> Option<u64> {
        match self {
            Value::Table(t) => Some(t.vid),
            Value::Func(f) => Some(f.vid),
            Value::Alias(a) => a.base.vid(),
            _ => None,
        }
    }

    /// True for standard-library functions: they borrow their arguments
    /// instead of taking ownership (caller keeps the value).
    pub fn is_builtin(&self) -> bool {
        matches!(self, Value::Func(f) if matches!(f.body, FuncBody::Builtin(_)))
    }

    /// The innermost value behind alias layers.
    pub fn base_value(&self) -> &Value {
        match self {
            Value::Alias(a) => a.base.base_value(),
            v => v,
        }
    }

    /// The outermost alias (borrow) around the base value (reserved API)
    #[allow(dead_code)]
    pub fn outer_alias(&self) -> Option<&AliasBox> {
        match self {
            Value::Alias(a) => Some(a),
            _ => None,
        }
    }

    /// Truthiness — only nil and false are falsy.
    pub fn truthy(&self) -> bool {
        !matches!(self, Value::Nil | Value::Bool(false))
    }

    pub fn is_nil(&self) -> bool {
        matches!(self, Value::Nil)
    }

    pub fn fmt_value(&self) -> String {
        match self {
            Value::Nil => "nil".into(),
            Value::Bool(b) => if *b { "true".into() } else { "false".into() },
            Value::Number(n) => format_number(*n),
            Value::Str(s) => s.to_string(),
            Value::Table(t) => format!("table: 0x{:x}", t.vid),
            Value::Func(f) => format!("function: 0x{:x}", f.vid),
            Value::Iter(_) => "iterator".into(),
            Value::Coroutine(c) => format!("coroutine: 0x{:x}", c.borrow().id),
            Value::Alias(_) => self.type_name().into(),
        }
    }

    /// Deep clone (used by `clone x`). Cloned contents get fresh ownership.
    pub fn deep_clone(&self) -> Value {
        match self {
            Value::Table(t) => {
                let obj = TableObj::new();
                {
                    let mut td = obj.cell.borrow_mut();
                    let entries: Vec<(Value, SlotV)> = {
                        let src = t.cell.borrow();
                        src.entries
                            .iter()
                            .map(|(k, sv)| {
                                let sv = match sv {
                                    SlotV::Set(v) => SlotV::Set(v.deep_clone()),
                                    SlotV::Moved(notes) => SlotV::Moved(notes.clone()),
                                };
                                (k.clone(), sv)
                            })
                            .collect()
                    };
                    for (k, sv) in entries {
                        if let Some(h) = key_hash_loose(&k) {
                            let idx = td.entries.len();
                            td.by_hash.entry(h).or_default().push(idx);
                        }
                        td.entries.push((k, sv));
                    }
                }
                obj.next_arr.set(t.next_arr.get());
                Value::Table(Rc::new(obj))
            }
            Value::Alias(a) => {
                // Clone the base first: recursive deep_clone calls take the
                // ownership registry mutably, so doing that inside with_reg
                // would re-enter the RefCell and panic.
                let base = a.base.deep_clone();
                let (tag, by, span) = with_reg(|r| {
                    let tag = r.next_tag();
                    if let Some(vid) = base.vid() {
                        r.add_borrower(
                            vid,
                            Borrower { tag, by: a.by.clone(), span: a.span, kind: AliasKind::Imm },
                        );
                    }
                    (tag, a.by.clone(), a.span)
                });
                Value::Alias(Box::new(AliasBox { base, tag, kind: AliasKind::Imm, by, span }))
            }
            other => other.clone(),
        }
    }
}

/// Converts an exact integer token (`123...` or `0x...`) to the dynamic
/// runtime's f64 representation.  The C11 backend uses the original string
/// instead, preserving all 128 bits.
pub fn int_literal_to_f64(text: &str) -> f64 {
    if let Some(hex) = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
    {
        u128::from_str_radix(hex, 16)
            .map(|v| v as f64)
            .unwrap_or(f64::INFINITY)
    } else {
        text.parse::<f64>().unwrap_or(f64::INFINITY)
    }
}

/// Formats a number the Lua way: integral values without decimals.
pub fn format_number(n: f64) -> String {
    if n.is_nan() {
        "nan".into()
    } else if n.is_infinite() {
        if n > 0.0 { "inf".into() } else { "-inf".into() }
    } else if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{:.0}", n)
    } else {
        format!("{}", n)
    }
}

// ===== table helpers =====

impl TableObj {
    pub fn new() -> Self {
        TableObj {
            vid: with_reg(|r| r.alloc_vid()),
            next_arr: Cell::new(1),
            cell: RefCell::new(TableData::default()),
        }
    }

    fn index_of(&self, key: &Value) -> Option<usize> {
        let h = key_hash_loose(key)?;
        let cell = self.cell.borrow();
        let bucket = cell.by_hash.get(&h)?;
        for &i in bucket {
            if key_eq_loose(&cell.entries[i].0, key) {
                return Some(i);
            }
        }
        None
    }

    /// membership test (reserved for future stdlib pieces)
    #[allow(dead_code)]
    pub fn has(&self, key: &Value) -> bool {
        self.index_of(key).is_some()
    }

    pub fn get(&self, key: &Value) -> Option<Value> {
        let i = self.index_of(key)?;
        let cell = self.cell.borrow();
        match &cell.entries[i].1 {
            SlotV::Set(v) => Some(v.clone()),
            SlotV::Moved(_) => None,
        }
    }

    pub fn get_slot(&self, key: &Value) -> Option<SlotV> {
        let i = self.index_of(key)?;
        let cell = self.cell.borrow();
        Some(match &cell.entries[i].1 {
            SlotV::Set(v) => SlotV::Set(v.clone()),
            SlotV::Moved(n) => SlotV::Moved(n.clone()),
        })
    }

    /// Sets a key, returning Err for invalid (nil/NaN) keys.
    pub fn set(&self, key: Value, val: Value) -> LResult<()> {
        if key.is_nil() || matches!(&key, Value::Number(n) if n.is_nan()) {
            return Err(LusaError::new(E_BAD_KEY, Some(span0()), "invalid table key (nil or NaN)"));
        }
        let arr_top = match &key {
            // keeps the Lua `#` heuristic fresh for dense integer keys
            Value::Number(n) if n.fract() == 0.0 && *n >= 1.0 => Some(*n as usize),
            _ => None,
        };
        let mut cell = self.cell.borrow_mut();
        let h = key_hash_loose(&key).expect("key validated");
        let probe: Vec<usize> = cell.by_hash.get(&h).cloned().unwrap_or_default();
        let mut replaced = false;
        for &i in &probe {
            if key_eq_loose(&cell.entries[i].0, &key) {
                cell.entries[i].1 = SlotV::Set(val.clone());
                replaced = true;
                break;
            }
        }
        if !replaced {
            let new_i = cell.entries.len();
            cell.entries.push((key, SlotV::Set(val)));
            cell.by_hash.entry(h).or_default().push(new_i);
        }
        if let Some(top) = arr_top {
            if top >= self.next_arr.get() {
                self.next_arr.set(top + 1);
            }
        }
        Ok(())
    }

    /// Moves a slot out of the table (emptying it), as `local x = t.k` does.
    pub fn move_out(&self, key: &Value, note: MoveNote) -> Option<Value> {
        let i = self.index_of(key)?;
        let mut cell = self.cell.borrow_mut();
        let (_, slot) = &mut cell.entries[i];
        match std::mem::replace(slot, SlotV::Moved(vec![note])) {
            SlotV::Set(v) => Some(v),
            SlotV::Moved(_) => None,
        }
    }

    /// Lua border heuristic: the largest n such that keys 1..=n exist.
    pub fn len(&self) -> usize {
        let cell = self.cell.borrow();
        let mut n = self.next_arr.get();
        while n > 0 {
            let k = Value::Number(n as f64);
            let found = cell.entries.iter().any(|(ek, esv)| {
                matches!(esv, SlotV::Set(_)) && key_eq_loose(ek, &k)
            });
            if found {
                break;
            }
            n -= 1;
        }
        n
    }

    /// clears all slots (reserved for future stdlib pieces)
    #[allow(dead_code)]
    pub fn clear(&self) {
        {
            let mut cell = self.cell.borrow_mut();
            cell.entries.clear();
            cell.by_hash.clear();
        }
        self.next_arr.set(1);
    }

    pub fn entry_count(&self) -> usize {
        self.cell.borrow().entries.len()
    }

    /// Entry at insertion-order index (used by `pairs` iteration).
    pub fn entry_at(&self, i: usize) -> Option<(Value, Option<Value>)> {
        let cell = self.cell.borrow();
        let (k, v) = cell.entries.get(i)?;
        Some((
            k.clone(),
            match v {
                SlotV::Set(val) => Some(val.clone()),
                SlotV::Moved(_) => None,
            },
        ))
    }

    /// sets a slot directly (reserved for future stdlib pieces)
    #[allow(dead_code)]
    pub fn set_slot(&self, key: &Value, slot: SlotV) -> Option<()> {
        let i = self.index_of(key)?;
        self.cell.borrow_mut().entries[i].1 = slot;
        Some(())
    }
}

impl Default for TableObj {
    fn default() -> Self {
        Self::new()
    }
}

// ===== function object =====

pub enum FuncBody {
    Lusa(Rc<crate::ast::FuncLit>),
    Builtin(Builtin),
    /// compiled bytecode function: prototype + captured upvalue slots
    Vm(Rc<crate::compiler::Compiled>, Rc<[Rc<RefCell<SlotV>>]>),
}

pub type Builtin =
    Box<dyn Fn(&mut crate::interp::Interp, Vec<Value>, Span) -> LResult<Vec<Value>>>;

pub struct FuncObj {
    pub vid: u64,
    pub name: Option<Arc<str>>,
    /// creation-time lexical chain (locals of enclosing frames)
    pub env: Vec<Rc<RefCell<crate::scope::ScopeData>>>,
    pub body: FuncBody,
}
