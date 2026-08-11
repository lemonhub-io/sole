//! The `sole` crate: CLI entry point and the M1 tree-walking interpreter.
//!
//! M1 semantics are provisional by design; see docs/GOALS.md and the README
//! for the list of known simplifications.

pub mod eval;

pub use sole_diag::Lang;

use sole_parser::parse;

/// Overrides the effective error-message language for this process.
pub fn set_lang(lang: Lang) {
    sole_diag::set_override(Some(lang));
}

/// Runs Sole source code end-to-end: lex → parse → evaluate.
pub fn run_source(source: &str) -> Result<(), String> {
    let program = parse(source).map_err(|e| e.to_string())?;
    let mut out = std::io::stdout();
    eval::run(&program, &mut out).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_with_output(source: &str) -> Result<String, String> {
        let program = parse(source).map_err(|e| e.to_string())?;
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
}
