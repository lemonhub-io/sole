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
    let path = std::env::temp_dir().join(format!("sole_undefined_{}.sole", std::process::id()));
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
    assert!(zh_err.contains("[E0201] 未定义变量 `x`"), "zh: {}", zh_err);
}

#[test]
fn cli_typecheck_error_is_bilingual() {
    let path = std::env::temp_dir().join(format!("sole_type_{}.sole", std::process::id()));
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(f, "let x: int = \"hi\"").unwrap();
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
        en_err.contains("[E0301] type mismatch in `let x`"),
        "en: {}",
        en_err
    );
    assert!(
        zh_err.contains("[E0301] `let x` 类型不匹配"),
        "zh: {}",
        zh_err
    );
}

#[test]
fn cli_borrow_error_is_bilingual() {
    let path = std::env::temp_dir().join(format!("sole_borrow_{}.sole", std::process::id()));
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(f, "let a = [1, 2]").unwrap();
    writeln!(f, "let r = ref a").unwrap();
    writeln!(f, "let b = a").unwrap();
    writeln!(f, "print(r.len())").unwrap();
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
        en_err.contains("[E0402] cannot move `a` while it is borrowed"),
        "en: {}",
        en_err
    );
    assert!(
        zh_err.contains("[E0402] `a` 被借用期间不能移动"),
        "zh: {}",
        zh_err
    );
}

#[test]
fn cli_list_and_struct_end_to_end() {
    let path = std::env::temp_dir().join(format!("sole_list_{}.sole", std::process::id()));
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(f, "struct Point:").unwrap();
    writeln!(f, "    x: int").unwrap();
    writeln!(f, "    y: int").unwrap();
    writeln!(f, "impl Point:").unwrap();
    writeln!(f, "    fn sum(self: ref Point) -> int:").unwrap();
    writeln!(f, "        return self.x + self.y").unwrap();
    writeln!(f, "let ps: List[Point] = [Point(1, 2), Point(3, 4)]").unwrap();
    writeln!(f, "for p in ref ps:").unwrap();
    writeln!(f, "    print(p.sum())").unwrap();
    drop(f);

    let out = sole()
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);

    assert!(out.status.success());
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "3\n7\n");
}
