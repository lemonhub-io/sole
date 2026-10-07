//! M3 bytecode VM for Sole.
//!
//! Programs are compiled to bytecode (`compiler.rs`) and executed by this
//! stack-based VM. Tasks (coroutines) are VM instances with their own value
//! and frame stacks, scheduled cooperatively: switching happens only at
//! channel operations and `yield` (GOALS ). Channels implement the
//! ownership-moving send/recv semantics; `task_group` provides structured
//! concurrency with cancellation (= closing channels).

use num_bigint::BigInt;
use sole_diag::Msg;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

mod program;
mod value;

pub use program::{BinOp, CompiledProgram, Function, Instr, VmError};
pub use value::*;

use program::err;
use value::IntVal;

impl<'a> Runtime<'a> {
    /// Source position of the instruction the given task is currently
    /// executing. The task's `ip` has already advanced past it, so the
    /// position of the instruction being executed is `ip - 1`.
    fn pos_at(&self, task: &Task) -> (usize, usize) {
        let Some(frame) = task.frames.last() else {
            return (0, 0);
        };
        let spans = &self.prog.functions[frame.func].spans;
        spans
            .get(task.ip.saturating_sub(1))
            .map(|&(l, c)| (l as usize, c as usize))
            .unwrap_or((0, 0))
    }
}

/// A stack frame: function index, return address, and the stack index where
/// this frame's locals live. Locals live directly on the task stack, so
/// calls are zero-copy: the caller's args become the callee's first locals.
///
/// A slot only gets an `Rc<RefCell>` cell when it is actually borrowed
/// (`ref`/`mut ref`) or passed by cell (`PushVarCell`), so the common case
/// never allocates per local.
pub struct Frame {
    pub func: usize,
    pub ret: usize,
    pub base: usize,
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
    /// group id set of task ids in it (not yet finished).
    pub groups: Vec<Vec<usize>>,
    pub current: usize,
    pub out: &'a mut dyn std::io::Write,
    /// Global variable storage by name.
    pub globals: HashMap<String, Rc<RefCell<Value>>>,
    /// Pending channel sends waiting for a receiver.
    pub pending_sends: VecDeque<(Rc<RefCell<Channel>>, Value, usize)>,
    /// Tasks waiting for their group's children to finish: (group, task).
    pub group_waiters: Vec<(usize, usize)>,
    /// Script arguments visible to the program via `args()`.
    pub args: Vec<String>,
    /// struct type name → (method name → function index), built once so
    /// method dispatch does not scan `prog.methods` per call.
    pub method_map: HashMap<String, HashMap<String, usize>>,
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

