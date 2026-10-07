//! Compiled-program types: the `Instr` bytecode set, `BinOp`, `Function`,
//! `CompiledProgram`, and the `VmError` diagnostic wrapper.

use num_bigint::BigInt;
use sole_diag::{Diagnostic, Lang, Msg};
use std::rc::Rc;

/// Bytecode instruction set.
#[derive(Debug, Clone)]
pub enum Instr {
    Halt,
    PushInt(i64),
    /// Big integer literal; index into `CompiledProgram::bigints`.
    PushBig(u32),
    PushFloat(f64),
    PushBool(bool),
    PushStr(u32),
    PushUnit,
    PushVar(u32),
    PushVarCell(u32),
    PushGlobal(u32),
    StoreVar(u32),
    StoreGlobal(u32),
    /// `BorrowVar(mutable, idx)` Ref/MutRef of the variable's cell.
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
    CallMethod(u32, u32),
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
    Neg,
    Binary(BinOp),
    /// Stops the current frame's function (implicit return at block end).
    RetUnit,
    PushNone,
    PushSome,
    PushOk,
    PushErr,
    MakeDict(u32),
    MakeSet(u32),
    MakeTuple(u32),
    Assert,
    /// Standard-library builtin by id (compiler::std_builtin_id).
    Builtin(u8),
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
    /// Source position per instruction (`code[i]` was emitted from
    /// `(line, column)` `spans[i]`); used to locate runtime errors.
    pub spans: Vec<(u32, u32)>,
}

/// Compiled program: functions, globals, string table, method table.
#[derive(Debug, Clone)]
pub struct CompiledProgram {
    pub functions: Vec<Function>,
    pub globals: Vec<String>,
    pub strings: Vec<Rc<String>>,
    /// Big integer literals (deduplicated at compile time).
    pub bigints: Vec<Rc<BigInt>>,
    /// (struct type name, method name) function index.
    pub methods: Vec<((String, String), usize)>,
    /// struct name field names (construction order).
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

pub(crate) fn err(msg: Msg, pos: (usize, usize)) -> VmError {
    VmError {
        diag: Diagnostic::new(msg, pos.0, pos.1),
    }
}
