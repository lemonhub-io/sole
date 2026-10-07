//! M2 static type checker.
//!
//! Full type checking for the M1/M2 language subset: literals, annotations,
//! assignment, function signatures, calls, operators, loops, `List[T]`,
//! structs, interfaces, and methods plus a flow-sensitive borrow/move
//! checker (use-after-move, moves while borrowed, mutable borrow conflicts,
//! borrow escape). Copy types (`int`/`float`/`bool`/`str`/`ref`) are exempt
//! from move semantics.

use sole_diag::{Diagnostic, Lang, Msg};
use sole_parser::{
    BinOp, Block, ElseBranch, Expr, FnDef, ImplDef, Item, MethodSig, Param, Program, Span, Stmt,
    Type, TypeParam, UnOp,
};
use std::collections::HashMap;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ty {
    Int,
    Float,
    Bool,
    Str,
    Unit,
    Range,
    Fn,
    Unknown,
    List(Box<Ty>),
    Chan(Box<Ty>),
    Option(Box<Ty>),
    Result(Box<Ty>, Box<Ty>),
    Dict(Box<Ty>, Box<Ty>),
    Set(Box<Ty>),
    Tuple(Vec<Ty>),
    Json,
    Struct(String),
    Interface(String),
    Ref(Box<Ty>),
    MutRef(Box<Ty>),
    TypeVar(String),
}

impl Ty {
    pub(crate) fn from_name(name: &str) -> Option<Ty> {
        match name {
            "int" => Some(Ty::Int),
            "float" => Some(Ty::Float),
            "bool" => Some(Ty::Bool),
            "str" => Some(Ty::Str),
            _ => None,
        }
    }

    /// Source-level name for this type (`int`, `List[str]`, `mut ref T`).
    /// Public for LSP hover text.
    pub fn name(&self) -> String {
        match self {
            Ty::Int => "int".into(),
            Ty::Float => "float".into(),
            Ty::Bool => "bool".into(),
            Ty::Str => "str".into(),
            Ty::Unit => "()".into(),
            Ty::Range => "Range".into(),
            Ty::Fn => "fn".into(),
            Ty::Unknown => "?".into(),
            Ty::List(inner) => format!("List[{}]", inner.name()),
            Ty::Chan(inner) => format!("Chan[{}]", inner.name()),
            Ty::Option(inner) => format!("Option[{}]", inner.name()),
            Ty::Result(ok, err) => format!("Result[{}, {}]", ok.name(), err.name()),
            Ty::Dict(k, v) => format!("Dict[{}, {}]", k.name(), v.name()),
            Ty::Set(inner) => format!("Set[{}]", inner.name()),
            Ty::Tuple(ts) => format!(
                "({})",
                ts.iter().map(Ty::name).collect::<Vec<_>>().join(", ")
            ),
            Ty::TypeVar(name) => name.clone(),
            Ty::Json => "Json".into(),
            Ty::Struct(name) => name.clone(),
            Ty::Interface(name) => name.clone(),
            Ty::Ref(inner) => format!("ref {}", inner.name()),
            Ty::MutRef(inner) => format!("mut ref {}", inner.name()),
        }
    }

    pub(crate) fn is_copy(&self) -> bool {
        matches!(
            self,
            Ty::Int
                | Ty::Float
                | Ty::Bool
                | Ty::Str
                | Ty::Ref(_)
                | Ty::Chan(_)
                | Ty::TypeVar(_)
                | Ty::Json
                | Ty::Fn
                | Ty::Unknown
        )
    }

    pub(crate) fn is_truthy(&self) -> bool {
        matches!(
            self,
            Ty::Int
                | Ty::Float
                | Ty::Bool
                | Ty::Str
                | Ty::List(_)
                | Ty::Dict(..)
                | Ty::Set(_)
                | Ty::Option(_)
                | Ty::Result(..)
                | Ty::TypeVar(_)
                | Ty::Unknown
        )
    }

    pub(crate) fn deref(&self) -> &Ty {
        match self {
            Ty::Ref(inner) | Ty::MutRef(inner) => inner.deref(),
            other => other,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeError {
    pub diag: Diagnostic,
}

impl fmt::Display for TypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.diag.render(Lang::current()))
    }
}

