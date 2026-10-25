//! Module 17: volatile MMIO access, packed structs, and explicit alignment.

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

/// Shared program: simulated MMIO through a polling loop, then results stored
/// into a packed struct and an over-aligned struct.  It runs unchanged in the
/// interpreter, the VM, and the native backend when allocated with
/// `unsafe_alloc` instead of a physical address.
const MMIO_PROGRAM: &str = r#"
#[packed]
struct Packet {
    kind: u32,
    payload: u32,
}

#[repr(align(16))]
struct Aligned {
    value: u32,
}

local packet: Packet
local aligned: Aligned

unsafe do
    local regs: *mut u32 = unsafe_alloc(16)
    regs.write_volatile(0)
    local spins: u32 = 0
    while spins ~= 3 do
        spins = spins + 1
        regs:write_volatile(spins)
    end
    local seen: u32 = regs:read_volatile()
    local p: *mut u32 = unsafe_alloc(64)
    volatile_write(p, 42)
    local q: u32 = volatile_read(p)
    packet.kind = 7
    packet.payload = seen
    aligned.value = q
end

print(packet.kind, packet.payload, aligned.value)
return 0
"#;

#[test]
fn volatile_methods_emit_typed_volatile_access() {
    let c = emit(
        "local regs: *mut u32 = 0x40000000\n\
         unsafe do\n\
             regs.write_volatile(1)\n\
             local a: u32 = regs.read_volatile()\n\
             volatile_write(regs, 2)\n\
             local b: u32 = volatile_read(regs)\n\
             regs:write_volatile(3)\n\
             local c: u32 = regs:read_volatile()\n\
             print(a, b, c)\n\
         end\n\
         return 0\n",
    );
    // Dot, colon, and free-function spellings all lower to the same volatile
    // lvalue: three stores and three loads, with no residual method names.
    assert_eq!(
        c.matches("*(volatile uint32_t *)(uintptr_t)(").count(),
        6,
        "volatile accesses:\n{c}"
    );
    assert!(c.contains("= (uint32_t)("), "typed volatile store:\n{c}");
    assert!(!c.contains("read_volatile"), "method name leaked:\n{c}");
    assert!(!c.contains("write_volatile"), "method name leaked:\n{c}");
}

#[test]
fn volatile_access_uses_the_pointer_element_type() {
    let c = emit(
        "local a: *mut i32 = 0x1000\n\
         local b: *mut f32 = 0x2000\n\
         local c: *mut bool = 0x3000\n\
         local d: *mut i128 = 0x4000\n\
         unsafe do\n\
             local x: i32 = a.read_volatile()\n\
             local y: f32 = b.read_volatile()\n\
             local z: bool = c.read_volatile()\n\
             local w: i128 = d.read_volatile()\n\
             print(x, y, z, w)\n\
         end\n\
         return 0\n",
    );
    for ty in ["int32_t", "float", "bool", "lusa_i128"] {
        let expected = format!("*(volatile {ty} *)(uintptr_t)(");
        assert!(c.contains(&expected), "expected {ty} access:\n{c}");
    }
}

#[test]
fn volatile_access_requires_unsafe_in_every_engine() {
    let read = "local regs: *mut u32 = 0x1000\n\
                local v: u32 = regs.read_volatile()\n\
                print(v)\n\
                return 0\n";
    let err = lusa::run_source(read).expect_err("interpreter must gate reads");
    assert_eq!(err.code, "E0321");
    let err = lusa::vm::run_source_vm(read).expect_err("VM must gate reads");
    assert_eq!(err.code, "E0321");
    let err = emit_err(read);
    assert!(err.contains("E0316"), "native unsafe gate:\n{err}");
    assert!(err.contains("volatile_read"), "native intrinsic name:\n{err}");

    let write = "local regs: *mut u32 = 0x1000\n\
                 regs.write_volatile(5)\n\
                 return 0\n";
    let err = lusa::run_source(write).expect_err("interpreter must gate writes");
    assert_eq!(err.code, "E0321");
    let err = lusa::vm::run_source_vm(write).expect_err("VM must gate writes");
    assert_eq!(err.code, "E0321");
    let err = emit_err(write);
    assert!(err.contains("E0316"), "native unsafe gate:\n{err}");
    assert!(err.contains("volatile_write"), "native intrinsic name:\n{err}");
}

#[test]
fn volatile_access_rejects_heap_elements_and_wrong_values() {
    let heap = "local p: *mut string = 0x1000\n\
                unsafe do\n\
                    local s: string = p.read_volatile()\n\
                    print(s)\n\
                end\n\
                return 0\n";
    let err = emit_err(heap);
    assert!(err.contains("E0312"), "scalar-only check:\n{err}");
    assert!(err.contains("scalar"), "scalar-only wording:\n{err}");

    let wrong = "local p: *mut u32 = 0x1000\n\
                 unsafe do\n\
                     p.write_volatile(\"no\")\n\
                 end\n\
                 return 0\n";
    let err = emit_err(wrong);
    assert!(err.contains("E0308"), "value type check:\n{err}");
    assert!(err.contains("volatile value"), "value diagnostic:\n{err}");

    let arity = "unsafe do\n\
                     local v: u32 = volatile_read()\n\
                     print(v)\n\
                 end\n\
                 return 0\n";
    let err = emit_err(arity);
    assert!(err.contains("E0311"), "arity check:\n{err}");
}

