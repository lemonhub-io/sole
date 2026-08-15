//! Bytecode compiler: AST → `CompiledProgram`.
//!
//! A single pass over the program. Local variables resolve to frame-slot
//! indices; globals to program-global indices. Calls with `ref`/`mut ref`
//! parameters push the caller's cell (`PushVarCell`) so writes propagate.

use sole_parser::{
    BinOp, Block, ElseBranch, Expr, FnDef, ImplDef, Item, Program, Stmt, TestDef, UnOp,
};
use std::collections::HashMap;

use crate::vm::{CompiledProgram, Function, Instr, Value};
use std::rc::Rc;

/// Bytecode buffer that records a source line per instruction, so runtime
/// errors can report where they happened (`lines[ip]`).
#[derive(Default)]
struct CodeBuf {
    code: Vec<Instr>,
    lines: Vec<u32>,
}

impl CodeBuf {
    fn push(&mut self, instr: Instr, line: u32) {
        self.lines.push(line);
        self.code.push(instr);
    }

    fn len(&self) -> usize {
        self.code.len()
    }

    /// Replaces an instruction in place (jump patching); the recorded line
    /// stays with the slot.
    fn patch(&mut self, at: usize, instr: Instr) {
        self.code[at] = instr;
    }
}

struct FuncCtx {
    locals: HashMap<String, u32>,
    next_local: u32,
}

impl FuncCtx {
    fn new(fdef: &FnDef) -> Self {
        let mut locals = HashMap::new();
        for (i, p) in fdef.params.iter().enumerate() {
            locals.insert(p.name.clone(), i as u32);
        }
        Self {
            locals,
            next_local: fdef.params.len() as u32,
        }
    }

    fn slot(&mut self, name: &str) -> u32 {
        if let Some(&i) = self.locals.get(name) {
            return i;
        }
        let i = self.next_local;
        self.next_local += 1;
        self.locals.insert(name.to_string(), i);
        i
    }
}

pub struct Compiler<'a> {
    program: &'a Program,
    functions: Vec<Function>,
    strings: Vec<String>,
    string_map: HashMap<String, u32>,
    methods: Vec<((String, String), usize)>,
    structs: Vec<(String, Vec<String>)>,
    chan_elem: Vec<String>,
    /// Break/continue jump targets: (loop start, break patch positions).
    loop_stack: Vec<(usize, Vec<usize>)>,
}

/// Compiles a program to bytecode.
pub fn compile(program: &Program) -> Result<CompiledProgram, String> {
    let mut c = Compiler {
        program,
        functions: Vec::new(),
        strings: Vec::new(),
        string_map: HashMap::new(),
        methods: Vec::new(),
        structs: Vec::new(),
        chan_elem: Vec::new(),
        loop_stack: Vec::new(),
    };
    c.collect_decls()?;
    c.emit_entry()?;
    for item in &program.items {
        if let Item::Fn(f) = item {
            c.compile_fn(f)?;
        }
        if let Item::Test(t) = item {
            c.compile_test(t)?;
        }
        if let Item::Impl(imp) = item {
            c.compile_impl(imp)?;
        }
        // Item::Import is resolved by the loader / type checker; it
        // produces no code.
        let _ = item;
    }
    Ok(CompiledProgram {
        functions: c.functions,
        globals: Vec::new(),
        strings: c.strings.into_iter().map(Rc::from).collect(),
        methods: c.methods,
        structs: c.structs,
        chan_elem: c.chan_elem,
        entry: 0,
    })
}

