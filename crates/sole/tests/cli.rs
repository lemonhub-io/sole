//! End-to-end CLI tests (run against the built `sole` binary).

use std::io::Write;
use std::process::Command;

fn sole() -> Command {
    Command::new(env!("CARGO_BIN_EXE_sole"))
}

#[test]
fn cli_hello_world() {
    let out = sole()
        .args(["run", "../../examples/hello.sole"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "hello, sole!\n");
}

#[test]
fn cli_bilingual_error_messages() {
    let path = std::env::temp_dir().join(format!(
        "sole_undefined_{}.sole",
        std::process::id()
    ));
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(f, "print(x)").unwrap();
    drop(f);

    let en = sole()
        .args(["run", "--lang", "en", path.to_str().unwrap()])
        .output()
        .unwrap();
    let zh = sole()
        .args(["run", "--lang", "zh", path.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);

    assert!(!en.status.success());
    assert!(!zh.status.success());
    let en_err = String::from_utf8(en.stderr).unwrap();
    let zh_err = String::from_utf8(zh.stderr).unwrap();
    assert!(
        en_err.contains("[E0201] undefined variable `x`"),
        "en: {}",
        en_err
    );
    assert!(
        zh_err.contains("[E0201] 未定义变量 `x`"),
        "zh: {}",
        zh_err
    );
}
