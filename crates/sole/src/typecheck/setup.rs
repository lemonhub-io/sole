//! Part of the M2 static type checker — see `typecheck.rs`.

use super::*;
use sole_diag::Msg;

impl<'a> Checker<'a> {
    pub(crate) fn run(&mut self) -> Result<(), TypeError> {
        self.collect_types()?;
        self.collect_fns()?;
        self.collect_impls()?;
        self.check_interfaces()?;
        self.vars.push(HashMap::new());
        // Top-level statements form an implicit block so that NLL borrow
        // release applies there too.
        let top_stmts: Vec<Stmt> = self
            .program
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Stmt(s) => Some(s.clone()),
                _ => None,
            })
            .collect();
        self.check_block(&Block { stmts: top_stmts })?;
        self.vars.pop();
        self.check_imports()?;
        self.check_functions()?;
        self.check_tests()?;
        Ok(())
    }

    pub(crate) fn collect_fns(&mut self) -> Result<(), TypeError> {
        for item in &self.program.items {
            if let Item::Fn(f) = item {
                let sig = self.fn_sig(&f.name, &f.type_params, &f.params, f.ret.as_ref(), None)?;
                self.symbols.insert(f.name.clone(), sig_text(&sig));
                self.fns.insert(f.name.clone(), sig);
            }
        }
        Ok(())
    }

    pub(crate) fn fn_sig(
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
    pub(crate) fn ty_of_type_with(
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
    pub(crate) fn substitute_typevars(
        &self,
        t: &Type,
        type_params: &[(String, Option<String>)],
    ) -> Type {
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
    pub(crate) fn ty_of_type(&self, t: &Type, self_ty: Option<&Ty>) -> Result<Ty, TypeError> {
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

    pub(crate) fn collect_types(&mut self) -> Result<(), TypeError> {
        for item in &self.program.items {
            match item {
                Item::Struct(s) => {
                    let mut fields = Vec::new();
                    for (name, t) in &s.fields {
                        let ty = self.ty_of_type(t, None)?;
                        fields.push((name.clone(), ty));
                    }
                    self.symbols
                        .insert(s.name.clone(), format!("struct {}", s.name));
                    self.structs.insert(s.name.clone(), fields);
                }
                Item::Interface(i) => {
                    let methods = i.methods.clone();
                    self.symbols
                        .insert(i.name.clone(), format!("interface {}", i.name));
                    self.interfaces.insert(i.name.clone(), methods);
                }
                Item::Test(t) => {
                    self.symbols
                        .insert(t.name.clone(), format!("test \"{}\"", t.name));
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub(crate) fn collect_impls(&mut self) -> Result<(), TypeError> {
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
    pub(crate) fn method_sig(
        &self,
        m: &FnDef,
        ty: &str,
        impl_span: Span,
    ) -> Result<FnSig, TypeError> {
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

    pub(crate) fn check_interfaces(&mut self) -> Result<(), TypeError> {
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

    pub(crate) fn check_functions(&mut self) -> Result<(), TypeError> {
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
    pub(crate) fn check_imports(&mut self) -> Result<(), TypeError> {
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

    pub(crate) fn check_tests(&mut self) -> Result<(), TypeError> {
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

    pub(crate) fn check_impl_body(&mut self, imp: &ImplDef) -> Result<(), TypeError> {
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
    pub(crate) fn is_hole(t: &Ty) -> bool {
        matches!(
            t,
            Ty::Option(inner) if matches!(inner.as_ref(), Ty::Unknown)
        )
    }

    pub(crate) fn is_compatible(&self, expected: &Ty, actual: &Ty) -> bool {
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
