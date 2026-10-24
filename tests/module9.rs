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
fn typed_local_annotations_are_preserved_in_the_ast() {
    let block = lusa::parser::parse(
        "local data: u32 = 255\n\
         local delta: i32 = -7\n\
         local ratio: f32 = 1.5\n",
    )
    .expect("parse");

    let expected = ["u32", "i32", "f32"];
    for (st, want) in block.iter().zip(expected) {
        match &st.kind {
            StKind::Local { annotations, .. } => {
                assert_eq!(annotations, &[Some(want.to_string())]);
            }
            other => panic!("expected Local, got {other:?}"),
        }
    }

    let out = lusa::run_source("local data: u32 = 255\nprint(data)\n").expect("run");
    assert_eq!(out, ["255"]);
}

#[test]
fn simulated_memory_cells_keep_fractional_values() {
    let src = r#"
unsafe do
    unsafe_write_mem(0x3000, 2.5)
    unsafe_write_mem(0x3004, -7.25)
    print(unsafe_read_mem(0x3000), unsafe_read_mem(0x3004), unsafe_read_mem(0x3008))
end
"#;
    let (interp, vm) = both_engines(src);
    assert_eq!(interp, "2.5 -7.25 0");
    assert_eq!(vm, interp);
}

#[test]
fn memory_intrinsics_require_an_active_unsafe_block() {
    for src in [
        "unsafe_read_mem(0x4000)\n",
        "unsafe_write_mem(0x4000, 1)\n",
        "unsafe do\n    unsafe_write_mem(0x4000, 1)\nend\nprint(unsafe_read_mem(0x4000))\n",
    ] {
        let interp =
            lusa::run_source(src).expect_err("interpreter must reject unsafe memory access");
        assert_eq!(interp.code, "E0321");
        assert!(
            interp.msg.contains("requires an `unsafe` block"),
            "interpreter diagnostic: {}",
            interp.msg
        );

        let vm = lusa::vm::run_source_vm(src).expect_err("VM must reject unsafe memory access");
        assert_eq!(vm.code, "E0321");
        assert!(
            vm.msg.contains("requires an `unsafe` block"),
            "VM diagnostic: {}",
            vm.msg
        );
    }
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
