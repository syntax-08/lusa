//! Scopes — storage for variables.
//!
//! Every frame keeps a stack of block scopes (locals). Closures capture the
//! lexical chain of outer frames (`env`). Globals live in one long-lived
//! scope. Name resolution order: current frame scopes → closure env → globals.

use std::cell::RefCell;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::rc::Rc;

use crate::error::{LResult, LusaError, Span, E_MOVED};
use crate::interp::move_from_history;
use crate::value::{SlotV, Value};

pub struct ScopeData {
    pub vars: HashMap<String, SlotV>,
    /// reserved: distinguishes the (single) globals scope
    #[allow(dead_code)]
    pub is_global: bool,
}

pub type ScopeRef = Rc<RefCell<ScopeData>>;

pub fn new_scope(is_global: bool) -> ScopeRef {
    Rc::new(RefCell::new(ScopeData {
        vars: HashMap::new(),
        is_global,
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
        match b.vars.entry(name.to_string()) {
            Entry::Occupied(mut occ) => {
                occ.insert(slot);
                return;
            }
            Entry::Vacant(_) => {
                drop(b);
                continue;
            }
        }
    }
    globals.borrow_mut().vars.insert(name.to_string(), slot);
}

/// Declares a name in the topmost scope (a fresh slot). If the name already
/// exists in frame scopes, *overwrite it there* (Lua `local` semantics).
pub fn declare(chain: &[ScopeRef], globals: &ScopeRef, name: &str, slot: SlotV) {
    match chain.last() {
        Some(s) => {
            s.borrow_mut().vars.insert(name.to_string(), slot);
        }
        None => {
            globals.borrow_mut().vars.insert(name.to_string(), slot);
        }
    }
}
