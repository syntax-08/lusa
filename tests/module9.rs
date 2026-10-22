//! Module 9: typed surface parity and exact integer literal tokens.

use lusa::ast::{ExprKind, StKind};

fn both_engines(src: &str) -> (String, String) {
    let interp = lusa::run_source(src).expect("interpreter failed");
    let vm = lusa::vm::run_source_vm(src).expect("VM failed");
    (interp.join("\n"), vm.join("\n"))
}

#[test]
fn typed_locals_and_functions_run_in_both_dynamic_engines() {
    let src = r#"
local n: u32 = 41
local ratio: f32 = 2.5
local grid: [[i32]] = {{1, 2}, {3, 4}}

function add(x: i32): i32
    return x + 1
end

function label(v: u32): [string]
    local names: [string] = {"zero", "one"}
    return names
end

print(add(n), ratio, grid[2][1], label(1)[2])
"#;
    let (interp, vm) = both_engines(src);
    assert_eq!(interp, "42 2.5 3 one");
    assert_eq!(vm, interp);
}

#[test]
fn unsafe_blocks_use_safe_simulated_memory_in_both_engines() {
    let src = r#"
unsafe do
    local reg: i128 = unsafe_ptr_cast(0x40000000)
    unsafe_write_mem(reg, 7)
    local value: u32 = unsafe_read_mem(reg)
    print("register:", value)
end
"#;
    let (interp, vm) = both_engines(src);
    assert_eq!(interp, "register: 7");
    assert_eq!(vm, interp);
}

#[test]
fn large_integer_tokens_keep_their_exact_spelling() {
    let src = "local big: i128 = 123456789012345678901234567890\n\
               local hex: i128 = 0x1FFFFFFFFFFFFFFFFFFFFFFFF\n";
    let block = lusa::parser::parse(src).expect("parse");
    assert_eq!(block.len(), 2);
    let first = match &block[0].kind {
        StKind::Local { values, .. } => values,
        other => panic!("expected Local, got {other:?}"),
    };
    match &first[0].kind {
        ExprKind::Int(text) => assert_eq!(text, "123456789012345678901234567890"),
        other => panic!("expected exact Int, got {other:?}"),
    }
    let second = match &block[1].kind {
        StKind::Local { values, .. } => values,
        other => panic!("expected Local, got {other:?}"),
    };
    match &second[0].kind {
        ExprKind::Int(text) => assert_eq!(text, "0x1FFFFFFFFFFFFFFFFFFFFFFFF"),
        other => panic!("expected exact Int, got {other:?}"),
    }
}
