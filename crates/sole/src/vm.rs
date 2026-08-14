//! M3 bytecode VM for Sole.
//!
//! Programs are compiled to bytecode (`compiler.rs`) and executed by this
//! stack-based VM. Tasks (coroutines) are VM instances with their own value
//! and frame stacks, scheduled cooperatively: switching happens only at
//! channel operations and `yield` (GOALS 闂?). Channels implement the
//! ownership-moving send/recv semantics; `task_group` provides structured
//! concurrency with cancellation (= closing channels).

use sole_diag::{Diagnostic, Lang, Msg};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

#[derive(Debug, Clone, PartialEq)]
pub struct StructVal {
    pub name: String,
    pub fields: Vec<(String, Value)>,
}

/// Runtime value. Sized so the common variants stay small (24 bytes total):
/// `str` is reference-counted (immutable, shared) and structs are boxed.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(Rc<str>),
    Range { start: i64, end: i64 },
    List(Rc<RefCell<Vec<Value>>>),
    Struct(Box<StructVal>),
    Chan(Rc<RefCell<Channel>>),
    Ref(Rc<RefCell<Value>>),
    MutRef(Rc<RefCell<Value>>),
    Fn(usize),
    Unit,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Channel {
    pub buf: VecDeque<Value>,
    pub cap: usize,
    pub closed: bool,
    /// Tasks blocked waiting to receive (or send when buf is full).
    pub waiting: VecDeque<usize>,
}

impl Channel {
    pub fn new(cap: usize) -> Self {
        Self {
            buf: VecDeque::with_capacity(cap),
            cap,
            closed: false,
            waiting: VecDeque::new(),
        }
    }
}

impl Value {
    pub fn deref(&self) -> Value {
        match self {
            Value::Ref(cell) | Value::MutRef(cell) => cell.borrow().clone(),
            other => other.clone(),
        }
    }

    pub fn type_tag(&self) -> &'static str {
        match self {
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Bool(_) => "bool",
            Value::Str(_) => "str",
            Value::Range { .. } => "Range",
            Value::List(_) => "List",
            Value::Struct(_) => "struct",
            Value::Chan(_) => "Chan",
            Value::Ref(_) | Value::MutRef(_) => "ref",
            Value::Fn(_) => "fn",
            Value::Unit => "()",
        }
    }
}

/// Bytecode instruction set.
#[derive(Debug, Clone)]
pub enum Instr {
    Halt,
    PushInt(i64),
    PushFloat(f64),
    PushBool(bool),
    PushStr(u32),
    PushUnit,
    PushVar(u32),
    PushVarCell(u32),
    PushGlobal(u32),
    StoreVar(u32),
    StoreGlobal(u32),
    /// `BorrowVar(mutable, idx)` 闂?Ref/MutRef of the variable's cell.
    BorrowVar(bool, u32),
    MakeList(u32),
    ListLen,
    ListGet,
    ListSet,
    IndexGet,
    MakeStruct(u32),
    GetField(u32),
    SetField(u32),
    Call(u32),
    CallMethod(u32),
    BuiltinPrint(u32),
    BuiltinRange,
    Return,
    Jump(u32),
    JumpIfFalse(u32),
    ForInit,
    ForNext(u32),
    ChanSend,
    ChanRecv,
    ChanClose,
    MakeChan(u32),
    Go(u32, u32),
    TaskGroupBegin,
    TaskGroupEnd,
    Yield,
    Pop,
    Dup,
    Not,
    Binary(BinOp),
    /// Stops the current frame's function (implicit return at block end).
    RetUnit,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

/// A compiled function.
#[derive(Debug, Clone)]
pub struct Function {
    pub name: String,
    pub nparams: u32,
    pub nlocals: u32,
    pub code: Vec<Instr>,
}

/// Compiled program: functions, globals, string table, method table.
#[derive(Debug, Clone)]
pub struct CompiledProgram {
    pub functions: Vec<Function>,
    pub globals: Vec<String>,
    pub strings: Vec<Rc<str>>,
    /// (struct type name, method name) 闂?function index.
    pub methods: Vec<((String, String), usize)>,
    /// struct name 闂?field names (construction order).
    pub structs: Vec<(String, Vec<String>)>,
    /// chan element type names (unused at runtime; kept for error messages).
    pub chan_elem: Vec<String>,
    /// Function index of the implicit top-level group runner.
    pub entry: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VmError {
    pub diag: Diagnostic,
}

impl std::fmt::Display for VmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.diag.render(Lang::current()))
    }
}

impl std::error::Error for VmError {}

fn err(msg: Msg, line: usize, column: usize) -> VmError {
    VmError {
        diag: Diagnostic::new(msg, line, column),
    }
}

/// A stack frame: function index, return address, local values.
///
/// Locals are plain values; a slot only gets an `Rc<RefCell>` cell when it is
/// actually borrowed (`ref`/`mut ref`) or passed by cell (`PushVarCell`), so
/// the common case never allocates per local.
pub struct Frame {
    pub func: usize,
    pub ret: usize,
    pub locals: Vec<Value>,
    pub cells: Vec<Option<Rc<RefCell<Value>>>>,
}
#[derive(Default)]
pub struct Task {
    pub ip: usize,
    pub stack: Vec<Value>,
    pub frames: Vec<Frame>,
    pub group: usize,
    pub done: bool,
    /// Per-task loop iteration state.
    pub iters: Vec<IterState>,
    /// Per-task task_group stack: (group id, parent group).
    pub groups: Vec<(usize, usize)>,
}

/// Scheduling states a task can be in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TaskState {
    Ready,
    /// Blocked waiting on a channel operation.
    Blocked(usize, usize), // (channel id, op: 0=send, 1=recv)
    Done,
    Cancelled,
}

