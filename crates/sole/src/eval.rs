//! M1 tree-walking interpreter for Sole.
//!
//! This is the reference semantics implementation. It deliberately keeps
//! things small: values, a scope-based environment with mutability tracking,
//! and a small set of builtins (`print`, `range`).

use sole_diag::{Diagnostic, Lang, Msg};
use sole_parser::{BinOp, Block, ElseBranch, Expr, FnDef, Item, Program, Stmt, UnOp};
use std::collections::HashMap;
use std::io::Write;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    Range { start: i64, end: i64 },
    Fn(usize),
    Unit,
}

#[derive(Debug, Clone)]
pub struct Binding {
    pub value: Value,
    pub mutable: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EvalError {
    pub diag: Diagnostic,
}

impl std::fmt::Display for EvalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.diag.render(Lang::current()))
    }
}

impl std::error::Error for EvalError {}

fn err(msg: Msg) -> EvalError {
    EvalError {
        diag: Diagnostic::new(msg, 0, 0),
    }
}

struct Env {
    global: HashMap<String, Binding>,
    locals: Vec<HashMap<String, Binding>>,
}

impl Env {
    fn new() -> Self {
        Self {
            global: HashMap::new(),
            locals: Vec::new(),
        }
    }

    fn lookup(&self, name: &str) -> Option<Value> {
        for scope in self.locals.iter().rev() {
            if let Some(b) = scope.get(name) {
                return Some(b.value.clone());
            }
        }
        self.global.get(name).map(|b| b.value.clone())
    }

    fn insert_local(&mut self, name: String, value: Value, mutable: bool) {
        let binding = Binding { value, mutable };
        match self.locals.last_mut() {
            Some(scope) => {
                scope.insert(name, binding);
            }
            None => {
                self.global.insert(name, binding);
            }
        }
    }

    fn push_scope(&mut self) {
        self.locals.push(HashMap::new());
    }

    fn set(&mut self, name: &str, value: Value) -> Result<(), EvalError> {
        for scope in self.locals.iter_mut().rev() {
            if let Some(b) = scope.get_mut(name) {
                return Self::set_binding(b, name, value);
            }
        }
        if let Some(b) = self.global.get_mut(name) {
            return Self::set_binding(b, name, value);
        }
        Err(err(Msg::UndefinedVariable(name.to_string())))
    }

    fn set_binding(b: &mut Binding, name: &str, value: Value) -> Result<(), EvalError> {
        if !b.mutable {
            return Err(err(Msg::ImmutableReassign(name.to_string())));
        }
        b.value = value;
        Ok(())
    }
}

enum Flow {
    Next,
    Break,
    Continue,
    Return(Value),
}

/// Evaluates a parsed program, writing printed output to `out`.
pub fn run(program: &Program, out: &mut dyn Write) -> Result<(), EvalError> {
    let mut env = Env::new();
    for (idx, item) in program.items.iter().enumerate() {
        if let Item::Fn(f) = item {
            env.global.insert(
                f.name.clone(),
                Binding {
                    value: Value::Fn(idx),
                    mutable: false,
                },
            );
        }
    }
    for item in &program.items {
        match item {
            Item::Fn(_) => {}
            Item::Stmt(stmt) => {
                eval_stmt(stmt, program, &mut env, &mut *out)?;
            }
        }
    }
    Ok(())
}

