//! Module 10: native keyed tables, methods, and generic iterators.

use std::process::Command;

use lusa::cbackend;

fn emit(source: &str) -> String {
    cbackend::emit_c(source).unwrap_or_else(|e| panic!("C backend failed:\n{e}"))
}

#[test]
fn keyed_tables_emit_hash_map_and_typed_field_access() {
    let c = emit(
        "local user = { id = 7, name = \"Lusa\" }\n\
         print(user.id, user.name)\n\
         user.name = \"New\"\n\
         print(user.name)\n\
         return 0\n",
    );
    assert!(c.contains("lusa_table_set_string("));
    assert!(c.contains("lusa_table_get_i32_key("));
    assert!(c.contains("lusa_table_get_string("));
    assert!(c.contains("lusa_table_map_slot("));
}

#[test]
fn methods_and_generic_iterators_emit_native_forms() {
    let c = emit(
        "local user = { name = \"Lusa\" }\n\
         function user:greet(prefix: string): string\n\
             return prefix .. self.name\n\
         end\n\
         print(user:greet(\"hi \"))\n\
         local n = 0\n\
         for k, v in pairs(user) do n = n + 1 end\n\
         local sum = 0\n\
         for i, value in ipairs({10, 20, 30}) do sum = sum + value end\n\
         print(n, sum)\n\
         return 0\n",
    );
    assert!(c.contains("static LusaString * f"));
    assert!(c.contains("lusa_table_next_pair("));
    assert!(c.contains("lusa_table_get_i32_at("));
    assert!(c.contains("->obj.length"));
}

#[test]
fn native_keyed_methods_and_iterators_run() {
    let compiler = match cbackend::find_compiler(None) {
        Ok(cc) => cc,
        Err(e) => {
            eprintln!("skipping native Module 10 test: {e}");
            return;
        }
    };
    let out_dir = std::env::temp_dir().join("opencode");
    std::fs::create_dir_all(&out_dir).expect("temp dir");
    let exe = out_dir.join(format!("lusa-module10-{}.exe", std::process::id()));
    let _ = std::fs::remove_file(&exe);
    let source = r#"
local user = { id = 1, name = "Lusa", profile = { city = "Oslo" } }
user.name = "New"
user.profile.city = "Bergen"

function user:greet(prefix: string): string
    return prefix .. self.name
end

print(user.id, user.name, user.profile.city, user:greet("hi "))

local settings = { width = 800, height = 600 }
local fields = 0
for key, value in pairs(settings) do
    fields = fields + 1
end

local total = 0
for index, value in ipairs({10, 20, 30}) do
    total = total + value
end

print("fields", fields, "total", total)
return 0
"#;
    cbackend::build(source, &exe, None)
        .unwrap_or_else(|e| panic!("native build with {} failed:\n{e}", compiler.display_name()));
    let run = Command::new(&exe).output().expect("run native program");
    let _ = std::fs::remove_file(&exe);
    assert!(run.status.success(), "native program failed: {run:?}");
    let stdout = String::from_utf8_lossy(&run.stdout).replace("\r\n", "\n");
    assert_eq!(stdout, "1 New Bergen hi New\nfields 2 total 60\n");
}

#[test]
fn keyed_values_are_copied_when_they_escape_their_owner() {
    let c = emit(
        "local user = { name = \"Lusa\" }\n\
         function user:get(): string\n\
             return self.name\n\
         end\n\
         local name = user.name\n\
         name = \"New\"\n\
         local copy = clone user\n\
         copy.name = \"Copy\"\n\
         local names = { user.name, user.name }\n\
         local first = names[1]\n\
         print(user.name, copy.name, name, first, user:get())\n\
         return 0\n",
    );
    // Field/array reads are views: they are copied when they become owned
    // locals, escape through a return, or are stored in a table.
    assert!(c.contains("lusa_string_clone(lusa_table_get_string("));
    assert!(c.contains("lusa_string_clone(lusa_table_get_string_at("));
    // `clone` is a deep copy for the keyed representation too.
    assert!(c.contains("lusa_table_clone("));
}