impl<'a> Compiler<'a> {
    fn collect_decls(&mut self) -> Result<(), String> {
        for item in &self.program.items {
            match item {
                Item::Struct(s) => {
                    let fields: Vec<String> = s.fields.iter().map(|(n, _)| n.clone()).collect();
                    self.structs.push((s.name.clone(), fields));
                }
                Item::Fn(f) => {
                    self.functions.push(Function {
                        name: f.name.clone(),
                        nparams: f.params.len() as u32,
                        nlocals: f.params.len() as u32,
                        code: vec![Instr::Halt],
                        lines: vec![0],
                    });
                }
                Item::Test(t) => {
                    self.functions.push(Function {
                        name: format!("test:{}", t.name),
                        nparams: 0,
                        nlocals: 0,
                        code: vec![Instr::Halt],
                        lines: vec![0],
                    });
                }
                Item::Impl(imp) => {
                    for m in &imp.methods {
                        self.functions.push(Function {
                            name: format!("{}::{}", imp.ty, m.name),
                            nparams: m.params.len() as u32,
                            nlocals: m.params.len() as u32,
                            code: vec![Instr::Halt],
                            lines: vec![0],
                        });
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn emit_entry(&mut self) -> Result<(), String> {
        // Entry function: top-level statements, implicitly inside a
        // task_group (GOALS §7.3). Reserve slot 0 first so that `fn_index`
        // during body compilation sees the final layout.
        self.functions.insert(
            0,
            Function {
                name: "<entry>".into(),
                nparams: 0,
                nlocals: 0,
                code: Vec::new(),
                lines: Vec::new(),
            },
        );
        let mut code = CodeBuf::default();
        let mut ctx = FuncCtx {
            locals: HashMap::new(),
            next_local: 0,
        };
        for item in &self.program.items {
            if let Item::Stmt(stmt) = item {
                self.compile_stmt(stmt, &mut code, &mut ctx)?;
            }
        }
        code.push(Instr::Halt, 0);
        self.functions[0].lines = code.lines;
        self.functions[0].code = code.code;
        self.functions[0].nlocals = ctx.next_local;
        Ok(())
    }

    fn compile_test(&mut self, t: &TestDef) -> Result<(), String> {
        let name = format!("test:{}", t.name);
        let idx = self.fn_index(&name);
        let mut code = CodeBuf::default();
        let mut ctx = FuncCtx {
            locals: HashMap::new(),
            next_local: 0,
        };
        self.compile_block(&t.body, &mut code, &mut ctx)?;
        code.push(Instr::RetUnit, 0);
        self.functions[idx].lines = code.lines;
        self.functions[idx].code = code.code;
        self.functions[idx].nlocals = ctx.next_local;
        Ok(())
    }

    fn compile_fn(&mut self, f: &FnDef) -> Result<(), String> {
        // Function index = order of appearance among Fn items.
        let idx = self.fn_index(&f.name);
        let mut code = CodeBuf::default();
        let mut ctx = FuncCtx::new(f);
        self.compile_block(&f.body, &mut code, &mut ctx)?;
        code.push(Instr::Return, 0);
        self.functions[idx].lines = code.lines;
        self.functions[idx].code = code.code;
        self.functions[idx].nlocals = ctx.next_local;
        Ok(())
    }

    fn compile_impl(&mut self, imp: &ImplDef) -> Result<(), String> {
        for m in &imp.methods {
            let idx = self.method_index(&imp.ty, &m.name);
            let mut code = CodeBuf::default();
            let mut ctx = FuncCtx::new(m);
            // `self` is slot 0, bound by CallMethod at runtime.
            self.compile_block(&m.body, &mut code, &mut ctx)?;
            code.push(Instr::Return, 0);
            self.functions[idx].lines = code.lines;
            self.functions[idx].code = code.code;
            self.functions[idx].nlocals = ctx.next_local;
            self.methods.push(((imp.ty.clone(), m.name.clone()), idx));
        }
        Ok(())
    }

    fn fn_index(&self, name: &str) -> usize {
        self.functions
            .iter()
            .position(|f| f.name == name)
            .expect("function declared")
    }

    fn method_index(&self, ty: &str, method: &str) -> usize {
        self.functions
            .iter()
            .position(|f| f.name == format!("{}::{}", ty, method))
            .expect("method declared")
    }

    fn intern_string(&mut self, s: &str) -> u32 {
        if let Some(&i) = self.string_map.get(s) {
            return i;
        }
        let i = self.strings.len() as u32;
        self.strings.push(s.to_string());
        self.string_map.insert(s.to_string(), i);
        i
    }

    fn compile_block(
        &mut self,
        block: &Block,
        code: &mut CodeBuf,
        ctx: &mut FuncCtx,
    ) -> Result<(), String> {
        for stmt in &block.stmts {
            self.compile_stmt(stmt, code, ctx)?;
        }
        Ok(())
    }

    fn compile_stmt(
        &mut self,
        stmt: &Stmt,
        code: &mut CodeBuf,
        ctx: &mut FuncCtx,
    ) -> Result<(), String> {
        let line = stmt.span().line as u32;
        match stmt {
            Stmt::Let { name, value, .. } => {
                self.compile_expr(value, code, ctx)?;
                let slot = ctx.slot(name);
                code.push(Instr::StoreVar(slot), line);
            }
            Stmt::Assign { name, value, .. } => {
                self.compile_expr(value, code, ctx)?;
                let slot = ctx.slot(name);
                code.push(Instr::StoreVar(slot), line);
            }
            Stmt::FieldAssign {
                obj, field, value, ..
            } => {
                let obj_slot = ctx.slot(obj);
                code.push(Instr::PushVarCell(obj_slot), line);
                self.compile_expr(value, code, ctx)?;
                let f = self.intern_string(field);
                code.push(Instr::SetField(f), line);
            }
            Stmt::Expr(expr) => {
                self.compile_expr(expr, code, ctx)?;
                code.push(Instr::Pop, line);
            }
            Stmt::Return { value, .. } => {
                match value {
                    Some(e) => self.compile_expr(e, code, ctx)?,
                    None => code.push(Instr::PushUnit, line),
                }
                code.push(Instr::Return, 0);
            }
            Stmt::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                self.compile_expr(cond, code, ctx)?;
                let jump_false = code.len();
                code.push(Instr::JumpIfFalse(0), line);
                self.compile_block(then_block, code, ctx)?;
                let jump_end = code.len();
                code.push(Instr::Jump(0), line);
                let else_start = code.len();
                code.patch(jump_false, Instr::JumpIfFalse(else_start as u32));
                match else_block {
                    Some(ElseBranch::If(s)) => self.compile_stmt(s, code, ctx)?,
                    Some(ElseBranch::Block(b)) => self.compile_block(b, code, ctx)?,
                    None => {}
                }
                code.patch(jump_end, Instr::Jump(code.len() as u32));
            }
            Stmt::While { cond, body, .. } => {
                let loop_start = code.len();
                self.compile_expr(cond, code, ctx)?;
                let jump_false = code.len();
                code.push(Instr::JumpIfFalse(0), line);
                self.loop_stack.push((loop_start, Vec::new()));
                self.compile_block(body, code, ctx)?;
                let (_, break_patches) = self.loop_stack.pop().unwrap();
                code.push(Instr::Jump(loop_start as u32), line);
                let loop_end_addr = code.len();
                code.patch(jump_false, Instr::JumpIfFalse(loop_end_addr as u32));
                for p in break_patches {
                    code.patch(p, Instr::Jump(loop_end_addr as u32));
                }
            }
            Stmt::For {
                var,
                iterable,
                body,
                ..
            } => {
                self.compile_expr(iterable, code, ctx)?;
                code.push(Instr::ForInit, line);
                let loop_start = code.len();
                let jump_next = code.len();
                code.push(Instr::ForNext(0), line);
                // Store the yielded element into the loop variable.
                let var_slot = ctx.slot(var);
                code.push(Instr::StoreVar(var_slot), line);
                self.loop_stack.push((loop_start, Vec::new()));
                self.compile_block(body, code, ctx)?;
                let (_, break_patches) = self.loop_stack.pop().unwrap();
                code.push(Instr::Jump(loop_start as u32), line);
                let loop_end_addr = code.len();
                code.patch(jump_next, Instr::ForNext(loop_end_addr as u32));
                for p in break_patches {
                    code.patch(p, Instr::Jump(loop_end_addr as u32));
                }
            }
            Stmt::Break { .. } => {
                let Some((_, patches)) = self.loop_stack.last_mut() else {
                    return Err("break outside loop".into());
                };
                let pos = code.len();
                code.push(Instr::Jump(0), line);
                patches.push(pos);
            }
            Stmt::Continue { .. } => {
                let Some((start, _)) = self.loop_stack.last() else {
                    return Err("continue outside loop".into());
                };
                code.push(Instr::Jump(*start as u32), line);
            }
            Stmt::TaskGroup { body, .. } => {
                code.push(Instr::TaskGroupBegin, line);
                self.compile_block(body, code, ctx)?;
                code.push(Instr::TaskGroupEnd, line);
            }
            Stmt::Go { call, .. } => {
                let Expr::Call { callee, args, .. } = call.as_ref() else {
                    return Err("go must spawn a call".into());
                };
                let Expr::Ident(name, _) = callee.as_ref() else {
                    return Err("go must spawn a named function call".into());
                };
                let fn_idx = self.fn_index(name);
                for a in args {
                    self.compile_expr(a, code, ctx)?;
                }
                code.push(Instr::Go(fn_idx as u32, args.len() as u32), line);
                code.push(Instr::Pop, line);
            }
            Stmt::Yield { .. } => {
                code.push(Instr::Yield, line);
            }
            Stmt::Assert { expr, .. } => {
                self.compile_expr(expr, code, ctx)?;
                code.push(Instr::Assert, line);
            }
        }
        Ok(())
    }

    fn compile_expr(
        &mut self,
        expr: &Expr,
        code: &mut CodeBuf,
        ctx: &mut FuncCtx,
    ) -> Result<(), String> {
        let line = expr.span().line as u32;
        match expr {
            Expr::Int(n, _) => code.push(Instr::PushInt(*n), line),
            Expr::Float(f, _) => code.push(Instr::PushFloat(*f), line),
            Expr::Str(s, _) => {
                let i = self.intern_string(s);
                code.push(Instr::PushStr(i), line);
            }
            Expr::Bool(b, _) => code.push(Instr::PushBool(*b), line),
            Expr::List(items, _) => {
                for it in items {
                    self.compile_expr(it, code, ctx)?;
                }
                code.push(Instr::MakeList(items.len() as u32), line);
            }
            Expr::Dict(pairs, _) => {
                for (k, v) in pairs {
                    self.compile_expr(k, code, ctx)?;
                    self.compile_expr(v, code, ctx)?;
                }
                code.push(Instr::MakeDict(pairs.len() as u32), line);
            }
            Expr::Set(items, _) => {
                for it in items {
                    self.compile_expr(it, code, ctx)?;
                }
                code.push(Instr::MakeSet(items.len() as u32), line);
            }
            Expr::Tuple(items, _) => {
                for it in items {
                    self.compile_expr(it, code, ctx)?;
                }
                code.push(Instr::MakeTuple(items.len() as u32), line);
            }
            Expr::Ident(name, _) => {
                if name == "None" {
                    code.push(Instr::PushNone, line);
                } else {
                    let slot = ctx.slot(name);
                    code.push(Instr::PushVar(slot), line);
                }
            }
            Expr::Unary { op, expr, .. } => match op {
                UnOp::Neg => {
                    self.compile_expr(expr, code, ctx)?;
                    code.push(Instr::Neg, line);
                }
                UnOp::Not => {
                    self.compile_expr(expr, code, ctx)?;
                    code.push(Instr::Not, line);
                }
            },
            Expr::Binary { op, lhs, rhs, .. } => {
                self.compile_expr(lhs, code, ctx)?;
                self.compile_expr(rhs, code, ctx)?;
                code.push(Instr::Binary(op_to_binop(*op)), line);
            }
            Expr::Call { callee, args, span } => {
                // Method call: obj.method(args)
                if let Expr::Field { obj, name, .. } = callee.as_ref() {
                    if let Expr::Ident(obj_name, _) = obj.as_ref() {
                        let obj_slot = ctx.slot(obj_name);
                        code.push(Instr::PushVarCell(obj_slot), line);
                    } else {
                        self.compile_expr(obj, code, ctx)?;
                    }
                    for a in args {
                        self.compile_expr(a, code, ctx)?;
                    }
                    let m = self.intern_string(name);
                    code.push(Instr::CallMethod(m, args.len() as u32 + 1), line);
                    return Ok(());
                }
                // Channel construction: `Chan[int]()` / `Chan[int](10)`.
                if let Expr::Index { obj, .. } = callee.as_ref() {
                    if let Expr::Ident(cname, _) = obj.as_ref() {
                        if cname == "Chan" {
                            for a in args {
                                self.compile_expr(a, code, ctx)?;
                            }
                            if args.is_empty() {
                                code.push(Instr::PushInt(0), line);
                            }
                            code.push(Instr::MakeChan(0), line);
                            return Ok(());
                        }
                    }
                }
                // Struct construction: `Circle(1, 2)`.
                if let Expr::Ident(name, _) = callee.as_ref() {
                    let is_struct = self.structs.iter().any(|(n, _)| n == name);
                    if is_struct {
                        for a in args {
                            self.compile_expr(a, code, ctx)?;
                        }
                        let si = self.structs.iter().position(|(n, _)| n == name).unwrap() as u32;
                        code.push(Instr::MakeStruct(si), line);
                        return Ok(());
                    }
                    // Channel construction handled above (Index form).
                }
                // Function call.
                let fn_idx = match callee.as_ref() {
                    Expr::Ident(name, _) => match name.as_str() {
                        "Some" => {
                            for a in args {
                                self.compile_expr(a, code, ctx)?;
                            }
                            code.push(Instr::PushSome, line);
                            return Ok(());
                        }
                        "None" => {
                            code.push(Instr::PushNone, line);
                            return Ok(());
                        }
                        "Ok" => {
                            for a in args {
                                self.compile_expr(a, code, ctx)?;
                            }
                            code.push(Instr::PushOk, line);
                            return Ok(());
                        }
                        "Err" => {
                            for a in args {
                                self.compile_expr(a, code, ctx)?;
                            }
                            code.push(Instr::PushErr, line);
                            return Ok(());
                        }
                        "print" => {
                            for a in args {
                                self.compile_expr(a, code, ctx)?;
                            }
                            code.push(Instr::BuiltinPrint(args.len() as u32), line);
                            return Ok(());
                        }
                        n if is_std_builtin(n) => {
                            for a in args {
                                self.compile_expr(a, code, ctx)?;
                            }
                            code.push(Instr::Builtin(std_builtin_id(n)), line);
                            return Ok(());
                        }
                        "range" => {
                            // `range(n)` = `range(0, n)`: push the default
                            // start so BuiltinRange always pops two values.
                            if args.len() == 1 {
                                code.push(Instr::PushInt(0), line);
                            }
                            for a in args {
                                self.compile_expr(a, code, ctx)?;
                            }
                            code.push(Instr::BuiltinRange, line);
                            return Ok(());
                        }
                        _ => self.fn_index(name),
                    },
                    _ => {
                        return Err(format!(
                            "unsupported callee at {}:{}",
                            span.line, span.column
                        ))
                    }
                };
                for a in args {
                    self.compile_expr(a, code, ctx)?;
                }
                code.push(Instr::Call(fn_idx as u32), line);
            }
            Expr::Field { obj, name, .. } => {
                if let Expr::Ident(obj_name, _) = obj.as_ref() {
                    let obj_slot = ctx.slot(obj_name);
                    code.push(Instr::PushVarCell(obj_slot), line);
                } else {
                    self.compile_expr(obj, code, ctx)?;
                }
                let f = self.intern_string(name);
                code.push(Instr::GetField(f), line);
            }
            Expr::Index { obj, index, .. } => {
                if let Expr::Ident(obj_name, _) = obj.as_ref() {
                    let obj_slot = ctx.slot(obj_name);
                    code.push(Instr::PushVarCell(obj_slot), line);
                } else {
                    self.compile_expr(obj, code, ctx)?;
                }
                self.compile_expr(index, code, ctx)?;
                code.push(Instr::IndexGet, line);
            }
            Expr::Borrow { mutable, expr, .. } => {
                let Expr::Ident(name, _) = expr.as_ref() else {
                    return Err("ref target must be a variable or index".into());
                };
                let slot = ctx.slot(name);
                code.push(Instr::BorrowVar(*mutable, slot), line);
            }
        }
        Ok(())
    }
}

/// Standard-library builtins (IO / time / math / JSON). They are global
/// functions backed by the VM, keeping the stdlib dependency-free.
pub fn is_std_builtin(name: &str) -> bool {
    std_builtin_id(name) != 0
}

pub fn std_builtin_id(name: &str) -> u8 {
    match name {
        "read_to_str" => 1,
        "write" => 2,
        "clock" => 3,
        "sleep" => 4,
        "abs" => 5,
        "floor" => 6,
        "ceil" => 7,
        "round" => 8,
        "sqrt" => 9,
        "pow" => 10,
        "json_encode" => 11,
        "json_decode" => 12,
        _ => 0,
    }
}

fn op_to_binop(op: BinOp) -> crate::vm::BinOp {
    use crate::vm::BinOp::*;
    match op {
        BinOp::Add => Add,
        BinOp::Sub => Sub,
        BinOp::Mul => Mul,
        BinOp::Div => Div,
        BinOp::Mod => Mod,
        BinOp::Eq => Eq,
        BinOp::Ne => Ne,
        BinOp::Lt => Lt,
        BinOp::Le => Le,
        BinOp::Gt => Gt,
        BinOp::Ge => Ge,
        BinOp::And => And,
        BinOp::Or => Or,
    }
}

impl Value {
    pub fn display(&self) -> String {
        match self {
            Value::Int(n) => n.to_string(),
            Value::Float(f) => f.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Str(s) => s.to_string(),
            Value::Range { start, end } => format!("range({}, {})", start, end),
            Value::List(items) => format!(
                "[{}]",
                items
                    .borrow()
                    .iter()
                    .map(Value::display)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Value::Struct(sv) => format!(
                "{} {{ {} }}",
                sv.name,
                sv.fields
                    .iter()
                    .map(|(n, v)| format!("{}: {}", n, v.display()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Value::Chan(_) => "<chan>".into(),
            Value::None => "None".into(),
            Value::Some(v) => format!("Some({})", v.display()),
            Value::Ok(v) => format!("Ok({})", v.display()),
            Value::Err(v) => format!("Err({})", v.display()),
            Value::Dict(pairs) => format!(
                "{{{}}}",
                pairs
                    .borrow()
                    .iter()
                    .map(|(k, v)| format!("{}: {}", k.display(), v.display()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Value::Set(items) => format!(
                "{{{}}}",
                items
                    .borrow()
                    .iter()
                    .map(Value::display)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Value::Tuple(items) => format!(
                "({})",
                items
                    .iter()
                    .map(Value::display)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Value::Ref(cell) | Value::MutRef(cell) => cell.borrow().display(),
            Value::Fn(i) => format!("<fn {}>", i),
            Value::Unit => String::new(),
        }
    }
}
