//! Part of the M2 static type checker — see `typecheck.rs`.

use super::*;
use sole_diag::Msg;

impl<'a> Checker<'a> {
    pub(crate) fn lookup(&self, name: &str) -> Option<&Var> {
        for scope in self.vars.iter().rev() {
            if let Some(v) = scope.get(name) {
                return Some(v);
            }
        }
        None
    }

    pub(crate) fn lookup_mut(&mut self, name: &str) -> Option<&mut Var> {
        for scope in self.vars.iter_mut().rev() {
            if let Some(v) = scope.get_mut(name) {
                return Some(v);
            }
        }
        None
    }

    pub(crate) fn bind(&mut self, name: String, ty: Ty, mutable: bool) {
        self.symbols.insert(name.clone(), ty.name());
        if let Some(scope) = self.vars.last_mut() {
            scope.insert(name, Var::new(ty, mutable));
        }
    }

    /// Snapshots the current scope's variables and borrow table so an `if`
    /// branch can be checked in isolation and merged back.
    pub(crate) fn snapshot_scope(&self) -> ScopeSnapshot {
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
    pub(crate) fn restore_scope(&mut self, snapshot: &ScopeSnapshot) {
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
    pub(crate) fn moved_names(&self, snapshot: &ScopeSnapshot) -> Vec<String> {
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
    pub(crate) fn read_var(&self, name: &str, span: Span) -> Result<Ty, TypeError> {
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
    pub(crate) fn move_var(&mut self, name: &str, span: Span) -> Result<Ty, TypeError> {
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
    pub(crate) fn borrow_var(
        &mut self,
        target: &Expr,
        mutable: bool,
        span: Span,
    ) -> Result<Ty, TypeError> {
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
    pub(crate) fn borrow_var_transient(
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
    pub(crate) fn borrow_path(&self, target: &Expr) -> Result<(String, Option<Ty>), TypeError> {
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
}
