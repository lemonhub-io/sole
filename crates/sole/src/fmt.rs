//! The official Sole formatter (GOALS §9.1): one canonical layout, like
//! gofmt. Source is parsed to an AST and re-emitted with canonical
//! indentation and spacing; the output is idempotent (formatting twice
//! yields the same text) and preserves semantics.
//!
//! Comments are preserved: the lexer reports them on a side channel and
//! the formatter drains them in source order — own-line comments print
//! before the next node at that node's indent; trailing comments append
//! to the line the code before them emitted.

use sole_parser::{
    BinOp, Block, ElseBranch, Expr, FnDef, ImplDef, InterfaceDef, Item, Program, Stmt, StructDef,
    TestDef, UnOp,
};

/// Cursor over the comments a `lex_with_comments` pass produced.
struct Comments<'a> {
    all: &'a [sole_lexer::Comment],
    next: usize,
    /// Extra indentation (in columns) the surrounding construct adds on
    /// output — impl methods render their bodies one level deeper than
    /// the level the formatter passes, so their drains raise `min_col`.
    shift: usize,
}

impl<'a> Comments<'a> {
    /// Emits every pending comment before `line`. Own-line comments print
    /// at `level` when their source column says they belong to a scope at
    /// least that deep; shallower comments are left for the enclosing
    /// drain. Trailing comments append to the last emitted line — output
    /// follows source order, so that is the line they trail. Emission
    /// leaves `out` mid-line; callers `end_line` before the next item.
    fn drain_before(&mut self, line: usize, level: usize, out: &mut String) {
        let min_col = level * 4 + 1 + self.shift;
        while let Some(c) = self.all.get(self.next) {
            if c.line >= line {
                break;
            }
            if c.own_line && c.column < min_col {
                break;
            }
            self.next += 1;
            if c.own_line || out.is_empty() || out.ends_with('\n') {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                indent(out, level);
                out.push_str(&c.text);
            } else {
                out.push_str("  ");
                out.push_str(&c.text);
            }
        }
    }
}

/// Parses and formats a source string.
pub fn format_source(source: &str) -> Result<String, String> {
    let program = sole_parser::parse(source).map_err(|e| e.to_string())?;
    let (_, comments) = sole_lexer::lex_with_comments(source).map_err(|e| e.to_string())?;
    Ok(format_program(&program, &comments))
}

/// Formats a parsed program to its canonical text. `comments` comes from
/// `lex_with_comments` on the same source; pass `&[]` when formatting a
/// synthesized AST.
pub fn format_program(program: &Program, comments: &[sole_lexer::Comment]) -> String {
    let mut comments = Comments {
        all: comments,
        next: 0,
        shift: 0,
    };
    let mut out = String::new();
    let mut prev_was_import = false;
    let mut first = true;
    for (i, item) in program.items.iter().enumerate() {
        // Drain before emitting separators so trailing comments attach to
        // the previous item's line rather than to a blank line.
        comments.drain_before(item_line(item), 0, &mut out);
        end_line(&mut out);
        let is_import = matches!(item, Item::Import(_));
        if !first && !(prev_was_import && is_import) {
            out.push('\n');
        }
        first = false;
        prev_was_import = is_import;
        format_item(
            item,
            &mut comments,
            &mut out,
            next_line_for(&program.items, i),
        );
    }
    comments.drain_before(usize::MAX, 0, &mut out);
    end_line(&mut out);
    out
}

/// Terminates the current line if any text is pending.
fn end_line(out: &mut String) {
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
}

fn item_line(item: &Item) -> usize {
    match item {
        Item::Fn(f) => f.span.line,
        Item::Test(t) => t.span.line,
        Item::Import(i) => i.span.line,
        Item::Struct(s) => s.span.line,
        Item::Interface(i) => i.span.line,
        Item::Impl(i) => i.span.line,
        Item::Stmt(s) => s.span().line,
    }
}

/// The start line of the item after `i`, bounding `i`'s body for comment
/// draining; `usize::MAX` on the last item.
fn next_line_for(items: &[Item], i: usize) -> usize {
    items.get(i + 1).map(item_line).unwrap_or(usize::MAX)
}

fn format_item(item: &Item, comments: &mut Comments, out: &mut String, end: usize) {
    match item {
        Item::Fn(f) => format_fn(f, comments, out, end),
        Item::Test(t) => format_test(t, comments, out, end),
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
        Item::Struct(s) => format_struct(s, comments, out, end),
        Item::Interface(i) => format_interface(i, comments, out, end),
        Item::Impl(imp) => format_impl(imp, comments, out, end),
        Item::Stmt(stmt) => format_stmt(stmt, 0, comments, out),
    }
}

fn indent(out: &mut String, level: usize) {
    for _ in 0..level {
        out.push_str("    ");
    }
}

