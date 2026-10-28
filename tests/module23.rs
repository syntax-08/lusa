//! Module 23: namespaced modules (`mod name;` / `mod name { ... }`), path
//! imports (`use ... as ...`), visibility (`pub`), and re-exports.
//!
//! The resolver expands the module tree before parsing, so every test checks
//! the interpreter, bytecode VM, C11 emission, and (when a host compiler is
//! available) the native executable against the same source.

use std::path::{Path, PathBuf};
use std::process::Command;

use lusa::cbackend;
use lusa::error::E_MODULE;

fn project(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lusa-module23-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("project directory");
    dir
}

fn write(dir: &Path, relative: &str, text: &str) {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("module directory");
    }
    std::fs::write(&path, text).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

fn assert_dynamic(src: &str, base: Option<&Path>, expected: &str) {
    let interp = lusa::run_source_with_base(src, base).expect("interpreter module program");
    let vm = lusa::vm::run_source_vm_with_base(src, base).expect("VM module program");
    assert_eq!(interp.join("\n"), expected, "interpreter output");
    assert_eq!(vm.join("\n"), expected, "bytecode VM output");
}

fn native_output(src: &str, base: Option<&Path>, tag: &str) -> Option<String> {
    let compiler = match cbackend::find_compiler(None) {
        Ok(cc) => cc,
        Err(e) => {
            eprintln!("skipping native Module 23 test `{tag}`: {e}");
            return None;
        }
    };
    let out_dir = std::env::temp_dir().join("opencode");
    std::fs::create_dir_all(&out_dir).expect("temp dir");
    let exe = out_dir.join(format!(
        "lusa-module23-{tag}-{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ));
    let _ = std::fs::remove_file(&exe);
    cbackend::build_with_base(src, &exe, None, base).unwrap_or_else(|e| {
        panic!(
            "native build with {} failed:\n{e}",
            compiler.display_name()
        )
    });
    let output = Command::new(&exe).output().expect("run native program");
    let _ = std::fs::remove_file(&exe);
    assert!(output.status.success(), "native program failed: {output:?}");
    Some(
        String::from_utf8_lossy(&output.stdout)
            .replace("\r\n", "\n")
            .trim_end_matches('\n')
            .to_string(),
    )
}

fn assert_native(src: &str, base: Option<&Path>, tag: &str, expected: &str) {
    if let Some(stdout) = native_output(src, base, tag) {
        assert_eq!(stdout, expected, "native output for `{tag}`");
    }
}

fn assert_privacy_error(src: &str, base: Option<&Path>, needle: &str) {
    let err = lusa::run_source_with_base(src, base).expect_err("interpreter privacy error");
    assert_eq!(err.code, E_MODULE, "{}", err.msg);
    assert!(err.msg.contains(needle), "{}", err.msg);

    let vm = lusa::vm::run_source_vm_with_base(src, base).expect_err("VM privacy error");
    assert_eq!(vm.code, E_MODULE, "{}", vm.msg);
    assert!(vm.msg.contains(needle), "{}", vm.msg);

    let c = cbackend::emit_c_with_base(src, base).expect_err("C backend privacy error");
    assert!(c.contains("E0350"), "{c}");
    assert!(c.contains(needle), "{c}");
}

const INLINE_SOURCE: &str = r#"
mod math {
    pub local BASE = 10

    pub struct Pair {
        a: i32,
        b: i32
    }

    function secret(): i32
        return 7
    end

    pub function double(x: i32): i32
        return x * 2
    end

    pub function reveal(): i32
        return secret()
    end
}

local p: math::Pair = math::Pair{a: 3, b: 4}
print(math::double(21))
print(math::reveal())
print(p.a + p.b + math::BASE)
"#;

const INLINE_EXPECTED: &str = "42\n7\n17";

#[test]
fn inline_modules_namespace_items_and_keep_parent_access() {
    assert_dynamic(INLINE_SOURCE, None, INLINE_EXPECTED);
    let expanded = lusa::imports::expand_imports(INLINE_SOURCE, None).expect("module expansion");
    assert!(
        expanded.contains("function __lusa_m_math__double"),
        "{expanded}"
    );
    assert!(
        expanded.contains("function __lusa_m_math__secret"),
        "{expanded}"
    );
    let c = cbackend::emit_c(INLINE_SOURCE).expect("C emission");
    assert!(c.contains("__lusa_m_math__Pair"), "mangled struct missing");
    assert!(!c.contains("mod math"), "module header leaked into C");
    assert_native(INLINE_SOURCE, None, "inline", INLINE_EXPECTED);
}

const FILE_MAIN: &str = r#"
mod geometry;
mod util;

use geometry::Point;
use geometry::Point as LocalPoint;
use geometry::GeoPoint;
use geometry::shapes as sh;
use geometry::area2;
use util::triple;
use util::triple as times_three;

local p: Point = Point{x: 2, y: 3}
local q: GeoPoint = GeoPoint{x: 10, y: 20}
local lp: LocalPoint = LocalPoint{x: 8, y: 9}
local r = geometry::shifted(p)
local u = geometry::unit()
print(p.x + p.y)
print(q.x + q.y)
print(lp.x)
print(r.x, r.y)
print(u.x, u.y)
print(sh::circle_area(2))
print(sh::crate_point())
print(area2(3))
print(triple(7))
print(times_three(4))
print(geometry::VERSION)
print(geometry::visible())
"#;

const FILE_EXPECTED: &str = "5\n30\n8\n3 4\n0 0\n12\n5\n27\n21\n12\n9\n5";

fn write_file_project(dir: &Path) {
    write(
        dir,
        "geometry/mod.lusa",
        r#"
pub mod shapes;

pub use self::Point as GeoPoint;
pub use shapes::circle_area as area2;

pub local VERSION = 9

pub struct Point {
    x: i32,
    y: i32
}

function hidden(): i32
    return 5
end

pub function visible(): i32
    return hidden()
end

pub function shifted(p: Point): Point
    return Point{x: p.x + 1, y: p.y + 1}
end

pub function unit(): Point
    return self::Point{x: 0, y: 0}
end
"#,
    );
    write(
        dir,
        "geometry/shapes/mod.lusa",
        r#"
use super::Point;
use crate::geometry::Point as CratePoint;

pub function circle_area(r: i32): i32
    return 3 * r * r
end

pub function double_area(r: i32): i32
    local p = Point{x: r, y: r}
    return p.x * circle_area(r)
end

pub function crate_point(): i32
    local p: CratePoint = CratePoint{x: 5, y: 6}
    return p.x
end
"#,
    );
    write(
        dir,
        "util.lusa",
        r#"
pub function triple(x: i32): i32
    return x * 3
end
"#,
    );
}

#[test]
fn file_modules_support_use_aliases_and_public_reexports() {
    let dir = project("files");
    write_file_project(&dir);
    let main = dir.join("main.lusa");
    write(&dir, "main.lusa", FILE_MAIN);

    assert_dynamic(FILE_MAIN, Some(&dir), FILE_EXPECTED);

    let c = cbackend::emit_c_with_base(FILE_MAIN, Some(&dir)).expect("C module expansion");
    let expanded = lusa::imports::expand_imports(FILE_MAIN, Some(&dir)).expect("module expansion");
    assert!(
        expanded.contains("function __lusa_m_geometry_shapes__circle_area"),
        "{expanded}"
    );
    assert!(
        expanded.contains("function __lusa_m_geometry__visible"),
        "{expanded}"
    );
    assert!(c.contains("__lusa_m_geometry__Point"), "{c}");
    assert!(!c.contains("mod geometry"), "module header leaked into C");
    assert!(!c.contains("use geometry"), "use declaration leaked into C");

    if let Some(stdout) = native_output(FILE_MAIN, Some(&dir), "files") {
        assert_eq!(stdout, FILE_EXPECTED);
    }

    // `build_file` must derive the module root from the entry file.
    let compiler = cbackend::find_compiler(None);
    if let Ok(cc) = compiler {
        let exe = std::env::temp_dir()
            .join("opencode")
            .join(format!(
                "lusa-module23-file-entry-{}{}",
                std::process::id(),
                std::env::consts::EXE_SUFFIX
            ));
        let _ = std::fs::remove_file(&exe);
        cbackend::build_file(&main, &exe, None)
            .unwrap_or_else(|e| panic!("native build_file with {} failed:\n{e}", cc.display_name()));
        let output = Command::new(&exe).output().expect("run native entry");
        let _ = std::fs::remove_file(&exe);
        assert!(output.status.success(), "native entry failed: {output:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n"),
            format!("{FILE_EXPECTED}\n")
        );
    }

    let missing = lusa::run_source_with_base("mod nope;\nreturn 0\n", Some(&dir))
        .expect_err("missing module must fail");
    assert_eq!(missing.code, "E0101");
    assert!(missing.msg.contains("cannot find module `nope`"), "{}", missing.msg);

    let _ = std::fs::remove_dir_all(&dir);
}

fn write_privacy_project(dir: &Path) {
    write(
        dir,
        "core.lusa",
        r#"
mod internal {
    pub function value(): i32
        return 4
    end
}

pub mod internal2 {
    pub function value(): i32
        return 4
    end
}

function hidden(): i32
    return 3
end

pub function shown(): i32
    return hidden()
end

pub function call_internal(): i32
    return internal::value()
end
"#,
    );
    write(
        dir,
        "sneaky.lusa",
        r#"
pub function steal_public(): i32
    return crate::core::shown()
end
"#,
    );
    write(
        dir,
        "sneaky_secret.lusa",
        r#"
pub function steal_secret(): i32
    return crate::core::hidden()
end
"#,
    );
    write(
        dir,
        "leaky.lusa",
        r#"
function hidden(): i32
    return 3
end

pub use self::hidden as leaked;
"#,
    );
}

#[test]
fn private_items_and_modules_reject_outside_access() {
    let dir = project("privacy");
    write_privacy_project(&dir);

    assert_privacy_error("mod core;\nprint(core::hidden())\n", Some(&dir), "hidden");
    assert_privacy_error(
        "mod core;\nuse core::hidden;\nprint(hidden())\n",
        Some(&dir),
        "hidden",
    );
    assert_privacy_error(
        "mod core;\nprint(core::internal::value())\n",
        Some(&dir),
        "internal",
    );
    assert_privacy_error(
        "mod core;\nmod sneaky_secret;\nprint(sneaky_secret::steal_secret())\n",
        Some(&dir),
        "hidden",
    );
    assert_privacy_error(
        "mod leaky;\nprint(leaky::leaked())\n",
        Some(&dir),
        "re-exported",
    );

    // A private child is still visible to its own module and descendants, and
    // `pub` items/modules remain accessible from outside.
    assert_dynamic(
        "mod core;\nprint(core::shown())\nprint(core::internal2::value())\nprint(core::call_internal())\n",
        Some(&dir),
        "3\n4\n4",
    );
    assert_dynamic(
        "mod core;\nmod sneaky;\nprint(sneaky::steal_public())\n",
        Some(&dir),
        "3",
    );
    assert_native(
        "mod core;\nmod sneaky;\nprint(sneaky::steal_public())\n",
        Some(&dir),
        "privacy-ok",
        "3",
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn module_directory_takes_precedence_and_children_resolve_inside_it() {
    let dir = project("resolution");
    write(
        &dir,
        "dual.lusa",
        "pub function which(): i32\n    return 1\nend\n",
    );
    write(
        &dir,
        "dual/mod.lusa",
        "pub function which(): i32\n    return 2\nend\n",
    );
    write(
        &dir,
        "geometry.lusa",
        r#"
pub mod helper;

pub function value(): i32
    return helper::value()
end
"#,
    );
    write(
        &dir,
        "geometry/helper/mod.lusa",
        "pub function value(): i32\n    return 9\nend\n",
    );
    let src = "mod dual;\nmod geometry;\nprint(dual::which())\nprint(geometry::value())\n";
    write(&dir, "main.lusa", src);

    assert_dynamic(src, Some(&dir), "2\n9");
    assert_native(src, Some(&dir), "resolution", "2\n9");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn nested_inline_modules_support_self_and_crate_paths() {
    let src = r#"
mod a {
    pub function value(): i32
        return 3
    end
}

mod outer {
    pub mod inner {
        pub function f(): i32
            return 10
        end
    }

    pub function g(): i32
        return self::inner::f() + 1
    end
}

mod b {
    pub function via_crate(): i32
        return crate::a::value()
    end
}

print(outer::inner::f())
print(outer::g())
print(b::via_crate())
"#;
    assert_dynamic(src, None, "10\n11\n3");
    assert_native(src, None, "nested-paths", "10\n11\n3");
}

#[test]
fn method_sugar_targets_module_items() {
    let src = r#"
mod counter {
    pub local state = {n = 0}

    pub function state:add(v: i32): i32
        self.n = self.n + v
        return self.n
    end
}

print(counter::state:add(5))
print(counter::state:add(2))
"#;
    assert_dynamic(src, None, "5\n7");
    assert_native(src, None, "method-sugar", "5\n7");
}

#[test]
fn shipped_example_runs_on_all_engines() {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/modules_project");
    let src = std::fs::read_to_string(dir.join("main.lusa")).expect("example entry point");
    let expected = "150\n40\n32 true\ntrue\n101 true\n1 32 34\n322";
    assert_dynamic(&src, Some(&dir), expected);
    assert_native(&src, Some(&dir), "example", expected);
}

#[test]
fn module_file_cycles_are_reported() {
    let dir = project("cycle");
    write(&dir, "a.lusa", "mod b;\n\npub function a_value(): i32\n    return 1\nend\n");
    write(&dir, "b.lusa", "mod a;\n");
    let src = "mod a;\nprint(a::a_value())\n";

    let err = lusa::run_source_with_base(src, Some(&dir)).expect_err("interpreter cycle");
    assert_eq!(err.code, "E0101", "{}", err.msg);
    assert!(err.msg.contains("cycle"), "{}", err.msg);

    let vm = lusa::vm::run_source_vm_with_base(src, Some(&dir)).expect_err("VM cycle");
    assert_eq!(vm.code, "E0101", "{}", vm.msg);
    assert!(vm.msg.contains("cycle"), "{}", vm.msg);

    let c = cbackend::emit_c_with_base(src, Some(&dir)).expect_err("C backend cycle");
    assert!(c.contains("cycle"), "{c}");

    let _ = std::fs::remove_dir_all(&dir);
}
