//! The `sole` crate: CLI entry point and the M1 tree-walking interpreter.
//!
//! M1 semantics are provisional by design; see docs/GOALS.md and the README
//! for the list of known simplifications.

pub mod eval;
pub mod typecheck;

pub use sole_diag::Lang;

use sole_parser::parse;

/// Overrides the effective error-message language for this process.
pub fn set_lang(lang: Lang) {
    sole_diag::set_override(Some(lang));
}

/// Runs Sole source code end-to-end: lex → parse → typecheck → evaluate.
pub fn run_source(source: &str) -> Result<(), String> {
    let program = parse(source).map_err(|e| e.to_string())?;
    typecheck::check(&program).map_err(|e| e.to_string())?;
    let mut out = std::io::stdout();
    eval::run(&program, &mut out).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_with_output(source: &str) -> Result<String, String> {
        let program = parse(source).map_err(|e| e.to_string())?;
        typecheck::check(&program).map_err(|e| e.to_string())?;
        let mut buf = Vec::new();
        eval::run(&program, &mut buf).map_err(|e| e.to_string())?;
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
}
