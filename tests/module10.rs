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
    cbackend::build(source, &exe, None).unwrap_or_else(|e| {
        panic!("native build with {} failed:\n{e}", compiler.display_name())
    });
    let run = Command::new(&exe).output().expect("run native program");
    let _ = std::fs::remove_file(&exe);
    assert!(run.status.success(), "native program failed: {run:?}");
    let stdout = String::from_utf8_lossy(&run.stdout).replace("\r\n", "\n");
    assert_eq!(stdout, "1 New Bergen hi New\nfields 2 total 60\n");
}
