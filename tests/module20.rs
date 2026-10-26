//! Module 20: heap allocation, `Vec<T>`, `Box<T>`, and the
//! `#[global_allocator]` contract.

use std::process::Command;

use lusa::cbackend;

fn emit(source: &str) -> String {
    cbackend::emit_c(source).unwrap_or_else(|e| panic!("C backend failed:\n{e}"))
}

fn emit_err(source: &str) -> String {
    cbackend::emit_c(source).expect_err("C backend must reject the program")
}

fn both_engines(src: &str) -> (String, String) {
    let interp = lusa::run_source(src).expect("interpreter failed");
    let vm = lusa::vm::run_source_vm(src).expect("VM failed");
    (interp.join("\n"), vm.join("\n"))
}

/// Builds and runs a native program.  Returns `None` when the host has no
/// detectable C compiler so local developer runs still exercise emission and
/// the dynamic engines; CI always has one.
fn native_output(source: &str, tag: &str) -> Option<String> {
    let compiler = match cbackend::find_compiler(None) {
        Ok(cc) => cc,
        Err(e) => {
            eprintln!("skipping native Module 20 test `{tag}`: {e}");
            return None;
        }
    };
    let out_dir = std::env::temp_dir().join("opencode");
    std::fs::create_dir_all(&out_dir).expect("temp dir");
    let exe = out_dir.join(format!("lusa-module20-{tag}-{}.exe", std::process::id()));
    let _ = std::fs::remove_file(&exe);
    cbackend::build(source, &exe, None).unwrap_or_else(|e| {
        panic!(
            "native build with {} failed:\n{e}",
            compiler.display_name()
        )
    });

    let run = Command::new(&exe).output().expect("run native program");
    let _ = std::fs::remove_file(&exe);
    assert!(run.status.success(), "native program failed: {run:?}");
    let stdout = String::from_utf8_lossy(&run.stdout).replace("\r\n", "\n");
    Some(stdout.trim_end_matches('\n').to_string())
}

fn assert_native(source: &str, tag: &str, expected: &str) {
    if let Some(stdout) = native_output(source, tag) {
        assert_eq!(stdout, expected, "native output for `{tag}`");
    }
}

