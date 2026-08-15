//! The official Sole formatter (GOALS §9.1): one canonical layout, like
//! gofmt. Source is parsed to an AST and re-emitted with canonical
//! indentation and spacing; the output is idempotent (formatting twice
//! yields the same text) and preserves semantics.

use sole_parser::{
    BinOp, Block, ElseBranch, Expr, FnDef, ImplDef, InterfaceDef, Item, Program, Stmt, StructDef,
    TestDef, UnOp,
};

/// Parses and formats a source string.
pub fn format_source(source: &str) -> Result<String, String> {
    let program = sole_parser::parse(source).map_err(|e| e.to_string())?;
    Ok(format_program(&program))
}

/// Formats a parsed program to its canonical text.
pub fn format_program(program: &Program) -> String {
    let mut out = String::new();
    let mut prev_was_import = false;
    let mut first = true;
    for item in &program.items {
        if !first {
            out.push('\n');
        }
        let is_import = matches!(item, Item::Import(_));
        if !first && !(prev_was_import && is_import) {
            out.push('\n');
        }
        first = false;
        prev_was_import = is_import;
        format_item(item, &mut out);
    }
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn format_item(item: &Item, out: &mut String) {
    match item {
        Item::Fn(f) => format_fn(f, out),
        Item::Test(t) => format_test(t, out),
        Item::Import(imp) => {
            if imp.names.is_empty() {
                out.push_str(&format!("import {}", imp.module));
            } else {
                out.push_str(&format!(
                    "from {} import {}",
                    imp.module,
                    imp.names.join(", ")
                ));
            }
        }
        Item::Struct(s) => format_struct(s, out),
        Item::Interface(i) => format_interface(i, out),
        Item::Impl(imp) => format_impl(imp, out),
        Item::Stmt(stmt) => format_stmt(stmt, 0, out),
    }
}

fn indent(out: &mut String, level: usize) {
    for _ in 0..level {
        out.push_str("    ");
    }
}

fn format_fn(f: &FnDef, out: &mut String) {
    out.push_str("fn ");
    out.push_str(&f.name);
    if !f.type_params.is_empty() {
        let tps: Vec<String> = f
            .type_params
            .iter()
            .map(|p| match &p.bound {
                Some(b) => format!("{}: {}", p.name, b),
                None => p.name.clone(),
            })
            .collect();
        out.push_str(&format!("[{}]", tps.join(", ")));
    }
    format_params(&f.params, out);
    if let Some(ret) = &f.ret {
        out.push_str(&format!(" -> {}", format_type(ret)));
    }
    out.push(':');
    format_block(&f.body, 0, out);
}

fn format_test(t: &TestDef, out: &mut String) {
    out.push_str(&format!("test {}:", t.name));
    format_block(&t.body, 0, out);
}

fn format_params(params: &[sole_parser::Param], out: &mut String) {
    let parts: Vec<String> = params
        .iter()
        .map(|p| {
            let mut s = String::new();
            if p.is_mut {
                s.push_str("mut ");
            }
            s.push_str(&p.name);
            s.push_str(&format!(": {}", format_type(&p.ty)));
            s
        })
        .collect();
    out.push_str(&format!("({})", parts.join(", ")));
}

fn format_struct(s: &StructDef, out: &mut String) {
    out.push_str(&format!("struct {}:", s.name));
    for (name, ty) in &s.fields {
        out.push('\n');
        indent(out, 1);
        out.push_str(&format!("{}: {}", name, format_type(ty)));
    }
}

fn format_interface(i: &InterfaceDef, out: &mut String) {
    out.push_str(&format!("interface {}:", i.name));
    for m in &i.methods {
        out.push('\n');
        indent(out, 1);
        out.push_str("fn ");
        out.push_str(&m.name);
        format_params(&m.params, out);
        if let Some(ret) = &m.ret {
            out.push_str(&format!(" -> {}", format_type(ret)));
        }
    }
}

fn format_impl(imp: &ImplDef, out: &mut String) {
    out.push_str(&format!("impl {}", imp.ty));
    if let Some(iface) = &imp.interface {
        out.push_str(&format!(": {}", iface));
    }
    out.push(':');
    for m in &imp.methods {
        out.push('\n');
        indent(out, 1);
        // Reuse format_fn minus the leading "fn" line: emit signature at
        // the current indentation, body indented one more level.
        let mut buf = String::new();
        format_fn(m, &mut buf);
        let first_line = buf.lines().next().unwrap_or("");
        out.push_str(first_line.trim_start());
        for line in buf.lines().skip(1) {
            out.push('\n');
            indent(out, 2);
            out.push_str(line.trim_start());
        }
    }
}

fn format_block(block: &Block, level: usize, out: &mut String) {
    for stmt in &block.stmts {
        out.push('\n');
        indent(out, level + 1);
        format_stmt(stmt, level + 1, out);
    }
}

fn format_stmt(stmt: &Stmt, level: usize, out: &mut String) {
    match stmt {
        Stmt::Let {
            name,
            is_mut,
            ty,
            value,
            ..
        } => {
            out.push_str("let ");
            if *is_mut {
                out.push_str("mut ");
            }
            out.push_str(name);
            if let Some(t) = ty {
                out.push_str(&format!(": {}", format_type(t)));
            }
            out.push_str(&format!(" = {}", format_expr(value)));
        }
        Stmt::Assign { name, value, .. } => {
            out.push_str(&format!("{} = {}", name, format_expr(value)));
        }
        Stmt::FieldAssign {
            obj, field, value, ..
        } => {
            out.push_str(&format!("{}.{} = {}", obj, field, format_expr(value)));
        }
        Stmt::Expr(e) => {
            out.push_str(&format_expr(e));
        }
        Stmt::Return { value, .. } => match value {
            Some(v) => out.push_str(&format!("return {}", format_expr(v))),
            None => out.push_str("return"),
        },
        Stmt::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            out.push_str(&format!("if {}:", format_expr(cond)));
            format_block(then_block, level, out);
            match else_block {
                Some(ElseBranch::If(s)) => {
                    out.push('\n');
                    indent(out, level);
                    out.push_str("else ");
                    let mut buf = String::new();
                    format_stmt(s, level, &mut buf);
                    // `else if cond:` stays on the same line; its body is
                    // already emitted at the right level.
                    out.push_str(buf.trim_start());
                }
                Some(ElseBranch::Block(b)) => {
                    out.push('\n');
                    indent(out, level);
                    out.push_str("else:");
                    format_block(b, level, out);
                }
                None => {}
            }
        }
        Stmt::While { cond, body, .. } => {
            out.push_str(&format!("while {}:", format_expr(cond)));
            format_block(body, level, out);
        }
        Stmt::For {
            var,
            is_mut,
            mode,
            iterable,
            body,
            ..
        } => {
            out.push_str("for ");
            if *is_mut {
                out.push_str("mut ");
            }
            out.push_str(var);
            out.push_str(" in ");
            match mode {
                sole_parser::IterMode::Move => {}
                sole_parser::IterMode::Borrow => out.push_str("ref "),
                sole_parser::IterMode::MutBorrow => out.push_str("mut ref "),
            }
            out.push_str(&format!("{}:", format_expr(iterable)));
            format_block(body, level, out);
        }
        Stmt::Break { .. } => out.push_str("break"),
        Stmt::Continue { .. } => out.push_str("continue"),
        Stmt::TaskGroup { body, .. } => {
            out.push_str("task_group:");
            format_block(body, level, out);
        }
        Stmt::Go { call, .. } => {
            out.push_str(&format!("go {}", format_expr(call)));
        }
        Stmt::Yield { .. } => out.push_str("yield"),
        Stmt::Assert { expr, .. } => {
            out.push_str(&format!("assert {}", format_expr(expr)));
        }
    }
}