    fn set_global(&mut self, name: &str, value: Value, pos: (usize, usize)) -> Result<(), VmError> {
        if let Some(cell) = self.globals.get(name) {
            let cur = cell.borrow().clone();
            match cur {
                Value::MutRef(target) => {
                    *target.borrow_mut() = value;
                }
                Value::Ref(_) => {
                    return Err(err(Msg::ImmutableReassign(name.into()), pos));
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
        let mut method_map: HashMap<String, HashMap<String, usize>> = HashMap::new();
        for ((ty, name), i) in &prog.methods {
            method_map
                .entry(ty.clone())
                .or_default()
                .insert(name.clone(), *i);
        }
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
            args: Vec::new(),
            method_map,
        }
    }

    /// Runs a single function in a fresh task (used by `sole test`).
    pub fn run_function(&mut self, func: usize) -> Result<(), VmError> {
        self.spawn(func, vec![], 0);
        self.schedule_all()
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
        let nlocals = f.nlocals as usize;
        let mut stack = args;
        stack.resize(nlocals.max(stack.len()), Value::Unit);
        let id = self.tasks.len();
        self.tasks.push(Some(Box::new(Task {
            ip: 0,
            stack,
            frames: vec![Frame {
                func,
                ret: 0,
                base: 0,
                cells: Vec::new(),
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
        let mut code = &prog.functions[task.frames.last().unwrap().func].code;
        // Frame switches are only possible inside Call/Return/RetUnit,
        // which refresh `code` themselves — the per-instruction path
        // keeps just the ip bounds check.
        macro_rules! refresh {
            () => {
                match task.frames.last() {
                    Some(f) => {
                        code = &prog.functions[f.func].code;
                    }
                    None => {
                        task.done = true;
                        break;
                    }
                }
            };
        }
        loop {
            budget -= 1;
            if budget == 0 {
                return Err(err(Msg::InternalFnIndex, self.pos_at(task)));
            }
            if task.ip >= code.len() {
                task.done = true;
                break;
            }
            let instr = &code[task.ip];
            task.ip += 1;
            match instr {
                Instr::Halt => {
                    task.done = true;
                }
                Instr::PushInt(n) => {
                    task.stack.push(Value::Int(*n));
                }
                Instr::PushBig(i) => {
                    task.stack
                        .push(Value::Big(prog.bigints[*i as usize].clone()));
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
                            None => task.stack[frame.base + *i as usize].clone(),
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
                                let idx = frame.base + *i as usize;
                                match task.stack[idx].clone() {
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
                    let (base, cell) = {
                        let frame = task.frames.last().unwrap();
                        (
                            frame.base,
                            frame.cells.get(*i as usize).and_then(|c| c.clone()),
                        )
                    };
                    if let Some(cell) = cell {
                        let cur = cell.borrow().clone();
                        match cur {
                            Value::MutRef(target) => {
                                *target.borrow_mut() = v;
                            }
                            Value::Ref(_) => {
                                return Err(err(
                                    Msg::ImmutableReassign("<ref>".into()),
                                    self.pos_at(task),
                                ));
                            }
                            _ => {
                                *cell.borrow_mut() = v;
                            }
                        }
                    } else {
                        let slot = &mut task.stack[base + *i as usize];
                        match slot {
                            Value::MutRef(target) => {
                                *target.borrow_mut() = v;
                            }
                            Value::Ref(_) => {
                                return Err(err(
                                    Msg::ImmutableReassign("<ref>".into()),
                                    self.pos_at(task),
                                ));
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
                    self.set_global(&name, v, self.pos_at(task))?;
                }
                Instr::BorrowVar(mutable, i) => {
                    let cell = {
                        let frame = task.frames.last().unwrap();
                        match frame.cells.get(*i as usize).and_then(|c| c.clone()) {
                            Some(cell) => cell,
                            None => {
                                let idx = frame.base + *i as usize;
                                let cell = Rc::new(RefCell::new(task.stack[idx].clone()));
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
                    let Some(i) = idx.as_int() else {
                        return Err(err(Msg::IndexNotIntRt, self.pos_at(task)));
                    };
                    let item = match list {
                        Value::List(items) => items
                            .borrow()
                            .get(i.to_usize_sat())
                            .cloned()
                            .unwrap_or(Value::Unit),
                        _ => {
                            return Err(err(
                                Msg::IndexOnNonListRt(list.type_tag().into()),
                                self.pos_at(task),
                            ))
                        }
                    };
                    task.stack.push(item);
                }
                Instr::ListSet => {
                    let v = task.stack.pop().unwrap_or(Value::Unit);
                    let idx = task.stack.pop().unwrap_or(Value::Int(0));
                    let list = task.stack.pop().unwrap_or(Value::Unit);
                    let Some(i) = idx.as_int() else {
                        return Err(err(Msg::IndexNotIntRt, self.pos_at(task)));
                    };
                    match list {
                        Value::List(items) => {
                            let mut items = items.borrow_mut();
                            let i = i.to_usize_sat();
                            if i < items.len() {
                                items[i] = v;
                            }
                        }
                        other => {
                            return Err(err(
                                Msg::IndexOnNonListRt(other.type_tag().into()),
                                self.pos_at(task),
                            ))
                        }
                    }
                    task.stack.push(Value::Unit);
                }
                Instr::IndexGet => {
                    let idx = task.stack.pop().unwrap_or(Value::Int(0));
                    let obj = task.stack.pop().unwrap_or(Value::Unit).deref();
                    let v = match (&obj, &idx) {
                        (Value::List(items), i) if i.is_int() => {
                            let items = items.borrow();
                            let len = items.len() as i64;
                            let i = i.as_int().unwrap();
                            items.get(i.to_usize_sat()).cloned().ok_or_else(|| {
                                err(
                                    Msg::IndexOutOfRange {
                                        index: i.to_i64_sat(),
                                        len,
                                    },
                                    self.pos_at(task),
                                )
                            })?
                        }
                        (Value::Dict(d), key) => d.borrow().get(key).unwrap_or(Value::Unit),
                        (Value::Tuple(items), i) if i.is_int() => {
                            let len = items.len() as i64;
                            let i = i.as_int().unwrap();
                            items.get(i.to_usize_sat()).cloned().ok_or_else(|| {
                                err(
                                    Msg::IndexOutOfRange {
                                        index: i.to_i64_sat(),
                                        len,
                                    },
                                    self.pos_at(task),
                                )
                            })?
                        }
                        (Value::List(_), _) | (Value::Tuple(_), _) => {
                            return Err(err(Msg::IndexNotIntRt, self.pos_at(task)))
                        }
                        other => {
                            return Err(err(
                                Msg::IndexOnNonListRt(other.0.type_tag().into()),
                                self.pos_at(task),
                            ))
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
                            .find(|(n, _)| n == field.as_ref())
                            .map(|(_, v)| v.clone())
                            .unwrap_or(Value::Unit),
                        other => {
                            return Err(err(
                                Msg::UnknownField {
                                    ty: other.type_tag().into(),
                                    field: field.to_string(),
                                },
                                self.pos_at(task),
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
                                    self.pos_at(task),
                                ));
                            };
                            if let Some(slot) =
                                sv.fields.iter_mut().find(|(n, _)| n == field.as_ref())
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
                                    self.pos_at(task),
                                ));
                            };
                            if let Some(slot) =
                                sv.fields.iter_mut().find(|(n, _)| n == field.as_ref())
                            {
                                slot.1 = v;
                            }
                            task.stack.push(Value::Struct(sv));
                        }
                    }
                }
                Instr::Call(fi) => {
                    let f = &prog.functions[*fi as usize];
                    // Zero-copy call: the args already on the stack become the
                    // callee's first locals; only extra locals are pushed.
                    let nparams = f.nparams as usize;
                    let base = task.stack.len().saturating_sub(nparams);
                    task.stack.resize(base + f.nlocals as usize, Value::Unit);
                    let ret = task.ip;
                    task.frames.push(Frame {
                        func: *fi as usize,
                        ret,
                        base,
                        cells: Vec::new(),
                    });
                    task.ip = 0;
                    code = &prog.functions[*fi as usize].code;
                }
                Instr::CallMethod(m, argc) => {
                    let method = prog.strings[*m as usize].clone();
                    // Find the method implementation by runtime struct type.
                    // Stack layout: [..locals.., receiver, arg1, ..., argN].
                    // The receiver sits at `len - argc`; the compiler encodes
                    // the total argument count (receiver included).
                    let argc = *argc as usize;
                    let recv = task.stack[task.stack.len().saturating_sub(argc)].clone();
                    if method.as_str() == "to_str" && argc == 1 {
                        let recv = task.stack.pop().unwrap_or(Value::Unit);
                        task.stack.push(Value::Str(Rc::new(recv.display())));
                        continue;
                    }
                    if method.as_str() == "is_int" && argc == 1 {
                        let recv = task.stack.pop().unwrap_or(Value::Unit).deref();
                        task.stack.push(Value::Bool(recv.is_int()));
                        continue;
                    }
                    if method.as_str() == "is_str" && argc == 1 {
                        let recv = task.stack.pop().unwrap_or(Value::Unit).deref();
                        task.stack.push(Value::Bool(matches!(recv, Value::Str(_))));
                        continue;
                    }
                    // Json introspection: cheap runtime-type queries that
                    // avoid stringifying the value first.
                    if matches!(
                        method.as_str(),
                        "is_obj" | "is_arr" | "is_num" | "is_bool" | "is_null"
                    ) && argc == 1
                    {
                        let recv = task.stack.pop().unwrap_or(Value::Unit).deref();
                        let yes = match method.as_str() {
                            "is_obj" => matches!(recv, Value::Dict(_)),
                            "is_arr" => matches!(recv, Value::List(_)),
                            "is_num" => recv.is_int() || matches!(recv, Value::Float(_)),
                            "is_bool" => matches!(recv, Value::Bool(_)),
                            _ => matches!(recv, Value::None),
                        };
                        task.stack.push(Value::Bool(yes));
                        continue;
                    }
                    let ty = match recv.deref() {
                        Value::Struct(sv) => sv.name.clone(),
                        Value::Str(_) => {
                            let args = collect_args(&mut task.stack, argc)?;
                            let recv = args[0].deref();
                            let pos = self.pos_at(task);
                            return self.str_method(
                                &mut task.stack,
                                recv,
                                &method,
                                &args[1..],
                                pos,
                            );
                        }
                        Value::Dict(_) => {
                            let args = collect_args(&mut task.stack, argc)?;
                            let recv = args[0].deref();
                            let pos = self.pos_at(task);
                            return self.dict_method(
                                &mut task.stack,
                                recv,
                                &method,
                                &args[1..],
                                pos,
                            );
                        }
                        Value::Set(_) => {
                            let args = collect_args(&mut task.stack, argc)?;
                            let recv = args[0].deref();
                            let pos = self.pos_at(task);
                            return self.set_method(
                                &mut task.stack,
                                recv,
                                &method,
                                &args[1..],
                                pos,
                            );
                        }
                        Value::Tuple(_) => {
                            let args = collect_args(&mut task.stack, argc)?;
                            let recv = args[0].deref();
                            let pos = self.pos_at(task);
                            return self.tuple_method(
                                &mut task.stack,
                                recv,
                                &method,
                                &args[1..],
                                pos,
                            );
                        }
                        Value::None | Value::Some(_) => {
                            let args = collect_args(&mut task.stack, argc)?;
                            let recv = args[0].deref();
                            let pos = self.pos_at(task);
                            return self.option_method(
                                &mut task.stack,
                                recv,
                                &method,
                                &args[1..],
                                pos,
                            );
                        }
                        Value::Ok(_) | Value::Err(_) => {
                            let args = collect_args(&mut task.stack, argc)?;
                            let recv = args[0].deref();
                            let pos = self.pos_at(task);
                            return self.result_method(
                                &mut task.stack,
                                recv,
                                &method,
                                &args[1..],
                                pos,
                            );
                        }
                        Value::List(_) => {
                            let args = collect_args(&mut task.stack, argc)?;
                            let recv = args[0].deref();
                            let pos = self.pos_at(task);
                            return self.list_method(
                                &mut task.stack,
                                recv,
                                &method,
                                &args[1..],
                                pos,
                            );
                        }
                        Value::Chan(_) => {
                            let args = collect_args(&mut task.stack, argc)?;
                            let recv = args[0].deref();
                            let pos = self.pos_at(task);
                            let blocked = self.chan_method(
                                &mut task.stack,
                                recv,
                                &method,
                                &args[1..],
                                id,
                                pos,
                            )?;
                            if !blocked && method.as_str() == "recv" {
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
                                self.pos_at(task),
                            ))
                        }
                    };
                    let Some(&fi) = self
                        .method_map
                        .get(ty.as_str())
                        .and_then(|m| m.get(method.as_ref()))
                    else {
                        return Err(err(
                            Msg::UnknownMethod {
                                ty: ty.clone(),
                                method: method.to_string(),
                            },
                            self.pos_at(task),
                        ));
                    };
                    let f = &prog.functions[fi];
                    let base = task.stack.len().saturating_sub(argc);
                    task.stack.resize(base + f.nlocals as usize, Value::Unit);
                    let ret = task.ip;
                    task.frames.push(Frame {
                        func: fi,
                        ret,
                        base,
                        cells: Vec::new(),
                    });
                    task.ip = 0;
                    code = &prog.functions[fi].code;
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
                        .map_err(|e| err(Msg::Io(e.to_string()), self.pos_at(task)))?;
                    task.stack.push(Value::Unit);
                }
                Instr::BuiltinRange => {
                    let end = task.stack.pop().unwrap_or(Value::Int(0));
                    let start = task.stack.pop().unwrap_or(Value::Int(0));
                    let (Some(s), Some(e)) = (start.as_int(), end.as_int()) else {
                        return Err(err(Msg::RangeNotInt, self.pos_at(task)));
                    };
                    // Range bounds are `i64`; a huge bound saturates to an
                    // effectively-infinite range.
                    let (s, e) = (s.to_i64_sat(), e.to_i64_sat());
                    task.stack
                        .push(Value::Range(Rc::new(RangeVal { start: s, end: e })));
                }
                Instr::Return => {
                    let v = task.stack.pop().unwrap_or(Value::Unit);
                    let done = task.frames.len() == 1;
                    let (ret, base) = task.frames.pop().map(|f| (f.ret, f.base)).unwrap_or((0, 0));
                    // Discard the callee's locals region, leaving the result.
                    task.stack.truncate(base);
                    task.stack.push(v);
                    if !done {
                        task.ip = ret;
                        refresh!();
                    } else {
                        task.done = true;
                    }
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
                        Value::Range(r) => IterState::Range {
                            cur: r.start,
                            end: r.end,
                        },
                        Value::List(items) => IterState::List { items, idx: 0 },
                        Value::Chan(chan) => IterState::Chan { chan },
                        other => {
                            return Err(err(
                                Msg::ForNotSupported(other.type_tag().into()),
                                self.pos_at(task),
                            ))
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
                        return Err(err(Msg::BadCall("not a channel".into()), self.pos_at(task)));
                    };
                    let mut c = chan.borrow_mut();
                    if c.closed {
                        return Err(err(
                            Msg::BadCall("send on closed channel".into()),
                            self.pos_at(task),
                        ));
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
                        return Err(err(Msg::BadCall("not a channel".into()), self.pos_at(task)));
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
                        return Err(err(Msg::BadCall("not a channel".into()), self.pos_at(task)));
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
                    let Some(cap) = buf.as_int() else {
                        return Err(err(Msg::RangeNotInt, self.pos_at(task)));
                    };
                    let chan = Rc::new(RefCell::new(Channel::new(cap.to_usize_sat())));
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
                Instr::Neg => {
                    let v = task.stack.pop().unwrap_or(Value::Int(0));
                    let v = v.deref();
                    if let Some(i) = v.as_int() {
                        task.stack.push(Value::from(i.neg()));
                    } else {
                        match v {
                            Value::Float(f) => task.stack.push(Value::Float(-f)),
                            other => {
                                return Err(err(
                                    Msg::OpTypeMismatch {
                                        op: "-".into(),
                                        actual: other.type_tag().into(),
                                    },
                                    self.pos_at(task),
                                ))
                            }
                        }
                    }
                }
                Instr::Binary(op) => {
                    let r = task.stack.pop().unwrap_or(Value::Unit);
                    let l = task.stack.pop().unwrap_or(Value::Unit);
                    // Fast path: i64 arithmetic without the generic dispatch;
                    // overflow promotes to `Big`.
                    let v = match (*op, &l, &r) {
                        (BinOp::Add, Value::Int(a), Value::Int(b)) => match a.checked_add(*b) {
                            Some(n) => Value::Int(n),
                            None => Value::Big(Rc::new(BigInt::from(*a) + BigInt::from(*b))),
                        },
                        (BinOp::Sub, Value::Int(a), Value::Int(b)) => match a.checked_sub(*b) {
                            Some(n) => Value::Int(n),
                            None => Value::Big(Rc::new(BigInt::from(*a) - BigInt::from(*b))),
                        },
                        (BinOp::Mul, Value::Int(a), Value::Int(b)) => match a.checked_mul(*b) {
                            Some(n) => Value::Int(n),
                            None => Value::Big(Rc::new(BigInt::from(*a) * BigInt::from(*b))),
                        },
                        (BinOp::Div, Value::Int(a), Value::Int(b)) => {
                            if *b == 0 {
                                return Err(err(Msg::DivByZero, self.pos_at(task)));
                            }
                            match a.checked_div(*b) {
                                Some(n) => Value::Int(n),
                                None => Value::Big(Rc::new(BigInt::from(*a) / BigInt::from(*b))),
                            }
                        }
                        (BinOp::Mod, Value::Int(a), Value::Int(b)) => {
                            if *b == 0 {
                                return Err(err(Msg::ModByZero, self.pos_at(task)));
                            }
                            match a.checked_rem(*b) {
                                Some(n) => Value::Int(n),
                                None => Value::Big(Rc::new(BigInt::from(*a) % BigInt::from(*b))),
                            }
                        }
                        (BinOp::And, _, _) => Value::Bool(truthy(&l) && truthy(&r)),
                        (BinOp::Or, _, _) => Value::Bool(truthy(&l) || truthy(&r)),
                        _ => binary(*op, &l, &r).map_err(|m| err(m, self.pos_at(task)))?,
                    };
                    task.stack.push(v);
                }
                Instr::RetUnit => {
                    let done = task.frames.len() == 1;
                    let (ret, base) = task.frames.pop().map(|f| (f.ret, f.base)).unwrap_or((0, 0));
                    // Discard the callee's locals region, leaving the unit value.
                    task.stack.truncate(base);
                    task.stack.push(Value::Unit);
                    if !done {
                        task.ip = ret;
                        refresh!();
                    } else {
                        task.done = true;
                    }
                }
                Instr::PushNone => {
                    task.stack.push(Value::None);
                }
                Instr::PushSome => {
                    let v = task.stack.pop().unwrap_or(Value::Unit);
                    task.stack.push(Value::Some(Box::new(v)));
                }
                Instr::PushOk => {
                    let v = task.stack.pop().unwrap_or(Value::Unit);
                    task.stack.push(Value::Ok(Box::new(v)));
                }
                Instr::PushErr => {
                    let v = task.stack.pop().unwrap_or(Value::Unit);
                    task.stack.push(Value::Err(Box::new(v)));
                }
                Instr::MakeDict(n) => {
                    let mut pairs = Vec::with_capacity(*n as usize);
                    for _ in 0..*n {
                        let v = task.stack.pop().unwrap_or(Value::Unit);
                        let k = task.stack.pop().unwrap_or(Value::Unit);
                        pairs.push((k, v));
                    }
                    pairs.reverse();
                    task.stack
                        .push(Value::Dict(Rc::new(RefCell::new(DictVal::from_items(
                            pairs,
                        )))));
                }
                Instr::MakeSet(n) => {
                    let mut items = Vec::with_capacity(*n as usize);
                    for _ in 0..*n {
                        items.push(task.stack.pop().unwrap_or(Value::Unit));
                    }
                    items.reverse();
                    // Set semantics: elements are unique.
                    task.stack
                        .push(Value::Set(Rc::new(RefCell::new(SetVal::from_items(items)))));
                }
                Instr::MakeTuple(n) => {
                    let mut items = Vec::with_capacity(*n as usize);
                    for _ in 0..*n {
                        items.push(task.stack.pop().unwrap_or(Value::Unit));
                    }
                    items.reverse();
                    task.stack.push(Value::Tuple(Rc::new(items)));
                }
                Instr::Assert => {
                    let v = task.stack.pop().unwrap_or(Value::Bool(false));
                    if !truthy(&v) {
                        return Err(err(Msg::AssertFailed, self.pos_at(task)));
                    }
                }
                Instr::Builtin(id) => {
                    self.std_builtin(task, *id)?;
                }
            }
        }
        Ok(true)
    }

    fn str_method(
        &mut self,
        stack: &mut Vec<Value>,
        recv: Value,
        method: &str,
        args: &[Value],
        pos: (usize, usize),
    ) -> Result<bool, VmError> {
        let Value::Str(s) = recv else {
            return Err(err(
                Msg::UnknownMethod {
                    ty: recv.type_tag().into(),
                    method: method.into(),
                },
                pos,
            ));
        };
        match method {
            "len" => {
                stack.push(Value::Int(s.len() as i64));
                Ok(true)
            }
            "sub" => {
                let Some(start) = args.first().unwrap_or(&Value::Int(0)).as_int() else {
                    return Err(err(Msg::IndexNotIntRt, pos));
                };
                let Some(end) = args.get(1).unwrap_or(&Value::Int(s.len() as i64)).as_int() else {
                    return Err(err(Msg::IndexNotIntRt, pos));
                };
                let start = start.to_i64_sat().max(0).min(s.len() as i64) as usize;
                let end = end.to_i64_sat().max(0).min(s.len() as i64) as usize;
                stack.push(Value::Str(Rc::new(if start <= end {
                    s[start..end].to_string()
                } else {
                    String::new()
                })));
                Ok(true)
            }
            "split" => {
                let Value::Str(sep) = args
                    .first()
                    .cloned()
                    .unwrap_or(Value::Str(Rc::new(" ".to_string())))
                else {
                    return Err(err(Msg::BadCall("split needs a str separator".into()), pos));
                };
                let parts: Vec<Value> = s
                    .split(sep.as_ref())
                    .map(|p| Value::Str(Rc::new(p.to_string())))
                    .collect();
                stack.push(Value::List(Rc::new(RefCell::new(parts))));
                Ok(true)
            }
            "join" => {
                let Value::List(items) = args.first().cloned().unwrap_or(Value::Unit) else {
                    return Err(err(Msg::BadCall("join needs a List[str]".into()), pos));
                };
                let parts: Vec<String> = items
                    .borrow()
                    .iter()
                    .map(|v| match v {
                        Value::Str(t) => t.to_string(),
                        other => other.display(),
                    })
                    .collect();
                stack.push(Value::Str(Rc::new(parts.join(s.as_str()))));
                Ok(true)
            }
            "contains" => {
                let Value::Str(sub) = args
                    .first()
                    .cloned()
                    .unwrap_or(Value::Str(Rc::new(String::new())))
                else {
                    return Err(err(Msg::BadCall("contains needs a str".into()), pos));
                };
                stack.push(Value::Bool(s.contains(sub.as_ref())));
                Ok(true)
            }
            "starts_with" => {
                let Value::Str(sub) = args
                    .first()
                    .cloned()
                    .unwrap_or(Value::Str(Rc::new(String::new())))
                else {
                    return Err(err(Msg::BadCall("starts_with needs a str".into()), pos));
                };
                stack.push(Value::Bool(s.starts_with(sub.as_ref())));
                Ok(true)
            }
            "ends_with" => {
                let Value::Str(sub) = args
                    .first()
                    .cloned()
                    .unwrap_or(Value::Str(Rc::new(String::new())))
                else {
                    return Err(err(Msg::BadCall("ends_with needs a str".into()), pos));
                };
                stack.push(Value::Bool(s.ends_with(sub.as_ref())));
                Ok(true)
            }
            "trim" => {
                stack.push(Value::Str(Rc::new(s.trim().to_string())));
                Ok(true)
            }
            "to_int" => match IntVal::parse(s.as_ref()) {
                Some(i) => {
                    stack.push(Value::Ok(Box::new(Value::from(i))));
                    Ok(true)
                }
                None => {
                    stack.push(Value::Err(Box::new(Value::Str(Rc::new(format!(
                        "cannot parse '{}' as int",
                        s
                    ))))));
                    Ok(true)
                }
            },
            "to_float" => match s.parse::<f64>() {
                Ok(f) => {
                    stack.push(Value::Ok(Box::new(Value::Float(f))));
                    Ok(true)
                }
                Err(e) => {
                    stack.push(Value::Err(Box::new(Value::Str(Rc::new(e.to_string())))));
                    Ok(true)
                }
            },
            _ => Err(err(
                Msg::UnknownMethod {
                    ty: "str".into(),
                    method: method.into(),
                },
                pos,
            )),
        }
    }

    fn dict_method(
        &mut self,
        stack: &mut Vec<Value>,
        recv: Value,
        method: &str,
        args: &[Value],
        pos: (usize, usize),
    ) -> Result<bool, VmError> {
        let Value::Dict(dict) = recv else {
            return Err(err(
                Msg::UnknownMethod {
                    ty: recv.type_tag().into(),
                    method: method.into(),
                },
                pos,
            ));
        };
        match method {
            "len" => {
                stack.push(Value::Int(dict.borrow().len() as i64));
                Ok(true)
            }
            "get" => {
                let k = args.first().cloned().unwrap_or(Value::Unit);
                let v = dict
                    .borrow()
                    .get(&k)
                    .map(|v| Value::Some(Box::new(v)))
                    .unwrap_or(Value::None);
                stack.push(v);
                Ok(true)
            }
            "set" => {
                let k = args.first().cloned().unwrap_or(Value::Unit);
                let v = args.get(1).cloned().unwrap_or(Value::Unit);
                dict.borrow_mut().set(k, v);
                stack.push(Value::Unit);
                Ok(true)
            }
            "contains" => {
                let k = args.first().cloned().unwrap_or(Value::Unit);
                stack.push(Value::Bool(dict.borrow().contains(&k)));
                Ok(true)
            }
            "remove" => {
                let k = args.first().cloned().unwrap_or(Value::Unit);
                dict.borrow_mut().remove(&k);
                stack.push(Value::Unit);
                Ok(true)
            }
            "keys" => {
                let keys = dict.borrow().keys();
                stack.push(Value::List(Rc::new(RefCell::new(keys))));
                Ok(true)
            }
            "values" => {
                let values = dict.borrow().values();
                stack.push(Value::List(Rc::new(RefCell::new(values))));
                Ok(true)
            }
            _ => Err(err(
                Msg::UnknownMethod {
                    ty: "Dict".into(),
                    method: method.into(),
                },
                pos,
            )),
        }
    }

    fn set_method(
        &mut self,
        stack: &mut Vec<Value>,
        recv: Value,
        method: &str,
        args: &[Value],
        pos: (usize, usize),
    ) -> Result<bool, VmError> {
        let Value::Set(set) = recv else {
            return Err(err(
                Msg::UnknownMethod {
                    ty: recv.type_tag().into(),
                    method: method.into(),
                },
                pos,
            ));
        };
        match method {
            "len" => {
                stack.push(Value::Int(set.borrow().len() as i64));
                Ok(true)
            }
            "add" => {
                let v = args.first().cloned().unwrap_or(Value::Unit);
                set.borrow_mut().add(v);
                stack.push(Value::Unit);
                Ok(true)
            }
            "contains" => {
                let v = args.first().cloned().unwrap_or(Value::Unit);
                stack.push(Value::Bool(set.borrow().contains(&v)));
                Ok(true)
            }
            "remove" => {
                let v = args.first().cloned().unwrap_or(Value::Unit);
                set.borrow_mut().remove(&v);
                stack.push(Value::Unit);
                Ok(true)
            }
            _ => Err(err(
                Msg::UnknownMethod {
                    ty: "Set".into(),
                    method: method.into(),
                },
                pos,
            )),
        }
    }

    fn tuple_method(
        &mut self,
        stack: &mut Vec<Value>,
        recv: Value,
        method: &str,
        _args: &[Value],
        pos: (usize, usize),
    ) -> Result<bool, VmError> {
        let Value::Tuple(items) = recv else {
            return Err(err(
                Msg::UnknownMethod {
                    ty: recv.type_tag().into(),
                    method: method.into(),
                },
                pos,
            ));
        };
        match method {
            "len" => {
                stack.push(Value::Int(items.len() as i64));
                Ok(true)
            }
            _ => Err(err(
                Msg::UnknownMethod {
                    ty: "tuple".into(),
                    method: method.into(),
                },
                pos,
            )),
        }
    }

    fn option_method(
        &mut self,
        stack: &mut Vec<Value>,
        recv: Value,
        method: &str,
        _args: &[Value],
        pos: (usize, usize),
    ) -> Result<bool, VmError> {
        match (&recv, method) {
            (Value::None, "is_none") => {
                stack.push(Value::Bool(true));
                Ok(true)
            }
            (Value::Some(_), "is_none") => {
                stack.push(Value::Bool(false));
                Ok(true)
            }
            (Value::Some(_), "is_some") => {
                stack.push(Value::Bool(true));
                Ok(true)
            }
            (Value::None, "is_some") => {
                stack.push(Value::Bool(false));
                Ok(true)
            }
            (Value::Some(v), "unwrap") => {
                stack.push((**v).clone());
                Ok(true)
            }
            (Value::None, "unwrap") => Err(err(Msg::UnwrapNone, pos)),
            _ => Err(err(
                Msg::UnknownMethod {
                    ty: "Option".into(),
                    method: method.into(),
                },
                pos,
            )),
        }
    }

    fn result_method(
        &mut self,
        stack: &mut Vec<Value>,
        recv: Value,
        method: &str,
        _args: &[Value],
        pos: (usize, usize),
    ) -> Result<bool, VmError> {
        match (&recv, method) {
            (Value::Ok(_), "is_ok") => {
                stack.push(Value::Bool(true));
                Ok(true)
            }
            (Value::Err(_), "is_ok") => {
                stack.push(Value::Bool(false));
                Ok(true)
            }
            (Value::Err(_), "is_err") => {
                stack.push(Value::Bool(true));
                Ok(true)
            }
            (Value::Ok(_), "is_err") => {
                stack.push(Value::Bool(false));
                Ok(true)
            }
            (Value::Ok(v), "unwrap") => {
                stack.push((**v).clone());
                Ok(true)
            }
            (Value::Err(v), "unwrap") => Err(err(Msg::UnwrapErr(v.display()), pos)),
            (Value::Err(v), "unwrap_err") => {
                stack.push((**v).clone());
                Ok(true)
            }
            (Value::Ok(_), "unwrap_err") => Err(err(Msg::BadCall("unwrap_err on Ok".into()), pos)),
            _ => Err(err(
                Msg::UnknownMethod {
                    ty: "Result".into(),
                    method: method.into(),
                },
                pos,
            )),
        }
    }

    /// Runs a standard-library builtin (see `compiler::std_builtin_id`).
    fn std_builtin(&mut self, task: &mut Task, id: u8) -> Result<(), VmError> {
        use std::time::{SystemTime, UNIX_EPOCH};
        let pop = |task: &mut Task| task.stack.pop().unwrap_or(Value::Unit);
        match id {
            1 => {
                // read_to_str(path) -> Result[str, str]
                let path = pop(task);
                let Value::Str(p) = path else {
                    return Err(err(
                        Msg::BadCall("read_to_str needs a str path".into()),
                        self.pos_at(task),
                    ));
                };
                let v = std::fs::read_to_string(p.as_ref()).map_err(|e| e.to_string());
                task.stack.push(match v {
                    Ok(s) => Value::Ok(Box::new(Value::Str(Rc::new(s)))),
                    Err(e) => Value::Err(Box::new(Value::Str(Rc::new(e)))),
                });
            }
            2 => {
                // write(path, content) -> Result[(), str]
                let content = pop(task);
                let path = pop(task);
                let Value::Str(p) = path else {
                    return Err(err(
                        Msg::BadCall("write needs a str path".into()),
                        self.pos_at(task),
                    ));
                };
                let v = match content {
                    Value::Str(c) => std::fs::write(p.as_ref(), c.as_ref()),
                    other => {
                        return Err(err(
                            Msg::BadCall(format!(
                                "write content must be a str, got {}",
                                other.type_tag()
                            )),
                            self.pos_at(task),
                        ))
                    }
                };
                task.stack.push(match v {
                    Ok(()) => Value::Ok(Box::new(Value::Unit)),
                    Err(e) => Value::Err(Box::new(Value::Str(Rc::new(e.to_string())))),
                });
            }
            3 => {
                // clock() -> int (ms since epoch)
                let ms = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                task.stack.push(Value::Int(ms));
            }
            4 => {
                // sleep(ms)
                let v = pop(task);
                let Some(ms) = v.as_int() else {
                    return Err(err(
                        Msg::BadCall("sleep needs an int".into()),
                        self.pos_at(task),
                    ));
                };
                std::thread::sleep(std::time::Duration::from_millis(
                    ms.to_i64_sat().max(0) as u64
                ));
                task.stack.push(Value::Unit);
            }
            5 => {
                // abs(int) -> int; abs(float) -> float
                let v = pop(task);
                if let Some(i) = v.as_int() {
                    task.stack.push(Value::from(i.abs()));
                    return Ok(());
                }
                task.stack.push(match v {
                    Value::Float(f) => Value::Float(f.abs()),
                    other => {
                        return Err(err(
                            Msg::BadCall(format!(
                                "abs needs int or float, got {}",
                                other.type_tag()
                            )),
                            self.pos_at(task),
                        ))
                    }
                });
            }
            6..=8 => {
                // floor / ceil / round(float) -> int
                let v = pop(task);
                let f = match v {
                    Value::Float(f) => f,
                    _ if v.is_int() => v.as_int().unwrap().to_f64(),
                    other => {
                        return Err(err(
                            Msg::BadCall(format!("math fn needs float, got {}", other.type_tag())),
                            self.pos_at(task),
                        ))
                    }
                };
                let r = match id {
                    6 => f.floor(),
                    7 => f.ceil(),
                    _ => f.round(),
                };
                // Exact conversion: a huge float becomes a `Big`, not an
                // `i64` saturating cast.
                task.stack.push(Value::from(IntVal::from_f64(r)));
            }
            9 => {
                // sqrt(float) -> float
                let v = pop(task);
                let f = match v {
                    Value::Float(f) => f,
                    _ if v.is_int() => v.as_int().unwrap().to_f64(),
                    other => {
                        return Err(err(
                            Msg::BadCall(format!("sqrt needs float, got {}", other.type_tag())),
                            self.pos_at(task),
                        ))
                    }
                };
                task.stack.push(Value::Float(f.sqrt()));
            }
            10 => {
                // pow(x, y) -> float
                let y = pop(task);
                let x = pop(task);
                let to_f = |v: Value| -> Result<f64, VmError> {
                    match v {
                        Value::Float(f) => Ok(f),
                        _ if v.is_int() => Ok(v.as_int().unwrap().to_f64()),
                        other => Err(err(
                            Msg::BadCall(format!("pow needs floats, got {}", other.type_tag())),
                            self.pos_at(task),
                        )),
                    }
                };
                task.stack.push(Value::Float(to_f(x)?.powf(to_f(y)?)));
            }
            11 => {
                // json_encode(v) -> str
                let v = pop(task);
                match value_to_json(&v) {
                    Some(j) => task.stack.push(Value::Str(Rc::new(j.to_string()))),
                    None => {
                        return Err(err(
                            Msg::BadCall("json_encode: value is not JSON-encodable".into()),
                            self.pos_at(task),
                        ))
                    }
                }
            }
            12 => {
                // json_decode(s) -> Result[Json, str]
                let s = pop(task);
                let Value::Str(s) = s else {
                    return Err(err(
                        Msg::BadCall("json_decode needs a str".into()),
                        self.pos_at(task),
                    ));
                };
                match serde_json::from_str::<serde_json::Value>(s.as_ref()) {
                    Ok(j) => task.stack.push(Value::Ok(Box::new(json_to_value(&j)))),
                    Err(e) => task
                        .stack
                        .push(Value::Err(Box::new(Value::Str(Rc::new(e.to_string()))))),
                }
            }
            13 => {
                // args() -> List[str]
                let items: Vec<Value> = self
                    .args
                    .iter()
                    .map(|a| Value::Str(Rc::new(a.clone())))
                    .collect();
                task.stack.push(Value::List(Rc::new(RefCell::new(items))));
            }
            14 => {
                // exit(code): terminate the process with the given code.
                let v = pop(task);
                let Some(code) = v.as_int() else {
                    return Err(err(
                        Msg::BadCall("exit needs an int".into()),
                        self.pos_at(task),
                    ));
                };
                std::process::exit(code.to_i64_sat() as i32);
            }
            _ => {
                return Err(err(Msg::InternalFnIndex, self.pos_at(task)));
            }
        }
        Ok(())
    }

    fn list_method(
        &mut self,
        stack: &mut Vec<Value>,
        recv: Value,
        method: &str,
        args: &[Value],
        pos: (usize, usize),
    ) -> Result<bool, VmError> {
        let Value::List(items) = recv else {
            return Err(err(
                Msg::UnknownMethod {
                    ty: recv.type_tag().into(),
                    method: method.into(),
                },
                pos,
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
            "contains" => {
                let v = args.first().cloned().unwrap_or(Value::Unit);
                stack.push(Value::Bool(items.borrow().contains(&v)));
                Ok(true)
            }
            "get" => {
                let Some(i) = args.first().unwrap_or(&Value::Int(0)).as_int() else {
                    return Err(err(Msg::IndexNotIntRt, pos));
                };
                stack.push(
                    items
                        .borrow()
                        .get(i.to_usize_sat())
                        .cloned()
                        .unwrap_or(Value::Unit),
                );
                Ok(true)
            }
            "set" => {
                let Some(i) = args.first().unwrap_or(&Value::Int(0)).as_int() else {
                    return Err(err(Msg::IndexNotIntRt, pos));
                };
                let i = i.to_usize_sat();
                let v = args.get(1).cloned().unwrap_or(Value::Unit);
                let mut items = items.borrow_mut();
                if i < items.len() {
                    items[i] = v;
                }
                stack.push(Value::Unit);
                Ok(true)
            }
            _ => Err(err(
                Msg::UnknownMethod {
                    ty: "List".into(),
                    method: method.into(),
                },
                pos,
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
        pos: (usize, usize),
    ) -> Result<bool, VmError> {
        let Value::Chan(chan) = recv else {
            return Err(err(
                Msg::UnknownMethod {
                    ty: recv.type_tag().into(),
                    method: method.into(),
                },
                pos,
            ));
        };
        match method {
            "send" => {
                let v = args.first().cloned().unwrap_or(Value::Unit);
                let mut c = chan.borrow_mut();
                if c.closed {
                    return Err(err(Msg::BadCall("send on closed channel".into()), pos));
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
                pos,
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

/// Converts a runtime value to a JSON value (None for non-JSON values).
fn value_to_json(v: &Value) -> Option<serde_json::Value> {
    use serde_json::Value as J;
    Some(match v {
        Value::Int(n) => J::Number((*n).into()),
        // `arbitrary_precision` keeps the full digit text.
        Value::Big(b) => J::Number(b.to_string().parse().ok()?),
        Value::Float(f) => J::from(*f),
        Value::Bool(b) => J::Bool(*b),
        Value::Str(s) => J::String(s.to_string()),
        Value::List(items) => {
            let arr: Vec<J> = items.borrow().iter().filter_map(value_to_json).collect();
            J::Array(arr)
        }
        Value::Dict(d) => {
            let mut map = serde_json::Map::new();
            for (k, val) in d.borrow().items.iter() {
                let Value::Str(key) = k else { return None };
                map.insert(key.to_string(), value_to_json(val)?);
            }
            J::Object(map)
        }
        Value::None => J::Null,
        _ => return None,
    })
}

/// Converts a parsed JSON value back into a runtime value.
fn json_to_value(j: &serde_json::Value) -> Value {
    match j {
        serde_json::Value::Null => Value::None,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else {
                // Integer-shaped text beyond `i64` keeps full precision as a
                // `Big`; anything else is a float.
                match IntVal::parse(&n.to_string()) {
                    Some(i) => Value::from(i),
                    None => Value::Float(n.as_f64().unwrap_or(0.0)),
                }
            }
        }
        serde_json::Value::String(s) => Value::Str(Rc::new(s.clone())),
        serde_json::Value::Array(items) => {
            let vals: Vec<Value> = items.iter().map(json_to_value).collect();
            Value::List(Rc::new(RefCell::new(vals)))
        }
        serde_json::Value::Object(map) => {
            let pairs: Vec<(Value, Value)> = map
                .iter()
                .map(|(k, v)| (Value::Str(Rc::new(k.clone())), json_to_value(v)))
                .collect();
            Value::Dict(Rc::new(RefCell::new(DictVal::from_items(pairs))))
        }
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Int(n) => *n != 0,
        // Canonical `Big` values are never zero.
        Value::Big(_) => true,
        Value::Float(f) => *f != 0.0,
        Value::Str(s) => !s.is_empty(),
        Value::List(items) => !items.borrow().is_empty(),
        Value::Dict(d) => !d.borrow().is_empty(),
        Value::Set(s) => !s.borrow().is_empty(),
        Value::None => false,
        Value::Some(_) | Value::Ok(_) | Value::Err(_) => true,
        Value::Struct(_) => true,
        _ => false,
    }
}

fn binary(op: BinOp, l: &Value, r: &Value) -> Result<Value, Msg> {
    use BinOp::*;
    match op {
        Add | Sub | Mul | Div | Mod => {
            if let (Some(a), Some(b)) = (l.as_int(), r.as_int()) {
                return match op {
                    Add => Ok(Value::from(a.add(&b))),
                    Sub => Ok(Value::from(a.sub(&b))),
                    Mul => Ok(Value::from(a.mul(&b))),
                    Div => {
                        if b.is_zero() {
                            return Err(Msg::DivByZero);
                        }
                        Ok(Value::from(a.div(&b)))
                    }
                    Mod => {
                        if b.is_zero() {
                            return Err(Msg::ModByZero);
                        }
                        Ok(Value::from(a.rem(&b)))
                    }
                    _ => unreachable!(),
                };
            }
            match (l, r) {
                (il, Value::Float(b)) if il.is_int() => {
                    float_op(op, il.as_int().unwrap().to_f64(), *b)
                }
                (Value::Float(a), ir) if ir.is_int() => {
                    float_op(op, *a, ir.as_int().unwrap().to_f64())
                }
                (Value::Float(a), Value::Float(b)) => float_op(op, *a, *b),
                (Value::Str(a), Value::Str(b)) if op == Add => {
                    let mut s = String::with_capacity(a.len() + b.len());
                    s.push_str(a);
                    s.push_str(b);
                    Ok(Value::Str(s.into()))
                }
                _ => Err(Msg::TypeMismatch),
            }
        }
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
    use std::cmp::Ordering;
    use BinOp::*;
    if let (Value::Int(a), Value::Int(b)) = (l, r) {
        return Ok(Value::Bool(match op {
            Eq => a == b,
            Ne => a != b,
            Lt => a < b,
            Le => a <= b,
            Gt => a > b,
            Ge => a >= b,
            _ => unreachable!(),
        }));
    }
    if let (Some(a), Some(b)) = (l.as_int(), r.as_int()) {
        let ord = a.cmp(&b);
        return Ok(Value::Bool(match op {
            Eq => ord == Ordering::Equal,
            Ne => ord != Ordering::Equal,
            Lt => ord == Ordering::Less,
            Le => ord != Ordering::Greater,
            Gt => ord == Ordering::Greater,
            Ge => ord != Ordering::Less,
            _ => unreachable!(),
        }));
    }
    let result = match (l, r) {
        (il, Value::Float(b)) if il.is_int() => num_cmp(op, il.as_int().unwrap().to_f64(), *b),
        (Value::Float(a), ir) if ir.is_int() => num_cmp(op, *a, ir.as_int().unwrap().to_f64()),
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
        (Value::None, Value::None) => match op {
            Eq => true,
            Ne => false,
            _ => return Err(Msg::CmpMismatch),
        },
        // `Json` values are native values at runtime; comparing any of
        // them with `None` is supported (GOALS D13: Json 与 None 可比较).
        (Value::None, _) | (_, Value::None) => match op {
            Eq => false,
            Ne => true,
            _ => return Err(Msg::CmpMismatch),
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