/// The runtime: channels, task registry, group tree.
pub struct Runtime<'a> {
    pub prog: Rc<CompiledProgram>,
    pub channels: Vec<Rc<RefCell<Channel>>>,
    pub tasks: Vec<Option<Box<Task>>>,
    pub task_states: Vec<TaskState>,
    /// group id 闂?set of task ids in it (not yet finished).
    pub groups: Vec<Vec<usize>>,
    pub current: usize,
    pub out: &'a mut dyn std::io::Write,
    /// Global variable storage by name.
    pub globals: HashMap<String, Rc<RefCell<Value>>>,
    /// Pending channel sends waiting for a receiver.
    pub pending_sends: VecDeque<(Rc<RefCell<Channel>>, Value, usize)>,
    /// Tasks waiting for their group's children to finish: (group, task).
    pub group_waiters: Vec<(usize, usize)>,
}

/// Iteration state for `for` loops (Range / List / Chan).
pub enum IterState {
    Range {
        cur: i64,
        end: i64,
    },
    List {
        items: Rc<RefCell<Vec<Value>>>,
        idx: usize,
    },
    Chan {
        chan: Rc<RefCell<Channel>>,
    },
}

impl<'a> Runtime<'a> {
    fn global_value(&self, name: &str) -> Value {
        self.globals
            .get(name)
            .map(|c| c.borrow().clone())
            .unwrap_or(Value::Unit)
    }

    fn set_global(&mut self, name: &str, value: Value) -> Result<(), VmError> {
        if let Some(cell) = self.globals.get(name) {
            let cur = cell.borrow().clone();
            match cur {
                Value::MutRef(target) => {
                    *target.borrow_mut() = value;
                }
                Value::Ref(_) => {
                    return Err(err(Msg::ImmutableReassign(name.into()), 0, 0));
                }
                _ => {
                    *cell.borrow_mut() = value;
                }
            }
        } else {
            self.globals
                .insert(name.to_string(), Rc::new(RefCell::new(value)));
        }
        Ok(())
    }

    /// Creates a new runtime over a compiled program.
    pub fn new(prog: Rc<CompiledProgram>, out: &'a mut dyn std::io::Write) -> Self {
        Self {
            prog,
            channels: Vec::new(),
            tasks: Vec::new(),
            task_states: Vec::new(),
            groups: Vec::new(),
            current: 0,
            out,
            globals: HashMap::new(),
            pending_sends: VecDeque::new(),
            group_waiters: Vec::new(),
        }
    }

    /// Runs a compiled program to completion (top-level = implicit
    /// task_group), returning the first error.
    pub fn run(&mut self) -> Result<(), VmError> {
        // Initialize globals.
        for name in &self.prog.globals {
            self.globals
                .entry(name.clone())
                .or_insert_with(|| Rc::new(RefCell::new(Value::Unit)));
        }
        self.spawn(0, vec![], 0);
        self.schedule_all()
    }

    /// Creates a new task running function `func` with `args`.
    fn spawn(&mut self, func: usize, args: Vec<Value>, group: usize) -> usize {
        let f = &self.prog.functions[func];
        let mut locals = args;
        locals.resize(f.nlocals as usize, Value::Unit);
        let cells = Vec::new();
        let id = self.tasks.len();
        self.tasks.push(Some(Box::new(Task {
            ip: 0,
            stack: Vec::new(),
            frames: vec![Frame {
                func,
                ret: 0,
                locals,
                cells,
            }],
            group,
            done: false,
            iters: Vec::new(),
            groups: Vec::new(),
        })));
        self.task_states.push(TaskState::Ready);
        if group < self.groups.len() {
            self.groups[group].push(id);
        }
        id
    }

    /// Cooperative scheduler: runs tasks round-robin until none are ready.
    /// Each ready task runs until it blocks, yields, or finishes (GOALS §7.3).
    fn schedule_all(&mut self) -> Result<(), VmError> {
        loop {
            let mut progressed = false;
            let n = self.tasks.len();
            let mut i = 0;
            while i < n {
                if self.task_states[i] == TaskState::Ready {
                    self.current = i;
                    progressed |= self.run_task(i)?;
                    if self.task_states[i] == TaskState::Done {
                        self.finish_task(i);
                    }
                }
                i += 1;
            }
            if !progressed {
                // All tasks blocked or done.
                break;
            }
        }
        Ok(())
    }

    fn finish_task(&mut self, id: usize) {
        self.task_states[id] = TaskState::Done;
        let group = self.tasks[id].as_ref().unwrap().group;
        if group < self.groups.len() {
            self.groups[group].retain(|&t| t != id);
            // If the group is now empty, wake tasks waiting on its end.
            if self.groups[group].is_empty() {
                let waiters: Vec<usize> = self
                    .group_waiters
                    .iter()
                    .filter(|(g, _)| *g == group)
                    .map(|(_, t)| *t)
                    .collect();
                self.group_waiters.retain(|(g, _)| *g != group);
                for w in waiters {
                    self.task_states[w] = TaskState::Ready;
                }
            }
        }
    }

    /// Runs task `id` until it blocks, yields, or finishes; returns whether
    /// it executed at least one instruction. The task is taken out of the
    /// registry for the duration of the run so that `&mut self` (channels,
    /// globals, scheduler) stays usable.
    fn run_task(&mut self, id: usize) -> Result<bool, VmError> {
        let mut task = self.tasks[id].take().unwrap();
        let result = self.run_task_inner(&mut task, id);
        if task.done && self.task_states[id] != TaskState::Done {
            self.task_states[id] = TaskState::Done;
        }
        self.tasks[id] = Some(task);
        result
    }

