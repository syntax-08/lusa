//! Scopes — storage for variables.
//!
//! Every frame keeps a stack of block scopes (locals). Closures capture the
//! lexical chain of outer frames (`env`). Globals live in one long-lived
//! scope. Name resolution order: current frame scopes → closure env → globals.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::rc::Rc;

use crate::error::{LResult, LusaError, Span, E_MOVED};
use crate::interp::move_from_history;
use crate::value::{SlotV, Value};

/// Small non-cryptographic hasher for variable names. The default SipHash is
/// overkill for a single-threaded language runtime and dominates short global
/// lookups (`GetGlob` in recursive functions).
pub struct NameHasher(u64);

const FNV_INIT: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;

impl Default for NameHasher {
    #[inline]
    fn default() -> Self {
        Self(FNV_INIT)
    }
}

impl Hasher for NameHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut h = self.0;
        for &b in bytes {
            h ^= b as u64;
            h = h.wrapping_mul(FNV_PRIME);
        }
        self.0 = h;
    }
}

pub type NameMap<K, V> = HashMap<K, V, BuildHasherDefault<NameHasher>>;

pub struct ScopeData {
    pub vars: NameMap<String, SlotV>,
    /// reserved: distinguishes the (single) globals scope
    #[allow(dead_code)]
    pub is_global: bool,
    /// bumped on every global mutation; lets the VM keep a one-entry read
    /// cache without ever observing a stale global value
    pub version: u64,
}

pub type ScopeRef = Rc<RefCell<ScopeData>>;

pub fn new_scope(is_global: bool) -> ScopeRef {
    Rc::new(RefCell::new(ScopeData {
        vars: NameMap::default(),
        is_global,
        version: 0,
    }))
}

/// Finds which scope contains `name`, searching top-down.
pub fn find_in(chain: &[ScopeRef], name: &str) -> Option<ScopeRef> {
    for s in chain.iter().rev() {
        if s.borrow().vars.contains_key(name) {
            return Some(Rc::clone(s));
        }
    }
    None
}

/// Reads the slot for `name` as a value. Missing names read as nil. Errors if
/// the slot was moved out.
pub fn read_slot(chain: &[ScopeRef], name: &str, span: Span) -> LResult<Value> {
    let sc = match find_in(chain, name) {
        Some(s) => s,
        None => return Ok(Value::Nil),
    };
    let found = {
        let b = sc.borrow();
        b.vars.get(name).map(|s| s.as_value().is_some()).unwrap_or(false)
    };
    if !found {
        let notes = {
            let b = sc.borrow();
            b.vars
                .get(name)
                .and_then(|s| match s {
                    SlotV::Moved(n) => Some(n.clone()),
                    _ => None,
                })
                .unwrap_or_default()
        };
        let mut err = LusaError::new(
            E_MOVED,
            Some(span),
            format!("use of moved-out value `{}`", name),
        );
        err = move_from_history(err, name, &notes);
        return Err(err.help(format!(
            "assignments and calls consume heap values; re-assign `{}` or use `clone {}` to make an independent copy",
            name, name
        )));
    }
    let b = sc.borrow();
    match b.vars.get(name) {
        Some(SlotV::Set(v)) => Ok(v.clone()),
        _ => Ok(Value::Nil),
    }
}

/// Declares or overwrites a slot; falls back to `globals` when not found.
pub fn set_slot(chain: &[ScopeRef], globals: &ScopeRef, name: &str, slot: SlotV) {
    for s in chain.iter().rev() {
        let mut b = s.borrow_mut();
        // Look up by `&str` first: allocating the key on every assignment is
        // pure overhead when the slot already exists (the common case).
        if let Some(existing) = b.vars.get_mut(name) {
            *existing = slot;
            return;
        }
    }
    let mut g = globals.borrow_mut();
    if let Some(existing) = g.vars.get_mut(name) {
        *existing = slot;
    } else {
        g.vars.insert(name.to_string(), slot);
    }
    g.version = g.version.wrapping_add(1);
}

/// Declares a name in the topmost scope (a fresh slot). If the name already
/// exists in frame scopes, *overwrite it there* (Lua `local` semantics).
pub fn declare(chain: &[ScopeRef], globals: &ScopeRef, name: &str, slot: SlotV) {
    match chain.last() {
        Some(s) => {
            s.borrow_mut().vars.insert(name.to_string(), slot);
        }
        None => {
            let mut g = globals.borrow_mut();
            g.vars.insert(name.to_string(), slot);
            g.version = g.version.wrapping_add(1);
        }
    }
}
