//! The `sole` crate: CLI entry point and the M3 bytecode VM runtime.
//!
//! Semantics follow docs/GOALS.md; see the README for known simplifications.

pub mod compiler;
pub mod typecheck;
pub mod vm;

pub use sole_diag::Lang;

use sole_parser::parse;
use std::rc::Rc;

/// Overrides the effective error-message language for this process.
pub fn set_lang(lang: Lang) {
    sole_diag::set_override(Some(lang));
}

/// Runs Sole source code end-to-end: lex → parse → typecheck → compile → run.
pub fn run_source(source: &str) -> Result<(), String> {
    run_source_to(source, &mut std::io::stdout())
}

/// Outcome of a single `test` block.
pub type TestOutcome = (String, Result<(), String>);

/// Runs every `test` block in the source; returns (name, outcome) pairs.
pub fn run_tests(source: &str) -> Result<Vec<TestOutcome>, String> {
    let program = parse(source).map_err(|e| e.to_string())?;
    typecheck::check(&program).map_err(|e| e.to_string())?;
    let compiled = compiler::compile(&program).map_err(|e| e.to_string())?;
    let test_fns: Vec<(String, usize)> = compiled
        .functions
        .iter()
        .enumerate()
        .filter(|(_, f)| f.name.starts_with("test:"))
        .map(|(i, f)| (f.name[5..].to_string(), i))
        .collect();
    let mut results = Vec::new();
    for (name, fi) in test_fns {
        let mut sink = Vec::new();
        let mut rt = vm::Runtime::new(Rc::new(compiled.clone()), &mut sink);
        let outcome = rt.run_function(fi).map_err(|e| e.to_string());
        results.push((name, outcome));
    }
    Ok(results)
}

