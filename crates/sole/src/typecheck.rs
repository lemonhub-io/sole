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
    fn from_name(name: &str) -> Option<Ty> {
        match name {
            "int" => Some(Ty::Int),
            "float" => Some(Ty::Float),
            "bool" => Some(Ty::Bool),
            "str" => Some(Ty::Str),
            _ => None,
        }
    }

    fn name(&self) -> String {
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

    fn is_copy(&self) -> bool {
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

    fn is_truthy(&self) -> bool {
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

    fn deref(&self) -> &Ty {
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

fn err(msg: Msg, span: Span) -> TypeError {
    TypeError {
        diag: Diagnostic::new(msg, span.line, span.column),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Live,
    Moved,
    BorrowedImm,
    BorrowedMut,
}

#[derive(Debug, Clone)]
struct Var {
    ty: Ty,
    state: State,
    mutable: bool,
}

impl Var {
    fn new(ty: Ty, mutable: bool) -> Self {
        Self {
            ty,
            state: State::Live,
            mutable,
        }
    }
}

#[derive(Debug, Clone)]
struct FnSig {
    name: String,
    type_params: Vec<(String, Option<String>)>,
    params: Vec<(String, Ty)>,
    ret: Ty,
}

pub struct Checker<'a> {
    program: &'a Program,
    fns: HashMap<String, FnSig>,
    structs: HashMap<String, Vec<(String, Ty)>>,
    interfaces: HashMap<String, Vec<MethodSig>>,
    impls: HashMap<String, Vec<String>>,
    methods: HashMap<(String, String), FnSig>,
    vars: Vec<HashMap<String, Var>>,
    current_fn: Option<String>,
    task_group_depth: usize,
    /// Active persistent borrows: borrow variable → (target, mutable).
    borrows: HashMap<String, (String, bool)>,
}

/// Scope snapshot taken before an `if`: every variable declared so far plus
/// the active borrow table. Each branch is checked from the same snapshot,
/// then the branch effects are merged (see `Stmt::If` handling).
struct ScopeSnapshot {
    names: Vec<String>,
    states: HashMap<String, State>,
    borrows: HashMap<String, (String, bool)>,
}

/// A block diverges when control can never fall through its end: it ends in
/// `return`, or in an `if` whose branches all diverge. Conservative: anything
/// else counts as falling through.
fn block_diverges(stmts: &[Stmt]) -> bool {
    match stmts.last() {
        Some(Stmt::Return { .. }) => true,
        Some(stmt) => stmt_diverges(stmt),
        None => false,
    }
}

fn stmt_diverges(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::If {
            then_block,
            else_block,
            ..
        } => {
            block_diverges(&then_block.stmts)
                && match else_block {
                    Some(ElseBranch::If(s)) => stmt_diverges(s),
                    Some(ElseBranch::Block(block)) => block_diverges(&block.stmts),
                    None => false,
                }
        }
        _ => false,
    }
}

/// Checks a whole program for static type and borrow errors.
pub fn check(program: &Program) -> Result<(), TypeError> {
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
    };
    checker.collect_types()?;
    checker.collect_fns()?;
    checker.collect_impls()?;
    checker.check_interfaces()?;
    checker.vars.push(HashMap::new());
    // Top-level statements form an implicit block so that NLL borrow
    // release applies there too.
    let top_stmts: Vec<Stmt> = program
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Stmt(s) => Some(s.clone()),
            _ => None,
        })
        .collect();
    checker.check_block(&Block { stmts: top_stmts })?;
    checker.vars.pop();
    checker.check_imports()?;
    checker.check_functions()?;
    checker.check_tests()?;
    Ok(())
}

impl<'a> Checker<'a> {
    fn collect_fns(&mut self) -> Result<(), TypeError> {
        for item in &self.program.items {
            if let Item::Fn(f) = item {
                let sig = self.fn_sig(&f.name, &f.type_params, &f.params, f.ret.as_ref(), None)?;
                self.fns.insert(f.name.clone(), sig);
            }
        }
        Ok(())
    }

    fn fn_sig(
        &self,
        name: &str,
        type_params: &[TypeParam],
        params: &[Param],
        ret: Option<&Type>,
        self_ty: Option<&Ty>,
    ) -> Result<FnSig, TypeError> {
        let tps: Vec<(String, Option<String>)> = type_params
            .iter()
            .map(|p| (p.name.clone(), p.bound.clone()))
            .collect();
        let mut sig_params = Vec::new();
        for p in params {
            let ty = self.ty_of_type_with(&p.ty, self_ty, &tps)?;
            sig_params.push((p.name.clone(), ty));
        }
        let ret = match ret {
            Some(t) => self.ty_of_type_with(t, self_ty, &tps)?,
            None => Ty::Unit,
        };
        Ok(FnSig {
            name: name.to_string(),
            type_params: tps,
            params: sig_params,
            ret,
        })
    }

    /// Like `ty_of_type` but with a generic parameter list in scope: bare
    /// identifiers naming a type parameter resolve to `Ty::TypeVar`.
    fn ty_of_type_with(
        &self,
        t: &Type,
        self_ty: Option<&Ty>,
        type_params: &[(String, Option<String>)],
    ) -> Result<Ty, TypeError> {
        match t {
            Type::Named(name, args) => {
                if args.is_empty() && type_params.iter().any(|(n, _)| n == name) {
                    return Ok(Ty::TypeVar(name.clone()));
                }
                // Recurse into generic arguments with the type params in
                // scope (e.g. `List[T]`, `Option[T]`).
                if !args.is_empty() {
                    let rebuilt: Vec<Type> = args
                        .iter()
                        .map(|a| self.substitute_typevars(a, type_params))
                        .collect();
                    return self.ty_of_type(&Type::Named(name.clone(), rebuilt), self_ty);
                }
                self.ty_of_type(t, self_ty)
            }
            Type::Ref(inner) => Ok(Ty::Ref(Box::new(self.ty_of_type_with(
                inner,
                self_ty,
                type_params,
            )?))),
            Type::MutRef(inner) => Ok(Ty::MutRef(Box::new(self.ty_of_type_with(
                inner,
                self_ty,
                type_params,
            )?))),
            Type::TypeVar(name) => Ok(Ty::TypeVar(name.clone())),
        }
    }

    /// Replaces type-parameter identifiers with `Type::TypeVar` markers.
    fn substitute_typevars(&self, t: &Type, type_params: &[(String, Option<String>)]) -> Type {
        match t {
            Type::Named(name, args) => {
                if args.is_empty() && type_params.iter().any(|(n, _)| n == name) {
                    Type::TypeVar(name.clone())
                } else {
                    Type::Named(
                        name.clone(),
                        args.iter()
                            .map(|a| self.substitute_typevars(a, type_params))
                            .collect(),
                    )
                }
            }
            Type::Ref(inner) => Type::Ref(Box::new(self.substitute_typevars(inner, type_params))),
            Type::MutRef(inner) => {
                Type::MutRef(Box::new(self.substitute_typevars(inner, type_params)))
            }
            Type::TypeVar(name) => Type::TypeVar(name.clone()),
        }
    }

    /// Resolves a parsed type to a `Ty`. `self_ty` substitutes `Self`.
    fn ty_of_type(&self, t: &Type, self_ty: Option<&Ty>) -> Result<Ty, TypeError> {
        match t {
            Type::TypeVar(name) => Ok(Ty::TypeVar(name.clone())),
            Type::Named(name, args) => {
                if args.is_empty() {
                    // Type variable (generic parameter) or plain type name.
                    match name.as_str() {
                        "int" => return Ok(Ty::Int),
                        "float" => return Ok(Ty::Float),
                        "bool" => return Ok(Ty::Bool),
                        "str" => return Ok(Ty::Str),
                        "Json" => return Ok(Ty::Json),
                        "Self" => {
                            if let Some(st) = self_ty {
                                return Ok(st.deref().clone());
                            }
                        }
                        _ => {}
                    }
                }
                match name.as_str() {
                    "Option" => {
                        if args.len() != 1 {
                            return Err(err(
                                Msg::ArgCount("Option".into(), 1, args.len()),
                                Span::new(0, 0),
                            ));
                        }
                        let inner = self.ty_of_type(&args[0], self_ty)?;
                        Ok(Ty::Option(Box::new(inner)))
                    }
                    "Result" => {
                        if args.len() != 2 {
                            return Err(err(
                                Msg::ArgCount("Result".into(), 2, args.len()),
                                Span::new(0, 0),
                            ));
                        }
                        let ok = self.ty_of_type(&args[0], self_ty)?;
                        let err = self.ty_of_type(&args[1], self_ty)?;
                        Ok(Ty::Result(Box::new(ok), Box::new(err)))
                    }
                    "Dict" => {
                        if args.len() != 2 {
                            return Err(err(
                                Msg::ArgCount("Dict".into(), 2, args.len()),
                                Span::new(0, 0),
                            ));
                        }
                        let k = self.ty_of_type(&args[0], self_ty)?;
                        let v = self.ty_of_type(&args[1], self_ty)?;
                        Ok(Ty::Dict(Box::new(k), Box::new(v)))
                    }
                    "Set" => {
                        if args.len() != 1 {
                            return Err(err(
                                Msg::ArgCount("Set".into(), 1, args.len()),
                                Span::new(0, 0),
                            ));
                        }
                        let inner = self.ty_of_type(&args[0], self_ty)?;
                        Ok(Ty::Set(Box::new(inner)))
                    }
                    "List" => {
                        if args.len() != 1 {
                            return Err(err(
                                Msg::ArgCount("List".into(), 1, args.len()),
                                Span::new(0, 0),
                            ));
                        }
                        let inner = self.ty_of_type(&args[0], self_ty)?;
                        Ok(Ty::List(Box::new(inner)))
                    }
                    "Chan" => {
                        if args.len() != 1 {
                            return Err(err(
                                Msg::ArgCount("Chan".into(), 1, args.len()),
                                Span::new(0, 0),
                            ));
                        }
                        let inner = self.ty_of_type(&args[0], self_ty)?;
                        Ok(Ty::Chan(Box::new(inner)))
                    }
                    _ => {
                        if args.is_empty() && self.structs.contains_key(name) {
                            return Ok(Ty::Struct(name.clone()));
                        }
                        if args.is_empty() && self.interfaces.contains_key(name) {
                            return Ok(Ty::Interface(name.clone()));
                        }
                        Err(err(Msg::UnknownType(name.clone()), Span::new(0, 0)))
                    }
                }
            }
            Type::Ref(inner) => Ok(Ty::Ref(Box::new(self.ty_of_type(inner, self_ty)?))),
            Type::MutRef(inner) => Ok(Ty::MutRef(Box::new(self.ty_of_type(inner, self_ty)?))),
        }
    }