    fn run_task_inner(&mut self, task: &mut Task, id: usize) -> Result<bool, VmError> {
        let prog = self.prog.clone();
        // Run until the task blocks, yields, or finishes (GOALS §7.3:
        // cooperative switching happens only at channel operations and
        // `yield`). The dispatch loop stays hot; the scheduler and the
        // per-run prologue are amortized over many instructions.
        let mut budget = 100_000_000usize;
        loop {
            budget -= 1;
            if budget == 0 {
                return Err(err(Msg::InternalFnIndex, 0, 0));
            }
            let Some(frame) = task.frames.last() else {
                task.done = true;
                break;
            };
            let func = &prog.functions[frame.func];
            if task.ip >= func.code.len() {
                task.done = true;
                break;
            }
            let instr = &func.code[task.ip];
            task.ip += 1;
            match instr {
                Instr::Halt => {
                    task.done = true;
                }
                Instr::PushInt(n) => {
                    task.stack.push(Value::Int(*n));
                }
                Instr::PushFloat(f) => {
                    task.stack.push(Value::Float(*f));
                }
                Instr::PushBool(b) => {
                    task.stack.push(Value::Bool(*b));
                }
                Instr::PushStr(i) => {
                    task.stack
                        .push(Value::Str(prog.strings[*i as usize].clone()));
                }
                Instr::PushUnit => {
                    task.stack.push(Value::Unit);
                }
                Instr::PushVar(i) => {
                    let v = {
                        let frame = task.frames.last().unwrap();
                        match frame.cells.get(*i as usize).and_then(|c| c.clone()) {
                            Some(cell) => cell.borrow().clone(),
                            None => frame.locals[*i as usize].clone(),
                        }
                    };
                    task.stack.push(v);
                }
                Instr::PushVarCell(i) => {
                    let v = {
                        let frame = task.frames.last().unwrap();
                        match frame.cells.get(*i as usize).and_then(|c| c.clone()) {
                            Some(cell) => {
                                // If the variable already holds a reference, pass it through.
                                let cur = cell.borrow().clone();
                                match cur {
                                    Value::Ref(inner) | Value::MutRef(inner) => Value::Ref(inner),
                                    _ => Value::Ref(cell),
                                }
                            }
                            None => {
                                // First cell use: materialize the slot's cell.
                                match frame.locals[*i as usize].clone() {
                                    Value::Ref(inner) | Value::MutRef(inner) => Value::Ref(inner),
                                    cur => {
                                        let cell = Rc::new(RefCell::new(cur));
                                        let frame = task.frames.last_mut().unwrap();
                                        if frame.cells.len() <= *i as usize {
                                            frame.cells.resize(*i as usize + 1, None);
                                        }
                                        frame.cells[*i as usize] = Some(cell.clone());
                                        Value::Ref(cell)
                                    }
                                }
                            }
                        }
                    };
                    task.stack.push(v);
                }
                Instr::PushGlobal(i) => {
                    let name = &prog.globals[*i as usize];
                    let v = self.global_value(name);
                    task.stack.push(v);
                }
                Instr::StoreVar(i) => {
                    let v = task.stack.pop().unwrap_or(Value::Unit);
                    let cell = task
                        .frames
                        .last()
                        .unwrap()
                        .cells
                        .get(*i as usize)
                        .and_then(|c| c.clone());
                    if let Some(cell) = cell {
                        let cur = cell.borrow().clone();
                        match cur {
                            Value::MutRef(target) => {
                                *target.borrow_mut() = v;
                            }
                            Value::Ref(_) => {
                                return Err(err(Msg::ImmutableReassign("<ref>".into()), 0, 0));
                            }
                            _ => {
                                *cell.borrow_mut() = v;
                            }
                        }
                    } else {
                        let slot = &mut task.frames.last_mut().unwrap().locals[*i as usize];
                        match slot {
                            Value::MutRef(target) => {
                                *target.borrow_mut() = v;
                            }
                            Value::Ref(_) => {
                                return Err(err(Msg::ImmutableReassign("<ref>".into()), 0, 0));
                            }
                            _ => {
                                *slot = v;
                            }
                        }
                    }
                }
                Instr::StoreGlobal(i) => {
                    let v = task.stack.pop().unwrap_or(Value::Unit);
                    let name = prog.globals[*i as usize].clone();
                    self.set_global(&name, v)?;
                }
                Instr::BorrowVar(mutable, i) => {
                    let cell = {
                        let frame = task.frames.last().unwrap();
                        match frame.cells.get(*i as usize).and_then(|c| c.clone()) {
                            Some(cell) => cell,
                            None => {
                                let cell = Rc::new(RefCell::new(frame.locals[*i as usize].clone()));
                                let frame = task.frames.last_mut().unwrap();
                                if frame.cells.len() <= *i as usize {
                                    frame.cells.resize(*i as usize + 1, None);
                                }
                                frame.cells[*i as usize] = Some(cell.clone());
                                cell
                            }
                        }
                    };
                    if *mutable {
                        task.stack.push(Value::MutRef(cell));
                    } else {
                        task.stack.push(Value::Ref(cell));
                    }
                }
                Instr::MakeList(n) => {
                    let mut items = Vec::with_capacity(*n as usize);
                    for _ in 0..*n {
                        items.push(task.stack.pop().unwrap_or(Value::Unit));
                    }
                    items.reverse();
                    task.stack.push(Value::List(Rc::new(RefCell::new(items))));
                }
                Instr::ListLen => {
                    let list = task.stack.pop().unwrap_or(Value::Unit);
                    let len = match list {
                        Value::List(items) => items.borrow().len(),
                        _ => 0,
                    };
                    task.stack.push(Value::Int(len as i64));
                }
                Instr::ListGet => {
                    let idx = task.stack.pop().unwrap_or(Value::Int(0));
                    let list = task.stack.pop().unwrap_or(Value::Unit);
                    let Value::Int(i) = idx else {
                        return Err(err(Msg::IndexNotInt, 0, 0));
                    };
                    let item = match list {
                        Value::List(items) => items
                            .borrow()
                            .get(i as usize)
                            .cloned()
                            .unwrap_or(Value::Unit),
                        _ => return Err(err(Msg::IndexOnNonList(list.type_tag().into()), 0, 0)),
                    };
                    task.stack.push(item);
                }
                Instr::ListSet => {
                    let v = task.stack.pop().unwrap_or(Value::Unit);
                    let idx = task.stack.pop().unwrap_or(Value::Int(0));
                    let list = task.stack.pop().unwrap_or(Value::Unit);
                    let Value::Int(i) = idx else {
                        return Err(err(Msg::IndexNotInt, 0, 0));
                    };
                    match list {
                        Value::List(items) => {
                            let mut items = items.borrow_mut();
                            if (i as usize) < items.len() {
                                items[i as usize] = v;
                            }
                        }
                        other => {
                            return Err(err(Msg::IndexOnNonList(other.type_tag().into()), 0, 0))
                        }
                    }
                    task.stack.push(Value::Unit);
                }
                Instr::IndexGet => {
                    let idx = task.stack.pop().unwrap_or(Value::Int(0));
                    let obj = task.stack.pop().unwrap_or(Value::Unit).deref();
                    let Value::Int(i) = idx else {
                        return Err(err(Msg::IndexNotInt, 0, 0));
                    };
                    let v = match obj {
                        Value::List(items) => items
                            .borrow()
                            .get(i as usize)
                            .cloned()
                            .unwrap_or(Value::Unit),
                        other => {
                            return Err(err(Msg::IndexOnNonList(other.type_tag().into()), 0, 0))
                        }
                    };
                    task.stack.push(v);
                }
                Instr::MakeStruct(si) => {
                    let (name, fields) = prog.structs[*si as usize].clone();
                    let mut values = Vec::with_capacity(fields.len());
                    for _ in 0..fields.len() {
                        values.push(task.stack.pop().unwrap_or(Value::Unit));
                    }
                    values.reverse();
                    let fields: Vec<(String, Value)> = fields.into_iter().zip(values).collect();
                    task.stack
                        .push(Value::Struct(Box::new(StructVal { name, fields })));
                }
                Instr::GetField(f) => {
                    let obj = task.stack.pop().unwrap_or(Value::Unit);
                    let field = prog.strings[*f as usize].clone();
                    let obj = obj.deref();
                    let v = match obj {
                        Value::Struct(sv) => sv
                            .fields
                            .iter()
                            .find(|(n, _)| *n == field.as_ref())
                            .map(|(_, v)| v.clone())
                            .unwrap_or(Value::Unit),
                        other => {
                            return Err(err(
                                Msg::UnknownField {
                                    ty: other.type_tag().into(),
                                    field: field.to_string(),
                                },
                                0,
                                0,
                            ))
                        }
                    };
                    task.stack.push(v);
                }
                Instr::SetField(f) => {
                    let v = task.stack.pop().unwrap_or(Value::Unit);
                    let obj = task.stack.pop().unwrap_or(Value::Unit);
                    let field = prog.strings[*f as usize].clone();
                    // If the receiver is a shared cell, mutate through it.
                    match obj {
                        Value::Ref(cell) | Value::MutRef(cell) => {
                            let mut cur = cell.borrow_mut();
                            let tag = cur.type_tag();
                            let Value::Struct(sv) = &mut *cur else {
                                return Err(err(
                                    Msg::UnknownField {
                                        ty: tag.into(),
                                        field: field.to_string(),
                                    },
                                    0,
                                    0,
                                ));
                            };
                            if let Some(slot) =
                                sv.fields.iter_mut().find(|(n, _)| *n == field.as_ref())
                            {
                                slot.1 = v;
                            }
                            task.stack.push(Value::Unit);
                        }
                        obj => {
                            let tag = obj.type_tag();
                            let obj = obj.deref();
                            let Value::Struct(mut sv) = obj else {
                                return Err(err(
                                    Msg::UnknownField {
                                        ty: tag.into(),
                                        field: field.to_string(),
                                    },
                                    0,
                                    0,
                                ));
                            };
                            if let Some(slot) =
                                sv.fields.iter_mut().find(|(n, _)| *n == field.as_ref())
                            {
                                slot.1 = v;
                            }
                            task.stack.push(Value::Struct(sv));
                        }
                    }
                }
                Instr::Call(fi) => {
                    let f = &prog.functions[*fi as usize];
                    let mut args = Vec::with_capacity(f.nparams as usize);
                    for _ in 0..f.nparams {
                        args.push(task.stack.pop().unwrap_or(Value::Unit));
                    }
                    args.reverse();
                    let mut locals = args;
                    locals.resize(f.nlocals as usize, Value::Unit);
                    let ret = task.ip;
                    task.frames.push(Frame {
                        func: *fi as usize,
                        ret,
                        locals,
                        cells: Vec::new(),
                    });
                    task.ip = 0;
                }
                Instr::CallMethod(m) => {
                    let method = prog.strings[*m as usize].clone();
                    // Find the method implementation by runtime struct type.
                    // Stack layout: [receiver, arg1, ..., argN].
                    let recv = task.stack.first().cloned().unwrap_or(Value::Unit);
                    let ty = match recv.deref() {
                        Value::Struct(sv) => sv.name.clone(),
                        Value::List(_) => {
                            let argc = match method.as_ref() {
                                "len" => 1,
                                "push" | "get" => 2,
                                "set" => 3,
                                _ => {
                                    return Err(err(
                                        Msg::UnknownMethod {
                                            ty: "List".into(),
                                            method: method.to_string(),
                                        },
                                        0,
                                        0,
                                    ))
                                }
                            };
                            let args = collect_args(&mut task.stack, argc)?;
                            let recv = args[0].deref();
                            return self.list_method(&mut task.stack, recv, &method, &args[1..]);
                        }
                        Value::Chan(_) => {
                            let argc = match method.as_ref() {
                                "send" => 2,
                                "recv" | "close" => 1,
                                _ => {
                                    return Err(err(
                                        Msg::UnknownMethod {
                                            ty: "Chan".into(),
                                            method: method.to_string(),
                                        },
                                        0,
                                        0,
                                    ))
                                }
                            };
                            let args = collect_args(&mut task.stack, argc)?;
                            let recv = args[0].deref();
                            let blocked =
                                self.chan_method(&mut task.stack, recv, &method, &args[1..], id)?;
                            if !blocked && method.as_ref() == "recv" {
                                // Blocked recv: retry CallMethod after being woken
                                // by re-executing it (args restored on the stack).
                                task.ip = task.ip.saturating_sub(1);
                                for a in args.iter().rev() {
                                    task.stack.push(a.clone());
                                }
                            }
                            return Ok(true);
                        }
                        other => {
                            return Err(err(
                                Msg::UnknownMethod {
                                    ty: other.type_tag().into(),
                                    method: method.to_string(),
                                },
                                0,
                                0,
                            ))
                        }
                    };
                    let Some(&fi) = prog
                        .methods
                        .iter()
                        .find(|((t, n), _)| t == &ty && n.as_str() == method.as_ref())
                        .map(|(_, i)| i)
                    else {
                        return Err(err(
                            Msg::UnknownMethod {
                                ty: ty.clone(),
                                method: method.to_string(),
                            },
                            0,
                            0,
                        ));
                    };
                    let f = &prog.functions[fi];
                    let argc = f.nparams as usize;
                    let mut args = Vec::with_capacity(argc);
                    for _ in 0..argc {
                        args.push(task.stack.pop().unwrap_or(Value::Unit));
                    }
                    args.reverse();
                    let mut locals = args;
                    locals.resize(f.nlocals as usize, Value::Unit);
                    let ret = task.ip;
                    task.frames.push(Frame {
                        func: fi,
                        ret,
                        locals,
                        cells: Vec::new(),
                    });
                    task.ip = 0;
                }
                Instr::BuiltinPrint(n) => {
                    let mut parts = Vec::with_capacity(*n as usize);
                    for _ in 0..*n {
                        parts.push(task.stack.pop().unwrap_or(Value::Unit));
                    }
                    parts.reverse();
                    let line = parts
                        .iter()
                        .map(|v| v.display())
                        .collect::<Vec<_>>()
                        .join(" ");
                    writeln!(self.out, "{}", line)
                        .map_err(|e| err(Msg::Io(e.to_string()), 0, 0))?;
                    task.stack.push(Value::Unit);
                }
                Instr::BuiltinRange => {
                    let end = task.stack.pop().unwrap_or(Value::Int(0));
                    let start = task.stack.pop().unwrap_or(Value::Int(0));
                    let (Value::Int(s), Value::Int(e)) = (start, end) else {
                        return Err(err(Msg::RangeNotInt, 0, 0));
                    };
                    task.stack.push(Value::Range { start: s, end: e });
                }
                Instr::Return => {
                    let v = task.stack.pop().unwrap_or(Value::Unit);
                    let done = task.frames.len() == 1;
                    let ret = task.frames.pop().map(|f| f.ret).unwrap_or(0);
                    if !done {
                        task.ip = ret;
                    } else {
                        task.done = true;
                    }
                    task.stack.push(v);
                }
                Instr::Jump(target) => {
                    task.ip = *target as usize;
                }
                Instr::JumpIfFalse(target) => {
                    let v = task.stack.pop().unwrap_or(Value::Bool(false));
                    if !truthy(&v) {
                        task.ip = *target as usize;
                    }
                }
                Instr::ForInit => {
                    let it = task.stack.pop().unwrap_or(Value::Unit);
                    let iter = match it {
                        Value::Range { start, end } => IterState::Range { cur: start, end },
                        Value::List(items) => IterState::List { items, idx: 0 },
                        Value::Chan(chan) => IterState::Chan { chan },
                        other => {
                            return Err(err(Msg::ForNotSupported(other.type_tag().into()), 0, 0))
                        }
                    };
                    task.iters.push(iter);
                }
                Instr::ForNext(target) => {
                    // Take the top iterator out so we can also touch the stack.
                    let Some(iter) = task.iters.pop() else {
                        task.ip = *target as usize;
                        return Ok(true);
                    };
                    match iter {
                        IterState::Range { mut cur, end } => {
                            if cur < end {
                                let v = Value::Int(cur);
                                cur += 1;
                                task.iters.push(IterState::Range { cur, end });
                                task.stack.push(v);
                            } else {
                                task.ip = *target as usize;
                            }
                        }
                        IterState::List { items, mut idx } => {
                            let len = items.borrow().len();
                            if idx < len {
                                let v = items.borrow()[idx].clone();
                                idx += 1;
                                task.iters.push(IterState::List { items, idx });
                                task.stack.push(v);
                            } else {
                                task.ip = *target as usize;
                            }
                        }
                        IterState::Chan { chan } => {
                            let mut c = chan.borrow_mut();
                            if let Some(v) = c.buf.pop_front() {
                                // A slot freed: admit a blocked sender.
                                if let Some(pos) = self
                                    .pending_sends
                                    .iter()
                                    .position(|(ch2, _, _)| Rc::ptr_eq(ch2, &chan))
                                {
                                    if let Some((_, v2, sender)) = self.pending_sends.remove(pos) {
                                        c.buf.push_back(v2);
                                        self.task_states[sender] = TaskState::Ready;
                                    }
                                }
                                drop(c);
                                task.iters.push(IterState::Chan { chan });
                                task.stack.push(v);
                            } else if let Some(pos) = self
                                .pending_sends
                                .iter()
                                .position(|(ch2, _, _)| Rc::ptr_eq(ch2, &chan))
                            {
                                if let Some((_, v, sender)) = self.pending_sends.remove(pos) {
                                    self.task_states[sender] = TaskState::Ready;
                                    drop(c);
                                    task.iters.push(IterState::Chan { chan });
                                    task.stack.push(v);
                                } else {
                                    unreachable!()
                                }
                            } else if c.closed {
                                task.ip = *target as usize;
                            } else {
                                // Block until a value or close arrives; retry
                                // the ForNext after being woken.
                                c.waiting.push_back(id);
                                drop(c);
                                task.iters.push(IterState::Chan { chan });
                                task.ip = task.ip.saturating_sub(1);
                                self.task_states[id] = TaskState::Blocked(0, 0);
                                return Ok(true);
                            }
                        }
                    }
                }
                Instr::ChanSend => {
                    let v = task.stack.pop().unwrap_or(Value::Unit);
                    let ch = task.stack.pop().unwrap_or(Value::Unit);
                    let Value::Chan(chan) = ch else {
                        return Err(err(Msg::BadCall("not a channel".into()), 0, 0));
                    };
                    let mut c = chan.borrow_mut();
                    if c.closed {
                        return Err(err(Msg::BadCall("send on closed channel".into()), 0, 0));
                    }
                    if c.buf.len() < c.cap || c.cap == 0 {
                        if c.cap == 0 {
                            if let Some(w) = c.waiting.pop_front() {
                                c.buf.push_back(v);
                                self.task_states[w] = TaskState::Ready;
                                task.stack.push(Value::Unit);
                                return Ok(true);
                            }
                        } else {
                            c.buf.push_back(v);
                            if let Some(w) = c.waiting.pop_front() {
                                self.task_states[w] = TaskState::Ready;
                            }
                            task.stack.push(Value::Unit);
                            return Ok(true);
                        }
                    }
                    // Block: store the pending value on a side queue. When a
                    // receiver matches, the send is complete; no retry needed.
                    self.pending_sends.push_back((chan.clone(), v, id));
                    self.task_states[id] = TaskState::Blocked(0, 1);
                    return Ok(true);
                }
                Instr::ChanRecv => {
                    let ch = task.stack.pop().unwrap_or(Value::Unit);
                    let Value::Chan(chan) = ch else {
                        return Err(err(Msg::BadCall("not a channel".into()), 0, 0));
                    };
                    let mut c = chan.borrow_mut();
                    if let Some(v) = c.buf.pop_front() {
                        // A slot freed up: admit a waiting sender if any.
                        if let Some(pos) = self
                            .pending_sends
                            .iter()
                            .position(|(ch2, _, _)| Rc::ptr_eq(ch2, &chan))
                        {
                            if let Some((_, v2, sender)) = self.pending_sends.remove(pos) {
                                c.buf.push_back(v2);
                                self.task_states[sender] = TaskState::Ready;
                            }
                        }
                        task.stack.push(v);
                    } else if let Some(pos) = self
                        .pending_sends
                        .iter()
                        .position(|(ch2, _, _)| Rc::ptr_eq(ch2, &chan))
                    {
                        // Hand the value over directly (unbuffered rendezvous).
                        if let Some((_, v, sender)) = self.pending_sends.remove(pos) {
                            self.task_states[sender] = TaskState::Ready;
                            task.stack.push(v);
                        } else {
                            unreachable!()
                        }
                    } else if c.closed {
                        task.stack.push(Value::Unit);
                    } else {
                        // Block: the matching sender pushes the value directly
                        // onto this task's stack when it wakes us.
                        c.waiting.push_back(id);
                        self.task_states[id] = TaskState::Blocked(0, 0);
                        return Ok(true);
                    }
                }
                Instr::ChanClose => {
                    let ch = task.stack.pop().unwrap_or(Value::Unit);
                    let Value::Chan(chan) = ch else {
                        return Err(err(Msg::BadCall("not a channel".into()), 0, 0));
                    };
                    let mut c = chan.borrow_mut();
                    c.closed = true;
                    // Wake all waiters; they observe the closed state.
                    let waiters: Vec<usize> = c.waiting.drain(..).collect();
                    for w in waiters {
                        self.task_states[w] = TaskState::Ready;
                    }
                    task.stack.push(Value::Unit);
                }
                Instr::MakeChan(_) => {
                    let buf = task.stack.pop().unwrap_or(Value::Int(0));
                    let Value::Int(cap) = buf else {
                        return Err(err(Msg::RangeNotInt, 0, 0));
                    };
                    let chan = Rc::new(RefCell::new(Channel::new(cap.max(0) as usize)));
                    self.channels.push(chan.clone());
                    task.stack.push(Value::Chan(chan));
                }
                Instr::Go(fi, argc) => {
                    let mut args = Vec::with_capacity(*argc as usize);
                    for _ in 0..*argc {
                        args.push(task.stack.pop().unwrap_or(Value::Unit));
                    }
                    args.reverse();
                    let group = task.group;
                    let child = self.spawn(*fi as usize, args, group);
                    let _ = child;
                    task.stack.push(Value::Unit);
                }
                Instr::TaskGroupBegin => {
                    let gid = self.groups.len();
                    self.groups.push(Vec::new());
                    task.groups.push((gid, task.group));
                    task.group = gid;
                }
                Instr::TaskGroupEnd => {
                    let (gid, parent) = task.groups.pop().unwrap_or((0, 0));
                    // Wait for all children of this group to finish.
                    let done = self.groups[gid].is_empty();
                    if done {
                        task.group = parent;
                    } else {
                        // Re-execute TaskGroupEnd after being woken.
                        task.ip = task.ip.saturating_sub(1);
                        task.groups.push((gid, parent));
                        self.task_states[id] = TaskState::Blocked(0, 2);
                        self.group_waiters.push((gid, id));
                        return Ok(true);
                    }
                }
                Instr::Yield => {
                    // Cooperative: switch to the next ready task.
                    return Ok(true);
                }
                Instr::Pop => {
                    task.stack.pop();
                }
                Instr::Dup => {
                    if let Some(v) = task.stack.last() {
                        task.stack.push(v.clone());
                    }
                }
                Instr::Not => {
                    let v = task.stack.pop().unwrap_or(Value::Bool(false));
                    task.stack.push(Value::Bool(!truthy(&v)));
                }
                Instr::Binary(op) => {
                    let r = task.stack.pop().unwrap_or(Value::Unit);
                    let l = task.stack.pop().unwrap_or(Value::Unit);
                    // Fast path: int arithmetic without the generic dispatch.
                    let v = match (*op, &l, &r) {
                        (BinOp::Add, Value::Int(a), Value::Int(b)) => {
                            Value::Int(a.wrapping_add(*b))
                        }
                        (BinOp::Sub, Value::Int(a), Value::Int(b)) => {
                            Value::Int(a.wrapping_sub(*b))
                        }
                        (BinOp::Mul, Value::Int(a), Value::Int(b)) => {
                            Value::Int(a.wrapping_mul(*b))
                        }
                        (BinOp::Div, Value::Int(a), Value::Int(b)) => {
                            if *b == 0 {
                                return Err(err(Msg::DivByZero, 0, 0));
                            }
                            Value::Int(a / b)
                        }
                        (BinOp::Mod, Value::Int(a), Value::Int(b)) => {
                            if *b == 0 {
                                return Err(err(Msg::ModByZero, 0, 0));
                            }
                            Value::Int(a % b)
                        }
                        _ => binary(*op, &l, &r).map_err(|m| err(m, 0, 0))?,
                    };
                    task.stack.push(v);
                }
                Instr::RetUnit => {
                    let done = task.frames.len() == 1;
                    let ret = task.frames.pop().map(|f| f.ret).unwrap_or(0);
                    if !done {
                        task.ip = ret;
                    } else {
                        task.done = true;
                    }
                    task.stack.push(Value::Unit);
                }
            }
        }
        Ok(true)
    }