/// Like `run_source` but writes output to the given writer.
pub fn run_source_to(source: &str, out: &mut dyn std::io::Write) -> Result<(), String> {
    let program = parse(source).map_err(|e| e.to_string())?;
    typecheck::check(&program).map_err(|e| e.to_string())?;
    let compiled = compiler::compile(&program).map_err(|e| e.to_string())?;
    let mut rt = vm::Runtime::new(Rc::new(compiled), out);
    rt.run().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_with_output(source: &str) -> Result<String, String> {
        let program = parse(source).map_err(|e| e.to_string())?;
        typecheck::check(&program).map_err(|e| e.to_string())?;
        let compiled = compiler::compile(&program).map_err(|e| e.to_string())?;
        let mut buf = Vec::new();
        let mut rt = vm::Runtime::new(Rc::new(compiled), &mut buf);
        rt.run().map_err(|e| e.to_string())?;
        String::from_utf8(buf).map_err(|e| e.to_string())
    }

    #[test]
    fn hello_world() {
        assert_eq!(
            run_with_output("print(\"hello, sole!\")\n").unwrap(),
            "hello, sole!\n"
        );
    }

    #[test]
    fn fibonacci() {
        let src = r#"
fn fib(n: int) -> int:
    if n < 2:
        return n
    return fib(n - 1) + fib(n - 2)

print(fib(10))
"#;
        assert_eq!(run_with_output(src).unwrap(), "55\n");
    }

    #[test]
    fn for_loop_and_reassignment() {
        let src = r#"
let mut total = 0
for i in range(5):
    total = total + i
print(total)
"#;
        assert_eq!(run_with_output(src).unwrap(), "10\n");
    }

    #[test]
    fn while_loop_with_break() {
        let src = r#"
let mut n = 0
while true:
    n = n + 1
    if n >= 3:
        break
print(n)
"#;
        assert_eq!(run_with_output(src).unwrap(), "3\n");
    }

    #[test]
    fn else_branch_and_else_if() {
        let src = r#"
fn sign(n: int) -> str:
    if n > 0:
        return "pos"
    else if n < 0:
        return "neg"
    else:
        return "zero"

print(sign(5))
print(sign(-5))
print(sign(0))
"#;
        assert_eq!(run_with_output(src).unwrap(), "pos\nneg\nzero\n");
    }

    #[test]
    fn reassigning_immutable_binding_fails() {
        let src = "let x = 1\nx = 2\n";
        assert!(run_with_output(src).is_err());
    }

    #[test]
    fn type_annotation_is_parsed_and_ignored_at_runtime() {
        let src = "let x: int = 42\nprint(x)\n";
        assert_eq!(run_with_output(src).unwrap(), "42\n");
    }

    #[test]
    fn runtime_errors_have_source_position() {
        let err = run_with_output("print(x)\n").unwrap_err();
        assert!(
            err.contains("1:7: [E0201] undefined variable `x`"),
            "err: {err}"
        );
    }

    #[test]
    fn typecheck_errors_have_stable_code_and_position() {
        let err = run_with_output("let x: int = \"hi\"\n").unwrap_err();
        assert_eq!(
            err,
            "1:1: [E0301] type mismatch in `let x`: expected `int`, got `str`"
        );
    }

    #[test]
    fn list_end_to_end() {
        let src = "let xs: List[int] = [1, 2, 3]\nxs.push(4)\nprint(xs.len())\nprint(xs[0])\nprint(xs.get(3))\nxs.set(0, 9)\nprint(xs[0])\n";
        assert_eq!(run_with_output(src).unwrap(), "4\n1\n4\n9\n");
    }

    #[test]
    fn list_for_loop_borrow() {
        let src = "let xs = [1, 2, 3]\nfor x in ref xs:\n    print(x)\nprint(xs.len())\n";
        assert_eq!(run_with_output(src).unwrap(), "1\n2\n3\n3\n");
    }

    #[test]
    fn borrow_parameter_end_to_end() {
        let src = "fn sum(xs: ref List[int]) -> int:\n    let mut total = 0\n    for x in ref xs:\n        total = total + x\n    return total\nprint(sum([1, 2, 3]))\n";
        assert_eq!(run_with_output(src).unwrap(), "6\n");
    }

    #[test]
    fn struct_and_methods_end_to_end() {
        let src = "struct Point:\n    x: int\n    y: int\nimpl Point:\n    fn move_x(self: mut ref Point, dx: int) -> int:\n        self.x = self.x + dx\n        return self.x\nlet mut p = Point(1, 2)\nprint(p.x)\nprint(p.y)\nprint(p.move_x(5))\n";
        assert_eq!(run_with_output(src).unwrap(), "1\n2\n6\n");
    }

    #[test]
    fn interface_dispatch_end_to_end() {
        let src = "interface Shape:\n    fn area(self: ref Shape) -> float\nstruct Circle:\n    r: float\nimpl Circle: Shape:\n    fn area(self: ref Circle) -> float:\n        return 3.14 * self.r * self.r\nfn describe(s: ref Shape) -> float:\n    return s.area()\nlet c = Circle(1.0)\nprint(describe(ref c))\n";
        assert_eq!(run_with_output(src).unwrap(), "3.14\n");
    }

    #[test]
    fn mut_ref_parameter_writes_back_to_caller() {
        let src = "fn fill(xs: mut ref List[int], n: int) -> int:\n    xs.push(n)\n    return xs.len()\nlet mut data = [10]\nprint(fill(data, 20))\nprint(data.len())\nprint(data[1])\n";
        assert_eq!(run_with_output(src).unwrap(), "2\n2\n20\n");
    }

    #[test]
    fn ref_parameter_does_not_copy() {
        let src = "fn bump(xs: ref List[int]) -> int:\n    return xs.len()\nlet data = [1, 2, 3]\nprint(bump(data))\nprint(data.len())\n";
        assert_eq!(run_with_output(src).unwrap(), "3\n3\n");
    }

    #[test]
    fn channel_send_recv_end_to_end() {
        let src = "fn worker(ch: Chan[int], n: int) -> int:\n    ch.send(n)\n    ch.send(n * 2)\n    return 0\ntask_group:\n    let ch = Chan[int]()\n    go worker(ch, 10)\n    let a = ch.recv()\n    let b = ch.recv()\n    print(a)\n    print(b)\n";
        assert_eq!(run_with_output(src).unwrap(), "10\n20\n");
    }

    #[test]
    fn channel_for_in_end_to_end() {
        let src = "fn producer(ch: Chan[int], n: int) -> int:\n    for i in range(n):\n        ch.send(i)\n    ch.close()\n    return 0\ntask_group:\n    let ch = Chan[int](2)\n    go producer(ch, 5)\n    let mut sum = 0\n    for v in ch:\n        sum = sum + v\n    print(sum)\n";
        assert_eq!(run_with_output(src).unwrap(), "10\n");
    }

    #[test]
    fn buffered_channel_end_to_end() {
        let src = "fn worker(ch: Chan[int]) -> int:\n    ch.send(1)\n    ch.send(2)\n    ch.send(3)\n    return 0\ntask_group:\n    let ch = Chan[int](3)\n    go worker(ch)\n    print(ch.recv())\n    print(ch.recv())\n    print(ch.recv())\n";
        assert_eq!(run_with_output(src).unwrap(), "1\n2\n3\n");
    }
}

#[cfg(test)]
mod m4_tests {
    use super::*;

