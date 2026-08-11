//! M2 tree-walking interpreter for Sole.
//!
//! Values live in `Rc<RefCell<Value>>` cells so that references (`ref`,
//! `mut ref`) share storage. `Value::Ref`/`Value::MutRef` wrap a shared
//! cell; reading dereferences it, writing through `MutRef` mutates the
//! target. Methods dispatch through the program's `impl` blocks; `List` has
//! builtin methods (`len`/`push`/`get`/`set`).

use sole_diag::{Diagnostic, Lang, Msg};
use sole_parser::{BinOp, Block, ElseBranch, Expr, FnDef, Item, Program, Span, Stmt, Type, UnOp};
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write;
use std::rc::Rc;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    Range {
        start: i64,
        end: i64,
    },
    List(Vec<Value>),
    Struct {
        name: String,
        fields: Vec<(String, Value)>,
    },
    Ref(Rc<RefCell<Value>>),
    MutRef(Rc<RefCell<Value>>),
    Fn(usize),
    Unit,
}

impl Value {
    fn deref(&self) -> Value {
        match self {
            Value::Ref(cell) | Value::MutRef(cell) => cell.borrow().clone(),
            other => other.clone(),
        }
    }

    fn type_tag(&self) -> &'static str {
        match self {
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Bool(_) => "bool",
            Value::Str(_) => "str",
            Value::Range { .. } => "Range",
            Value::List(_) => "List",
            Value::Struct { .. } => "struct",
            Value::Ref(_) | Value::MutRef(_) => "ref",
            Value::Fn(_) => "fn",
            Value::Unit => "()",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Binding {
    pub cell: Rc<RefCell<Value>>,
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

fn err_at(msg: Msg, span: Span) -> EvalError {
    EvalError {
        diag: Diagnostic::new(msg, span.line, span.column),
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

    fn lookup_cell(&self, name: &str) -> Option<Rc<RefCell<Value>>> {
        for scope in self.locals.iter().rev() {
            if let Some(b) = scope.get(name) {
                return Some(b.cell.clone());
            }
        }
        self.global.get(name).map(|b| b.cell.clone())
    }

    fn insert_local(&mut self, name: String, value: Value, mutable: bool) {
        let binding = Binding {
            cell: Rc::new(RefCell::new(value)),
            mutable,
        };
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
        *b.cell.borrow_mut() = value;
        Ok(())
    }
}

enum Flow {
    Next,
    Break,
    Continue,
    Return(Value),
}

struct MethodTable {
    methods: HashMap<(String, String), usize>,
    structs: HashMap<String, Vec<String>>,
}

impl MethodTable {
    fn from_program(program: &Program) -> Self {
        let mut methods = HashMap::new();
        let mut structs = HashMap::new();
        for (idx, item) in program.items.iter().enumerate() {
            match item {
                Item::Struct(s) => {
                    structs.insert(
                        s.name.clone(),
                        s.fields.iter().map(|(n, _)| n.clone()).collect(),
                    );
                }
                Item::Impl(imp) => {
                    for m in &imp.methods {
                        methods.insert((imp.ty.clone(), m.name.clone()), idx);
                    }
                }
                _ => {}
            }
        }
        Self { methods, structs }
    }

    fn lookup(&self, ty: &str, method: &str) -> Option<usize> {
        self.methods
            .get(&(ty.to_string(), method.to_string()))
            .copied()
    }
}

/// Evaluates a parsed program, writing printed output to `out`.
pub fn run(program: &Program, out: &mut dyn Write) -> Result<(), EvalError> {
    let mut env = Env::new();
    let table = MethodTable::from_program(program);
    for (idx, item) in program.items.iter().enumerate() {
        if let Item::Fn(f) = item {
            env.global.insert(
                f.name.clone(),
                Binding {
                    cell: Rc::new(RefCell::new(Value::Fn(idx))),
                    mutable: false,
                },
            );
        }
    }
    for item in &program.items {
        match item {
            Item::Fn(_) | Item::Struct(_) | Item::Interface(_) | Item::Impl(_) => {}
            Item::Stmt(stmt) => {
                eval_stmt(stmt, program, &mut env, &table, &mut *out)?;
            }
        }
    }
    Ok(())
}

fn eval_stmt(
    stmt: &Stmt,
    program: &Program,
    env: &mut Env,
    table: &MethodTable,
    out: &mut dyn Write,
) -> Result<Flow, EvalError> {
    match stmt {
        Stmt::Let {
            name,
            is_mut,
            value,
            span,
            ..
        } => {
            let v =
                eval_expr(value, program, env, table, &mut *out).map_err(|e| attach(e, *span))?;
            env.insert_local(name.clone(), v, *is_mut);
            Ok(Flow::Next)
        }
        Stmt::Assign { name, value, span } => {
            let v =
                eval_expr(value, program, env, table, &mut *out).map_err(|e| attach(e, *span))?;
            // Assignment through a `mut ref` writes the target cell.
            let current = env.lookup_cell(name).map(|c| c.borrow().clone());
            match current {
                Some(Value::MutRef(target)) => {
                    *target.borrow_mut() = v;
                }
                Some(Value::Ref(_)) => {
                    return Err(err_at(Msg::ImmutableReassign(name.clone()), *span))
                }
                _ => env.set(name, v).map_err(|e| attach(e, *span))?,
            }
            Ok(Flow::Next)
        }
        Stmt::FieldAssign {
            obj,
            field,
            value,
            span,
        } => {
            let v =
                eval_expr(value, program, env, table, &mut *out).map_err(|e| attach(e, *span))?;
            let cell = env
                .lookup_cell(obj)
                .ok_or_else(|| err_at(Msg::UndefinedVariable(obj.clone()), *span))?;
            // Resolve through a `mut ref` binding (self.x = ... in methods).
            let cur0 = cell.borrow().clone();
            let target = match cur0 {
                Value::Ref(inner) | Value::MutRef(inner) => inner,
                _ => cell,
            };
            let cur = target.borrow().clone();
            let Value::Struct {
                name: sname,
                fields,
            } = cur
            else {
                return Err(err_at(
                    Msg::UnknownField {
                        ty: cur.type_tag().into(),
                        field: field.clone(),
                    },
                    *span,
                ));
            };
            let mut fields = fields;
            let Some(slot) = fields.iter_mut().find(|(n, _)| n == field) else {
                return Err(err_at(
                    Msg::UnknownField {
                        ty: sname,
                        field: field.clone(),
                    },
                    *span,
                ));
            };
            slot.1 = v;
            *target.borrow_mut() = Value::Struct {
                name: sname,
                fields,
            };
            Ok(Flow::Next)
        }
        Stmt::Expr(expr) => {
            eval_expr(expr, program, env, table, &mut *out)?;
            Ok(Flow::Next)
        }
        Stmt::Return { value, span } => {
            let value = match value {
                Some(expr) => {
                    eval_expr(expr, program, env, table, &mut *out).map_err(|e| attach(e, *span))?
                }
                None => Value::Unit,
            };
            Ok(Flow::Return(value))
        }
        Stmt::If {
            cond,
            then_block,
            else_block,
            span,
        } => {
            let c =
                eval_expr(cond, program, env, table, &mut *out).map_err(|e| attach(e, *span))?;
            if truthy(&c).map_err(|e| attach(e, *span))? {
                eval_block(then_block, program, env, table, &mut *out)
            } else {
                match else_block {
                    Some(ElseBranch::If(stmt)) => eval_stmt(stmt, program, env, table, &mut *out),
                    Some(ElseBranch::Block(block)) => {
                        eval_block(block, program, env, table, &mut *out)
                    }
                    None => Ok(Flow::Next),
                }
            }
        }
        Stmt::While { cond, body, span } => {
            loop {
                let c = eval_expr(cond, program, env, table, &mut *out)
                    .map_err(|e| attach(e, *span))?;
                if !truthy(&c).map_err(|e| attach(e, *span))? {
                    break;
                }
                match eval_block(body, program, env, table, &mut *out)? {
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
            span,
            ..
        } => {
            let it = eval_expr(iterable, program, env, table, &mut *out)
                .map_err(|e| attach(e, *span))?;
            let it = it.deref();
            match it {
                Value::Range { start, end } => {
                    for i in start..end {
                        env.insert_local(var.clone(), Value::Int(i), *is_mut);
                        match eval_block(body, program, env, table, &mut *out)? {
                            Flow::Next | Flow::Continue => {}
                            Flow::Break => break,
                            Flow::Return(v) => return Ok(Flow::Return(v)),
                        }
                    }
                }
                Value::List(items) => {
                    for item in items {
                        env.insert_local(var.clone(), item, *is_mut);
                        match eval_block(body, program, env, table, &mut *out)? {
                            Flow::Next | Flow::Continue => {}
                            Flow::Break => break,
                            Flow::Return(v) => return Ok(Flow::Return(v)),
                        }
                    }
                }
                other => {
                    return Err(err_at(Msg::ForNotSupported(format_value(&other)), *span));
                }
            }
            Ok(Flow::Next)
        }
        Stmt::Break { .. } => Ok(Flow::Break),
        Stmt::Continue { .. } => Ok(Flow::Continue),
    }
}

/// Fills in a missing position on an error with the given span.
fn attach(e: EvalError, span: Span) -> EvalError {
    if e.diag.line == 0 {
        EvalError {
            diag: Diagnostic::new(e.diag.msg, span.line, span.column),
        }
    } else {
        e
    }
}

fn eval_block(
    block: &Block,
    program: &Program,
    env: &mut Env,
    table: &MethodTable,
    out: &mut dyn Write,
) -> Result<Flow, EvalError> {
    for stmt in &block.stmts {
        match eval_stmt(stmt, program, env, table, &mut *out)? {
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
    table: &MethodTable,
    out: &mut dyn Write,
) -> Result<Value, EvalError> {
    match expr {
        Expr::Int(n, _) => Ok(Value::Int(*n)),
        Expr::Float(f, _) => Ok(Value::Float(*f)),
        Expr::Str(s, _) => Ok(Value::Str(s.clone())),
        Expr::Bool(b, _) => Ok(Value::Bool(*b)),
        Expr::List(items, span) => {
            let mut values = Vec::new();
            for it in items {
                values.push(
                    eval_expr(it, program, env, table, &mut *out).map_err(|e| attach(e, *span))?,
                );
            }
            Ok(Value::List(values))
        }
        Expr::Ident(name, span) => env
            .lookup_cell(name)
            .map(|c| c.borrow().clone())
            .ok_or_else(|| err_at(Msg::UndefinedVariable(name.clone()), *span)),
        Expr::Unary { op, expr, span } => {
            let v =
                eval_expr(expr, program, env, table, &mut *out).map_err(|e| attach(e, *span))?;
            match op {
                UnOp::Neg => match v {
                    Value::Int(n) => Ok(Value::Int(-n)),
                    Value::Float(f) => Ok(Value::Float(-f)),
                    _ => Err(err_at(Msg::NotNegatable, *span)),
                },
                UnOp::Not => Ok(Value::Bool(!truthy(&v).map_err(|e| attach(e, *span))?)),
            }
        }
        Expr::Binary { op, lhs, rhs, span } => {
            if matches!(op, BinOp::And | BinOp::Or) {
                // Short-circuit evaluation (provisional Python-like semantics).
                let l =
                    eval_expr(lhs, program, env, table, &mut *out).map_err(|e| attach(e, *span))?;
                let lv = truthy(&l).map_err(|e| attach(e, *span))?;
                if *op == BinOp::And {
                    return if lv {
                        let r = eval_expr(rhs, program, env, table, &mut *out)
                            .map_err(|e| attach(e, *span))?;
                        Ok(Value::Bool(truthy(&r).map_err(|e| attach(e, *span))?))
                    } else {
                        Ok(Value::Bool(false))
                    };
                }
                return if lv {
                    Ok(Value::Bool(true))
                } else {
                    let r = eval_expr(rhs, program, env, table, &mut *out)
                        .map_err(|e| attach(e, *span))?;
                    Ok(Value::Bool(truthy(&r).map_err(|e| attach(e, *span))?))
                };
            }
            let l = eval_expr(lhs, program, env, table, &mut *out).map_err(|e| attach(e, *span))?;
            let r = eval_expr(rhs, program, env, table, &mut *out).map_err(|e| attach(e, *span))?;
            eval_binary(*op, l, r).map_err(|e| attach(e, *span))
        }
        Expr::Call { callee, args, span } => {
            // Method call: obj.method(args)
            if let Expr::Field { obj, name, .. } = callee.as_ref() {
                return eval_method_call(obj, name, args, program, env, table, &mut *out, *span);
            }
            if let Expr::Ident(name, _) = callee.as_ref() {
                if let Some(v) = call_builtin(name, args, program, env, table, &mut *out)
                    .map_err(|e| attach(e, *span))?
                {
                    return Ok(v);
                }
                // Struct construction: `Circle(1, 2)`
                if let Some(field_names) = table.structs.get(name) {
                    if args.len() != field_names.len() {
                        return Err(err_at(
                            Msg::ArgCount(name.clone(), field_names.len(), args.len()),
                            *span,
                        ));
                    }
                    let mut fields = Vec::new();
                    for (fname, a) in field_names.iter().zip(args) {
                        let v = eval_expr(a, program, env, table, &mut *out)
                            .map_err(|e| attach(e, *span))?;
                        fields.push((fname.clone(), v));
                    }
                    return Ok(Value::Struct {
                        name: name.clone(),
                        fields,
                    });
                }
            }
            let callee_val =
                eval_expr(callee, program, env, table, &mut *out).map_err(|e| attach(e, *span))?;
            match callee_val {
                Value::Fn(idx) => {
                    let item = &program.items[idx];
                    let Item::Fn(fdef) = item else {
                        return Err(err_at(Msg::InternalFnIndex, *span));
                    };
                    call_fn(fdef, args, program, env, table, &mut *out, *span)
                        .map_err(|e| attach(e, *span))
                }
                other => Err(err_at(Msg::BadCall(format_value(&other)), *span)),
            }
        }
        Expr::Field { obj, name, span } => {
            let obj_val =
                eval_expr(obj, program, env, table, &mut *out).map_err(|e| attach(e, *span))?;
            let obj_val = obj_val.deref();
            match obj_val {
                Value::Struct {
                    name: sname,
                    fields,
                } => {
                    for (n, v) in fields {
                        if &n == name {
                            return Ok(v);
                        }
                    }
                    Err(err_at(
                        Msg::UnknownField {
                            ty: sname,
                            field: name.clone(),
                        },
                        *span,
                    ))
                }
                other => Err(err_at(
                    Msg::UnknownField {
                        ty: other.type_tag().into(),
                        field: name.clone(),
                    },
                    *span,
                )),
            }
        }
        Expr::Index { obj, index, span } => {
            let obj_val =
                eval_expr(obj, program, env, table, &mut *out).map_err(|e| attach(e, *span))?;
            let idx_val =
                eval_expr(index, program, env, table, &mut *out).map_err(|e| attach(e, *span))?;
            let Value::Int(i) = idx_val else {
                return Err(err_at(Msg::IndexNotInt, *span));
            };
            match obj_val.deref() {
                Value::List(items) => {
                    let i = usize::try_from(i).map_err(|_| err_at(Msg::IndexNotInt, *span))?;
                    items.get(i).cloned().ok_or_else(|| {
                        err_at(
                            Msg::OpTypeMismatch {
                                op: "index".into(),
                                actual: format!("index {} out of bounds (len {})", i, items.len()),
                            },
                            *span,
                        )
                    })
                }
                other => Err(err_at(Msg::IndexOnNonList(other.type_tag().into()), *span)),
            }
        }
        Expr::Borrow {
            mutable,
            expr,
            span,
        } => {
            let Expr::Ident(name, _) = expr.as_ref() else {
                return Err(err_at(
                    Msg::UnknownBorrowTarget("non-variable".into()),
                    *span,
                ));
            };
            let cell = env
                .lookup_cell(name)
                .ok_or_else(|| err_at(Msg::UndefinedVariable(name.clone()), *span))?;
            if *mutable {
                Ok(Value::MutRef(cell))
            } else {
                Ok(Value::Ref(cell))
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn eval_method_call(
    obj: &Expr,
    method: &str,
    args: &[Expr],
    program: &Program,
    env: &mut Env,
    table: &MethodTable,
    out: &mut dyn Write,
    span: Span,
) -> Result<Value, EvalError> {
    // Resolve the receiver cell (borrow semantics for `self`).
    let recv_cell = match obj {
        Expr::Ident(name, _) => {
            let cell = env
                .lookup_cell(name)
                .ok_or_else(|| err_at(Msg::UndefinedVariable(name.clone()), span))?;
            // Pass through an existing reference instead of nesting it.
            let cur = cell.borrow().clone();
            match cur {
                Value::Ref(inner) | Value::MutRef(inner) => inner,
                _ => cell,
            }
        }
        _ => {
            let v = eval_expr(obj, program, env, table, &mut *out).map_err(|e| attach(e, span))?;
            Rc::new(RefCell::new(v))
        }
    };

    // List builtin methods.
    {
        let cur = recv_cell.borrow().clone();
        if let Value::List(items) = &cur {
            let mut items = items.clone();
            return match method {
                "len" => {
                    if !args.is_empty() {
                        return Err(err_at(
                            Msg::ArgCount("List.len".into(), 0, args.len()),
                            span,
                        ));
                    }
                    Ok(Value::Int(items.len() as i64))
                }
                "push" => {
                    if args.len() != 1 {
                        return Err(err_at(
                            Msg::ArgCount("List.push".into(), 1, args.len()),
                            span,
                        ));
                    }
                    let v = eval_expr(&args[0], program, env, table, &mut *out)
                        .map_err(|e| attach(e, span))?;
                    items.push(v);
                    *recv_cell.borrow_mut() = Value::List(items);
                    Ok(Value::Unit)
                }
                "get" => {
                    if args.len() != 1 {
                        return Err(err_at(
                            Msg::ArgCount("List.get".into(), 1, args.len()),
                            span,
                        ));
                    }
                    let i = eval_expr(&args[0], program, env, table, &mut *out)
                        .map_err(|e| attach(e, span))?;
                    let Value::Int(i) = i else {
                        return Err(err_at(Msg::IndexNotInt, span));
                    };
                    let i = usize::try_from(i).map_err(|_| err_at(Msg::IndexNotInt, span))?;
                    items.get(i).cloned().ok_or_else(|| {
                        err_at(
                            Msg::OpTypeMismatch {
                                op: "index".into(),
                                actual: format!("index {} out of bounds (len {})", i, items.len()),
                            },
                            span,
                        )
                    })
                }
                "set" => {
                    if args.len() != 2 {
                        return Err(err_at(
                            Msg::ArgCount("List.set".into(), 2, args.len()),
                            span,
                        ));
                    }
                    let i = eval_expr(&args[0], program, env, table, &mut *out)
                        .map_err(|e| attach(e, span))?;
                    let v = eval_expr(&args[1], program, env, table, &mut *out)
                        .map_err(|e| attach(e, span))?;
                    let Value::Int(i) = i else {
                        return Err(err_at(Msg::IndexNotInt, span));
                    };
                    let i = usize::try_from(i).map_err(|_| err_at(Msg::IndexNotInt, span))?;
                    if i >= items.len() {
                        return Err(err_at(
                            Msg::OpTypeMismatch {
                                op: "index".into(),
                                actual: format!("index {} out of bounds (len {})", i, items.len()),
                            },
                            span,
                        ));
                    }
                    items[i] = v;
                    *recv_cell.borrow_mut() = Value::List(items);
                    Ok(Value::Unit)
                }
                _ => Err(err_at(
                    Msg::UnknownMethod {
                        ty: "List".into(),
                        method: method.to_string(),
                    },
                    span,
                )),
            };
        }
    }

    // Dispatch to an `impl` method by the runtime struct type.
    let ty = match recv_cell.borrow().clone().deref() {
        Value::Struct { name, .. } => name,
        other => {
            return Err(err_at(
                Msg::UnknownMethod {
                    ty: other.type_tag().into(),
                    method: method.to_string(),
                },
                span,
            ))
        }
    };
    let Some(fn_idx) = table.lookup(&ty, method) else {
        return Err(err_at(
            Msg::UnknownMethod {
                ty: ty.clone(),
                method: method.to_string(),
            },
            span,
        ));
    };
    let Item::Impl(imp) = &program.items[fn_idx] else {
        return Err(err_at(Msg::InternalFnIndex, span));
    };
    let Some(fdef) = imp.methods.iter().find(|m| m.name == method) else {
        return Err(err_at(Msg::InternalFnIndex, span));
    };
    call_method(fdef, recv_cell, args, program, env, table, &mut *out, span)
}

#[allow(clippy::too_many_arguments)]
fn call_method(
    fdef: &FnDef,
    recv_cell: Rc<RefCell<Value>>,
    args: &[Expr],
    program: &Program,
    env: &mut Env,
    table: &MethodTable,
    out: &mut dyn Write,
    span: Span,
) -> Result<Value, EvalError> {
    let mut arg_values = Vec::new();
    for a in args {
        arg_values.push(eval_expr(a, program, env, table, &mut *out).map_err(|e| attach(e, span))?);
    }
    if arg_values.len() + 1 != fdef.params.len() {
        return Err(err_at(
            Msg::ArgCount(
                format!("{}::{}", ty_of_impl(fdef), fdef.name),
                fdef.params.len(),
                arg_values.len() + 1,
            ),
            span,
        ));
    }
    let saved = std::mem::take(&mut env.locals);
    env.push_scope();
    // Bind `self` according to its declared type.
    let self_param = &fdef.params[0];
    let self_value = match &self_param.ty {
        Type::MutRef(_) => Value::MutRef(recv_cell.clone()),
        Type::Ref(_) => Value::Ref(recv_cell.clone()),
        _ => recv_cell.borrow().clone(),
    };
    env.insert_local("self".into(), self_value, self_param.is_mut);
    for ((p, v), a) in fdef.params.iter().skip(1).zip(arg_values).zip(args.iter()) {
        let _ = a;
        env.insert_local(p.name.clone(), v, p.is_mut);
    }
    let result = eval_block(&fdef.body, program, env, table, &mut *out);
    env.locals = saved;
    match result? {
        Flow::Return(v) => Ok(v),
        _ => Ok(Value::Unit),
    }
}

fn ty_of_impl(fdef: &FnDef) -> String {
    fdef.params
        .first()
        .map(|p| p.name.clone())
        .unwrap_or_default()
}

fn call_fn(
    fdef: &FnDef,
    args: &[Expr],
    program: &Program,
    env: &mut Env,
    table: &MethodTable,
    out: &mut dyn Write,
    call_span: Span,
) -> Result<Value, EvalError> {
    if args.len() != fdef.params.len() {
        return Err(err_at(
            Msg::ArgCount(fdef.name.clone(), fdef.params.len(), args.len()),
            call_span,
        ));
    }
    let mut arg_values = Vec::new();
    for a in args {
        arg_values
            .push(eval_expr(a, program, env, table, &mut *out).map_err(|e| attach(e, call_span))?);
    }
    // Functions see globals + their own params, never caller locals.
    let saved = std::mem::take(&mut env.locals);
    env.push_scope();
    for (p, v) in fdef.params.iter().zip(arg_values) {
        env.insert_local(p.name.clone(), v, p.is_mut);
    }
    let result = eval_block(&fdef.body, program, env, table, &mut *out);
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
    table: &MethodTable,
    out: &mut dyn Write,
) -> Result<Option<Value>, EvalError> {
    match name {
        "print" => {
            let mut parts = Vec::new();
            for a in args {
                let v = eval_expr(a, program, env, table, &mut *out)?;
                parts.push(format_value(&v));
            }
            writeln!(out, "{}", parts.join(" ")).map_err(|e| err(Msg::Io(e.to_string())))?;
            Ok(Some(Value::Unit))
        }
        "range" => {
            let mut nums = Vec::new();
            for a in args {
                match eval_expr(a, program, env, table, &mut *out)? {
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
        Value::List(items) => Ok(!items.is_empty()),
        Value::Struct { .. } => Ok(true),
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
        Value::List(items) => format!(
            "[{}]",
            items
                .iter()
                .map(format_value)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Struct { name, fields } => format!(
            "{} {{ {} }}",
            name,
            fields
                .iter()
                .map(|(n, v)| format!("{}: {}", n, format_value(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Ref(cell) | Value::MutRef(cell) => format_value(&cell.borrow()),
        Value::Fn(idx) => format!("<fn {}>", idx),
        Value::Unit => String::new(),
    }
}