    fn list_method(
        &mut self,
        stack: &mut Vec<Value>,
        recv: Value,
        method: &str,
        args: &[Value],
    ) -> Result<bool, VmError> {
        let Value::List(items) = recv else {
            return Err(err(
                Msg::UnknownMethod {
                    ty: recv.type_tag().into(),
                    method: method.into(),
                },
                0,
                0,
            ));
        };
        match method {
            "len" => {
                stack.push(Value::Int(items.borrow().len() as i64));
                Ok(true)
            }
            "push" => {
                let v = args.first().cloned().unwrap_or(Value::Unit);
                items.borrow_mut().push(v);
                stack.push(Value::Unit);
                Ok(true)
            }
            "get" => {
                let Value::Int(i) = args.first().cloned().unwrap_or(Value::Int(0)) else {
                    return Err(err(Msg::IndexNotInt, 0, 0));
                };
                stack.push(
                    items
                        .borrow()
                        .get(i as usize)
                        .cloned()
                        .unwrap_or(Value::Unit),
                );
                Ok(true)
            }
            "set" => {
                let Value::Int(i) = args.first().cloned().unwrap_or(Value::Int(0)) else {
                    return Err(err(Msg::IndexNotInt, 0, 0));
                };
                let v = args.get(1).cloned().unwrap_or(Value::Unit);
                let mut items = items.borrow_mut();
                if (i as usize) < items.len() {
                    items[i as usize] = v;
                }
                stack.push(Value::Unit);
                Ok(true)
            }
            _ => Err(err(
                Msg::UnknownMethod {
                    ty: "List".into(),
                    method: method.into(),
                },
                0,
                0,
            )),
        }
    }

