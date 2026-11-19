//! End-to-end tests of the `lusa` binary: streaming `print`, and the
//! package-name guards of `install`/`uninstall`.

use std::path::PathBuf;
use std::process::{Command, Output};

fn lusa() -> Command {
    Command::new(env!("CARGO_BIN_EXE_lusa"))
}

fn tmp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lusa_cli_{}_{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn write_program(tag: &str, src: &str) -> PathBuf {
    let dir = tmp_dir(tag);
    let path = dir.join("main.lusa");
    std::fs::write(&path, src).expect("write program");
    path
}

fn text(out: &[u8]) -> String {
    String::from_utf8_lossy(out).replace("\r\n", "\n")
}

fn show(o: &Output) -> String {
    format!("stdout:\n{}\nstderr:\n{}", text(&o.stdout), text(&o.stderr))
}

// ===== print streaming =====

const PRINT_THEN_FAIL: &str = "print(\"before the error\")\nprint(1 + 1)\nerror(\"boom\")\n";

#[test]
fn interpreter_keeps_output_printed_before_a_runtime_error() {
    let path = write_program("interp_err", PRINT_THEN_FAIL);
    let o = lusa().arg("run").arg(&path).output().expect("run lusa");
    assert!(!o.status.success(), "{}", show(&o));
    let stdout = text(&o.stdout);
    assert!(stdout.contains("before the error\n2\n"), "{}", show(&o));
    assert!(text(&o.stderr).contains("boom"), "{}", show(&o));
}

#[test]
fn vm_keeps_output_printed_before_a_runtime_error() {
    let path = write_program("vm_err", PRINT_THEN_FAIL);
    let o = lusa()
        .arg("run")
        .arg(&path)
        .arg("--vm")
        .output()
        .expect("run lusa");
    assert!(!o.status.success(), "{}", show(&o));
    let stdout = text(&o.stdout);
    assert!(stdout.contains("before the error\n2\n"), "{}", show(&o));
    assert!(text(&o.stderr).contains("boom"), "{}", show(&o));
}

#[test]
fn shortcut_form_streams_too() {
    let path = write_program("shortcut_err", PRINT_THEN_FAIL);
    let o = lusa().arg(&path).output().expect("run lusa");
    assert!(!o.status.success(), "{}", show(&o));
    assert!(text(&o.stdout).contains("before the error"), "{}", show(&o));
}

#[test]
fn successful_programs_print_in_order() {
    let path = write_program("ok", "for i = 1, 3 do print(i) end\nprint(\"done\")\n");
    let o = lusa().arg("run").arg(&path).output().expect("run lusa");
    assert!(o.status.success(), "{}", show(&o));
    assert_eq!(text(&o.stdout), "1\n2\n3\ndone\n");
}

#[test]
fn library_api_still_captures_print() {
    // The C backend runs the self-hosted compiler through `run_source` and
    // reads the generated C from the captured lines, so capture must remain
    // the default for library callers.
    let lines = lusa::run_source("print(\"captured\")").expect("run");
    assert_eq!(lines, vec!["captured".to_string()]);
}

// ===== package-name guards =====

#[test]
fn install_rejects_specs_that_are_not_user_slash_repo() {
    for spec in [
        "../evil",
        "user/..",
        "../../x",
        "user/repo/extra",
        "/repo",
        "user/",
        "us er/repo",
        "user/repo#../../x",
        "user/repo#",
    ] {
        let o = lusa().arg("install").arg(spec).output().expect("run lusa");
        assert_eq!(
            o.status.code(),
            Some(2),
            "spec {:?} was accepted\n{}",
            spec,
            show(&o)
        );
    }
}

#[test]
fn uninstall_refuses_names_that_escape_the_packages_folder() {
    let root = tmp_dir("uninstall_guard");
    let packages = root.join("packages");
    std::fs::create_dir_all(&packages).unwrap();
    let sibling = root.join("bin");
    std::fs::create_dir_all(&sibling).unwrap();
    std::fs::write(sibling.join("lusa.exe"), "keep me").unwrap();

    for name in ["..", "../bin", ".", ".hidden", "a/b", "a\\b"] {
        let o = lusa()
            .env("LUSA_PACKAGES", &packages)
            .arg("uninstall")
            .arg(name)
            .output()
            .expect("run lusa");
        assert_eq!(
            o.status.code(),
            Some(2),
            "name {:?} was accepted\n{}",
            name,
            show(&o)
        );
    }
    assert!(
        sibling.join("lusa.exe").is_file(),
        "uninstall escaped the packages folder"
    );
    assert!(
        packages.is_dir(),
        "uninstall removed the packages folder itself"
    );

    // a legitimate package is still removable
    let pkg = packages.join("my-pkg.v2");
    std::fs::create_dir_all(&pkg).unwrap();
    let o = lusa()
        .env("LUSA_PACKAGES", &packages)
        .arg("uninstall")
        .arg("my-pkg.v2")
        .output()
        .expect("run lusa");
    assert!(o.status.success(), "{}", show(&o));
    assert!(!pkg.exists());
    let _ = std::fs::remove_dir_all(&root);
}