    fn collect_types(&mut self) -> Result<(), TypeError> {
        for item in &self.program.items {
            match item {
                Item::Struct(s) => {
                    let mut fields = Vec::new();
                    for (name, t) in &s.fields {
                        let ty = self.ty_of_type(t, None)?;
                        fields.push((name.clone(), ty));
                    }
                    self.structs.insert(s.name.clone(), fields);
                }
                Item::Interface(i) => {
                    let methods = i.methods.clone();
                    self.interfaces.insert(i.name.clone(), methods);
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn collect_impls(&mut self) -> Result<(), TypeError> {
        for item in &self.program.items {
            if let Item::Impl(imp) = item {
                if !self.structs.contains_key(&imp.ty) {
                    return Err(err(Msg::UnknownStruct(imp.ty.clone()), imp.span));
                }
                if let Some(iface) = &imp.interface {
                    if !self.interfaces.contains_key(iface) {
                        return Err(err(Msg::UnknownType(iface.clone()), imp.span));
                    }
                    self.impls
                        .entry(imp.ty.clone())
                        .or_default()
                        .push(iface.clone());
                }
                for m in &imp.methods {
                    let sig = self.method_sig(m, &imp.ty, imp.span)?;
                    self.methods.insert((imp.ty.clone(), m.name.clone()), sig);
                }
            }
        }
        Ok(())
    }

    /// Signature of an impl method; first parameter must be `self` typed as
    /// `Self`, `ref Self`, or `mut ref Self`.
    fn method_sig(&self, m: &FnDef, ty: &str, impl_span: Span) -> Result<FnSig, TypeError> {
        let Some(first) = m.params.first() else {
            return Err(err(
                Msg::OpTypeMismatch {
                    op: "method".into(),
                    actual: format!("method `{}` needs a `self` parameter", m.name),
                },
                impl_span,
            ));
        };
        if first.name != "self" {
            return Err(err(
                Msg::OpTypeMismatch {
                    op: "method".into(),
                    actual: format!("first parameter of method `{}` must be `self`", m.name),
                },
                impl_span,
            ));
        }
        let self_ty = Ty::Struct(ty.to_string());
        let resolved = self.ty_of_type(&first.ty, Some(&self_ty))?;
        if resolved != self_ty {
            let ok = matches!(
                resolved,
                Ty::Ref(ref i) if **i == self_ty
            ) || matches!(
                resolved,
                Ty::MutRef(ref i) if **i == self_ty
            );
            if !ok {
                return Err(err(
                    Msg::OpTypeMismatch {
                        op: "self".into(),
                        actual: format!(
                            "`self` of method `{}` must be `Self`, `ref Self`, or `mut ref Self`",
                            m.name
                        ),
                    },
                    impl_span,
                ));
            }
        }
        self.fn_sig(&m.name, &[], &m.params, m.ret.as_ref(), Some(&self_ty))
    }

    fn check_interfaces(&mut self) -> Result<(), TypeError> {
        let interfaces = self.interfaces.clone();
        let methods = self.methods.clone();
        let impls = self.impls.clone();
        for (ty, ifaces) in &impls {
            for iface in ifaces {
                let sigs = &interfaces[iface];
                for m in sigs {
                    if !methods.contains_key(&(ty.clone(), m.name.clone())) {
                        return Err(err(
                            Msg::MissingImplMethod {
                                interface: iface.clone(),
                                method: m.name.clone(),
                            },
                            Span::new(0, 0),
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    fn lookup(&self, name: &str) -> Option<&Var> {
        for scope in self.vars.iter().rev() {
            if let Some(v) = scope.get(name) {
                return Some(v);
            }
        }
        None
    }

    fn lookup_mut(&mut self, name: &str) -> Option<&mut Var> {
        for scope in self.vars.iter_mut().rev() {
            if let Some(v) = scope.get_mut(name) {
                return Some(v);
            }
        }
        None
    }

    fn bind(&mut self, name: String, ty: Ty, mutable: bool) {
        if let Some(scope) = self.vars.last_mut() {
            scope.insert(name, Var::new(ty, mutable));
        }
    }

    /// Snapshots the current scope's variables and borrow table so an `if`
    /// branch can be checked in isolation and merged back.
    fn snapshot_scope(&self) -> ScopeSnapshot {
        let mut names = Vec::new();
        let mut states = HashMap::new();
        if let Some(scope) = self.vars.last() {
            for (k, v) in scope {
                names.push(k.clone());
                states.insert(k.clone(), v.state);
            }
        }
        ScopeSnapshot {
            names,
            states,
            borrows: self.borrows.clone(),
        }
    }

    /// Restores the states of variables present in the snapshot and the
    /// borrow table. Variables declared inside a branch keep their end state.
    fn restore_scope(&mut self, snapshot: &ScopeSnapshot) {
        if let Some(scope) = self.vars.last_mut() {
            for (name, state) in &snapshot.states {
                if let Some(v) = scope.get_mut(name) {
                    v.state = *state;
                }
            }
        }
        self.borrows = snapshot.borrows.clone();
    }

    /// Names from the snapshot whose current state is `Moved` (moved inside
    /// the branch that was just checked).
    fn moved_names(&self, snapshot: &ScopeSnapshot) -> Vec<String> {
        snapshot
            .names
            .iter()
            .filter(|n| {
                matches!(
                    self.vars.last().and_then(|s| s.get(*n)),
                    Some(v) if v.state == State::Moved
                )
            })
            .cloned()
            .collect()
    }

    /// Reads a variable: `Moved` error. `BorrowedMut` conflict error.
    fn read_var(&self, name: &str, span: Span) -> Result<Ty, TypeError> {
        match self.lookup(name) {
            Some(v) => match v.state {
                State::Moved => Err(err(Msg::UseAfterMove(name.to_string()), span)),
                State::BorrowedMut => Err(err(Msg::MutBorrowConflict(name.to_string()), span)),
                State::Live | State::BorrowedImm => Ok(v.ty.clone()),
            },
            None => {
                if self.fns.contains_key(name) {
                    Ok(Ty::Fn)
                } else {
                    Err(err(Msg::UndefinedVariable(name.to_string()), span))
                }
            }
        }
    }

    /// Consumes a variable (move semantics). No-op for Copy types.
    fn move_var(&mut self, name: &str, span: Span) -> Result<Ty, TypeError> {
        let ty = self.read_var(name, span)?;
        if ty.is_copy() {
            return Ok(ty);
        }
        let Some(v) = self.lookup_mut(name) else {
            return Ok(ty);
        };
        match v.state {
            State::Live => {
                v.state = State::Moved;
                Ok(ty)
            }
            State::BorrowedImm | State::BorrowedMut => {
                Err(err(Msg::MoveWhileBorrowed(name.to_string()), span))
            }
            State::Moved => Err(err(Msg::UseAfterMove(name.to_string()), span)),
        }
    }

    /// Borrows a variable (or a path rooted at a variable). Returns the
    /// borrowed type. `persistent` marks a borrow that outlives the
    /// statement (explicit `ref x` expressions); transient borrows (call
    /// args, method receivers) check availability but do not mark.
    fn borrow_var(&mut self, target: &Expr, mutable: bool, span: Span) -> Result<Ty, TypeError> {
        let (root, field_ty) = self.borrow_path(target)?;
        let ty = self.read_var(&root, span)?;
        if ty.is_copy() {
            return Ok(if mutable {
                Ty::MutRef(Box::new(ty))
            } else {
                Ty::Ref(Box::new(ty))
            });
        }
        let borrowed_ty = match field_ty {
            Some(f) => f,
            None => ty,
        };
        let Some(v) = self.lookup_mut(&root) else {
            return Ok(borrowed_ty);
        };
        match v.state {
            State::Live | State::BorrowedImm if !mutable => {
                v.state = State::BorrowedImm;
                Ok(if mutable {
                    Ty::MutRef(Box::new(borrowed_ty))
                } else {
                    Ty::Ref(Box::new(borrowed_ty))
                })
            }
            State::Live if mutable => {
                v.state = State::BorrowedMut;
                Ok(Ty::MutRef(Box::new(borrowed_ty)))
            }
            State::Moved => Err(err(Msg::UseAfterMove(root.to_string()), span)),
            _ => Err(err(Msg::MutBorrowConflict(root.to_string()), span)),
        }
    }

    /// Transient borrow for call args / method receivers: checks the
    /// target can be borrowed but does not mark it (the borrow lives only
    /// for the call).
    fn borrow_var_transient(
        &mut self,
        target: &Expr,
        mutable: bool,
        span: Span,
    ) -> Result<Ty, TypeError> {
        let (root, field_ty) = self.borrow_path(target)?;
        let ty = self.read_var(&root, span)?;
        let borrowed_ty = match field_ty {
            Some(f) => f,
            None => ty,
        };
        if let Some(v) = self.lookup(&root) {
            match v.state {
                State::Live | State::BorrowedImm if !mutable => {}
                State::Live if mutable => {}
                State::Moved => return Err(err(Msg::UseAfterMove(root.to_string()), span)),
                _ => return Err(err(Msg::MutBorrowConflict(root.to_string()), span)),
            }
        }
        Ok(if mutable {
            Ty::MutRef(Box::new(borrowed_ty))
        } else {
            Ty::Ref(Box::new(borrowed_ty))
        })
    }

    /// Resolves a borrow target to its root variable and (for field/index
    /// borrows) the borrowed type.
    fn borrow_path(&self, target: &Expr) -> Result<(String, Option<Ty>), TypeError> {
        match target {
            Expr::Ident(name, _) => Ok((name.clone(), None)),
            Expr::Index { obj, index, span } => {
                let Expr::Ident(root, _) = obj.as_ref() else {
                    return Err(err(Msg::UnknownBorrowTarget("non-variable".into()), *span));
                };
                let Some(v) = self.lookup(root) else {
                    return Err(err(Msg::UndefinedVariable(root.clone()), *span));
                };
                let base = v.ty.deref().clone();
                match &base {
                    Ty::List(inner) => {
                        let _ = index;
                        Ok((root.clone(), Some(inner.as_ref().clone())))
                    }
                    other => Err(err(Msg::UnknownBorrowTarget(other.name()), *span)),
                }
            }
            Expr::Field { obj, name, span } => {
                let Expr::Ident(root, _) = obj.as_ref() else {
                    return Err(err(Msg::UnknownBorrowTarget("non-variable".into()), *span));
                };
                let Some(v) = self.lookup(root) else {
                    return Err(err(Msg::UndefinedVariable(root.clone()), *span));
                };
                let base = v.ty.deref().clone();
                match &base {
                    Ty::Struct(sname) => {
                        let fields = self
                            .structs
                            .get(sname)
                            .ok_or_else(|| err(Msg::UnknownStruct(sname.clone()), *span))?;
                        let Some((_, fty)) = fields.iter().find(|(n, _)| n == name) else {
                            return Err(err(
                                Msg::UnknownField {
                                    ty: sname.clone(),
                                    field: name.clone(),
                                },
                                *span,
                            ));
                        };
                        Ok((root.clone(), Some(fty.clone())))
                    }
                    other => Err(err(Msg::UnknownBorrowTarget(other.name()), *span)),
                }
            }
            _ => Err(err(
                Msg::UnknownBorrowTarget("non-variable".into()),
                Span::new(0, 0),
            )),
        }
    }

    fn check_fn_body(&mut self, f: &FnDef) -> Result<(), TypeError> {
        let sig = self
            .fns
            .get(&f.name)
            .cloned()
            .expect("function registered during collect_fns");
        self.vars.push(HashMap::new());
        self.current_fn = Some(f.name.clone());
        for (p, ty) in f.params.iter().zip(&sig.params) {
            self.bind(p.name.clone(), ty.1.clone(), p.is_mut);
        }
        let result = self.check_block(&f.body);
        self.current_fn = None;
        self.vars.pop();
        result
    }

    /// Checks a block. Persistent borrows are released at the last use of
    /// their borrow variable (NLL); borrows that existed on entry (outer
    /// borrows) survive the block. Reference-typed variables created inside
    /// the block die with it.
    fn check_block(&mut self, block: &Block) -> Result<(), TypeError> {
        let snapshot: HashMap<String, State> = self
            .vars
            .last()
            .map(|scope| scope.iter().map(|(k, v)| (k.clone(), v.state)).collect())
            .unwrap_or_default();
        // NLL: last-use position of every variable in this block.
        let mut last_uses: HashMap<String, usize> = HashMap::new();
        for (idx, stmt) in block.stmts.iter().enumerate() {
            collect_uses(stmt, &mut last_uses, idx);
        }
        // Borrows created outside this block must not be released by it.
        let outer_borrows: Vec<String> = self.borrows.keys().cloned().collect();
        for (idx, stmt) in block.stmts.iter().enumerate() {
            self.check_stmt(stmt)?;
            self.release_dead_borrows(&last_uses, idx, &outer_borrows);
        }
        if let Some(scope) = self.vars.last_mut() {
            for (name, var) in scope.iter_mut() {
                match snapshot.get(name) {
                    // Outer borrow or move: survives the block.
                    Some(State::BorrowedImm) | Some(State::BorrowedMut) | Some(State::Moved) => {}
                    // Live at entry: a borrow created in this block dies here.
                    Some(State::Live) => {
                        if matches!(var.state, State::BorrowedImm | State::BorrowedMut) {
                            var.state = State::Live;
                        }
                    }
                    // Created inside the block: reference bindings die with it.
                    None => {
                        if matches!(var.ty, Ty::Ref(_) | Ty::MutRef(_)) {
                            var.state = State::Moved;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Releases borrows whose borrow variable is no longer used after the
    /// given statement index (NLL). Only borrows created inside the current
    /// block are released; outer borrows survive nested blocks.
    fn release_dead_borrows(
        &mut self,
        last_uses: &HashMap<String, usize>,
        stmt_idx: usize,
        outer_borrows: &[String],
    ) {
        let dead: Vec<String> = self
            .borrows
            .keys()
            .filter(|r| {
                !outer_borrows.contains(r) && last_uses.get(*r).copied().unwrap_or(0) <= stmt_idx
            })
            .cloned()
            .collect();
        for r in dead {
            let Some((target, _)) = self.borrows.remove(&r) else {
                continue;
            };
            // Only release the target when no other borrow variable uses it.
            let still_borrowed = self.borrows.values().any(|(t, _)| *t == target);
            if !still_borrowed {
                if let Some(v) = self.lookup_mut(&target) {
                    v.state = State::Live;
                }
            }
        }
    }

    fn check_stmt(&mut self, stmt: &Stmt) -> Result<(), TypeError> {
        match stmt {
            Stmt::Let {
                name,
                is_mut,
                ty,
                value,
                span,
            } => {
                // `let r = ref x` registers a persistent borrow that NLL
                // releases at r's last use.
                if let Expr::Borrow { expr, mutable, .. } = value {
                    if let Ok((target, _)) = self.borrow_path(expr) {
                        self.borrows.insert(name.clone(), (target, *mutable));
                    }
                }
                // Annotated list: check each element against the inner type,
                // which also allows heterogenous struct elements when the
                // annotation is an interface type.
                if let (Some(annot), Expr::List(items, list_span)) = (ty, value) {
                    let expected = self.ty_of_type(annot, None)?;
                    if let Ty::List(inner) = &expected {
                        for it in items {
                            let t = self.infer_expr_consume(it, *list_span)?;
                            if !self.is_compatible(inner, &t) {
                                return Err(err(
                                    Msg::ListElemMismatch {
                                        expected: inner.name(),
                                        actual: t.name(),
                                    },
                                    *list_span,
                                ));
                            }
                        }
                        self.bind(name.clone(), expected, *is_mut);
                        return Ok(());
                    }
                }
                let actual = match (ty, value) {
                    // Empty collection literal under an annotation:
                    // the element types come from the annotation.
                    (Some(annot), Expr::List(items, _)) if items.is_empty() => {
                        match self.ty_of_type(annot, None)? {
                            Ty::List(inner) => Ty::List(inner),
                            other => other,
                        }
                    }
                    (Some(annot), Expr::Dict(pairs, _)) if pairs.is_empty() => {
                        match self.ty_of_type(annot, None)? {
                            Ty::Dict(k, v) => Ty::Dict(k, v),
                            other => other,
                        }
                    }
                    (Some(annot), Expr::Set(items, _)) if items.is_empty() => {
                        match self.ty_of_type(annot, None)? {
                            Ty::Set(inner) => Ty::Set(inner),
                            other => other,
                        }
                    }
                    _ => self.infer_expr_consume(value, *span)?,
                };
                match ty {
                    Some(annot) => {
                        let expected = self.ty_of_type(annot, None)?;
                        if !self.is_compatible(&expected, &actual) {
                            return Err(err(
                                Msg::LetTypeMismatch {
                                    name: name.clone(),
                                    expected: expected.name(),
                                    actual: actual.name(),
                                },
                                *span,
                            ));
                        }
                        self.bind(name.clone(), expected, *is_mut);
                    }
                    None => self.bind(name.clone(), actual, *is_mut),
                }
                Ok(())
            }
            Stmt::Assign { name, value, span } => {
                let var = self
                    .lookup(name)
                    .cloned()
                    .ok_or_else(|| err(Msg::UndefinedVariable(name.clone()), *span))?;
                if !var.mutable {
                    return Err(err(Msg::ImmutableReassign(name.clone()), *span));
                }
                let expected = var.ty;
                let actual = self.infer_expr_consume_skip(value, *span, name)?;
                if !self.is_compatible(&expected, &actual) {
                    return Err(err(
                        Msg::AssignTypeMismatch {
                            name: name.clone(),
                            expected: expected.name(),
                            actual: actual.name(),
                        },
                        *span,
                    ));
                }
                if let Some(v) = self.lookup_mut(name) {
                    match v.state {
                        State::Moved => return Err(err(Msg::UseAfterMove(name.clone()), *span)),
                        State::BorrowedImm | State::BorrowedMut => {
                            return Err(err(Msg::MoveWhileBorrowed(name.clone()), *span))
                        }
                        State::Live => {}
                    }
                    v.state = State::Live;
                }
                Ok(())
            }
            Stmt::FieldAssign {
                obj,
                field,
                value,
                span,
            } => {
                let obj_ty = self.read_var(obj, *span)?;
                let base = obj_ty.deref().clone();
                let Ty::Struct(sname) = &base else {
                    return Err(err(
                        Msg::UnknownField {
                            ty: base.name(),
                            field: field.clone(),
                        },
                        *span,
                    ));
                };
                let fields = self
                    .structs
                    .get(sname)
                    .cloned()
                    .ok_or_else(|| err(Msg::UnknownStruct(sname.clone()), *span))?;
                let Some((_, fty)) = fields.iter().find(|(n, _)| n == field) else {
                    return Err(err(
                        Msg::UnknownField {
                            ty: sname.clone(),
                            field: field.clone(),
                        },
                        *span,
                    ));
                };
                let actual = self.infer_expr_consume(value, *span)?;
                if !self.is_compatible(fty, &actual) {
                    return Err(err(
                        Msg::AssignTypeMismatch {
                            name: format!("{}.{}", obj, field),
                            expected: fty.name(),
                            actual: actual.name(),
                        },
                        *span,
                    ));
                }
                Ok(())
            }
            Stmt::Expr(expr) => {
                self.infer_expr(expr)?;
                Ok(())
            }
            Stmt::Return { value, span } => {
                let expected = match &self.current_fn {
                    Some(name) => self
                        .fns
                        .get(name)
                        .map(|sig| sig.ret.clone())
                        .unwrap_or(Ty::Unknown),
                    None => Ty::Unknown,
                };
                if matches!(expected, Ty::Ref(_) | Ty::MutRef(_)) {
                    self.check_ref_return(value.as_ref(), &expected, *span)?;
                } else {
                    let actual = match value {
                        Some(expr) => self.infer_expr_consume(expr, *span)?,
                        None => Ty::Unit,
                    };
                    if !self.is_compatible(&expected, &actual) {
                        let func = self
                            .current_fn
                            .clone()
                            .unwrap_or_else(|| "<top-level>".into());
                        return Err(err(
                            Msg::ReturnTypeMismatch {
                                func,
                                expected: expected.name(),
                                actual: actual.name(),
                            },
                            *span,
                        ));
                    }
                }
                Ok(())
            }
            Stmt::If {
                cond,
                then_block,
                else_block,
                span,
            } => {
                let cond_ty = self.infer_expr(cond)?;
                if !cond_ty.is_truthy() {
                    return Err(err(
                        Msg::OpTypeMismatch {
                            op: "condition".into(),
                            actual: cond_ty.name(),
                        },
                        *span,
                    ));
                }
                // Branch merge: each branch is checked from the same pre-`if`
                // state. Borrows created inside a branch die with it; a move
                // takes effect after the `if` unless every path reaching the
                // code after it leaves the value untouched (a diverging
                // branch — one that always returns — does not participate).
                let snapshot = self.snapshot_scope();
                self.check_block(then_block)?;
                let then_moved = self.moved_names(&snapshot);
                let then_diverges = block_diverges(&then_block.stmts);
                self.restore_scope(&snapshot);
                let (else_moved, else_diverges) = match else_block {
                    Some(ElseBranch::If(stmt)) => {
                        self.check_stmt(stmt)?;
                        let moved = self.moved_names(&snapshot);
                        let diverges = stmt_diverges(stmt);
                        self.restore_scope(&snapshot);
                        (moved, diverges)
                    }
                    Some(ElseBranch::Block(block)) => {
                        self.check_block(block)?;
                        let moved = self.moved_names(&snapshot);
                        let diverges = block_diverges(&block.stmts);
                        self.restore_scope(&snapshot);
                        (moved, diverges)
                    }
                    // No else: the fall-through path keeps the pre-`if` state.
                    None => (Vec::new(), false),
                };
                if !(then_diverges && else_diverges) {
                    for name in snapshot.names {
                        let moved_in_then = !then_diverges && then_moved.contains(&name);
                        let moved_in_else = !else_diverges && else_moved.contains(&name);
                        if moved_in_then || moved_in_else {
                            if let Some(v) = self.lookup_mut(&name) {
                                v.state = State::Moved;
                            }
                        }
                    }
                }
                self.borrows = snapshot.borrows;
                Ok(())
            }
            Stmt::While { cond, body, span } => {
                let cond_ty = self.infer_expr(cond)?;
                if !cond_ty.is_truthy() {
                    return Err(err(
                        Msg::OpTypeMismatch {
                            op: "condition".into(),
                            actual: cond_ty.name(),
                        },
                        *span,
                    ));
                }
                self.check_block(body)?;
                Ok(())
            }
            Stmt::For {
                var,
                is_mut,
                mode,
                iterable,
                body,
                span,
            } => {
                let elem_ty = match mode {
                    sole_parser::IterMode::Move => {
                        let t = self.infer_expr_consume(iterable, *span)?;
                        self.iter_elem(&t)?
                    }
                    sole_parser::IterMode::Borrow => {
                        let t = self.infer_expr(iterable)?;
                        let elem = self.iter_elem(t.deref())?;
                        if let Expr::Ident(name, _) = iterable {
                            if let Some(v) = self.lookup_mut(name) {
                                v.state = State::BorrowedImm;
                            }
                        }
                        elem
                    }
                    sole_parser::IterMode::MutBorrow => {
                        let t = self.infer_expr(iterable)?;
                        let elem = self.iter_elem(t.deref())?;
                        if let Expr::Ident(name, _) = iterable {
                            if let Some(v) = self.lookup_mut(name) {
                                match v.state {
                                    State::Live => v.state = State::BorrowedMut,
                                    _ => {
                                        return Err(err(
                                            Msg::MutBorrowConflict(name.clone()),
                                            *span,
                                        ))
                                    }
                                }
                            }
                        }
                        elem
                    }
                };
                self.bind(var.clone(), elem_ty, *is_mut);
                self.check_block(body)?;
                // Release the borrow on the iterable (D6: usable after loop).
                if let Expr::Ident(name, _) = iterable {
                    if let Some(v) = self.lookup_mut(name) {
                        v.state = State::Live;
                    }
                }
                Ok(())
            }
            Stmt::Break { .. } | Stmt::Continue { .. } => Ok(()),
            Stmt::TaskGroup { body, span } => {
                // `go` is only allowed inside a task_group (or the implicit
                // top-level group). Track the current task-group depth.
                let _ = span;
                self.task_group_depth += 1;
                let result = self.check_block(body);
                self.task_group_depth -= 1;
                result
            }
            Stmt::Go { call, span } => {
                // `go` must spawn a call expression.
                if !matches!(call.as_ref(), Expr::Call { .. }) {
                    return Err(err(
                        Msg::OpTypeMismatch {
                            op: "go".into(),
                            actual: "expected a call expression".into(),
                        },
                        *span,
                    ));
                }
                self.infer_expr(call)?;
                Ok(())
            }
            Stmt::Yield { .. } => Ok(()),
            Stmt::Assert { expr, span } => {
                let ty = self.infer_expr(expr)?;
                if !matches!(ty.deref(), Ty::Bool | Ty::Unknown | Ty::TypeVar(_)) {
                    return Err(err(
                        Msg::OpTypeMismatch {
                            op: "assert".into(),
                            actual: ty.name(),
                        },
                        *span,
                    ));
                }
                Ok(())
            }
        }
    }

    fn iter_elem(&self, t: &Ty) -> Result<Ty, TypeError> {
        match t {
            Ty::Range => Ok(Ty::Int),
            Ty::List(inner) => Ok(inner.as_ref().clone()),
            Ty::Chan(inner) => Ok(inner.as_ref().clone()),
            Ty::Unknown => Ok(Ty::Unknown),
            other => Err(err(
                Msg::OpTypeMismatch {
                    op: "for-in".into(),
                    actual: other.name(),
                },
                Span::new(0, 0),
            )),
        }
    }

    /// `return ref ...` the returned reference must derive from a
    /// reference parameter (borrow propagation); locals escape.
    fn check_ref_return(
        &mut self,
        value: Option<&Expr>,
        expected: &Ty,
        span: Span,
    ) -> Result<(), TypeError> {
        let Some(expr) = value else {
            return Err(err(
                Msg::ReturnTypeMismatch {
                    func: self.current_fn.clone().unwrap_or_default(),
                    expected: expected.name(),
                    actual: "()".into(),
                },
                span,
            ));
        };
        let root = self.ref_return_root(expr);
        let ok = match root {
            Some(name) => self
                .lookup(&name)
                .map(|v| matches!(v.ty, Ty::Ref(_) | Ty::MutRef(_)))
                .unwrap_or(false),
            None => false,
        };
        if ok {
            Ok(())
        } else {
            Err(err(Msg::BorrowEscape, span))
        }
    }

    /// Root variable of an expression that may be a reference return:
    /// `x`, `ref x`, `x.field`, `ref x.field`.
    fn ref_return_root(&self, expr: &Expr) -> Option<String> {
        match expr {
            Expr::Ident(name, _) => Some(name.clone()),
            Expr::Borrow { expr, .. } => self.ref_return_root(expr),
            Expr::Field { obj, .. } => self.ref_return_root(obj),
            _ => None,
        }
    }

    fn infer_expr(&mut self, expr: &Expr) -> Result<Ty, TypeError> {
        self.infer_expr_inner(expr, false, None)
    }

    fn infer_expr_consume(&mut self, expr: &Expr, _span: Span) -> Result<Ty, TypeError> {
        self.infer_expr_inner(expr, true, None)
    }

    fn infer_expr_consume_skip(
        &mut self,
        expr: &Expr,
        _span: Span,
        skip: &str,
    ) -> Result<Ty, TypeError> {
        self.infer_expr_inner(expr, true, Some(skip))
    }

    fn infer_expr_inner(
        &mut self,
        expr: &Expr,
        consume: bool,
        skip: Option<&str>,
    ) -> Result<Ty, TypeError> {
        match expr {
            Expr::Int(..) => Ok(Ty::Int),
            Expr::Float(..) => Ok(Ty::Float),
            Expr::Str(..) => Ok(Ty::Str),
            Expr::Bool(..) => Ok(Ty::Bool),
            Expr::Dict(pairs, span) => {
                let mut key_ty: Option<Ty> = None;
                let mut val_ty: Option<Ty> = None;
                for (k, v) in pairs {
                    let kt = self.infer_expr_consume(k, *span)?;
                    let vt = self.infer_expr_consume(v, *span)?;
                    match &key_ty {
                        None => key_ty = Some(kt),
                        Some(e) => {
                            if !self.is_compatible(e, &kt) && !matches!(e, Ty::Unknown) {
                                return Err(err(
                                    Msg::DictKeyMismatch {
                                        expected: e.name(),
                                        actual: kt.name(),
                                    },
                                    *span,
                                ));
                            }
                        }
                    }
                    if Self::is_hole(&vt) {
                        continue;
                    }
                    match &val_ty {
                        None => val_ty = Some(vt),
                        Some(e) => {
                            if !self.is_compatible(e, &vt) && !matches!(e, Ty::Unknown) {
                                return Err(err(
                                    Msg::ListElemMismatch {
                                        expected: e.name(),
                                        actual: vt.name(),
                                    },
                                    *span,
                                ));
                            }
                        }
                    }
                }
                match (key_ty, val_ty) {
                    (Some(k), Some(v)) => Ok(Ty::Dict(Box::new(k), Box::new(v))),
                    _ => Err(err(Msg::EmptyDictNoType, *span)),
                }
            }
            Expr::Set(items, span) => {
                let mut elem_ty: Option<Ty> = None;
                for it in items {
                    let t = self.infer_expr_consume(it, *span)?;
                    match &elem_ty {
                        None => elem_ty = Some(t),
                        Some(e) => {
                            if !self.is_compatible(e, &t) && !matches!(e, Ty::Unknown) {
                                return Err(err(
                                    Msg::SetElemMismatch {
                                        expected: e.name(),
                                        actual: t.name(),
                                    },
                                    *span,
                                ));
                            }
                        }
                    }
                }
                match elem_ty {
                    Some(t) => Ok(Ty::Set(Box::new(t))),
                    None => Err(err(Msg::EmptySetNoType, *span)),
                }
            }
            Expr::Tuple(items, span) => {
                let mut ts = Vec::with_capacity(items.len());
                for it in items {
                    ts.push(self.infer_expr_consume(it, *span)?);
                }
                Ok(Ty::Tuple(ts))
            }
            Expr::List(items, span) => {
                let mut elem_ty: Option<Ty> = None;
                for it in items {
                    let t = self.infer_expr_consume(it, *span)?;
                    if Self::is_hole(&t) {
                        continue;
                    }
                    match &elem_ty {
                        None => elem_ty = Some(t),
                        Some(e) => {
                            if !self.is_compatible(e, &t) && !matches!(e, Ty::Unknown) {
                                return Err(err(
                                    Msg::ListElemMismatch {
                                        expected: e.name(),
                                        actual: t.name(),
                                    },
                                    *span,
                                ));
                            }
                        }
                    }
                }
                match elem_ty {
                    Some(t) => Ok(Ty::List(Box::new(t))),
                    None => Err(err(Msg::EmptyListNoType, *span)),
                }
            }
            Expr::Ident(name, span) => {
                if name == "None" {
                    return Ok(Ty::Option(Box::new(Ty::Unknown)));
                }
                if skip == Some(name.as_str()) {
                    return self.read_var(name, *span);
                }
                if consume {
                    self.move_var(name, *span)
                } else {
                    self.read_var(name, *span)
                }
            }
            Expr::Unary { op, expr, span } => {
                let inner = self.infer_expr(expr)?;
                match op {
                    UnOp::Neg => match inner {
                        Ty::Int | Ty::Float => Ok(inner),
                        _ => Err(err(
                            Msg::OpTypeMismatch {
                                op: "-".into(),
                                actual: inner.name(),
                            },
                            *span,
                        )),
                    },
                    UnOp::Not => Ok(Ty::Bool),
                }
            }
            Expr::Binary { op, lhs, rhs, span } => self.infer_binary(*op, lhs, rhs, *span),
            Expr::Call { callee, args, span } => self.infer_call(callee, args, *span),
            Expr::Field { obj, name, span } => {
                let obj_ty = self.infer_expr(obj)?;
                let base = obj_ty.deref().clone();
                match &base {
                    Ty::Struct(sname) => {
                        let fields = self
                            .structs
                            .get(sname)
                            .ok_or_else(|| err(Msg::UnknownStruct(sname.clone()), *span))?;
                        let Some((_, fty)) = fields.iter().find(|(n, _)| n == name) else {
                            return Err(err(
                                Msg::UnknownField {
                                    ty: sname.clone(),
                                    field: name.clone(),
                                },
                                *span,
                            ));
                        };
                        Ok(fty.clone())
                    }
                    other => Err(err(
                        Msg::UnknownField {
                            ty: other.name(),
                            field: name.clone(),
                        },
                        *span,
                    )),
                }
            }
            Expr::Index { obj, index, span } => {
                let obj_ty = self.infer_expr(obj)?;
                let idx_ty = self.infer_expr(index)?;
                let base = obj_ty.deref();
                match base {
                    Ty::List(inner) => {
                        if !matches!(idx_ty, Ty::Int | Ty::Unknown) {
                            return Err(err(Msg::IndexNotInt, *span));
                        }
                        Ok(inner.as_ref().clone())
                    }
                    Ty::Tuple(ts) => {
                        if !matches!(idx_ty, Ty::Int | Ty::Unknown) {
                            return Err(err(Msg::IndexNotInt, *span));
                        }
                        Ok(ts.first().cloned().unwrap_or(Ty::Unknown))
                    }
                    Ty::Dict(k, v) => {
                        if !types_compatible(k, &idx_ty) && !matches!(k.as_ref(), Ty::Unknown) {
                            return Err(err(
                                Msg::DictKeyMismatch {
                                    expected: k.name(),
                                    actual: idx_ty.name(),
                                },
                                *span,
                            ));
                        }
                        Ok(v.as_ref().clone())
                    }
                    Ty::Json => Ok(Ty::Json),
                    Ty::Unknown => Ok(Ty::Unknown),
                    other => Err(err(Msg::IndexOnNonList(other.name()), *span)),
                }
            }
            Expr::Borrow {
                mutable,
                expr,
                span,
            } => self.borrow_var(expr, *mutable, *span),
        }
    }

    fn infer_binary(
        &mut self,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        span: Span,
    ) -> Result<Ty, TypeError> {
        use BinOp::*;
        match op {
            And | Or => {
                let and_str = if op == And { "and" } else { "or" };
                let l = self.infer_expr(lhs)?;
                if !l.is_truthy() {
                    return Err(err(
                        Msg::OpTypeMismatch {
                            op: and_str.into(),
                            actual: l.name(),
                        },
                        span,
                    ));
                }
                let r = self.infer_expr(rhs)?;
                if !r.is_truthy() {
                    return Err(err(
                        Msg::OpTypeMismatch {
                            op: and_str.into(),
                            actual: r.name(),
                        },
                        span,
                    ));
                }
                Ok(Ty::Bool)
            }
            Add | Sub | Mul | Div | Mod => {
                let l = self.infer_expr(lhs)?;
                let r = self.infer_expr(rhs)?;
                match (&l, &r) {
                    (Ty::Int, Ty::Int) => Ok(Ty::Int),
                    (Ty::Float, Ty::Float) | (Ty::Int, Ty::Float) | (Ty::Float, Ty::Int) => {
                        Ok(Ty::Float)
                    }
                    (Ty::Str, Ty::Str) if op == Add => Ok(Ty::Str),
                    (Ty::TypeVar(tv), _) | (_, Ty::TypeVar(tv)) => Ok(Ty::TypeVar(tv.clone())),
                    (Ty::Unknown, t) | (t, Ty::Unknown) => Ok((*t).clone()),
                    _ => Err(err(
                        Msg::OpTypeMismatch {
                            op: op_str(op).into(),
                            actual: format!("{} and {}", l.name(), r.name()),
                        },
                        span,
                    )),
                }
            }
            Eq | Ne | Lt | Le | Gt | Ge => {
                let l = self.infer_expr(lhs)?;
                let r = self.infer_expr(rhs)?;
                let ok = matches!(
                    (&l, &r),
                    (Ty::Int, Ty::Int)
                        | (Ty::Float, Ty::Float)
                        | (Ty::Int, Ty::Float)
                        | (Ty::Float, Ty::Int)
                        | (Ty::Str, Ty::Str)
                        | (Ty::Bool, Ty::Bool)
                        | (Ty::TypeVar(_), _)
                        | (_, Ty::TypeVar(_))
                        | (Ty::Json, _)
                        | (_, Ty::Json)
                        | (Ty::Unknown, _)
                        | (_, Ty::Unknown)
                );
                if ok {
                    Ok(Ty::Bool)
                } else {
                    Err(err(
                        Msg::OpTypeMismatch {
                            op: op_str(op).into(),
                            actual: format!("{} and {}", l.name(), r.name()),
                        },
                        span,
                    ))
                }
            }
        }
    }

    fn infer_call(&mut self, callee: &Expr, args: &[Expr], span: Span) -> Result<Ty, TypeError> {
        // Method call: obj.method(...)
        if let Expr::Field { obj, name, .. } = callee {
            return self.infer_method(obj, name, args, span);
        }
        // Channel construction: `Chan[int]()` / `Chan[int](10)`.
        if let Expr::Index { obj, index, .. } = callee {
            if let Expr::Ident(cname, _) = obj.as_ref() {
                if cname == "Chan" {
                    let elem_name = match index.as_ref() {
                        Expr::Ident(n, _) => n.clone(),
                        _ => return Err(err(Msg::UnknownType("<chan elem>".into()), span)),
                    };
                    let elem = Ty::from_name(&elem_name)
                        .ok_or_else(|| err(Msg::UnknownType(elem_name.clone()), span))?;
                    if args.len() > 1 {
                        return Err(err(Msg::ArgCount("Chan".into(), 1, args.len()), span));
                    }
                    if let Some(a) = args.first() {
                        let t = self.infer_expr(a)?;
                        if !matches!(t, Ty::Int | Ty::Unknown) {
                            return Err(err(
                                Msg::ArgTypeMismatch {
                                    func: "Chan".into(),
                                    index: 0,
                                    expected: "int".into(),
                                    actual: t.name(),
                                },
                                span,
                            ));
                        }
                    }
                    return Ok(Ty::Chan(Box::new(elem)));
                }
            }
        }
        if let Expr::Ident(name, _) = callee {
            match name.as_str() {
                "Some" => {
                    if args.len() != 1 {
                        return Err(err(Msg::ArgCount("Some".into(), 1, args.len()), span));
                    }
                    let inner = self.infer_expr(args.first().unwrap())?;
                    return Ok(Ty::Option(Box::new(inner)));
                }
                "None" => {
                    if !args.is_empty() {
                        return Err(err(Msg::ArgCount("None".into(), 0, args.len()), span));
                    }
                    return Ok(Ty::Option(Box::new(Ty::Unknown)));
                }
                "Ok" => {
                    if args.len() != 1 {
                        return Err(err(Msg::ArgCount("Ok".into(), 1, args.len()), span));
                    }
                    let inner = self.infer_expr(args.first().unwrap())?;
                    return Ok(Ty::Result(Box::new(inner), Box::new(Ty::Unknown)));
                }
                "Err" => {
                    if args.len() != 1 {
                        return Err(err(Msg::ArgCount("Err".into(), 1, args.len()), span));
                    }
                    let inner = self.infer_expr(args.first().unwrap())?;
                    return Ok(Ty::Result(Box::new(Ty::Unknown), Box::new(inner)));
                }
                "print" => {
                    for a in args {
                        self.infer_expr(a)?;
                    }
                    return Ok(Ty::Unit);
                }
                "read_to_str" | "json_decode" => {
                    if args.len() != 1 {
                        return Err(err(Msg::ArgCount(name.clone(), 1, args.len()), span));
                    }
                    let t = self.infer_expr(&args[0])?;
                    if !matches!(t.deref(), Ty::Str | Ty::Unknown) {
                        return Err(err(
                            Msg::ArgTypeMismatch {
                                func: name.clone(),
                                index: 0,
                                expected: "str".into(),
                                actual: t.name(),
                            },
                            span,
                        ));
                    }
                    let ok = if name == "read_to_str" {
                        Ty::Str
                    } else {
                        Ty::Json
                    };
                    return Ok(Ty::Result(Box::new(ok), Box::new(Ty::Str)));
                }
                "write" => {
                    if args.len() != 2 {
                        return Err(err(Msg::ArgCount("write".into(), 2, args.len()), span));
                    }
                    for a in args {
                        let t = self.infer_expr(a)?;
                        if !matches!(t.deref(), Ty::Str | Ty::Unknown) {
                            return Err(err(
                                Msg::ArgTypeMismatch {
                                    func: "write".into(),
                                    index: 0,
                                    expected: "str".into(),
                                    actual: t.name(),
                                },
                                span,
                            ));
                        }
                    }
                    return Ok(Ty::Result(Box::new(Ty::Unit), Box::new(Ty::Str)));
                }
                "abs" => {
                    if args.len() != 1 {
                        return Err(err(Msg::ArgCount("abs".into(), 1, args.len()), span));
                    }
                    let t = self.infer_expr(&args[0])?;
                    match t.deref() {
                        Ty::Int => return Ok(Ty::Int),
                        Ty::Float => return Ok(Ty::Float),
                        Ty::Unknown | Ty::TypeVar(_) => return Ok(Ty::Unknown),
                        other => {
                            return Err(err(
                                Msg::ArgTypeMismatch {
                                    func: "abs".into(),
                                    index: 0,
                                    expected: "int or float".into(),
                                    actual: other.name(),
                                },
                                span,
                            ))
                        }
                    }
                }
                "clock" => {
                    if !args.is_empty() {
                        return Err(err(Msg::ArgCount("clock".into(), 0, args.len()), span));
                    }
                    return Ok(Ty::Int);
                }
                "sleep" => {
                    if args.len() != 1 {
                        return Err(err(Msg::ArgCount("sleep".into(), 1, args.len()), span));
                    }
                    let t = self.infer_expr(&args[0])?;
                    if !matches!(t.deref(), Ty::Int | Ty::Unknown) {
                        return Err(err(
                            Msg::ArgTypeMismatch {
                                func: "sleep".into(),
                                index: 0,
                                expected: "int".into(),
                                actual: t.name(),
                            },
                            span,
                        ));
                    }
                    return Ok(Ty::Unit);
                }
                "floor" | "ceil" | "round" => {
                    if args.len() != 1 {
                        return Err(err(Msg::ArgCount(name.clone(), 1, args.len()), span));
                    }
                    let t = self.infer_expr(&args[0])?;
                    if !matches!(t.deref(), Ty::Float | Ty::Int | Ty::Unknown) {
                        return Err(err(
                            Msg::ArgTypeMismatch {
                                func: name.clone(),
                                index: 0,
                                expected: "float".into(),
                                actual: t.name(),
                            },
                            span,
                        ));
                    }
                    return Ok(Ty::Int);
                }
                "sqrt" | "pow" => {
                    if args.len() != (if name == "sqrt" { 1 } else { 2 }) {
                        return Err(err(
                            Msg::ArgCount(
                                name.clone(),
                                if name == "sqrt" { 1 } else { 2 },
                                args.len(),
                            ),
                            span,
                        ));
                    }
                    for a in args {
                        let t = self.infer_expr(a)?;
                        if !matches!(t.deref(), Ty::Float | Ty::Int | Ty::Unknown) {
                            return Err(err(
                                Msg::ArgTypeMismatch {
                                    func: name.clone(),
                                    index: 0,
                                    expected: "float".into(),
                                    actual: t.name(),
                                },
                                span,
                            ));
                        }
                    }
                    return Ok(Ty::Float);
                }
                "json_encode" => {
                    if args.len() != 1 {
                        return Err(err(
                            Msg::ArgCount("json_encode".into(), 1, args.len()),
                            span,
                        ));
                    }
                    self.infer_expr(&args[0])?;
                    return Ok(Ty::Str);
                }
                "range" => {
                    for (i, a) in args.iter().enumerate() {
                        let ty = self.infer_expr(a)?;
                        if !matches!(ty, Ty::Int | Ty::Unknown) {
                            return Err(err(
                                Msg::ArgTypeMismatch {
                                    func: "range".into(),
                                    index: i,
                                    expected: "int".into(),
                                    actual: ty.name(),
                                },
                                span,
                            ));
                        }
                    }
                    if args.is_empty() || args.len() > 2 {
                        return Err(err(Msg::ArgCount("range".into(), 2, args.len()), span));
                    }
                    return Ok(Ty::Range);
                }
                _ => {}
            }
            // Struct construction: `Circle(1, 2)` positional fields.
            if let Some(fields) = self.structs.get(name).cloned() {
                if args.len() != fields.len() {
                    return Err(err(
                        Msg::ArgCount(name.clone(), fields.len(), args.len()),
                        span,
                    ));
                }
                for (i, (a, (_, fty))) in args.iter().zip(&fields).enumerate() {
                    let actual = self.infer_expr_consume(a, span)?;
                    if !self.is_compatible(fty, &actual) {
                        return Err(err(
                            Msg::ArgTypeMismatch {
                                func: name.clone(),
                                index: i,
                                expected: fty.name(),
                                actual: actual.name(),
                            },
                            span,
                        ));
                    }
                }
                return Ok(Ty::Struct(name.clone()));
            }
            if let Some(sig) = self.fns.get(name).cloned() {
                return self.check_fn_call(&sig, args, span);
            }
        }
        // Non-identifier callee (e.g. a call expression result): skip.
        self.infer_expr(callee)?;
        for a in args {
            self.infer_expr(a)?;
        }
        Ok(Ty::Unknown)
    }

    fn check_fn_call(&mut self, sig: &FnSig, args: &[Expr], span: Span) -> Result<Ty, TypeError> {
        // Instantiate generic type variables from the argument types.
        let mut bindings: HashMap<String, Ty> = HashMap::new();
        if !sig.type_params.is_empty() {
            if args.len() > sig.params.len() {
                return Err(err(
                    Msg::ArgCount(sig.name.clone(), sig.params.len(), args.len()),
                    span,
                ));
            }
            for (a, (_, pty)) in args.iter().zip(&sig.params) {
                let actual = self.infer_expr(a)?;
                collect_bindings(pty, &actual, &mut bindings);
            }
            // Check constraints (e.g. `T: Comparable`).
            for (tv, bound) in &sig.type_params {
                if let Some(ty) = bindings.get(tv) {
                    if let Some(b) = bound {
                        if !self.satisfies_bound(ty, b) {
                            return Err(err(
                                Msg::GenericConstraint {
                                    func: sig.name.clone(),
                                    bound: b.clone(),
                                    ty: ty.name(),
                                },
                                span,
                            ));
                        }
                    }
                }
            }
        }
        fn subst(t: &Ty, bindings: &HashMap<String, Ty>) -> Ty {
            match t {
                Ty::TypeVar(tv) => bindings.get(tv).cloned().unwrap_or_else(|| t.clone()),
                Ty::List(inner) => Ty::List(Box::new(subst(inner, bindings))),
                Ty::Chan(inner) => Ty::Chan(Box::new(subst(inner, bindings))),
                Ty::Option(inner) => Ty::Option(Box::new(subst(inner, bindings))),
                Ty::Result(a, b) => {
                    Ty::Result(Box::new(subst(a, bindings)), Box::new(subst(b, bindings)))
                }
                Ty::Dict(a, b) => {
                    Ty::Dict(Box::new(subst(a, bindings)), Box::new(subst(b, bindings)))
                }
                Ty::Set(inner) => Ty::Set(Box::new(subst(inner, bindings))),
                Ty::Tuple(ts) => Ty::Tuple(ts.iter().map(|t| subst(t, bindings)).collect()),
                Ty::Ref(inner) => Ty::Ref(Box::new(subst(inner, bindings))),
                Ty::MutRef(inner) => Ty::MutRef(Box::new(subst(inner, bindings))),
                other => other.clone(),
            }
        }
        let expected_params = sig.params.clone();
        if args.len() != expected_params.len() {
            return Err(err(
                Msg::ArgCount(sig.name.clone(), expected_params.len(), args.len()),
                span,
            ));
        }
        for (i, (a, (_, expected))) in args.iter().zip(&expected_params).enumerate() {
            let expected = subst(expected, &bindings);
            let (actual, borrowed) = self.infer_call_arg(a, &expected, span)?;
            if !borrowed && !self.is_compatible(&expected, &actual) {
                return Err(err(
                    Msg::ArgTypeMismatch {
                        func: sig.name.clone(),
                        index: i,
                        expected: expected.name(),
                        actual: actual.name(),
                    },
                    span,
                ));
            }
        }
        Ok(subst(&sig.ret, &bindings))
    }

    fn satisfies_bound(&self, ty: &Ty, bound: &str) -> bool {
        match bound {
            // Comparable: supports `==` / ordering comparisons.
            "Comparable" => matches!(ty.deref(), Ty::Int | Ty::Float | Ty::Bool | Ty::Str),
            _ => false,
        }
    }

    /// Checks one call argument against the expected type. Returns the
    /// actual type and whether the argument was borrowed (ref param).
    fn infer_call_arg(
        &mut self,
        arg: &Expr,
        expected: &Ty,
        span: Span,
    ) -> Result<(Ty, bool), TypeError> {
        match expected {
            Ty::Ref(inner) => {
                let t = match arg {
                    Expr::Ident(..) => self.borrow_var_transient(arg, false, span)?,
                    // Value expressions coerce into a temporary the call
                    // borrows for its duration.
                    _ => self.infer_expr(arg)?,
                };
                let ty = t.deref().clone();
                if !self.is_compatible(inner, &ty) && !matches!(inner.as_ref(), Ty::Unknown) {
                    return Err(err(
                        Msg::ArgTypeMismatch {
                            func: "<call>".into(),
                            index: 0,
                            expected: inner.name(),
                            actual: ty.name(),
                        },
                        span,
                    ));
                }
                Ok((t, true))
            }
            Ty::MutRef(inner) => {
                let t = match arg {
                    Expr::Ident(..) => self.borrow_var_transient(arg, true, span)?,
                    _ => self.infer_expr(arg)?,
                };
                let ty = t.deref().clone();
                if !self.is_compatible(inner, &ty) && !matches!(inner.as_ref(), Ty::Unknown) {
                    return Err(err(
                        Msg::ArgTypeMismatch {
                            func: "<call>".into(),
                            index: 0,
                            expected: inner.name(),
                            actual: ty.name(),
                        },
                        span,
                    ));
                }
                Ok((t, true))
            }
            _ => {
                // Empty collection literals get their element type from the
                // expected parameter type (e.g. `[]` for `xs: List[int]`).
                let t = match (arg, expected) {
                    (Expr::List(items, _), Ty::List(inner)) if items.is_empty() => {
                        Ty::List(inner.clone())
                    }
                    (Expr::Dict(pairs, _), Ty::Dict(k, v)) if pairs.is_empty() => {
                        Ty::Dict(k.clone(), v.clone())
                    }
                    (Expr::Set(items, _), Ty::Set(inner)) if items.is_empty() => {
                        Ty::Set(inner.clone())
                    }
                    (Expr::Tuple(items, _), Ty::Tuple(ts)) if items.is_empty() => {
                        Ty::Tuple(ts.clone())
                    }
                    _ => self.infer_expr_consume(arg, span)?,
                };
                Ok((t, false))
            }
        }
    }

    fn infer_method(
        &mut self,
        obj: &Expr,
        name: &str,
        args: &[Expr],
        span: Span,
    ) -> Result<Ty, TypeError> {
        let obj_ty = self.infer_expr(obj)?;
        let base = obj_ty.deref().clone();
        match &base {
            Ty::List(inner) => self.infer_list_method(obj, inner, name, args, span),
            Ty::Chan(inner) => self.infer_chan_method(obj, inner, name, args, span),
            Ty::Json => self.infer_json_method(obj, name, args, span),
            Ty::Str => self.infer_str_method(obj, name, args, span),
            Ty::Dict(k, v) => self.infer_dict_method(obj, k, v, name, args, span),
            Ty::Set(inner) => self.infer_set_method(obj, inner, name, args, span),
            Ty::Tuple(ts) => self.infer_tuple_method(obj, ts, name, args, span),
            Ty::Option(inner) => self.infer_option_method(obj, inner, name, args, span),
            Ty::Result(ok, _) => self.infer_result_method(obj, ok, name, args, span),
            Ty::Struct(sname) => {
                if let Some(sig) = self
                    .methods
                    .get(&(sname.clone(), name.to_string()))
                    .cloned()
                {
                    // The receiver is borrowed for the duration of the call;
                    // `self` is the first parameter and is not user-supplied.
                    self.borrow_var_transient(obj, false, span)?;
                    let expected = sig.params[0].1.clone();
                    if !matches!(expected, Ty::Ref(_) | Ty::MutRef(_)) {
                        // self by value: receiver moves into the method.
                        self.move_var_obj(obj, span)?;
                    }
                    self.check_method_args(&sig, args, span)
                } else {
                    Err(err(
                        Msg::UnknownMethod {
                            ty: sname.clone(),
                            method: name.to_string(),
                        },
                        span,
                    ))
                }
            }
            Ty::Interface(iname) => {
                // Interface method calls check against the interface
                // declaration; dispatch happens at runtime by struct type.
                let sigs = self.interfaces.get(iname).cloned().unwrap_or_default();
                let Some(method) = sigs.iter().find(|m| m.name == *name) else {
                    return Err(err(
                        Msg::UnknownMethod {
                            ty: iname.clone(),
                            method: name.to_string(),
                        },
                        span,
                    ));
                };
                self.borrow_var_transient(obj, false, span)?;
                let sig = self.fn_sig(
                    &method.name,
                    &[],
                    &method.params,
                    method.ret.as_ref(),
                    Some(&Ty::Interface(iname.clone())),
                )?;
                self.check_method_args(&sig, args, span)
            }
            Ty::Unknown => {
                for a in args {
                    self.infer_expr(a)?;
                }
                Ok(Ty::Unknown)
            }
            other => {
                if name == "to_str" && args.is_empty() {
                    return Ok(Ty::Str);
                }
                Err(err(
                    Msg::UnknownMethod {
                        ty: other.name(),
                        method: name.to_string(),
                    },
                    span,
                ))
            }
        }
    }

    /// Checks a builtin `Json` method call (`len` / `contains`); dispatch
    /// happens at runtime on the underlying value.
    fn infer_json_method(
        &mut self,
        _obj: &Expr,
        name: &str,
        args: &[Expr],
        span: Span,
    ) -> Result<Ty, TypeError> {
        match name {
            "len" => {
                if !args.is_empty() {
                    return Err(err(Msg::ArgCount("Json.len".into(), 0, args.len()), span));
                }
                Ok(Ty::Int)
            }
            "contains" => {
                if args.len() != 1 {
                    return Err(err(
                        Msg::ArgCount("Json.contains".into(), 1, args.len()),
                        span,
                    ));
                }
                self.infer_expr(&args[0])?;
                Ok(Ty::Bool)
            }
            "keys" => {
                if !args.is_empty() {
                    return Err(err(Msg::ArgCount("Json.keys".into(), 0, args.len()), span));
                }
                Ok(Ty::List(Box::new(Ty::Json)))
            }
            "is_int" | "is_str" => {
                if !args.is_empty() {
                    return Err(err(
                        Msg::ArgCount(format!("Json.{}", name), 0, args.len()),
                        span,
                    ));
                }
                Ok(Ty::Bool)
            }
            "to_str" => {
                if !args.is_empty() {
                    return Err(err(
                        Msg::ArgCount("Json.to_str".into(), 0, args.len()),
                        span,
                    ));
                }
                Ok(Ty::Str)
            }
            _ => Err(err(
                Msg::UnknownMethod {
                    ty: "Json".into(),
                    method: name.to_string(),
                },
                span,
            )),
        }
    }

    /// Checks a builtin `str` method call.
    fn infer_str_method(
        &mut self,
        _obj: &Expr,
        name: &str,
        args: &[Expr],
        span: Span,
    ) -> Result<Ty, TypeError> {
        let expect = |n: usize| -> Result<(), TypeError> {
            if args.len() != n {
                return Err(err(
                    Msg::ArgCount(format!("str.{}", name), n, args.len()),
                    span,
                ));
            }
            Ok(())
        };
        match name {
            "len" => {
                expect(0)?;
                Ok(Ty::Int)
            }
            "to_str" => {
                expect(0)?;
                Ok(Ty::Str)
            }
            "sub" => {
                expect(2)?;
                self.check_int_args(args, "str.sub")?;
                Ok(Ty::Str)
            }
            "split" => {
                expect(1)?;
                self.check_str_arg(&args[0], "str.split")?;
                Ok(Ty::List(Box::new(Ty::Str)))
            }
            "join" => {
                expect(1)?;
                let a = self.infer_expr(&args[0])?;
                if !matches!(a.deref(), Ty::List(_) | Ty::Unknown) {
                    return Err(err(
                        Msg::ArgTypeMismatch {
                            func: "str.join".into(),
                            index: 0,
                            expected: "List[str]".into(),
                            actual: a.name(),
                        },
                        span,
                    ));
                }
                Ok(Ty::Str)
            }
            "contains" | "starts_with" | "ends_with" => {
                expect(1)?;
                self.check_str_arg(&args[0], &format!("str.{}", name))?;
                Ok(Ty::Bool)
            }
            "trim" => {
                expect(0)?;
                Ok(Ty::Str)
            }
            "to_int" => {
                expect(0)?;
                Ok(Ty::Result(Box::new(Ty::Int), Box::new(Ty::Str)))
            }
            "to_float" => {
                expect(0)?;
                Ok(Ty::Result(Box::new(Ty::Float), Box::new(Ty::Str)))
            }
            _ => Err(err(
                Msg::UnknownMethod {
                    ty: "str".into(),
                    method: name.to_string(),
                },
                span,
            )),
        }
    }

    fn check_int_args(&mut self, args: &[Expr], func: &str) -> Result<(), TypeError> {
        for (i, a) in args.iter().enumerate() {
            let t = self.infer_expr(a)?;
            if !matches!(t.deref(), Ty::Int | Ty::Unknown | Ty::TypeVar(_)) {
                return Err(err(
                    Msg::ArgTypeMismatch {
                        func: func.into(),
                        index: i,
                        expected: "int".into(),
                        actual: t.name(),
                    },
                    Span::new(0, 0),
                ));
            }
        }
        Ok(())
    }

    fn check_str_arg(&mut self, a: &Expr, func: &str) -> Result<(), TypeError> {
        let t = self.infer_expr(a)?;
        if !matches!(t.deref(), Ty::Str | Ty::Unknown) {
            return Err(err(
                Msg::ArgTypeMismatch {
                    func: func.into(),
                    index: 0,
                    expected: "str".into(),
                    actual: t.name(),
                },
                Span::new(0, 0),
            ));
        }
        Ok(())
    }

    /// Checks a builtin `Dict[K, V]` method call.
    fn infer_dict_method(
        &mut self,
        _obj: &Expr,
        k: &Ty,
        v: &Ty,
        name: &str,
        args: &[Expr],
        span: Span,
    ) -> Result<Ty, TypeError> {
        let expect = |n: usize| -> Result<(), TypeError> {
            if args.len() != n {
                return Err(err(
                    Msg::ArgCount(format!("Dict.{}", name), n, args.len()),
                    span,
                ));
            }
            Ok(())
        };
        match name {
            "len" | "keys" | "values" => {
                expect(0)?;
                Ok(match name {
                    "len" => Ty::Int,
                    "keys" => Ty::List(Box::new(k.clone())),
                    _ => Ty::List(Box::new(v.clone())),
                })
            }
            "get" => {
                expect(1)?;
                let at = self.infer_expr(&args[0])?;
                if !types_compatible(k, &at) && !matches!(k, Ty::Unknown) {
                    return Err(err(
                        Msg::DictKeyMismatch {
                            expected: k.name(),
                            actual: at.name(),
                        },
                        span,
                    ));
                }
                Ok(Ty::Option(Box::new(v.clone())))
            }
            "contains" => {
                expect(1)?;
                let at = self.infer_expr(&args[0])?;
                if !types_compatible(k, &at) && !matches!(k, Ty::Unknown) {
                    return Err(err(
                        Msg::DictKeyMismatch {
                            expected: k.name(),
                            actual: at.name(),
                        },
                        span,
                    ));
                }
                Ok(Ty::Bool)
            }
            "set" | "remove" => {
                expect(2 - (name == "remove") as usize)?;
                let at = self.infer_expr(&args[0])?;
                if !types_compatible(k, &at) && !matches!(k, Ty::Unknown) {
                    return Err(err(
                        Msg::DictKeyMismatch {
                            expected: k.name(),
                            actual: at.name(),
                        },
                        span,
                    ));
                }
                if name == "set" {
                    let vt = self.infer_expr(&args[1])?;
                    if !types_compatible(v, &vt) && !matches!(v, Ty::Unknown) {
                        return Err(err(
                            Msg::ListElemMismatch {
                                expected: v.name(),
                                actual: vt.name(),
                            },
                            span,
                        ));
                    }
                }
                Ok(Ty::Unit)
            }
            _ => Err(err(
                Msg::UnknownMethod {
                    ty: k.name() + "->" + &v.name() + " dict",
                    method: name.to_string(),
                },
                span,
            )),
        }
    }

    /// Checks a builtin `Set[T]` method call.
    fn infer_set_method(
        &mut self,
        _obj: &Expr,
        inner: &Ty,
        name: &str,
        args: &[Expr],
        span: Span,
    ) -> Result<Ty, TypeError> {
        let expect = |n: usize| -> Result<(), TypeError> {
            if args.len() != n {
                return Err(err(
                    Msg::ArgCount(format!("Set.{}", name), n, args.len()),
                    span,
                ));
            }
            Ok(())
        };
        match name {
            "len" => {
                expect(0)?;
                Ok(Ty::Int)
            }
            "add" | "remove" => {
                expect(1)?;
                let at = self.infer_expr(&args[0])?;
                if !types_compatible(inner, &at) && !matches!(inner, Ty::Unknown) {
                    return Err(err(
                        Msg::SetElemMismatch {
                            expected: inner.name(),
                            actual: at.name(),
                        },
                        span,
                    ));
                }
                Ok(Ty::Unit)
            }
            "contains" => {
                expect(1)?;
                let at = self.infer_expr(&args[0])?;
                if !types_compatible(inner, &at) && !matches!(inner, Ty::Unknown) {
                    return Err(err(
                        Msg::SetElemMismatch {
                            expected: inner.name(),
                            actual: at.name(),
                        },
                        span,
                    ));
                }
                Ok(Ty::Bool)
            }
            _ => Err(err(
                Msg::UnknownMethod {
                    ty: format!("Set[{}]", inner.name()),
                    method: name.to_string(),
                },
                span,
            )),
        }
    }

    /// Checks a builtin `Tuple` method call (currently only `len`).
    fn infer_tuple_method(
        &mut self,
        _obj: &Expr,
        _ts: &[Ty],
        name: &str,
        args: &[Expr],
        span: Span,
    ) -> Result<Ty, TypeError> {
        if name == "len" && args.is_empty() {
            return Ok(Ty::Int);
        }
        Err(err(
            Msg::UnknownMethod {
                ty: "tuple".into(),
                method: name.to_string(),
            },
            span,
        ))
    }

    /// Checks a builtin `Option[T]` method call.
    fn infer_option_method(
        &mut self,
        _obj: &Expr,
        inner: &Ty,
        name: &str,
        args: &[Expr],
        span: Span,
    ) -> Result<Ty, TypeError> {
        let expect = |n: usize| -> Result<(), TypeError> {
            if args.len() != n {
                return Err(err(
                    Msg::ArgCount(format!("Option.{}", name), n, args.len()),
                    span,
                ));
            }
            Ok(())
        };
        match name {
            "is_some" | "is_none" => {
                expect(0)?;
                Ok(Ty::Bool)
            }
            "unwrap" => {
                expect(0)?;
                Ok(inner.clone())
            }
            _ => Err(err(
                Msg::UnknownMethod {
                    ty: format!("Option[{}]", inner.name()),
                    method: name.to_string(),
                },
                span,
            )),
        }
    }

    /// Checks a builtin `Result[T, E]` method call.
    fn infer_result_method(
        &mut self,
        _obj: &Expr,
        ok: &Ty,
        name: &str,
        args: &[Expr],
        span: Span,
    ) -> Result<Ty, TypeError> {
        let expect = |n: usize| -> Result<(), TypeError> {
            if args.len() != n {
                return Err(err(
                    Msg::ArgCount(format!("Result.{}", name), n, args.len()),
                    span,
                ));
            }
            Ok(())
        };
        match name {
            "is_ok" | "is_err" => {
                expect(0)?;
                Ok(Ty::Bool)
            }
            "unwrap" => {
                expect(0)?;
                Ok(ok.clone())
            }
            "unwrap_err" => {
                expect(0)?;
                Ok(Ty::Unknown)
            }
            _ => Err(err(
                Msg::UnknownMethod {
                    ty: "Result".into(),
                    method: name.to_string(),
                },
                span,
            )),
        }
    }

    /// Checks the user-supplied args of a method call, skipping `self`.
    fn check_method_args(
        &mut self,
        sig: &FnSig,
        args: &[Expr],
        span: Span,
    ) -> Result<Ty, TypeError> {
        let expected_params = &sig.params[1..];
        if args.len() != expected_params.len() {
            return Err(err(
                Msg::ArgCount(sig.name.clone(), expected_params.len(), args.len()),
                span,
            ));
        }
        for (i, (a, (_, expected))) in args.iter().zip(expected_params).enumerate() {
            let (actual, borrowed) = self.infer_call_arg(a, expected, span)?;
            if !borrowed && !types_compatible(expected, &actual) {
                return Err(err(
                    Msg::ArgTypeMismatch {
                        func: sig.name.clone(),
                        index: i + 1,
                        expected: expected.name(),
                        actual: actual.name(),
                    },
                    span,
                ));
            }
        }
        Ok(sig.ret.clone())
    }

    /// Moves a non-copy receiver passed to a by-value `self` parameter.
    fn move_var_obj(&mut self, obj: &Expr, span: Span) -> Result<(), TypeError> {
        if let Expr::Ident(name, _) = obj {
            let ty = self.read_var(name, span)?;
            if !ty.is_copy() {
                self.move_var(name, span)?;
            }
        }
        Ok(())
    }

    fn infer_list_method(
        &mut self,
        obj: &Expr,
        inner: &Ty,
        name: &str,
        args: &[Expr],
        span: Span,
    ) -> Result<Ty, TypeError> {
        match name {
            "len" => {
                if !args.is_empty() {
                    return Err(err(Msg::ArgCount("List.len".into(), 0, args.len()), span));
                }
                Ok(Ty::Int)
            }
            "contains" => {
                if args.len() != 1 {
                    return Err(err(
                        Msg::ArgCount("List.contains".into(), 1, args.len()),
                        span,
                    ));
                }
                let t = self.infer_expr_consume(&args[0], span)?;
                if !self.is_compatible(inner, &t) && !matches!(inner, Ty::Unknown) {
                    return Err(err(
                        Msg::ArgTypeMismatch {
                            func: "List.contains".into(),
                            index: 0,
                            expected: inner.name(),
                            actual: t.name(),
                        },
                        span,
                    ));
                }
                Ok(Ty::Bool)
            }
            "push" => {
                if args.len() != 1 {
                    return Err(err(Msg::ArgCount("List.push".into(), 1, args.len()), span));
                }
                let t = self.infer_expr_consume(&args[0], span)?;
                if !self.is_compatible(inner, &t) && !matches!(inner, Ty::Unknown) {
                    return Err(err(
                        Msg::ArgTypeMismatch {
                            func: "List.push".into(),
                            index: 0,
                            expected: inner.name(),
                            actual: t.name(),
                        },
                        span,
                    ));
                }
                // push mutates: requires a mutable borrow of the receiver.
                self.borrow_var_transient(obj, true, span)?;
                Ok(Ty::Unit)
            }
            "get" => {
                if args.len() != 1 {
                    return Err(err(Msg::ArgCount("List.get".into(), 1, args.len()), span));
                }
                let t = self.infer_expr(&args[0])?;
                if !matches!(t, Ty::Int | Ty::Unknown) {
                    return Err(err(Msg::IndexNotInt, span));
                }
                Ok(inner.clone())
            }
            "set" => {
                if args.len() != 2 {
                    return Err(err(Msg::ArgCount("List.set".into(), 2, args.len()), span));
                }
                let t = self.infer_expr(&args[0])?;
                if !matches!(t, Ty::Int | Ty::Unknown) {
                    return Err(err(Msg::IndexNotInt, span));
                }
                let v = self.infer_expr_consume(&args[1], span)?;
                if !self.is_compatible(inner, &v) && !matches!(inner, Ty::Unknown) {
                    return Err(err(
                        Msg::ArgTypeMismatch {
                            func: "List.set".into(),
                            index: 1,
                            expected: inner.name(),
                            actual: v.name(),
                        },
                        span,
                    ));
                }
                self.borrow_var_transient(obj, true, span)?;
                Ok(Ty::Unit)
            }
            _ => Err(err(
                Msg::UnknownMethod {
                    ty: "List".into(),
                    method: name.to_string(),
                },
                span,
            )),
        }
    }

    /// `Chan[T]` methods: `send(v)`, `recv() -> Option[T]`, `close()`.
    fn infer_chan_method(
        &mut self,
        _obj: &Expr,
        inner: &Ty,
        name: &str,
        args: &[Expr],
        span: Span,
    ) -> Result<Ty, TypeError> {
        match name {
            "send" => {
                if args.len() != 1 {
                    return Err(err(Msg::ArgCount("Chan.send".into(), 1, args.len()), span));
                }
                let t = self.infer_expr_consume(&args[0], span)?;
                if !self.is_compatible(inner, &t) && !matches!(inner, Ty::Unknown) {
                    return Err(err(
                        Msg::ArgTypeMismatch {
                            func: "Chan.send".into(),
                            index: 0,
                            expected: inner.name(),
                            actual: t.name(),
                        },
                        span,
                    ));
                }
                Ok(Ty::Unit)
            }
            "recv" => {
                if !args.is_empty() {
                    return Err(err(Msg::ArgCount("Chan.recv".into(), 0, args.len()), span));
                }
                Ok(Ty::Unknown) // Option[T] is not modeled in M3; treat as inner.
            }
            "close" => {
                if !args.is_empty() {
                    return Err(err(Msg::ArgCount("Chan.close".into(), 0, args.len()), span));
                }
                Ok(Ty::Unit)
            }
            _ => Err(err(
                Msg::UnknownMethod {
                    ty: "Chan".into(),
                    method: name.to_string(),
                },
                span,
            )),
        }
    }

    fn check_functions(&mut self) -> Result<(), TypeError> {
        for item in &self.program.items {
            if let Item::Fn(f) = item {
                self.check_fn_body(f)?;
            }
            if let Item::Impl(imp) = item {
                self.check_impl_body(imp)?;
            }
        }
        Ok(())
    }

    /// Checks `test` blocks as parameter-less bodies (no callable
    /// registration: they run only under `sole test`).
    /// `from foo import a, b`: every imported name must exist as a global
    /// function or type. (The symbol table is shared across modules.)
    fn check_imports(&mut self) -> Result<(), TypeError> {
        for item in &self.program.items {
            if let Item::Import(imp) = item {
                for name in &imp.names {
                    if !self.fns.contains_key(name)
                        && !self.structs.contains_key(name)
                        && !self.interfaces.contains_key(name)
                        && name != "None"
                    {
                        return Err(err(
                            Msg::UndefinedVariable(format!("{}::{}", imp.module, name)),
                            imp.span,
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    fn check_tests(&mut self) -> Result<(), TypeError> {
        for item in &self.program.items {
            if let Item::Test(t) = item {
                self.vars.push(HashMap::new());
                self.current_fn = Some(format!("test:{}", t.name));
                let result = self.check_block(&t.body);
                self.current_fn = None;
                self.vars.pop();
                result?;
            }
        }
        Ok(())
    }

    fn check_impl_body(&mut self, imp: &ImplDef) -> Result<(), TypeError> {
        // Impl methods run with `self` bound as the struct type.
        let self_ty = Ty::Struct(imp.ty.clone());
        for m in &imp.methods {
            self.vars.push(HashMap::new());
            self.current_fn = Some(format!("{}::{}", imp.ty, m.name));
            let sig = self
                .method_sig(m, &imp.ty, imp.span)
                .expect("method signature parsed during collect_impls");
            for (p, (pname, pty)) in m.params.iter().zip(&sig.params) {
                self.bind(pname.clone(), pty.clone(), p.is_mut);
            }
            let _ = self_ty.clone();
            let result = self.check_block(&m.body);
            self.current_fn = None;
            self.vars.pop();
            result?;
        }
        Ok(())
    }

    /// Whether `actual` can be used where `expected` is declared.
    /// `Struct` implements `Interface` when an `impl T: I` exists.
    /// `None` inside a collection literal is a "hole": it never forces the
    /// element type (JSON-style heterogeneous literals like `[true, None]`).
    fn is_hole(t: &Ty) -> bool {
        matches!(
            t,
            Ty::Option(inner) if matches!(inner.as_ref(), Ty::Unknown)
        )
    }

    fn is_compatible(&self, expected: &Ty, actual: &Ty) -> bool {
        if types_compatible(expected, actual) {
            return true;
        }
        match (expected, actual) {
            (Ty::Interface(i), Ty::Struct(s)) => {
                self.impls.get(s).is_some_and(|ifaces| ifaces.contains(i))
            }
            _ => false,
        }
    }
}

/// Structural compatibility: equal shapes, with `Unknown` holes accepted at
/// any depth (e.g. `Option[?]` matches `Option[int]`).
fn types_compatible(expected: &Ty, actual: &Ty) -> bool {
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
fn collect_bindings(expected: &Ty, actual: &Ty, bindings: &mut HashMap<String, Ty>) {
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

fn collect_uses(stmt: &Stmt, uses: &mut HashMap<String, usize>, idx: usize) {
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
            if let Some(ElseBranch::Block(b)) = else_block {
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

fn op_str(op: BinOp) -> &'static str {
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