fn eval_stmt(
    stmt: &Stmt,
    program: &Program,
    env: &mut Env,
    out: &mut dyn Write,
) -> Result<Flow, EvalError> {
    match stmt {
        Stmt::Let {
            name,
            is_mut,
            value,
            ..
        } => {
            let v = eval_expr(value, program, env, &mut *out)?;
            env.insert_local(name.clone(), v, *is_mut);
            Ok(Flow::Next)
        }
        Stmt::Assign { name, value } => {
            let v = eval_expr(value, program, env, &mut *out)?;
            env.set(name, v)?;
            Ok(Flow::Next)
        }
        Stmt::Expr(expr) => {
            eval_expr(expr, program, env, &mut *out)?;
            Ok(Flow::Next)
        }
        Stmt::Return(value) => {
            let value = match value {
                Some(expr) => eval_expr(expr, program, env, &mut *out)?,
                None => Value::Unit,
            };
            Ok(Flow::Return(value))
        }
        Stmt::If {
            cond,
            then_block,
            else_block,
        } => {
            let c = eval_expr(cond, program, env, &mut *out)?;
            if truthy(&c)? {
                eval_block(then_block, program, env, &mut *out)
            } else {
                match else_block {
                    Some(ElseBranch::If(stmt)) => eval_stmt(stmt, program, env, &mut *out),
                    Some(ElseBranch::Block(block)) => eval_block(block, program, env, &mut *out),
                    None => Ok(Flow::Next),
                }
            }
        }
        Stmt::While { cond, body } => {
            loop {
                let c = eval_expr(cond, program, env, &mut *out)?;
                if !truthy(&c)? {
                    break;
                }
                match eval_block(body, program, env, &mut *out)? {
                    Flow::Next | Flow::Continue => {}
                    Flow::Break => break,
                    Flow::Return(v) => return Ok(Flow::Return(v)),
                }
            }
            Ok(Flow::Next)
        }
        Stmt::For {
            var,
            is_mut,
            iterable,
            body,
            ..
        } => {
            let it = eval_expr(iterable, program, env, &mut *out)?;
            match it {
                Value::Range { start, end } => {
                    for i in start..end {
                        env.insert_local(var.clone(), Value::Int(i), *is_mut);
                        match eval_block(body, program, env, &mut *out)? {
                            Flow::Next | Flow::Continue => {}
                            Flow::Break => break,
                            Flow::Return(v) => return Ok(Flow::Return(v)),
                        }
                    }
                }
                other => {
                    return Err(err(Msg::ForNotSupported(format_value(&other))));
                }
            }
            Ok(Flow::Next)
        }
        Stmt::Break => Ok(Flow::Break),
        Stmt::Continue => Ok(Flow::Continue),
    }
}

fn eval_block(
    block: &Block,
    program: &Program,
    env: &mut Env,
    out: &mut dyn Write,
) -> Result<Flow, EvalError> {
    for stmt in &block.stmts {
        match eval_stmt(stmt, program, env, &mut *out)? {
            Flow::Next => {}
            other => return Ok(other),
        }
    }
    Ok(Flow::Next)
}