const COLLECTIONS: &str = r#"
local v: Vec<i32> = []
v:push(10)
v:push(20)
v:push(30)
print(v:len(), v:capacity(), v:is_empty(), #v)
print(v[1])
v[1] = 22
print(v[0], v[1], v:pop(), v:len())
v:reserve(16)
print(v:capacity())
v:clear()
print(v:len(), v:capacity(), v:is_empty())

local b: Box<i32> = 5
print(*b)
*b = 8
print(*b)
local c = Box(9)
print(*c)
"#;

const COLLECTIONS_OUTPUT: &str = "3 4 false 3\n20\n10 22 30 2\n16\n0 16 true\n5\n8\n9";

#[test]
fn vec_growth_and_box_deref_run_in_all_engines() {
    assert_native(COLLECTIONS, "collections", COLLECTIONS_OUTPUT);

    let (interp, vm) = both_engines(COLLECTIONS);
    assert_eq!(interp, COLLECTIONS_OUTPUT);
    assert_eq!(vm, interp);
}

const OWNED_ELEMENTS: &str = r#"
struct Bag
    items: Vec<i32>,
    label: Box<string>
end

local bag: Bag
bag.items:push(7)
bag.items:push(8)
bag.label = Box("hi")
print(bag.items[0], bag.items[1], *bag.label, bag.items:len())

local words: Vec<string> = ["alpha", "beta"]
words:push("gamma")
print(words:len(), words[0], words[2])
local popped = words:pop()
print(popped, words:len())
words:clear()
print(words:len(), words:is_empty())

local name: Box<string> = Box("lusa")
print(*name)
*name = "heap"
print(*name)
"#;

const OWNED_ELEMENTS_OUTPUT: &str =
    "7 8 hi 2\n3 alpha gamma\ngamma 2\n0 true\nlusa\nheap";

#[test]
fn vec_strings_and_boxed_values_match_in_every_engine() {
    assert_native(OWNED_ELEMENTS, "owned", OWNED_ELEMENTS_OUTPUT);

    let (interp, vm) = both_engines(OWNED_ELEMENTS);
    assert_eq!(interp, OWNED_ELEMENTS_OUTPUT);
    assert_eq!(vm, interp);
}

const CUSTOM_ALLOCATOR: &str = r#"
alloc_calls = 0
free_calls = 0

#[global_allocator]
function arena_alloc(size: u32, align: u32): *mut u32
    alloc_calls = alloc_calls + 1
    unsafe do
        return unsafe_alloc(size)
    end
end

#[global_allocator]
function arena_dealloc(ptr: *mut u32, size: u32, align: u32): void
    free_calls = free_calls + 1
end

do
    local v: Vec<i32> = []
    v:push(1)
    v:push(2)
    local b: Box<i32> = 3
    print(v[0], v[1], *b)
end

print(alloc_calls > 0, free_calls > 0)
"#;

#[test]
fn global_allocator_dispatches_heap_allocation_and_raii_drops() {
    let c = emit(CUSTOM_ALLOCATOR);
    assert!(
        c.contains("static void *lusa_heap_alloc(uint32_t size, uint32_t align)"),
        "{c}"
    );
    assert!(
        c.contains("static void lusa_heap_dealloc(void *ptr, uint32_t size, uint32_t align)"),
        "{c}"
    );
    assert!(
        !c.contains("LusaHeapArena"),
        "a user allocator replaces the freestanding arena:\n{c}"
    );
    assert!(c.contains("lusa_vec_push_"), "{c}");
    assert!(c.contains("lusa_box_new_"), "{c}");

    assert_native(CUSTOM_ALLOCATOR, "allocator", "1 2 3\ntrue true");
}

const NO_STD_HEAP: &str = r#"
#[no_std]

#[link_section = ".data.heap"]
local values: Vec<i32> = []

#[entry]
function _start() : void
    values:push(1)
    values:push(2)
    values:push(3)
end
"#;

#[test]
fn freestanding_targets_use_the_static_bump_arena() {
    let c = emit(NO_STD_HEAP);
    assert!(c.starts_with("/* lusa:no_std */"), "{c}");
    assert!(c.contains("typedef union LusaHeapArena {"), "{c}");
    assert!(c.contains("static uint32_t lusa_heap_top = 0u;"), "{c}");
    assert!(c.contains("lusa_vec_push_"), "{c}");
    assert!(c.contains("void _start(void) {"), "{c}");
    for banned in [
        "#include <stdlib.h>",
        "malloc(",
        "free(",
        "int main(void)",
    ] {
        assert!(
            !c.contains(banned),
            "freestanding output must not contain {banned:?}:\n{c}"
        );
    }
}

#[test]
fn global_allocator_contract_is_validated() {
    let cases = [
        (
            "#[global_allocator]\nfunction bad(size: u32): void\nend\n",
            "`#[global_allocator]` requires either",
        ),
        (
            "#[global_allocator]\nfunction a(size: u32, align: u32): *mut u32\n\
             unsafe do\nreturn unsafe_alloc(size)\nend\nend\n\
             #[global_allocator]\nfunction b(size: u32, align: u32): *mut u32\n\
             unsafe do\nreturn unsafe_alloc(size)\nend\nend\n",
            "duplicate global allocator",
        ),
        (
            "#[global_allocator]\n#[entry]\nfunction start(): void\nend\n",
            "`#[entry]` and `#[global_allocator]` cannot annotate the same function",
        ),
    ];
    for (source, needle) in cases {
        let err = emit_err(source);
        assert!(err.contains("E0325"), "expected E0325 in:\n{err}");
        assert!(err.contains(needle), "expected {needle:?} in:\n{err}");
    }
}

#[test]
fn vec_parameters_and_returns_are_rejected_with_a_pointer_hint() {
    for source in [
        "function sink(values: Vec<i32>): void\nend\n",
        "function source(): Vec<i32>\nend\n",
    ] {
        let err = emit_err(source);
        assert!(err.contains("E0312"), "expected E0312 in:\n{err}");
        assert!(
            err.contains("raw pointer") || err.contains("passed by value"),
            "{err}"
        );
    }
}

#[test]
fn collection_helpers_are_specialized_per_element_type() {
    let c = emit(
        "local ints: Vec<i32> = []\n\
         ints:push(1)\n\
         local names: Vec<string> = []\n\
         names:push(\"a\")\n\
         local boxed = Box(3)\n\
         print(ints[0], names[0], *boxed)\n",
    );
    assert!(c.contains("typedef struct LusaVec {"), "{c}");
    assert!(c.contains("lusa_vec_push_i32"), "{c}");
    assert!(c.contains("lusa_vec_push_string"), "{c}");
    assert!(c.contains("lusa_drop_vec_string"), "{c}");
    assert!(c.contains("lusa_box_new_i32"), "{c}");
    assert!(c.contains("lusa_drop_box_i32"), "{c}");
    assert!(c.contains("lusa_heap_alloc("), "{c}");
    assert!(c.contains("lusa_heap_dealloc("), "{c}");
}
