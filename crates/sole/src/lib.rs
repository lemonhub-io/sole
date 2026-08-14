//! The `sole` crate: CLI entry point and the M3 bytecode VM runtime.
//!
//! Semantics follow docs/GOALS.md; see the README for known simplifications.

pub mod compiler;
pub mod typecheck;
pub mod vm;

pub use sole_diag::Lang;

use sole_parser::{parse, Block, ElseBranch, Expr, Item, Program, Stmt};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Overrides the effective error-message language for this process.
pub fn set_lang(lang: Lang) {
    sole_diag::set_override(Some(lang));
}

/// Runs Sole source code end-to-end: lex → parse → typecheck → compile → run.
/// `import` statements resolve relative to `base_dir` (when given).
pub fn run_source(source: &str) -> Result<(), String> {
    run_source_at(source, None, &mut std::io::stdout())
}

/// Runs a `.sole` file, resolving `import` relative to its directory.
pub fn run_file(path: &str) -> Result<(), String> {
    let source =
        std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {}", path, e))?;
    let dir = Path::new(path).parent().map(|p| p.to_path_buf());
    run_source_at(&source, dir.as_deref(), &mut std::io::stdout())
}

/// Loads a program and all its `import`ed modules (relative to
/// `base_dir`), merged into a single `Program`. `from x import a`
/// items are kept for the type checker; plain `import x` items are
/// consumed by loading. Cycles are rejected.
pub fn load_program(source: &str, base_dir: Option<&Path>) -> Result<Program, String> {
    let mut items = Vec::new();
    let mut modules: HashSet<String> = HashSet::new();
    let mut visited: HashSet<PathBuf> = HashSet::new();
    // Breadth-first: each file is loaded once (visited), so import cycles
    // terminate silently — the shared symbol table makes them harmless.
    let mut stack: Vec<(String, Option<PathBuf>)> =
        vec![(source.to_string(), base_dir.map(|p| p.to_path_buf()))];
    while let Some((src, dir)) = stack.pop() {
        let program = parse(&src).map_err(|e| e.to_string())?;
        for item in program.items {
            match &item {
                Item::Import(imp) if imp.names.is_empty() => {
                    // `import foo` → load foo.sole relative to dir.
                    modules.insert(imp.module.clone());
                    let path = resolve_module(&imp.module, dir.as_deref())?;
                    if visited.insert(path.clone()) {
                        let text = std::fs::read_to_string(&path)
                            .map_err(|e| format!("cannot read {}: {}", path.display(), e))?;
                        let new_dir = path.parent().map(|p| p.to_path_buf());
                        stack.push((text, new_dir));
                    }
                }
                Item::Import(imp) => {
                    // `from foo import a, b` stays for the type checker.
                    modules.insert(imp.module.clone());
                    items.push(item);
                }
                _ => items.push(item),
            }
        }
    }
    Ok(Program {
        items: rewrite_module_prefixes(items, &modules),
    })
}