    fn run(src: &str) -> Result<String, String> {
        let program = parse(src).map_err(|e| e.to_string())?;
        typecheck::check(&program).map_err(|e| e.to_string())?;
        let compiled = compiler::compile(&program).map_err(|e| e.to_string())?;
        let mut buf = Vec::new();
        let mut rt = vm::Runtime::new(Rc::new(compiled), &mut buf);
        rt.run().map_err(|e| e.to_string())?;
        String::from_utf8(buf).map_err(|e| e.to_string())
    }

    #[test]
    fn option_and_result_end_to_end() {
        let src = r#"
let a: Option[int] = Some(42)
let b: Option[int] = None
let r1: Result[int, str] = Ok(7)
let r2: Result[int, str] = Err("boom")
print(a.is_some())
print(a.unwrap())
print(b.is_none())
print(r1.is_ok())
print(r1.unwrap())
print(r2.is_err())
"#;
        assert_eq!(run(src).unwrap(), "true\n42\ntrue\ntrue\n7\ntrue\n");
    }

    #[test]
    fn unwrap_none_is_runtime_error() {
        let err = run("let o: Option[int] = None\nprint(o.unwrap())\n").unwrap_err();
        assert!(err.contains("[E0219]"), "err: {err}");
    }

    #[test]
    fn generic_function_end_to_end() {
        let src = r#"
fn max[T: Comparable](a: T, b: T) -> T:
    if a > b:
        return a
    return b
print(max(3, 9))
print(max("apple", "banana"))
"#;
        assert_eq!(run(src).unwrap(), "9\nbanana\n");
    }

    #[test]
    fn generic_constraint_violation_is_error() {
        let err =
            run("fn f[T: Comparable](x: T) -> T:\n    return x\nlet xs = [1]\nprint(f(xs))\n")
                .unwrap_err();
        assert!(err.contains("[E0320]"), "err: {err}");
    }

    #[test]
    fn dict_end_to_end() {
        let src = r#"
let d: Dict[str, int] = {"x": 1, "y": 2}
d.set("z", 3)
print(d.len())
print(d.get("x").unwrap())
print(d.contains("z"))
print(d["y"])
d.remove("x")
print(d.contains("x"))
let e: Dict[str, int] = {}
print(e.len())
"#;
        assert_eq!(run(src).unwrap(), "3\n1\ntrue\n2\nfalse\n0\n");
    }

    #[test]
    fn set_end_to_end() {
        let src = r#"
let s: Set[int] = {1, 2, 2, 3}
print(s.len())
print(s.contains(2))
s.add(4)
print(s.contains(4))
s.remove(2)
print(s.contains(2))
"#;
        assert_eq!(run(src).unwrap(), "3\ntrue\ntrue\nfalse\n");
    }

    #[test]
    fn tuple_end_to_end() {
        let src = "let t = (1, \"two\", 3.0)\nprint(t.len())\nprint(t[0])\nprint(t[1])\n";
        assert_eq!(run(src).unwrap(), "3\n1\ntwo\n");
    }

    #[test]
    fn str_methods_end_to_end() {
        let src = r#"
let s = "hello, world"
print(s.len())
print(s.sub(0, 5))
print(s.split(",").len())
print("-".join(["a", "b"]))
print(s.contains("world"))
print(s.starts_with("hello"))
print(s.ends_with("world"))
"#;
        assert_eq!(run(src).unwrap(), "12\nhello\n2\na-b\ntrue\ntrue\ntrue\n");
    }

    #[test]
    fn str_parse_returns_result() {
        let src = r#"
let n = "123".to_int()
print(n.unwrap())
let bad = "abc".to_int()
print(bad.is_err())
print("1.5".to_float().unwrap())
"#;
        assert_eq!(run(src).unwrap(), "123\ntrue\n1.5\n");
    }

    #[test]
    fn assert_failure_is_runtime_error() {
        let err = run("assert 1 == 2\n").unwrap_err();
        assert!(err.contains("[E0221]"), "err: {err}");
        assert!(run("assert 2 == 2\n").is_ok());
    }

    #[test]
    fn test_blocks_do_not_run_on_normal_execution() {
        let src = "test quiet:\n    print(\"boom\")\n";
        assert_eq!(run(src).unwrap(), "");
    }

    #[test]
    fn run_tests_runs_test_blocks() {
        let src = "test ok:\n    assert 1 == 1\ntest bad:\n    assert 1 == 2\n";
        let results = run_tests(src).unwrap();
        assert_eq!(results.len(), 2);
        assert!(results[0].1.is_ok());
        assert!(results[1].1.is_err());
    }

    #[test]
    fn to_str_method() {
        let src = "print((42).to_str())\nprint(\"x\".to_str())\n";
        assert_eq!(run(src).unwrap(), "42\nx\n");
    }
}