fn eval_expr(
    expr: &Expr,
    program: &Program,
    env: &mut Env,
    out: &mut dyn Write,
) -> Result<Value, EvalError> {
    match expr {
        Expr::Int(n) => Ok(Value::Int(*n)),
        Expr::Float(f) => Ok(Value::Float(*f)),
        Expr::Str(s) => Ok(Value::Str(s.clone())),
        Expr::Bool(b) => Ok(Value::Bool(*b)),
        Expr::Ident(name) => env
            .lookup(name)
            .ok_or_else(|| err(Msg::UndefinedVariable(name.clone()))),
        Expr::Unary { op, expr } => {
            let v = eval_expr(expr, program, env, &mut *out)?;
            match op {
                UnOp::Neg => match v {
                    Value::Int(n) => Ok(Value::Int(-n)),
                    Value::Float(f) => Ok(Value::Float(-f)),
                    _ => Err(err(Msg::NotNegatable)),
                },
                UnOp::Not => Ok(Value::Bool(!truthy(&v)?)),
            }
        }
        Expr::Binary { op, lhs, rhs } => {
            if matches!(op, BinOp::And | BinOp::Or) {
                // Short-circuit evaluation (provisional Python-like semantics).
                let l = eval_expr(lhs, program, env, &mut *out)?;
                let lv = truthy(&l)?;
                if *op == BinOp::And {
                    return if lv {
                        let r = eval_expr(rhs, program, env, &mut *out)?;
                        Ok(Value::Bool(truthy(&r)?))
                    } else {
                        Ok(Value::Bool(false))
                    };
                }
                return if lv {
                    Ok(Value::Bool(true))
                } else {
                    let r = eval_expr(rhs, program, env, &mut *out)?;
                    Ok(Value::Bool(truthy(&r)?))
                };
            }
            let l = eval_expr(lhs, program, env, &mut *out)?;
            let r = eval_expr(rhs, program, env, &mut *out)?;
            eval_binary(*op, l, r)
        }
        Expr::Call { callee, args } => {
            if let Expr::Ident(name) = callee.as_ref() {
                if let Some(v) = call_builtin(name, args, program, env, &mut *out)? {
                    return Ok(v);
                }
            }
            let callee_val = eval_expr(callee, program, env, &mut *out)?;
            match callee_val {
                Value::Fn(idx) => {
                    let item = &program.items[idx];
                    let Item::Fn(fdef) = item else {
                        return Err(err(Msg::InternalFnIndex));
                    };
                    call_fn(fdef, args, program, env, &mut *out)
                }
                other => Err(err(Msg::BadCall(format_value(&other)))),
            }
        }
        Expr::Field { .. } => Err(err(Msg::FieldNotImplemented)),
        Expr::Index { .. } => Err(err(Msg::IndexNotImplemented)),
    }
}

fn call_fn(
    fdef: &FnDef,
    args: &[Expr],
    program: &Program,
    env: &mut Env,
    out: &mut dyn Write,
) -> Result<Value, EvalError> {
    if args.len() != fdef.params.len() {
        return Err(err(Msg::ArgCount(
            fdef.name.clone(),
            fdef.params.len(),
            args.len(),
        )));
    }
    let mut arg_values = Vec::new();
    for a in args {
        arg_values.push(eval_expr(a, program, env, &mut *out)?);
    }
    // Functions see globals + their own params, never caller locals.
    let saved = std::mem::take(&mut env.locals);
    env.push_scope();
    for (p, v) in fdef.params.iter().zip(arg_values) {
        env.insert_local(p.name.clone(), v, p.is_mut);
    }
    let result = eval_block(&fdef.body, program, env, &mut *out);
    env.locals = saved;
    match result? {
        Flow::Return(v) => Ok(v),
        _ => Ok(Value::Unit),
    }
}

/// Dispatches builtin functions. Returns `Ok(None)` when `name` is not a
/// builtin, so the caller can fall back to user-defined functions.
fn call_builtin(
    name: &str,
    args: &[Expr],
    program: &Program,
    env: &mut Env,
    out: &mut dyn Write,
) -> Result<Option<Value>, EvalError> {
    match name {
        "print" => {
            let mut parts = Vec::new();
            for a in args {
                let v = eval_expr(a, program, env, &mut *out)?;
                parts.push(format_value(&v));
            }
            writeln!(out, "{}", parts.join(" ")).map_err(|e| err(Msg::Io(e.to_string())))?;
            Ok(Some(Value::Unit))
        }
        "range" => {
            let mut nums = Vec::new();
            for a in args {
                match eval_expr(a, program, env, &mut *out)? {
                    Value::Int(n) => nums.push(n),
                    _ => return Err(err(Msg::RangeNotInt)),
                }
            }
            let (start, end) = match nums.as_slice() {
                [end] => (0, *end),
                [start, end] => (*start, *end),
                _ => return Err(err(Msg::RangeArgCount)),
            };
            Ok(Some(Value::Range { start, end }))
        }
        _ => Ok(None),
    }
}