/// Rewrites `module.symbol` references (from `import module` /
/// `from module import ...`) into plain global symbols. Modules share one
/// symbol table (GOALS: explicit imports, no hidden globals; scoping is
/// file-level at compile time).
fn rewrite_module_prefixes(items: Vec<Item>, modules: &HashSet<String>) -> Vec<Item> {
    if modules.is_empty() {
        return items;
    }
    fn rewrite_expr(expr: &mut Expr, modules: &HashSet<String>) {
        match expr {
            Expr::Field { obj, name, span } => {
                rewrite_expr(obj, modules);
                if let Expr::Ident(m, _) = obj.as_ref() {
                    if modules.contains(m) {
                        *expr = Expr::Ident(name.clone(), *span);
                    }
                }
            }
            Expr::Call { callee, args, .. } => {
                rewrite_expr(callee, modules);
                for a in args {
                    rewrite_expr(a, modules);
                }
            }
            Expr::Index { obj, index, .. } => {
                rewrite_expr(obj, modules);
                rewrite_expr(index, modules);
            }
            Expr::Unary { expr: e, .. } => rewrite_expr(e, modules),
            Expr::Binary { lhs, rhs, .. } => {
                rewrite_expr(lhs, modules);
                rewrite_expr(rhs, modules);
            }
            Expr::List(items, _) => {
                for it in items {
                    rewrite_expr(it, modules);
                }
            }
            Expr::Dict(pairs, _) => {
                for (k, v) in pairs {
                    rewrite_expr(k, modules);
                    rewrite_expr(v, modules);
                }
            }
            Expr::Set(items, _) => {
                for it in items {
                    rewrite_expr(it, modules);
                }
            }
            Expr::Tuple(items, _) => {
                for it in items {
                    rewrite_expr(it, modules);
                }
            }
            Expr::Borrow { expr: e, .. } => rewrite_expr(e, modules),
            Expr::Int(..) | Expr::Float(..) | Expr::Str(..) | Expr::Bool(..) | Expr::Ident(..) => {}
        }
    }
    fn rewrite_stmt(stmt: &mut Stmt, modules: &HashSet<String>) {
        match stmt {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::FieldAssign { value, .. }
            | Stmt::Return {
                value: Some(value), ..
            }
            | Stmt::Assert { expr: value, .. } => rewrite_expr(value, modules),
            Stmt::Expr(e) => rewrite_expr(e, modules),
            Stmt::Return { value: None, .. } => {}
            Stmt::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                rewrite_expr(cond, modules);
                rewrite_block(then_block, modules);
                if let Some(ElseBranch::If(s)) = else_block {
                    rewrite_stmt(s, modules);
                }
                if let Some(ElseBranch::Block(b)) = else_block {
                    rewrite_block(b, modules);
                }
            }
            Stmt::While { cond, body, .. } => {
                rewrite_expr(cond, modules);
                rewrite_block(body, modules);
            }
            Stmt::For { iterable, body, .. } => {
                rewrite_expr(iterable, modules);
                rewrite_block(body, modules);
            }
            Stmt::TaskGroup { body, .. } => rewrite_block(body, modules),
            Stmt::Go { call, .. } => rewrite_expr(call, modules),
            Stmt::Break { .. } | Stmt::Continue { .. } | Stmt::Yield { .. } => {}
        }
    }
    fn rewrite_block(block: &mut Block, modules: &HashSet<String>) {
        for s in &mut block.stmts {
            rewrite_stmt(s, modules);
        }
    }
    items
        .into_iter()
        .map(|mut item| match &mut item {
            Item::Fn(f) => {
                rewrite_block(&mut f.body, modules);
                item
            }
            Item::Test(t) => {
                rewrite_block(&mut t.body, modules);
                item
            }
            Item::Impl(imp) => {
                for m in &mut imp.methods {
                    rewrite_block(&mut m.body, modules);
                }
                item
            }
            Item::Stmt(s) => {
                rewrite_stmt(s, modules);
                item
            }
            Item::Struct(_) | Item::Interface(_) => item,
            Item::Import(_) => item,
        })
        .collect()
}

fn resolve_module(module: &str, dir: Option<&Path>) -> Result<PathBuf, String> {
    let candidates = [format!("{}.sole", module)];
    for name in candidates {
        let p = match dir {
            Some(d) => d.join(&name),
            None => PathBuf::from(&name),
        };
        if p.exists() {
            return Ok(p);
        }
    }
    Err(format!("cannot find module `{}`", module))
}

/// Like `run_source_to` but resolves imports relative to `base_dir`.
pub fn run_source_at(
    source: &str,
    base_dir: Option<&Path>,
    out: &mut dyn std::io::Write,
) -> Result<(), String> {
    let program = load_program(source, base_dir)?;
    typecheck::check(&program).map_err(|e| e.to_string())?;
    let compiled = compiler::compile(&program).map_err(|e| e.to_string())?;
    let mut rt = vm::Runtime::new(Rc::new(compiled), out);
    rt.run().map_err(|e| e.to_string())
}

/// Outcome of a single `test` block.
pub type TestOutcome = (String, Result<(), String>);

/// Runs every `test` block in the source; returns (name, outcome) pairs.
pub fn run_tests(source: &str) -> Result<Vec<TestOutcome>, String> {
    run_tests_dir(source, None)
}

/// Like `run_tests` but resolves imports relative to the given file.
pub fn run_tests_at(source: &str, path: &str) -> Result<Vec<TestOutcome>, String> {
    let dir = std::path::Path::new(path).parent().map(|p| p.to_path_buf());
    run_tests_dir(source, dir.as_deref())
}

