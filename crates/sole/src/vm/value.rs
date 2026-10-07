//! Runtime value types: `Value` (16 bytes), its collections
//! (`DictVal`/`SetVal`/`Channel`), and the `IntVal` arbitrary-precision
//! integer arithmetic used by the VM's numeric paths.

use num_bigint::BigInt;
use num_traits::{FromPrimitive, Signed, ToPrimitive};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

#[derive(Debug, Clone, PartialEq)]
pub struct StructVal {
    pub name: String,
    pub fields: Vec<(String, Value)>,
}

/// A `range(a, b)` value (boxed: `Value` stays 16 bytes).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RangeVal {
    pub start: i64,
    pub end: i64,
}

/// Arbitrary-precision integer view: an `i64` fast path plus a `BigInt`
/// for out-of-range values. `Value` stores these as `Int(i64)` /
/// `Big(Rc<BigInt>)` — `IntVal` only materializes inside arithmetic, so
/// `Value` stays 16 bytes. `Big` is canonical: it never holds a value
/// that fits in `i64`, so equality and hashing stay consistent.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum IntVal {
    Small(i64),
    Big(Rc<BigInt>),
}

impl IntVal {
    /// Canonical constructor: demotes to `Small` when the value fits.
    pub(crate) fn from_big(b: BigInt) -> Self {
        match b.to_i64() {
            Some(n) => Self::Small(n),
            None => Self::Big(Rc::new(b)),
        }
    }

    /// Exact integer view of a float result (`floor`, `ceil`, `round`).
    pub(crate) fn from_f64(f: f64) -> Self {
        BigInt::from_f64(f).map_or(Self::Small(0), Self::from_big)
    }

    /// Parses decimal text; returns `None` for non-numeric input.
    pub(crate) fn parse(s: &str) -> Option<Self> {
        if let Ok(n) = s.parse::<i64>() {
            Some(Self::Small(n))
        } else {
            BigInt::parse_bytes(s.as_bytes(), 10).map(Self::from_big)
        }
    }

    /// `i64` view, saturating at the extremes. Used where an `i64`
    /// parameter is required (indexes, ranges, channel capacities): a
    /// huge value is always out of range anyway.
    pub(crate) fn to_i64_sat(&self) -> i64 {
        match self {
            Self::Small(n) => *n,
            Self::Big(b) => match b.sign() {
                num_bigint::Sign::Minus => i64::MIN,
                _ => i64::MAX,
            },
        }
    }

    pub(crate) fn to_usize_sat(&self) -> usize {
        usize::try_from(self.to_i64_sat()).unwrap_or(0)
    }

    pub(crate) fn to_f64(&self) -> f64 {
        match self {
            Self::Small(n) => *n as f64,
            Self::Big(b) => b.to_f64().unwrap_or(f64::INFINITY),
        }
    }

    pub(crate) fn is_zero(&self) -> bool {
        matches!(self, Self::Small(0))
    }

    pub(crate) fn to_big(&self) -> BigInt {
        match self {
            Self::Small(n) => BigInt::from(*n),
            Self::Big(b) => b.as_ref().clone(),
        }
    }

    pub(crate) fn binop(&self, other: &Self, f: impl Fn(&BigInt, &BigInt) -> BigInt) -> Self {
        Self::from_big(f(&self.to_big(), &other.to_big()))
    }

    pub(crate) fn add(&self, o: &Self) -> Self {
        match (self, o) {
            (Self::Small(a), Self::Small(b)) => a
                .checked_add(*b)
                .map_or_else(|| self.binop(o, |x, y| x + y), Self::Small),
            _ => self.binop(o, |x, y| x + y),
        }
    }

    pub(crate) fn sub(&self, o: &Self) -> Self {
        match (self, o) {
            (Self::Small(a), Self::Small(b)) => a
                .checked_sub(*b)
                .map_or_else(|| self.binop(o, |x, y| x - y), Self::Small),
            _ => self.binop(o, |x, y| x - y),
        }
    }

    pub(crate) fn mul(&self, o: &Self) -> Self {
        match (self, o) {
            (Self::Small(a), Self::Small(b)) => a
                .checked_mul(*b)
                .map_or_else(|| self.binop(o, |x, y| x * y), Self::Small),
            _ => self.binop(o, |x, y| x * y),
        }
    }

    /// Precondition: `o` is non-zero (callers report `DivByZero`).
    /// `i64::MIN / -1` overflows `i64` and promotes like the other ops.
    pub(crate) fn div(&self, o: &Self) -> Self {
        match (self, o) {
            (Self::Small(a), Self::Small(b)) => a
                .checked_div(*b)
                .map_or_else(|| self.binop(o, |x, y| x / y), Self::Small),
            _ => self.binop(o, |x, y| x / y),
        }
    }