fn eval_binary(op: BinOp, l: Value, r: Value) -> Result<Value, EvalError> {
    use BinOp::*;
    match op {
        Add | Sub | Mul | Div | Mod => match (l, r) {
            (Value::Int(a), Value::Int(b)) => match op {
                Add => Ok(Value::Int(a.wrapping_add(b))),
                Sub => Ok(Value::Int(a.wrapping_sub(b))),
                Mul => Ok(Value::Int(a.wrapping_mul(b))),
                Div => {
                    if b == 0 {
                        return Err(err(Msg::DivByZero));
                    }
                    Ok(Value::Int(a / b))
                }
                Mod => {
                    if b == 0 {
                        return Err(err(Msg::ModByZero));
                    }
                    Ok(Value::Int(a % b))
                }
                _ => unreachable!("arithmetic op"),
            },
            (Value::Int(a), Value::Float(b)) => float_op(op, a as f64, b),
            (Value::Float(a), Value::Int(b)) => float_op(op, a, b as f64),
            (Value::Float(a), Value::Float(b)) => float_op(op, a, b),
            (Value::Str(a), Value::Str(b)) if op == Add => Ok(Value::Str(format!("{}{}", a, b))),
            _ => Err(err(Msg::TypeMismatch)),
        },
        Eq | Ne | Lt | Le | Gt | Ge => cmp_op(op, &l, &r),
        And | Or => unreachable!("handled in eval_expr"),
    }
}

fn float_op(op: BinOp, a: f64, b: f64) -> Result<Value, EvalError> {
    use BinOp::*;
    let v = match op {
        Add => a + b,
        Sub => a - b,
        Mul => a * b,
        Div => a / b,
        Mod => a % b,
        _ => unreachable!("float arithmetic only"),
    };
    Ok(Value::Float(v))
}

fn cmp_op(op: BinOp, l: &Value, r: &Value) -> Result<Value, EvalError> {
    use BinOp::*;
    let result = match (l, r) {
        (Value::Int(a), Value::Int(b)) => match op {
            Eq => *a == *b,
            Ne => *a != *b,
            Lt => *a < *b,
            Le => *a <= *b,
            Gt => *a > *b,
            Ge => *a >= *b,
            _ => unreachable!("comparison op"),
        },
        (Value::Int(a), Value::Float(b)) => num_cmp(op, *a as f64, *b),
        (Value::Float(a), Value::Int(b)) => num_cmp(op, *a, *b as f64),
        (Value::Float(a), Value::Float(b)) => num_cmp(op, *a, *b),
        (Value::Str(a), Value::Str(b)) => match op {
            Eq => a == b,
            Ne => a != b,
            Lt => a < b,
            Le => a <= b,
            Gt => a > b,
            Ge => a >= b,
            _ => unreachable!("comparison op"),
        },
        (Value::Bool(a), Value::Bool(b)) => match op {
            Eq => a == b,
            Ne => a != b,
            _ => return Err(err(Msg::BoolOrderCmp)),
        },
        _ => return Err(err(Msg::CmpMismatch)),
    };
    Ok(Value::Bool(result))
}

fn num_cmp(op: BinOp, a: f64, b: f64) -> bool {
    use BinOp::*;
    match op {
        Eq => a == b,
        Ne => a != b,
        Lt => a < b,
        Le => a <= b,
        Gt => a > b,
        Ge => a >= b,
        _ => unreachable!("comparison op"),
    }
}

fn truthy(v: &Value) -> Result<bool, EvalError> {
    match v {
        Value::Bool(b) => Ok(*b),
        Value::Int(n) => Ok(*n != 0),
        Value::Float(f) => Ok(*f != 0.0),
        Value::Str(s) => Ok(!s.is_empty()),
        _ => Err(err(Msg::BadCondition)),
    }
}

fn format_value(v: &Value) -> String {
    match v {
        Value::Int(n) => n.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Str(s) => s.clone(),
        Value::Range { start, end } => format!("range({}, {})", start, end),
        Value::Fn(idx) => format!("<fn {}>", idx),
        Value::Unit => String::new(),
    }
}