#[test]
fn packed_and_aligned_structs_emit_portability_macros() {
    let c = emit(
        "struct Plain { a: u32, b: u32 }\n\
         #[packed]\n\
         struct Packet { tag: u32, value: u32 }\n\
         #[repr(align(16))]\n\
         struct Aligned { value: u32 }\n\
         local plain: Plain\n\
         local packet: Packet\n\
         local aligned: Aligned\n\
         plain.a = 1\n\
         packet.tag = 2\n\
         aligned.value = 3\n\
         print(plain.a, packet.tag, aligned.value)\n\
         return 0\n",
    );
    // Runtime macros cover MSVC, GCC/Clang, and the C11 fallback.
    assert!(c.contains("#define LUSA_PACK_BEGIN"), "pack begin macro:\n{c}");
    assert!(c.contains("#define LUSA_PACK_END"), "pack end macro:\n{c}");
    assert!(c.contains("#define LUSA_PACKED"), "packed macro:\n{c}");
    assert!(c.contains("#define LUSA_ALIGNAS(n)"), "align macro:\n{c}");
    // A plain struct is unchanged; `#[packed]` wraps its typedef.
    assert!(
        c.contains("typedef struct {\n    uint32_t a;\n    uint32_t b;\n} Plain;"),
        "plain layout:\n{c}"
    );
    assert!(
        c.contains(
            "LUSA_PACK_BEGIN\ntypedef struct LUSA_PACKED {\n    uint32_t tag;\n    uint32_t value;\n} Packet;\nLUSA_PACK_END"
        ),
        "packed layout:\n{c}"
    );
    // Member alignment raises the enclosing type without an ABI attribute.
    assert!(
        c.contains("typedef struct {\n    LUSA_ALIGNAS(16) uint32_t value;\n} Aligned;"),
        "aligned layout:\n{c}"
    );
}

#[test]
fn representation_attributes_are_validated_before_codegen() {
    let cases = [
        (
            "#[packed]\n#[packed]\nstruct Bad { a: u32 }\nreturn 0\n",
            "duplicate `#[packed]` attribute",
        ),
        (
            "#[repr(align(3))]\nstruct Bad { a: u32 }\nreturn 0\n",
            "alignment 3 is not a power of two",
        ),
        (
            "#[repr(align(0))]\nstruct Bad { a: u32 }\nreturn 0\n",
            "alignment 0 must be between 1 and 4096",
        ),
        (
            "#[repr(align(8192))]\nstruct Bad { a: u32 }\nreturn 0\n",
            "alignment 8192 must be between 1 and 4096",
        ),
        (
            "#[repr(C)]\nstruct Bad { a: u32 }\nreturn 0\n",
            "unsupported representation attribute",
        ),
        (
            "#[packed]\nlocal x: u32 = 1\nreturn 0\n",
            "attributes are only supported on `struct` declarations",
        ),
    ];
    for (source, needle) in cases {
        let err = emit_err(source);
        assert!(err.contains(needle), "expected {needle:?} in:\n{err}");
    }
    let err = lusa::run_source("#[repr(align(3))]\nstruct Bad { a: u32 }\nreturn 0\n")
        .expect_err("dynamic parser must validate alignment");
    assert_eq!(err.code, "E0101");
}

#[test]
fn dynamic_engines_run_volatile_packed_and_aligned_programs() {
    let (interp, vm) = both_engines(MMIO_PROGRAM);
    assert_eq!(interp, "7 3 42");
    assert_eq!(vm, interp);
}

#[test]
fn native_volatile_packed_and_aligned_program_runs() {
    let compiler = match cbackend::find_compiler(None) {
        Ok(cc) => cc,
        Err(e) => {
            eprintln!("skipping native Module 17 test: {e}");
            return;
        }
    };
    let out_dir = std::env::temp_dir().join("opencode");
    std::fs::create_dir_all(&out_dir).expect("temp dir");
    let exe = out_dir.join(format!("lusa-module17-{}.exe", std::process::id()));
    let _ = std::fs::remove_file(&exe);
    cbackend::build(MMIO_PROGRAM, &exe, None).unwrap_or_else(|e| {
        panic!(
            "native build with {} failed:\n{e}",
            compiler.display_name()
        )
    });
    let run = Command::new(&exe).output().expect("run native program");
    let _ = std::fs::remove_file(&exe);
    assert!(run.status.success(), "native program failed: {run:?}");
    let stdout = String::from_utf8_lossy(&run.stdout).replace("\r\n", "\n");
    assert_eq!(stdout, "7 3 42\n");
}