impl std::error::Error for TypeError {}

pub(crate) fn err(msg: Msg, span: Span) -> TypeError {
    TypeError {
        diag: Diagnostic::new(msg, span.line, span.column),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum State {
    Live,
    Moved,
    BorrowedImm,
    BorrowedMut,
}

#[derive(Debug, Clone)]
pub(crate) struct Var {
    pub(crate) ty: Ty,
    pub(crate) state: State,
    pub(crate) mutable: bool,
}

impl Var {
    pub(crate) fn new(ty: Ty, mutable: bool) -> Self {
        Self {
            ty,
            state: State::Live,
            mutable,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct FnSig {
    pub(crate) name: String,
    pub(crate) type_params: Vec<(String, Option<String>)>,
    pub(crate) params: Vec<(String, Ty)>,
    pub(crate) ret: Ty,
}

/// `fn name(a: int, b: str) -> bool` for hover text.
pub(crate) fn sig_text(sig: &FnSig) -> String {
    let params = sig
        .params
        .iter()
        .map(|(n, t)| format!("{}: {}", n, t.name()))
        .collect::<Vec<_>>()
        .join(", ");
    if sig.ret == Ty::Unit {
        format!("fn {}({})", sig.name, params)
    } else {
        format!("fn {}({}) -> {}", sig.name, params, sig.ret.name())
    }
}

pub struct Checker<'a> {
    pub(crate) program: &'a Program,
    pub(crate) fns: HashMap<String, FnSig>,
    pub(crate) structs: HashMap<String, Vec<(String, Ty)>>,
    pub(crate) interfaces: HashMap<String, Vec<MethodSig>>,
    pub(crate) impls: HashMap<String, Vec<String>>,
    pub(crate) methods: HashMap<(String, String), FnSig>,
    pub(crate) vars: Vec<HashMap<String, Var>>,
    pub(crate) current_fn: Option<String>,
    pub(crate) task_group_depth: usize,
    /// Active persistent borrows: borrow variable → (target, mutable).
    pub(crate) borrows: HashMap<String, (String, bool)>,
    /// name → type name observed while checking; last binding of a name
    /// wins (same approximation as the LSP symbol table). Filled for LSP
    /// hover; cheap enough to always collect.
    pub(crate) symbols: HashMap<String, String>,
}

/// Scope snapshot taken before an `if`: every variable declared so far plus
/// the active borrow table. Each branch is checked from the same snapshot,
/// then the branch effects are merged (see `Stmt::If` handling).
pub(crate) struct ScopeSnapshot {
    pub(crate) names: Vec<String>,
    pub(crate) states: HashMap<String, State>,
    pub(crate) borrows: HashMap<String, (String, bool)>,
}

/// A block diverges when control can never fall through its end: it ends in
/// `return`, or in an `if` whose branches all diverge. Conservative: anything
/// else counts as falling through.
pub(crate) fn block_diverges(stmts: &[Stmt]) -> bool {
    match stmts.last() {
        Some(Stmt::Return { .. }) => true,
        Some(stmt) => stmt_diverges(stmt),
        None => false,
    }
}

pub(crate) fn stmt_diverges(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::If {
            then_block,
            else_block,
            ..
        } => {
            block_diverges(&then_block.stmts)
                && match else_block {
                    Some(ElseBranch::If(s)) => stmt_diverges(s),
                    Some(ElseBranch::Block(block, _)) => block_diverges(&block.stmts),
                    None => false,
                }
        }
        _ => false,
    }
}

/// Checks a whole program for static type and borrow errors.
pub fn check(program: &Program) -> Result<(), TypeError> {
    check_symbols(program).1
}

/// Runs the full check and also returns the observed symbol → type-name
/// table (name-keyed; the last binding of a name wins). Used by LSP hover;
/// populated even when checking fails partway, since partial information
/// is still useful in the editor.
pub fn check_symbols(program: &Program) -> (HashMap<String, String>, Result<(), TypeError>) {
    let mut checker = Checker {
        program,
        fns: HashMap::new(),
        structs: HashMap::new(),
        interfaces: HashMap::new(),
        impls: HashMap::new(),
        methods: HashMap::new(),
        vars: Vec::new(),
        current_fn: None,
        task_group_depth: 0,
        borrows: HashMap::new(),
        symbols: HashMap::new(),
    };
    let result = checker.run();
    (checker.symbols, result)
}

mod call;
mod expr;
mod methods;
mod scope;
mod setup;
mod stmt;

/// Structural compatibility: equal shapes, with `Unknown` holes accepted at
/// any depth (e.g. `Option[?]` matches `Option[int]`).
pub(crate) fn types_compatible(expected: &Ty, actual: &Ty) -> bool {
    if matches!(expected, Ty::Unknown) || matches!(actual, Ty::Unknown) {
        return true;
    }
    // `Json` accepts any JSON-serializable value (dynamic value).
    if matches!(expected, Ty::Json) || matches!(actual, Ty::Json) {
        return true;
    }
    match (expected, actual) {
        (Ty::List(a), Ty::List(b)) => types_compatible(a, b),
        (Ty::Chan(a), Ty::Chan(b)) => types_compatible(a, b),
        (Ty::Option(a), Ty::Option(b)) => types_compatible(a, b),
        (Ty::Result(a1, b1), Ty::Result(a2, b2)) => {
            types_compatible(a1, a2) && types_compatible(b1, b2)
        }
        (Ty::Dict(a1, b1), Ty::Dict(a2, b2)) => {
            types_compatible(a1, a2) && types_compatible(b1, b2)
        }
        (Ty::Set(a), Ty::Set(b)) => types_compatible(a, b),
        (Ty::Tuple(a), Ty::Tuple(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| types_compatible(x, y))
        }
        (Ty::Ref(a), Ty::Ref(b)) | (Ty::MutRef(a), Ty::MutRef(b)) => types_compatible(a, b),
        _ => expected == actual,
    }
}

/// Records the last statement index at which each variable is used (NLL).
/// Nested blocks use the enclosing statement's index, which is conservative:
/// borrows may outlive their NLL point by one statement, never the reverse.
/// Walks `expected`, binding type variables to the corresponding parts of
/// `actual` (e.g. `List[T]` against `List[int]` binds `T -> int`).
pub(crate) fn collect_bindings(expected: &Ty, actual: &Ty, bindings: &mut HashMap<String, Ty>) {
    match expected {
        Ty::TypeVar(tv) => match bindings.get(tv) {
            Some(prev) => {
                if !types_compatible(prev, actual) && !matches!(actual, Ty::Unknown) {
                    bindings.insert(tv.clone(), actual.clone());
                }
            }
            None => {
                bindings.insert(tv.clone(), actual.clone());
            }
        },
        Ty::List(inner) => {
            if let Ty::List(a) = actual {
                collect_bindings(inner, a, bindings);
            }
        }
        Ty::Option(inner) => {
            if let Ty::Option(a) = actual {
                collect_bindings(inner, a, bindings);
            }
        }
        Ty::Set(inner) => {
            if let Ty::Set(a) = actual {
                collect_bindings(inner, a, bindings);
            }
        }
        Ty::Ref(inner) => {
            if let Ty::Ref(a) = actual {
                collect_bindings(inner, a, bindings);
            }
        }
        Ty::MutRef(inner) => {
            if let Ty::MutRef(a) = actual {
                collect_bindings(inner, a, bindings);
            }
        }
        Ty::Result(a1, b1) => {
            if let Ty::Result(a2, b2) = actual {
                collect_bindings(a1, a2, bindings);
                collect_bindings(b1, b2, bindings);
            }
        }
        Ty::Dict(a1, b1) => {
            if let Ty::Dict(a2, b2) = actual {
                collect_bindings(a1, a2, bindings);
                collect_bindings(b1, b2, bindings);
            }
        }
        Ty::Tuple(ts) => {
            if let Ty::Tuple(us) = actual {
                for (t, u) in ts.iter().zip(us) {
                    collect_bindings(t, u, bindings);
                }
            }
        }
        _ => {}
    }
}

pub(crate) fn collect_uses(stmt: &Stmt, uses: &mut HashMap<String, usize>, idx: usize) {
    fn expr_uses(expr: &Expr, uses: &mut HashMap<String, usize>, idx: usize) {
        match expr {
            Expr::Ident(name, _) => {
                uses.insert(name.clone(), idx);
            }
            Expr::Int(..) | Expr::Float(..) | Expr::Str(..) | Expr::Bool(..) => {}
            Expr::List(items, _) => {
                for it in items {
                    expr_uses(it, uses, idx);
                }
            }
            Expr::Dict(pairs, _) => {
                for (k, v) in pairs {
                    expr_uses(k, uses, idx);
                    expr_uses(v, uses, idx);
                }
            }
            Expr::Set(items, _) => {
                for it in items {
                    expr_uses(it, uses, idx);
                }
            }
            Expr::Tuple(items, _) => {
                for it in items {
                    expr_uses(it, uses, idx);
                }
            }
            Expr::Unary { expr, .. } => expr_uses(expr, uses, idx),
            Expr::Binary { lhs, rhs, .. } => {
                expr_uses(lhs, uses, idx);
                expr_uses(rhs, uses, idx);
            }
            Expr::Call { callee, args, .. } => {
                expr_uses(callee, uses, idx);
                for a in args {
                    expr_uses(a, uses, idx);
                }
            }
            Expr::Field { obj, name, .. } => {
                expr_uses(obj, uses, idx);
                uses.insert(name.clone(), idx);
            }
            Expr::Index { obj, index, .. } => {
                expr_uses(obj, uses, idx);
                expr_uses(index, uses, idx);
            }
            Expr::Borrow { expr, .. } => expr_uses(expr, uses, idx),
        }
    }

    fn block_uses(block: &Block, uses: &mut HashMap<String, usize>, idx: usize) {
        for s in &block.stmts {
            collect_uses(s, uses, idx);
        }
    }

    match stmt {
        Stmt::Let { value, name, .. } => {
            // The binding name is a definition, not a use.
            let _ = name;
            expr_uses(value, uses, idx);
        }
        Stmt::Assign { name, value, .. } => {
            uses.insert(name.clone(), idx);
            expr_uses(value, uses, idx);
        }
        Stmt::FieldAssign {
            obj, field, value, ..
        } => {
            uses.insert(obj.clone(), idx);
            uses.insert(field.clone(), idx);
            expr_uses(value, uses, idx);
        }
        Stmt::Expr(expr) => expr_uses(expr, uses, idx),
        Stmt::Return { value, .. } => {
            if let Some(v) = value {
                expr_uses(v, uses, idx);
            }
        }
        Stmt::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            expr_uses(cond, uses, idx);
            block_uses(then_block, uses, idx);
            if let Some(ElseBranch::If(s)) = else_block {
                collect_uses(s, uses, idx);
            }
            if let Some(ElseBranch::Block(b, _)) = else_block {
                block_uses(b, uses, idx);
            }
        }
        Stmt::While { cond, body, .. } => {
            expr_uses(cond, uses, idx);
            block_uses(body, uses, idx);
        }
        Stmt::For {
            var,
            iterable,
            body,
            ..
        } => {
            let _ = var;
            expr_uses(iterable, uses, idx);
            block_uses(body, uses, idx);
        }
        Stmt::Break { .. } | Stmt::Continue { .. } | Stmt::Yield { .. } => {}
        Stmt::TaskGroup { body, .. } => block_uses(body, uses, idx),
        Stmt::Go { call, .. } => expr_uses(call, uses, idx),
        Stmt::Assert { expr, .. } => expr_uses(expr, uses, idx),
    }
}

pub(crate) fn op_str(op: BinOp) -> &'static str {
    use BinOp::*;
    match op {
        Add => "+",
        Sub => "-",
        Mul => "*",
        Div => "/",
        Mod => "%",
        Eq => "==",
        Ne => "!=",
        Lt => "<",
        Le => "<=",
        Gt => ">",
        Ge => ">=",
        And => "and",
        Or => "or",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sole_parser::parse;

    fn check_ok(src: &str) {
        let p = parse(src).expect("parse");
        check(&p).expect("typecheck");
    }

    fn check_err(src: &str) -> String {
        let p = parse(src).expect("parse");
        match check(&p) {
            Ok(()) => panic!("expected type error for:\n{}", src),
            Err(e) => e.diag.render(Lang::En),
        }
    }

    // ---- M1: basics ----

    #[test]
    fn annotated_let_matches_literal() {
        check_ok("let x: int = 42\n");
        check_ok("let x: float = 1.5\n");
        check_ok("let x: bool = true\n");
        check_ok("let x: str = \"hi\"\n");
    }

    #[test]
    fn annotated_let_mismatch_is_an_error() {
        let msg = check_err("let x: int = \"hi\"\n");
        assert_eq!(
            msg,
            "1:1: [E0301] type mismatch in `let x`: expected `int`, got `str`"
        );
    }

    #[test]
    fn unannotated_let_infers_and_checks_assign() {
        check_ok("let mut x = 1\nx = 2\n");
        let msg = check_err("let mut x = 1\nx = \"hi\"\n");
        assert_eq!(
            msg,
            "2:1: [E0302] type mismatch in assignment to `x`: expected `int`, got `str`"
        );
    }

    #[test]
    fn assign_to_undefined_variable_is_an_error() {
        let msg = check_err("x = 1\n");
        assert_eq!(msg, "1:1: [E0201] undefined variable `x`");
    }

    #[test]
    fn call_arg_types_are_checked() {
        check_ok("fn f(a: int) -> int:\n    return a\nprint(f(1))\n");
        let msg = check_err("fn f(a: int) -> int:\n    return a\nprint(f(\"hi\"))\n");
        assert_eq!(
            msg,
            "3:8: [E0303] type mismatch in argument 1 of `f`: expected `int`, got `str`"
        );
    }

    #[test]
    fn call_arg_count_is_checked() {
        let msg = check_err("fn f(a: int) -> int:\n    return a\nprint(f())\n");
        assert_eq!(msg, "3:8: [E0211] function `f` expects 1 arguments, got 0");
    }

    #[test]
    fn return_type_is_checked() {
        check_ok("fn f(a: int) -> int:\n    return a\n");
        let msg = check_err("fn f(a: int) -> int:\n    return \"hi\"\n");
        assert_eq!(
            msg,
            "2:5: [E0304] type mismatch in return of `f`: expected `int`, got `str`"
        );
    }

    #[test]
    fn operator_types_are_checked() {
        check_ok("print(1 + 2)\n");
        check_ok("print(\"a\" + \"b\")\n");
        check_ok("print(1 + 2.5)\n");
        check_ok("print(1 == 1)\n");
        check_ok("print(true and false)\n");
        let msg = check_err("print(1 + \"hi\")\n");
        assert_eq!(
            msg,
            "1:9: [E0305] operator `+` does not support type `int and str`"
        );
        let msg = check_err("print(1 == \"hi\")\n");
        assert!(msg.contains("[E0305]"));
    }

    #[test]
    fn conditions_must_be_truthy_types() {
        check_ok("if true:\n    print(1)\n");
        check_ok("while 0:\n    break\n");
        let msg = check_err("fn f() -> int:\n    return 1\nif f:\n    print(1)\n");
        assert!(msg.contains("[E0305]"));
    }

    #[test]
    fn for_iterable_must_be_range() {
        check_ok("for i in range(3):\n    print(i)\n");
        let msg = check_err("for i in 5:\n    print(i)\n");
        assert!(msg.contains("[E0305]"));
    }

    #[test]
    fn undefined_variable_in_expr_is_an_error() {
        let msg = check_err("print(x)\n");
        assert_eq!(msg, "1:7: [E0201] undefined variable `x`");
    }

    #[test]
    fn recursive_and_mutual_functions_pass() {
        check_ok(
            "fn fib(n: int) -> int:\n    if n < 2:\n        return n\n    return fib(n - 1) + fib(n - 2)\n",
        );
    }

    #[test]
    fn unknown_types_are_skipped_not_rejected() {
        let msg = check_err("let x: Foo = 42\n");
        assert!(msg.contains("[E0306]"), "msg: {msg}");
    }

    #[test]
    fn condition_position_in_error() {
        let msg = check_err("let x: bool = 1\n");
        assert_eq!(
            msg,
            "1:1: [E0301] type mismatch in `let x`: expected `bool`, got `int`"
        );
    }

    // ---- M2: List ----

    #[test]
    fn list_literals_and_types() {
        check_ok("let xs: List[int] = [1, 2, 3]\n");
        check_ok("let xs = [1, 2, 3]\n");
        let msg = check_err("let xs: List[int] = [1, \"hi\"]\n");
        assert!(msg.contains("[E0312]"), "msg: {msg}");
        let msg = check_err("let xs = []\n");
        assert!(msg.contains("[E0313]"), "msg: {msg}");
    }

    #[test]
    fn list_indexing() {
        check_ok("let xs = [1, 2]\nprint(xs[0])\n");
        let msg = check_err("let xs = [1, 2]\nprint(xs[\"a\"])\n");
        assert!(msg.contains("[E0315]"), "msg: {msg}");
        let msg = check_err("let x = 5\nprint(x[0])\n");
        assert!(msg.contains("[E0314]"), "msg: {msg}");
    }

    #[test]
    fn list_methods() {
        check_ok("let xs = [1]\nxs.push(2)\nprint(xs.len())\nprint(xs.get(0))\nxs.set(0, 9)\n");
        let msg = check_err("let xs: List[int] = [1]\nxs.push(\"hi\")\n");
        assert!(msg.contains("[E0303]"), "msg: {msg}");
        let msg = check_err("let xs = [1]\nxs.unknown_method()\n");
        assert!(msg.contains("[E0309]"), "msg: {msg}");
    }

    #[test]
    fn for_over_list() {
        check_ok("let xs = [1, 2, 3]\nfor x in xs:\n    print(x)\n");
        check_ok("let xs = [1, 2, 3]\nfor x in ref xs:\n    print(x)\nprint(xs.len())\n");
        let msg = check_err("for x in 5:\n    print(x)\n");
        assert!(msg.contains("[E0305]"), "msg: {msg}");
    }

    // ---- M2: move semantics ----

    #[test]
    fn use_after_move_is_an_error() {
        let msg = check_err("let a = [1, 2]\nlet b = a\nprint(a.len())\n");
        assert_eq!(
            msg,
            "3:7: [E0401] use of moved value `a` (move it back or borrow instead)"
        );
    }

    #[test]
    fn copy_types_are_not_moved() {
        check_ok("let a = 1\nlet b = a\nprint(a)\n");
        check_ok("let a = \"s\"\nlet b = a\nprint(a)\n");
    }

    #[test]
    fn move_into_function_call() {
        check_ok("fn consume(xs: List[int]) -> int:\n    return xs.len()\nlet xs = [1]\nprint(consume(xs))\n");
        let msg = check_err(
            "fn consume(xs: List[int]) -> int:\n    return xs.len()\nlet xs = [1]\nprint(consume(xs))\nprint(xs.len())\n",
        );
        assert!(msg.contains("[E0401]"), "msg: {msg}");
    }

    #[test]
    fn assignment_resurrects_moved_variable() {
        check_ok("let mut x = [1]\nlet y = x\nx = [2]\nprint(x.len())\nprint(y.len())\n");
    }

    #[test]
    fn self_move_in_assignment_rhs() {
        // `x = f(x)`: the RHS may consume `x`; the assignment binds a
        // fresh value.
        check_ok(
            "fn id(xs: List[int]) -> List[int]:\n    return xs\nlet mut x = [1]\nx = id(x)\nprint(x.len())\n",
        );
        // Moving an already-moved variable still fails.
        let msg = check_err(
            "fn id(xs: List[int]) -> List[int]:\n    return xs\nlet mut x = [1]\nlet y = x\nx = id(x)\n",
        );
        assert!(msg.contains("[E0401]"), "msg: {msg}");
        // Moving `x` twice inside one RHS still fails.
        let msg = check_err(
            "fn pair(a: List[int], b: List[int]) -> List[int]:\n    return a\nlet mut x = [1]\nx = pair(x, x)\n",
        );
        assert!(msg.contains("[E0401]"), "msg: {msg}");
        // Writing a field on an object the RHS consumed fails.
        let msg = check_err(
            "struct P:\n    x: int\nfn take(a: P) -> int:\n    return a.x\nlet mut p = P(1)\np.x = take(p)\n",
        );
        assert!(msg.contains("[E0401]"), "msg: {msg}");
        // Borrowing via a `ref` parameter is fine.
        check_ok(
            "struct P:\n    x: int\nfn peek(a: ref P) -> int:\n    return a.x\nlet mut p = P(1)\np.x = peek(p)\nprint(p.x)\n",
        );
    }

    // ---- M2: borrows ----

    #[test]
    fn ref_parameter_does_not_move() {
        check_ok(
            "fn len(xs: ref List[int]) -> int:\n    return xs.len()\nlet xs = [1, 2]\nprint(len(xs))\nprint(xs.len())\n",
        );
    }

    #[test]
    fn mut_ref_parameter_can_mutate() {
        check_ok(
            "fn bump(xs: mut ref List[int]) -> int:\n    xs.push(1)\n    return 0\nlet mut xs = [1]\nbump(xs)\nprint(xs.len())\n",
        );
    }

    #[test]
    fn explicit_ref_borrows_are_persistent() {
        // NLL: borrow lives until the borrow variable's last use.
        // Move while the borrow variable is still alive → error.
        let msg = check_err("let a = [1, 2]\nlet r = ref a\nlet b = a\nprint(r.len())\n");
        assert!(msg.contains("[E0402]"), "msg: {msg}");
        // Move after the borrow variable's last use → allowed.
        check_ok("let a = [1, 2]\nlet r = ref a\nprint(r.len())\nlet b = a\nprint(b.len())\n");
    }

    #[test]
    fn mutable_borrow_conflicts() {
        // r1 alive when r2 is created → conflict.
        let msg =
            check_err("let a = [1, 2]\nlet r1 = ref a\nlet r2 = mut ref a\nprint(r1.len())\n");
        assert!(msg.contains("[E0403]"), "msg: {msg}");
    }

    #[test]
    fn borrow_escape_is_an_error() {
        let msg = check_err("fn f() -> ref int:\n    let x = 5\n    return ref x\n");
        assert_eq!(
            msg,
            "3:5: [E0404] cannot return a reference to a local value (it would dangle)"
        );
    }

    // ---- M2 收尾: lexical borrow regions ----

    #[test]
    fn outer_borrow_survives_blocks() {
        // r's last use is after the block; a move right after it is fine (NLL).
        check_ok("let a = [1, 2]\nlet r = ref a\nif true:\n    print(1)\nprint(r.len())\nlet b = a\nprint(b.len())\n");
        // r still alive (used later) → move is an error.
        let msg = check_err("let a = [1, 2]\nlet r = ref a\nif true:\n    print(r.len())\nlet b = a\nprint(r.len())\n");
        assert!(msg.contains("[E0402]"), "msg: {msg}");
    }

    #[test]
    fn inner_borrow_dies_with_block() {
        check_ok("let a = [1, 2]\nif true:\n    let r = ref a\nlet b = a\nprint(b.len())\n");
    }

    #[test]
    fn inner_ref_binding_dies_with_block() {
        let msg = check_err("let a = [1, 2]\nif true:\n    let r = ref a\nprint(r.len())\n");
        assert!(msg.contains("[E0401]"), "msg: {msg}");
    }

    #[test]
    fn nested_blocks_release_inner_borrows() {
        check_ok(
            "let a = [1, 2]\nif true:\n    if true:\n        let r = ref a\nlet b = a\nprint(b.len())\n",
        );
    }

    #[test]
    fn while_body_borrow_dies_with_loop() {
        check_ok("let a = [1, 2]\nlet mut n = 0\nwhile n < 1:\n    let r = ref a\n    n = n + 1\nlet b = a\nprint(b.len())\n");
    }

    // ---- M4 收尾: if 分支合并(发散分支不参与)----

    #[test]
    fn move_in_diverging_branch_does_not_leak() {
        // `parts` is moved inside a branch that always returns; the
        // fall-through path never runs it, so `parts` stays usable after.
        check_ok(
            "fn f(root: Json, parts: List[Json], value: Json) -> Result[Json, str]:
    let head = parts[0]
    if head.is_int():
        let r = parts
        print(r.len())
        return Ok(value)
    let rest = parts
    print(rest.len())
    return Ok(head)
",
        );
    }

    #[test]
    fn move_in_else_branch_does_not_leak_when_it_returns() {
        check_ok(
            "fn f(parts: List[Json]) -> Json:
    let head = parts[0]
    if head.is_int():
        print(head)
    else:
        let r = parts
        return r[0]
    let rest = parts
    return rest[0]
",
        );
    }

    #[test]
    fn tail_if_with_both_branches_returning_diverges() {
        check_ok(
            "fn f(parts: List[Json]) -> Json:
    let head = parts[0]
    if head.is_int():
        if head.to_str() == \"1\":
            let r = parts
            return r[0]
        else:
            return head
    let rest = parts
    return rest[0]
",
        );
    }

    #[test]
    fn move_in_non_diverging_branch_still_leaks() {
        let msg = check_err(
            "fn f(parts: List[Json]) -> Json:
    let head = parts[0]
    if head.is_int():
        let r = parts
        print(r.len())
    let rest = parts
    return rest[0]
",
        );
        assert!(msg.contains("[E0401]"), "msg: {msg}");
    }

    #[test]
    fn returning_a_reference_parameter_is_allowed() {
        check_ok("fn first(xs: ref List[int]) -> ref List[int]:\n    return xs\n");
        check_ok("struct Box:\n    v: int\nfn get(b: ref Box) -> ref int:\n    return ref b.v\n");
    }

    // ---- M2: structs, interfaces, methods ----

    #[test]
    fn struct_construction_and_fields() {
        check_ok(
            "struct Point:\n    x: int\n    y: int\nlet p = Point(1, 2)\nprint(p.x)\nprint(p.y)\n",
        );
        let msg = check_err("struct Point:\n    x: int\nlet p = Point(1)\nprint(p.z)\n");
        assert!(msg.contains("[E0308]"), "msg: {msg}");
        let msg = check_err("struct Point:\n    x: int\n    y: int\nlet p = Point(1, \"a\")\n");
        assert!(msg.contains("[E0303]"), "msg: {msg}");
    }

    #[test]
    fn struct_construction_arg_count() {
        let msg = check_err("struct Point:\n    x: int\nlet p = Point(1, 2, 3)\n");
        assert!(msg.contains("[E0211]"), "msg: {msg}");
    }

    #[test]
    fn interface_implementation_checked() {
        let src = "interface Shape:\n    fn area(self: ref Shape) -> float\nstruct Circle:\n    r: float\nimpl Circle: Shape:\n    fn area(self: ref Circle) -> float:\n        return 3.14 * self.r * self.r\nlet c = Circle(2.0)\nprint(c.area())\n";
        check_ok(src);
    }

    #[test]
    fn missing_impl_method_is_an_error() {
        let msg = check_err("interface Shape:\n    fn area(self: ref Shape) -> float\nstruct Circle:\n    r: float\nimpl Circle: Shape:\n    fn perimeter(self: ref Circle) -> float:\n        return 2.0\n");
        assert!(msg.contains("[E0311]"), "msg: {msg}");
    }

    #[test]
    fn interface_type_accepts_implementing_struct() {
        let src = "interface Shape:\n    fn area(self: ref Shape) -> float\nstruct Circle:\n    r: float\nimpl Circle: Shape:\n    fn area(self: ref Circle) -> float:\n        return 3.14 * self.r * self.r\nfn describe(s: ref Shape) -> float:\n    return s.area()\nlet c = Circle(1.0)\nprint(describe(ref c))\n";
        check_ok(src);
    }

    #[test]
    fn method_requires_self() {
        let msg = check_err(
            "struct Point:\n    x: int\nimpl Point:\n    fn dist() -> int:\n        return 1\n",
        );
        assert!(msg.contains("[E0305]"), "msg: {msg}");
    }

    #[test]
    fn unknown_method_is_an_error() {
        let msg = check_err("struct Point:\n    x: int\nlet p = Point(1)\np.unknown()\n");
        assert!(msg.contains("[E0309]"), "msg: {msg}");
    }
}