fn format_fn(f: &FnDef, comments: &mut Comments, out: &mut String, end: usize) {
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
    format_block(&f.body, 0, comments, out);
    // Own-line comments trailing the body stay inside the function.
    comments.drain_before(end, 1, out);
}

fn format_test(t: &TestDef, comments: &mut Comments, out: &mut String, end: usize) {
    out.push_str(&format!("test {}:", t.name));
    format_block(&t.body, 0, comments, out);
    comments.drain_before(end, 1, out);
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

fn format_struct(s: &StructDef, comments: &mut Comments, out: &mut String, end: usize) {
    out.push_str(&format!("struct {}:", s.name));
    for (name, ty) in &s.fields {
        out.push('\n');
        indent(out, 1);
        out.push_str(&format!("{}: {}", name, format_type(ty)));
    }
    // Fields carry no spans, so inner comments settle at the end of the
    // field list rather than between specific fields.
    comments.drain_before(end, 1, out);
}

fn format_interface(i: &InterfaceDef, comments: &mut Comments, out: &mut String, end: usize) {
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
    comments.drain_before(end, 1, out);
}

fn format_impl(imp: &ImplDef, comments: &mut Comments, out: &mut String, end: usize) {
    out.push_str(&format!("impl {}", imp.ty));
    if let Some(iface) = &imp.interface {
        out.push_str(&format!(": {}", iface));
    }
    out.push(':');
    for (i, m) in imp.methods.iter().enumerate() {
        comments.drain_before(m.span.line, 1, out);
        out.push('\n');
        indent(out, 1);
        // Reuse format_fn minus the leading "fn" line: emit signature at
        // the current indentation, body indented one more level.
        let mut buf = String::new();
        let bound = imp.methods.get(i + 1).map(|m| m.span.line).unwrap_or(end);
        // Method bodies re-indent one level past the signature, so
        // comments inside them have deeper source columns.
        comments.shift += 4;
        format_fn(m, comments, &mut buf, bound);
        comments.shift -= 4;
        let first_line = buf.lines().next().unwrap_or("");
        out.push_str(first_line.trim_start());
        for line in buf.lines().skip(1) {
            out.push('\n');
            indent(out, 2);
            out.push_str(line.trim_start());
        }
    }
    comments.drain_before(end, 1, out);
}

fn format_block(block: &Block, level: usize, comments: &mut Comments, out: &mut String) {
    for stmt in &block.stmts {
        comments.drain_before(stmt.span().line, level + 1, out);
        out.push('\n');
        indent(out, level + 1);
        format_stmt(stmt, level + 1, comments, out);
    }
}

fn format_stmt(stmt: &Stmt, level: usize, comments: &mut Comments, out: &mut String) {
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
            format_block(then_block, level, comments, out);
            match else_block {
                Some(ElseBranch::If(s)) => {
                    comments.drain_before(s.span().line, level + 1, out);
                    out.push('\n');
                    indent(out, level);
                    out.push_str("else ");
                    let mut buf = String::new();
                    format_stmt(s, level, comments, &mut buf);
                    // `else if cond:` stays on the same line; its body is
                    // already emitted at the right level.
                    out.push_str(buf.trim_start());
                }
                Some(ElseBranch::Block(b, else_span)) => {
                    // Comments before the `else` line stay inside the
                    // then-block; a comment on the `else` line itself
                    // drains inside the else-block and appends to it.
                    comments.drain_before(else_span.line, level + 1, out);
                    out.push('\n');
                    indent(out, level);
                    out.push_str("else:");
                    format_block(b, level, comments, out);
                }
                None => {}
            }
        }
        Stmt::While { cond, body, .. } => {
            out.push_str(&format!("while {}:", format_expr(cond)));
            format_block(body, level, comments, out);
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
            format_block(body, level, comments, out);
        }
        Stmt::Break { .. } => out.push_str("break"),
        Stmt::Continue { .. } => out.push_str("continue"),
        Stmt::TaskGroup { body, .. } => {
            out.push_str("task_group:");
            format_block(body, level, comments, out);
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
        Expr::Int(n, _) => n.clone(),
        Expr::Float(f, _) => {
            // Non-finite values have no literal form; emit text that
            // re-parses to the same value (`1e999` overflows to `inf`,
            // `0.0 / 0.0` evaluates to `NaN`).
            if f.is_infinite() {
                if f.is_sign_negative() {
                    "-1e999".into()
                } else {
                    "1e999".into()
                }
            } else if f.is_nan() {
                "0.0 / 0.0".into()
            } else {
                // Keep the float shape: `1.0` must not become `1`.
                let s = f.to_string();
                if s.contains('.') || s.contains('e') || s.contains('E') {
                    s
                } else {
                    format!("{}.0", s)
                }
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
