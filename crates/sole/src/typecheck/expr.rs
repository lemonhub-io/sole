//! Part of the M2 static type checker — see `typecheck.rs`.

use super::*;
use sole_diag::Msg;

impl<'a> Checker<'a> {
    pub(crate) fn infer_expr(&mut self, expr: &Expr) -> Result<Ty, TypeError> {
        self.infer_expr_inner(expr, false, None)
    }

    pub(crate) fn infer_expr_consume(&mut self, expr: &Expr, _span: Span) -> Result<Ty, TypeError> {
        self.infer_expr_inner(expr, true, None)
    }

    pub(crate) fn infer_expr_consume_skip(
        &mut self,
        expr: &Expr,
        _span: Span,
        skip: &str,
    ) -> Result<Ty, TypeError> {
        self.infer_expr_inner(expr, true, Some(skip))
    }

    pub(crate) fn infer_expr_inner(
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

    pub(crate) fn infer_binary(
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
}