fn run_tests_dir(source: &str, dir: Option<&std::path::Path>) -> Result<Vec<TestOutcome>, String> {
    let program = load_program(source, dir).map_err(|e| e.to_string())?;
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

/// Like `run_source_at` with no base directory (imports resolve in the
/// current working directory).
pub fn run_source_to(source: &str, out: &mut dyn std::io::Write) -> Result<(), String> {
    run_source_at(source, None, out)
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

#[cfg(test)]
mod m4_stage1_tests {
    use super::*;

    fn run(src: &str) -> Result<String, String> {
        let program = load_program(src, None).map_err(|e| e.to_string())?;
        typecheck::check(&program).map_err(|e| e.to_string())?;
        let compiled = compiler::compile(&program).map_err(|e| e.to_string())?;
        let mut buf = Vec::new();
        let mut rt = vm::Runtime::new(Rc::new(compiled), &mut buf);
        rt.run().map_err(|e| e.to_string())?;
        String::from_utf8(buf).map_err(|e| e.to_string())
    }

    #[test]
    fn std_io_end_to_end() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("sole_io_{}.txt", std::process::id()));
        let src = format!(
            "let r = write(\"{}\", \"data\")\nprint(r.is_ok())\nlet s = read_to_str(\"{}\")\nprint(s.unwrap())\n",
            path.display(),
            path.display()
        );
        assert_eq!(run(&src).unwrap(), "true\ndata\n");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn std_io_error_is_result_err() {
        let src = "let t = read_to_str(\"/nonexistent/xyz.sole\")\nprint(t.is_err())\n";
        assert_eq!(run(src).unwrap(), "true\n");
    }

    #[test]
    fn std_math_end_to_end() {
        let src = r#"
print(abs(-5))
print(abs(-2.5))
print(floor(3.7))
print(ceil(3.2))
print(round(3.5))
print(sqrt(16.0))
print(pow(2.0, 10.0))
"#;
        assert_eq!(run(src).unwrap(), "5\n2.5\n3\n4\n4\n4\n1024\n");
    }

    #[test]
    fn std_clock_and_sleep() {
        let src = "print(clock() > 0)\nsleep(5)\nprint(clock() >= 0)\n";
        assert_eq!(run(src).unwrap(), "true\ntrue\n");
    }

    #[test]
    fn std_json_roundtrip() {
        let src = r#"
let hom = {"a": 1, "b": 2}
let enc = json_encode(hom)
print(enc)
let dec = json_decode(enc).unwrap()
print(dec["a"])
let arr = json_decode("[1, \"two\", null]").unwrap()
print(arr[1])
print(arr[2])
print(arr[2] == None)
"#;
        assert_eq!(run(src).unwrap(), "{\"a\":1,\"b\":2}\n1\ntwo\nNone\ntrue\n");
    }

    #[test]
    fn json_decode_error_is_err() {
        let src = "let bad = json_decode(\"not json\")\nprint(bad.is_err())\n";
        assert_eq!(run(src).unwrap(), "true\n");
    }

    #[test]
    fn from_import_resolves_names() {
        let dir = std::env::temp_dir();
        let mod_path = dir.join(format!("sole_lib_{}.sole", std::process::id()));
        let main_path = dir.join(format!("sole_main_{}.sole", std::process::id()));
        std::fs::write(&mod_path, "fn twice(x: int) -> int:\n    return x * 2\n").unwrap();
        let main_src = "import sole_lib_XXXX\nfrom sole_lib_XXXX import twice\nprint(twice(21))\n"
            .replace("XXXX", &std::process::id().to_string());
        std::fs::write(&main_path, &main_src).unwrap();
        let program = load_program(&main_src, dir.to_str().map(std::path::Path::new)).unwrap();
        typecheck::check(&program).unwrap();
        let mut buf = Vec::new();
        let mut rt = vm::Runtime::new(Rc::new(compiler::compile(&program).unwrap()), &mut buf);
        rt.run().unwrap();
        assert_eq!(String::from_utf8(buf).unwrap(), "42\n");
        let _ = std::fs::remove_file(&mod_path);
        let _ = std::fs::remove_file(&main_path);
    }

    #[test]
    fn module_prefix_rewrite() {
        let dir = std::env::temp_dir();
        let mod_path = dir.join(format!("sole_lib2_{}.sole", std::process::id()));
        std::fs::write(
            &mod_path,
            "struct Pt:\n    x: int\nfn hi() -> int:\n    return 7\n",
        )
        .unwrap();
        let src = format!(
            "import sole_lib2_{}\nprint(sole_lib2_{}.hi())\nlet p = sole_lib2_{}.Pt(1)\nprint(p.x)\n",
            std::process::id(),
            std::process::id(),
            std::process::id()
        );
        let program = load_program(&src, dir.to_str().map(std::path::Path::new)).unwrap();
        typecheck::check(&program).unwrap();
        let mut buf = Vec::new();
        let mut rt = vm::Runtime::new(Rc::new(compiler::compile(&program).unwrap()), &mut buf);
        rt.run().unwrap();
        assert_eq!(String::from_utf8(buf).unwrap(), "7\n1\n");
        let _ = std::fs::remove_file(&mod_path);
    }

    #[test]
    fn from_import_unknown_name_is_error() {
        let dir = std::env::temp_dir();
        let mod_path = dir.join(format!("sole_lib3_{}.sole", std::process::id()));
        std::fs::write(&mod_path, "fn a() -> int:\n    return 1\n").unwrap();
        let src = format!(
            "import sole_lib3_{}\nfrom sole_lib3_{} import nope\n",
            std::process::id(),
            std::process::id()
        );
        let program = load_program(&src, dir.to_str().map(std::path::Path::new)).unwrap();
        let err = typecheck::check(&program).unwrap_err().to_string();
        assert!(err.contains("[E0201]"), "err: {err}");
        let _ = std::fs::remove_file(&mod_path);
    }

    #[test]
    fn missing_module_is_error() {
        let err = load_program("import definitely_not_a_module_xyz\n", None).unwrap_err();
        assert!(err.contains("cannot find module"), "err: {err}");
    }
}