fn format_type(t: &sole_parser::Type) -> String {
    match t {
        sole_parser::Type::Named(name, args) => {
            if args.is_empty() {
                name.clone()
            } else {
                let inner: Vec<String> = args.iter().map(format_type).collect();
                format!("{}[{}]", name, inner.join(", "))
            }
        }
        sole_parser::Type::Ref(inner) => format!("ref {}", format_type(inner)),
        sole_parser::Type::MutRef(inner) => format!("mut ref {}", format_type(inner)),
        sole_parser::Type::TypeVar(name) => name.clone(),
    }
}

// Operator precedence (higher binds tighter). Matches the parser.
fn bin_prec(op: BinOp) -> u8 {
    use BinOp::*;
    match op {
        Or => 1,
        And => 2,
        Eq | Ne | Lt | Le | Gt | Ge => 3,
        Add | Sub => 4,
        Mul | Div | Mod => 5,
    }
}

fn bin_str(op: BinOp) -> &'static str {
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

/// Re-escapes a decoded string value for canonical output.
fn escape_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\0"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{{{:x}}}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

fn format_expr(expr: &Expr) -> String {
    match expr {
        Expr::Int(n, _) => n.to_string(),
        Expr::Float(f, _) => {
            // Keep the float shape: `1.0` must not become `1`.
            let s = f.to_string();
            if s.contains('.') || s.contains('e') || s.contains('E') {
                s
            } else {
                format!("{}.0", s)
            }
        }
        Expr::Str(s, _) => format!("\"{}\"", escape_str(s)),
        Expr::Bool(b, _) => b.to_string(),
        Expr::Ident(name, _) => name.clone(),
        Expr::List(items, _) => {
            format!(
                "[{}]",
                items.iter().map(format_expr).collect::<Vec<_>>().join(", ")
            )
        }
        Expr::Dict(pairs, _) => {
            let parts: Vec<String> = pairs
                .iter()
                .map(|(k, v)| format!("{}: {}", format_expr(k), format_expr(v)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
        Expr::Set(items, _) => {
            format!(
                "{{{}}}",
                items.iter().map(format_expr).collect::<Vec<_>>().join(", ")
            )
        }
        Expr::Tuple(items, _) => {
            format!(
                "({})",
                items.iter().map(format_expr).collect::<Vec<_>>().join(", ")
            )
        }
        Expr::Unary { op, expr, .. } => match op {
            UnOp::Neg => format!("-{}", format_expr(expr)),
            UnOp::Not => format!("not {}", format_expr(expr)),
        },
        Expr::Binary { op, lhs, rhs, .. } => {
            let prec = bin_prec(*op);
            let lhs_s = format_expr(lhs);
            let rhs_s = format_expr(rhs);
            let l = match lhs.as_ref() {
                Expr::Binary { op: lo, .. } if bin_prec(*lo) < prec => {
                    format!("({})", lhs_s)
                }
                _ => lhs_s,
            };
            let r = match rhs.as_ref() {
                Expr::Binary { op: ro, .. } if bin_prec(*ro) <= prec => {
                    format!("({})", rhs_s)
                }
                _ => rhs_s,
            };
            format!("{} {} {}", l, bin_str(*op), r)
        }
        Expr::Call { callee, args, .. } => {
            let args_s: Vec<String> = args.iter().map(format_expr).collect();
            format!("{}({})", format_expr(callee), args_s.join(", "))
        }
        Expr::Field { obj, name, .. } => {
            let obj_s = format_expr(obj);
            let obj_s = match obj.as_ref() {
                Expr::Binary { .. } | Expr::Unary { .. } => format!("({})", obj_s),
                _ => obj_s,
            };
            format!("{}.{}", obj_s, name)
        }
        Expr::Index { obj, index, .. } => {
            let obj_s = format_expr(obj);
            let obj_s = match obj.as_ref() {
                Expr::Binary { .. } | Expr::Unary { .. } => format!("({})", obj_s),
                _ => obj_s,
            };
            format!("{}[{}]", obj_s, format_expr(index))
        }
        Expr::Borrow { mutable, expr, .. } => {
            if *mutable {
                format!("mut ref {}", format_expr(expr))
            } else {
                format!("ref {}", format_expr(expr))
            }
        }
    }
}
