//! Part of the M2 static type checker — see `typecheck.rs`.

use super::*;
use sole_diag::Msg;

impl<'a> Checker<'a> {
    pub(crate) fn infer_method(
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
    pub(crate) fn infer_json_method(
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
            "is_int" | "is_str" | "is_obj" | "is_arr" | "is_num" | "is_bool" | "is_null" => {
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
    pub(crate) fn infer_str_method(
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

    pub(crate) fn check_int_args(&mut self, args: &[Expr], func: &str) -> Result<(), TypeError> {
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

    pub(crate) fn check_str_arg(&mut self, a: &Expr, func: &str) -> Result<(), TypeError> {
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
    pub(crate) fn infer_dict_method(
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
    pub(crate) fn infer_set_method(
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
    pub(crate) fn infer_tuple_method(
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
    pub(crate) fn infer_option_method(
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
    pub(crate) fn infer_result_method(
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
    pub(crate) fn check_method_args(
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
    pub(crate) fn move_var_obj(&mut self, obj: &Expr, span: Span) -> Result<(), TypeError> {
        if let Expr::Ident(name, _) = obj {
            let ty = self.read_var(name, span)?;
            if !ty.is_copy() {
                self.move_var(name, span)?;
            }
        }
        Ok(())
    }

    pub(crate) fn infer_list_method(
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
    pub(crate) fn infer_chan_method(
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
}
