//! Part of the M2 static type checker — see `typecheck.rs`.

use super::*;
use sole_diag::Msg;

impl<'a> Checker<'a> {
    pub(crate) fn check_fn_body(&mut self, f: &FnDef) -> Result<(), TypeError> {
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
    pub(crate) fn check_block(&mut self, block: &Block) -> Result<(), TypeError> {
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
    pub(crate) fn release_dead_borrows(
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

    pub(crate) fn check_stmt(&mut self, stmt: &Stmt) -> Result<(), TypeError> {
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
                        State::BorrowedImm | State::BorrowedMut => {
                            return Err(err(Msg::MoveWhileBorrowed(name.clone()), *span))
                        }
                        // `x = f(x)`: the RHS may consume `x`; the assignment
                        // binds a fresh value, so `x` is live again. A value
                        // moved before this statement is resurrected the same
                        // way (`x = [3]` reinitializes a moved binding).
                        State::Live | State::Moved => {}
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
                // `p.x = f(p)`: if the RHS consumed `obj`, writing a field
                // on the moved value would mutate data the new owner sees.
                if matches!(self.lookup(obj).map(|v| v.state), Some(State::Moved)) {
                    return Err(err(Msg::UseAfterMove(obj.clone()), *span));
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
                    Some(ElseBranch::Block(block, _)) => {
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

    pub(crate) fn iter_elem(&self, t: &Ty) -> Result<Ty, TypeError> {
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
    pub(crate) fn check_ref_return(
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
    pub(crate) fn ref_return_root(&self, expr: &Expr) -> Option<String> {
        match expr {
            Expr::Ident(name, _) => Some(name.clone()),
            Expr::Borrow { expr, .. } => self.ref_return_root(expr),
            Expr::Field { obj, .. } => self.ref_return_root(obj),
            _ => None,
        }
    }
}
