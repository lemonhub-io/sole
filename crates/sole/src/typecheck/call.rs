//! Part of the M2 static type checker — see `typecheck.rs`.

use super::*;
use sole_diag::Msg;

impl<'a> Checker<'a> {
    pub(crate) fn infer_call(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        span: Span,
    ) -> Result<Ty, TypeError> {
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
                "args" => {
                    if !args.is_empty() {
                        return Err(err(Msg::ArgCount("args".into(), 0, args.len()), span));
                    }
                    return Ok(Ty::List(Box::new(Ty::Str)));
                }
                "exit" => {
                    if args.len() != 1 {
                        return Err(err(Msg::ArgCount("exit".into(), 1, args.len()), span));
                    }
                    let t = self.infer_expr(&args[0])?;
                    if !matches!(t.deref(), Ty::Int | Ty::Unknown) {
                        return Err(err(
                            Msg::ArgTypeMismatch {
                                func: "exit".into(),
                                index: 0,
                                expected: "int".into(),
                                actual: t.name(),
                            },
                            span,
                        ));
                    }
                    return Ok(Ty::Unit);
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

    pub(crate) fn check_fn_call(
        &mut self,
        sig: &FnSig,
        args: &[Expr],
        span: Span,
    ) -> Result<Ty, TypeError> {
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

    pub(crate) fn satisfies_bound(&self, ty: &Ty, bound: &str) -> bool {
        match bound {
            // Comparable: supports `==` / ordering comparisons.
            "Comparable" => matches!(ty.deref(), Ty::Int | Ty::Float | Ty::Bool | Ty::Str),
            _ => false,
        }
    }

    /// Checks one call argument against the expected type. Returns the
    /// actual type and whether the argument was borrowed (ref param).
    pub(crate) fn infer_call_arg(
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
}