    /// Precondition: `o` is non-zero (callers report `ModByZero`).
    pub(crate) fn rem(&self, o: &Self) -> Self {
        match (self, o) {
            (Self::Small(a), Self::Small(b)) => a
                .checked_rem(*b)
                .map_or_else(|| self.binop(o, |x, y| x % y), Self::Small),
            _ => self.binop(o, |x, y| x % y),
        }
    }

    pub(crate) fn neg(&self) -> Self {
        match self {
            Self::Small(n) => n
                .checked_neg()
                .map_or_else(|| Self::from_big(-BigInt::from(*n)), Self::Small),
            Self::Big(b) => Self::from_big(-b.as_ref()),
        }
    }

    pub(crate) fn abs(&self) -> Self {
        match self {
            Self::Small(n) => n
                .checked_abs()
                .map_or_else(|| Self::from_big(BigInt::from(*n).abs()), Self::Small),
            Self::Big(b) => Self::Big(Rc::new(b.as_ref().abs())),
        }
    }

    pub(crate) fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        match (self, o) {
            (Self::Small(a), Self::Small(b)) => a.cmp(b),
            (Self::Small(a), Self::Big(b)) => BigInt::from(*a).cmp(b),
            (Self::Big(a), Self::Small(b)) => a.as_ref().cmp(&BigInt::from(*b)),
            (Self::Big(a), Self::Big(b)) => a.cmp(b),
        }
    }
}

impl std::fmt::Display for IntVal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Small(n) => write!(f, "{}", n),
            Self::Big(b) => write!(f, "{}", b),
        }
    }
}

/// Runtime value. Sized so the common variants stay small (16 bytes total):
/// `str` is reference-counted (immutable, shared), structs and ranges are
/// boxed behind pointers, and big integers live behind `Big` so `Int`
/// remains a plain `i64`.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// Small integer fast path.
    Int(i64),
    /// Arbitrary-precision integer; canonical — never holds a value that
    /// fits in `i64` (produced only through `Value::from(IntVal)`).
    Big(Rc<BigInt>),
    Float(f64),
    Bool(bool),
    /// `Rc<String>` not `Rc<str>`: `Rc<str>` is a fat pointer (16 B) and
    /// would make `Value` 24 bytes.
    Str(Rc<String>),
    Range(Rc<RangeVal>),
    List(Rc<RefCell<Vec<Value>>>),
    Struct(Box<StructVal>),
    Chan(Rc<RefCell<Channel>>),
    None,
    Some(Box<Value>),
    Ok(Box<Value>),
    Err(Box<Value>),
    Dict(Rc<RefCell<DictVal>>),
    Set(Rc<RefCell<SetVal>>),
    Tuple(Rc<Vec<Value>>),
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

/// Dict payload: insertion-ordered pairs plus a hash index over the keys.
/// The index is derived state — equality is defined by `items` only.
#[derive(Debug, Clone)]
pub struct DictVal {
    pub items: Vec<(Value, Value)>,
    index: HashMap<Value, usize>,
}

impl DictVal {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            index: HashMap::new(),
        }
    }

    /// Builds a dict from literal pairs; first occurrence of a duplicated
    /// key wins (same rule as the previous linear representation).
    pub fn from_items(items: Vec<(Value, Value)>) -> Self {
        let mut d = Self::new();
        for (k, v) in items {
            if !d.index.contains_key(&k) {
                d.index.insert(k.clone(), d.items.len());
                d.items.push((k, v));
            }
        }
        d
    }

    fn rebuild(&mut self) {
        self.index = self
            .items
            .iter()
            .enumerate()
            .map(|(i, (k, _))| (k.clone(), i))
            .collect();
    }

    pub fn get(&self, k: &Value) -> Option<Value> {
        self.index.get(k).map(|&i| self.items[i].1.clone())
    }

    pub fn set(&mut self, k: Value, v: Value) {
        if let Some(&i) = self.index.get(&k) {
            self.items[i].1 = v;
        } else {
            self.index.insert(k.clone(), self.items.len());
            self.items.push((k, v));
        }
    }

    pub fn contains(&self, k: &Value) -> bool {
        self.index.contains_key(k)
    }

    pub fn remove(&mut self, k: &Value) {
        if self.index.remove(k).is_some() {
            self.items.retain(|(key, _)| key != k);
            self.rebuild();
        }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn keys(&self) -> Vec<Value> {
        self.items.iter().map(|(k, _)| k.clone()).collect()
    }

    pub fn values(&self) -> Vec<Value> {
        self.items.iter().map(|(_, v)| v.clone()).collect()
    }
}