    fn chan_method(
        &mut self,
        stack: &mut Vec<Value>,
        recv: Value,
        method: &str,
        args: &[Value],
        task_id: usize,
    ) -> Result<bool, VmError> {
        let Value::Chan(chan) = recv else {
            return Err(err(
                Msg::UnknownMethod {
                    ty: recv.type_tag().into(),
                    method: method.into(),
                },
                0,
                0,
            ));
        };
        match method {
            "send" => {
                let v = args.first().cloned().unwrap_or(Value::Unit);
                let mut c = chan.borrow_mut();
                if c.closed {
                    return Err(err(Msg::BadCall("send on closed channel".into()), 0, 0));
                }
                if c.buf.len() < c.cap || c.cap == 0 {
                    // Buffered: push. Unbuffered: rendezvous into the buffer.
                    if c.cap == 0 {
                        if let Some(w) = c.waiting.pop_front() {
                            c.buf.push_back(v);
                            self.task_states[w] = TaskState::Ready;
                            stack.push(Value::Unit);
                            return Ok(true);
                        }
                    } else {
                        c.buf.push_back(v);
                        if let Some(w) = c.waiting.pop_front() {
                            self.task_states[w] = TaskState::Ready;
                        }
                        stack.push(Value::Unit);
                        return Ok(true);
                    }
                }
                self.pending_sends.push_back((chan.clone(), v, task_id));
                self.task_states[task_id] = TaskState::Blocked(0, 1);
                Ok(false)
            }
            "recv" => {
                let mut c = chan.borrow_mut();
                if let Some(v) = c.buf.pop_front() {
                    if let Some(pos) = self
                        .pending_sends
                        .iter()
                        .position(|(ch2, _, _)| Rc::ptr_eq(ch2, &chan))
                    {
                        if let Some((_, v2, sender)) = self.pending_sends.remove(pos) {
                            c.buf.push_back(v2);
                            self.task_states[sender] = TaskState::Ready;
                        }
                    }
                    stack.push(v);
                    Ok(true)
                } else if let Some(pos) = self
                    .pending_sends
                    .iter()
                    .position(|(ch2, _, _)| Rc::ptr_eq(ch2, &chan))
                {
                    if let Some((_, v, sender)) = self.pending_sends.remove(pos) {
                        self.task_states[sender] = TaskState::Ready;
                        stack.push(v);
                        Ok(true)
                    } else {
                        unreachable!()
                    }
                } else if c.closed {
                    stack.push(Value::Unit);
                    Ok(true)
                } else {
                    // Block; CallMethod restores the stack and retries.
                    c.waiting.push_back(task_id);
                    self.task_states[task_id] = TaskState::Blocked(0, 0);
                    Ok(false)
                }
            }
            "close" => {
                let mut c = chan.borrow_mut();
                c.closed = true;
                let waiters: Vec<usize> = c.waiting.drain(..).collect();
                for w in waiters {
                    self.task_states[w] = TaskState::Ready;
                }
                stack.push(Value::Unit);
                Ok(true)
            }
            _ => Err(err(
                Msg::UnknownMethod {
                    ty: "Chan".into(),
                    method: method.into(),
                },
                0,
                0,
            )),
        }
    }
}