#[test]
fn schema_survives_clone_and_parentheses() {
    let c = emit(
        "local user = { profile = { city = \"Oslo\" } }\n\
         local copy = clone user\n\
         local view = borrow copy.profile\n\
         print(view.city)\n\
         return 0\n",
    );
    assert!(c.contains("lusa_table_get_string("));
    assert!(c.contains("lusa_table_get_table_key("));
}

#[test]
fn immutable_borrows_reject_field_writes() {
    let err = cbackend::emit_c(
        "local user = { name = \"Lusa\" }\n\
         local view = borrow user\n\
         view.name = \"New\"\n\
         return 0\n",
    )
    .expect_err("writing through a borrow must fail");
    assert!(err.contains("error[E1004]"), "diagnostic: {err}");
    assert!(err.contains("immutable borrow `view`"), "diagnostic: {err}");
}

#[test]
fn top_level_locals_do_not_leak_into_functions() {
    let err = cbackend::emit_c(
        "local hidden = { value = 1 }\n\
         function read(): i32\n\
             return hidden.value\n\
         end\n\
         return 0\n",
    )
    .expect_err("functions must not capture top-level locals");
    assert!(err.contains("error[E0309]"), "diagnostic: {err}");
    assert!(err.contains("hidden"), "diagnostic: {err}");
}

#[test]
fn native_hash_map_grows_and_resolves_collisions() {
    let compiler = match cbackend::find_compiler(None) {
        Ok(cc) => cc,
        Err(e) => {
            eprintln!("skipping native hash-map test: {e}");
            return;
        }
    };
    let out_dir = std::env::temp_dir().join("opencode");
    std::fs::create_dir_all(&out_dir).expect("temp dir");
    let exe = out_dir.join(format!("lusa-module10-map-{}.exe", std::process::id()));
    let _ = std::fs::remove_file(&exe);

    let mut source = String::from("local values = {\n");
    for i in 0..64 {
        source.push_str(&format!("    field{i} = {i},\n"));
    }
    source.push_str("}\nlocal total = 0\n");
    for i in 0..64 {
        source.push_str(&format!("total = total + values.field{i}\n"));
    }
    source.push_str("print(total, values[\"field63\"])\nreturn 0\n");

    cbackend::build(&source, &exe, None)
        .unwrap_or_else(|e| panic!("native build with {} failed:\n{e}", compiler.display_name()));
    let run = Command::new(&exe).output().expect("run native program");
    let _ = std::fs::remove_file(&exe);
    assert!(run.status.success(), "native program failed: {run:?}");
    let stdout = String::from_utf8_lossy(&run.stdout).replace("\r\n", "\n");
    assert_eq!(stdout, "2016 63\n");
}

#[test]
fn native_method_mutation_clone_and_escape_run_correctly() {
    let compiler = match cbackend::find_compiler(None) {
        Ok(cc) => cc,
        Err(e) => {
            eprintln!("skipping native Module 10 ownership test: {e}");
            return;
        }
    };
    let out_dir = std::env::temp_dir().join("opencode");
    std::fs::create_dir_all(&out_dir).expect("temp dir");
    let exe = out_dir.join(format!("lusa-module10-own-{}.exe", std::process::id()));
    let _ = std::fs::remove_file(&exe);
    let source = r#"
local user = { name = "Lusa" }

function user:rename(value: string): void
    self.name = value
end

function user:get(): string
    return self.name
end

function user:label(): string
    return "[" .. self:get() .. "]"
end

user:rename("Renamed")
local name = user.name
name = "Local"
local copy = clone user
copy.name = "Copy"
local names = { user.name, copy.name }
print(user.name, copy.name, name, names[1], names[2], user:get(), user:label())
return 0
"#;
    cbackend::build(source, &exe, None)
        .unwrap_or_else(|e| panic!("native build with {} failed:\n{e}", compiler.display_name()));
    let run = Command::new(&exe).output().expect("run native program");
    let _ = std::fs::remove_file(&exe);
    assert!(run.status.success(), "native program failed: {run:?}");
    let stdout = String::from_utf8_lossy(&run.stdout).replace("\r\n", "\n");
    assert_eq!(
        stdout,
        "Renamed Copy Local Renamed Copy Renamed [Renamed]\n"
    );
}