impl Default for DictVal {
    fn default() -> Self {
        Self::new()
    }
}

impl PartialEq for DictVal {
    fn eq(&self, other: &Self) -> bool {
        self.items == other.items
    }
}

/// Set payload: insertion-ordered elements plus a hash index.
#[derive(Debug, Clone)]
pub struct SetVal {
    pub items: Vec<Value>,
    index: HashMap<Value, usize>,
}

impl SetVal {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            index: HashMap::new(),
        }
    }

    pub fn from_items(items: Vec<Value>) -> Self {
        let mut s = Self::new();
        for v in items {
            s.add(v);
        }
        s
    }

    fn rebuild(&mut self) {
        self.index = self
            .items
            .iter()
            .enumerate()
            .map(|(i, v)| (v.clone(), i))
            .collect();
    }

    pub fn add(&mut self, v: Value) {
        if !self.index.contains_key(&v) {
            self.index.insert(v.clone(), self.items.len());
            self.items.push(v);
        }
    }

    pub fn contains(&self, v: &Value) -> bool {
        self.index.contains_key(v)
    }

    pub fn remove(&mut self, v: &Value) {
        if self.index.remove(v).is_some() {
            self.items.retain(|x| x != v);
            self.rebuild();
        }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

impl Default for SetVal {
    fn default() -> Self {
        Self::new()
    }
}

impl PartialEq for SetVal {
    fn eq(&self, other: &Self) -> bool {
        self.items == other.items
    }
}

/// Hash consistent with `PartialEq`: equal values hash equal. Composite
/// variants hash their contents; a cyclic value would recurse — the same
/// failure mode as `==` today.
impl std::hash::Hash for Value {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            Value::Int(n) => n.hash(state),
            Value::Big(b) => b.hash(state),
            // `-0.0 == 0.0` must hash equal; NaN never equals itself, so its
            // bit pattern is unconstrained.
            Value::Float(f) => {
                let f = if *f == 0.0 { 0.0 } else { *f };
                f.to_bits().hash(state);
            }
            Value::Bool(b) => b.hash(state),
            Value::Str(s) => s.hash(state),
            Value::Range(r) => {
                r.start.hash(state);
                r.end.hash(state);
            }
            Value::List(xs) => xs.borrow().hash(state),
            Value::Tuple(xs) => xs.hash(state),
            Value::Dict(d) => d.borrow().items.hash(state),
            Value::Set(s) => s.borrow().items.hash(state),
            Value::Struct(sv) => {
                sv.name.hash(state);
                for (_, v) in &sv.fields {
                    v.hash(state);
                }
            }
            Value::Some(v) | Value::Ok(v) | Value::Err(v) => v.hash(state),
            Value::Ref(c) | Value::MutRef(c) => c.borrow().hash(state),
            _ => {}
        }
    }
}

impl Eq for Value {}

impl From<IntVal> for Value {
    fn from(i: IntVal) -> Value {
        match i {
            IntVal::Small(n) => Value::Int(n),
            IntVal::Big(b) => Value::Big(b),
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

    /// Integer view of `Int`/`Big` values; `None` for other types.
    pub(crate) fn as_int(&self) -> Option<IntVal> {
        match self {
            Value::Int(n) => Some(IntVal::Small(*n)),
            Value::Big(b) => Some(IntVal::Big(b.clone())),
            _ => None,
        }
    }

    pub(crate) fn is_int(&self) -> bool {
        matches!(self, Value::Int(_) | Value::Big(_))
    }

    pub fn type_tag(&self) -> &'static str {
        match self {
            Value::Int(_) | Value::Big(_) => "int",
            Value::Float(_) => "float",
            Value::Bool(_) => "bool",
            Value::Str(_) => "str",
            Value::Range(_) => "Range",
            Value::List(_) => "List",
            Value::Struct(_) => "struct",
            Value::Chan(_) => "Chan",
            Value::None | Value::Some(_) => "Option",
            Value::Ok(_) | Value::Err(_) => "Result",
            Value::Dict(_) => "Dict",
            Value::Set(_) => "Set",
            Value::Tuple(_) => "tuple",
            Value::Ref(_) | Value::MutRef(_) => "ref",
            Value::Fn(_) => "fn",
            Value::Unit => "()",
        }
    }
}