fn collect_args(stack: &mut Vec<Value>, n: usize) -> Result<Vec<Value>, VmError> {
    let mut args = Vec::with_capacity(n);
    for _ in 0..n {
        args.push(stack.pop().unwrap_or(Value::Unit));
    }
    args.reverse();
    Ok(args)
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Int(n) => *n != 0,
        Value::Float(f) => *f != 0.0,
        Value::Str(s) => !s.is_empty(),
        Value::List(items) => !items.borrow().is_empty(),
        Value::Struct(_) => true,
        _ => false,
    }
}

fn binary(op: BinOp, l: &Value, r: &Value) -> Result<Value, Msg> {
    use BinOp::*;
    match op {
        Add | Sub | Mul | Div | Mod => match (l, r) {
            (Value::Int(a), Value::Int(b)) => match op {
                Add => Ok(Value::Int(a.wrapping_add(*b))),
                Sub => Ok(Value::Int(a.wrapping_sub(*b))),
                Mul => Ok(Value::Int(a.wrapping_mul(*b))),
                Div => {
                    if *b == 0 {
                        return Err(Msg::DivByZero);
                    }
                    Ok(Value::Int(a / b))
                }
                Mod => {
                    if *b == 0 {
                        return Err(Msg::ModByZero);
                    }
                    Ok(Value::Int(a % b))
                }
                _ => unreachable!(),
            },
            (Value::Int(a), Value::Float(b)) => float_op(op, *a as f64, *b),
            (Value::Float(a), Value::Int(b)) => float_op(op, *a, *b as f64),
            (Value::Float(a), Value::Float(b)) => float_op(op, *a, *b),
            (Value::Str(a), Value::Str(b)) if op == Add => {
                let mut s = String::with_capacity(a.len() + b.len());
                s.push_str(a);
                s.push_str(b);
                Ok(Value::Str(s.into()))
            }
            _ => Err(Msg::TypeMismatch),
        },
        Eq | Ne | Lt | Le | Gt | Ge => cmp_op(op, l, r),
        And | Or => unreachable!("and/or handled by compiler as binary"),
    }
}

fn float_op(op: BinOp, a: f64, b: f64) -> Result<Value, Msg> {
    use BinOp::*;
    let v = match op {
        Add => a + b,
        Sub => a - b,
        Mul => a * b,
        Div => a / b,
        Mod => a % b,
        _ => unreachable!(),
    };
    Ok(Value::Float(v))
}

fn cmp_op(op: BinOp, l: &Value, r: &Value) -> Result<Value, Msg> {
    use BinOp::*;
    let result = match (l, r) {
        (Value::Int(a), Value::Int(b)) => match op {
            Eq => a == b,
            Ne => a != b,
            Lt => a < b,
            Le => a <= b,
            Gt => a > b,
            Ge => a >= b,
            _ => unreachable!(),
        },
        (Value::Int(a), Value::Float(b)) => num_cmp(op, *a as f64, *b),
        (Value::Float(a), Value::Int(b)) => num_cmp(op, *a, *b as f64),
        (Value::Float(a), Value::Float(b)) => num_cmp(op, *a, *b),
        (Value::Str(a), Value::Str(b)) => match op {
            Eq => a == b,
            Ne => a != b,
            Lt => a < b,
            Le => a <= b,
            Gt => a > b,
            Ge => a >= b,
            _ => unreachable!(),
        },
        (Value::Bool(a), Value::Bool(b)) => match op {
            Eq => a == b,
            Ne => a != b,
            _ => return Err(Msg::BoolOrderCmp),
        },
        _ => return Err(Msg::CmpMismatch),
    };
    Ok(Value::Bool(result))
}

fn num_cmp(op: BinOp, a: f64, b: f64) -> bool {
    use BinOp::*;
    match op {
        Eq => a == b,
        Ne => a != b,
        Lt => a < b,
        Le => a <= b,
        Gt => a > b,
        Ge => a >= b,
        _ => unreachable!(),
    }
}
