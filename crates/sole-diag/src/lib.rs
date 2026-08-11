//! Diagnostics for the Sole toolchain.
//!
//! Errors are *data*: a stable error code plus parameters. Language (English
//! by default, Chinese optional) is only a rendering concern. This matches
//! GOALS §9.2: structured, AI-parseable error messages.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    Zh,
}

impl Lang {
    pub fn parse(s: &str) -> Option<Lang> {
        match s {
            "en" | "en_US" | "en-US" => Some(Lang::En),
            "zh" | "zh_CN" | "zh-CN" => Some(Lang::Zh),
            _ => None,
        }
    }

    pub fn from_env() -> Lang {
        std::env::var("SOLE_LANG")
            .ok()
            .and_then(|v| Lang::parse(&v))
            .unwrap_or(Lang::En)
    }

    /// The language currently in effect: CLI override > env var > English.
    pub fn current() -> Lang {
        match OVERRIDE.get() {
            Some(Some(lang)) => *lang,
            Some(None) => Lang::En,
            None => Lang::from_env(),
        }
    }
}

static OVERRIDE: std::sync::OnceLock<Option<Lang>> = std::sync::OnceLock::new();

/// Overrides the effective language (used by the CLI `--lang` flag).
pub fn set_override(lang: Option<Lang>) {
    let _ = OVERRIDE.set(lang);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentKind {
    FnName,
    VarName,
    ParamName,
    TypeName,
    FieldName,
    LoopVar,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Msg {
    // Lexer (E00xx)
    TabNotAllowed,
    TabAtLineStart,
    BadIndent,
    UnterminatedString,
    IncompleteEscape,
    UnknownEscape(String),
    StringAcrossLines,
    InvalidUtf8,
    BadFloat,
    BadInt,
    UnexpectedBang,
    UnknownChar(char),
    // Parser (E01xx)
    ExpectedToken(String),
    ExpectedIdent(IdentKind),
    ExpectedNewline,
    ExpectedIndent,
    ExpectedExpr,
    BlockNotClosed,
    // Evaluator (E02xx)
    UndefinedVariable(String),
    ImmutableReassign(String),
    NotNegatable,
    TypeMismatch,
    DivByZero,
    ModByZero,
    BadCall(String),
    FieldNotImplemented,
    IndexNotImplemented,
    ForNotSupported(String),
    ArgCount(String, usize, usize),
    RangeNotInt,
    RangeArgCount,
    BadCondition,
    BoolOrderCmp,
    CmpMismatch,
    InternalFnIndex,
    Io(String),
}

impl Msg {
    pub fn code(&self) -> &'static str {
        match self {
            Msg::TabNotAllowed => "E0001",
            Msg::TabAtLineStart => "E0002",
            Msg::BadIndent => "E0003",
            Msg::UnterminatedString => "E0004",
            Msg::IncompleteEscape => "E0005",
            Msg::UnknownEscape(_) => "E0006",
            Msg::StringAcrossLines => "E0007",
            Msg::InvalidUtf8 => "E0008",
            Msg::BadFloat => "E0009",
            Msg::BadInt => "E0010",
            Msg::UnexpectedBang => "E0011",
            Msg::UnknownChar(_) => "E0012",
            Msg::ExpectedToken(_) => "E0101",
            Msg::ExpectedIdent(_) => "E0102",
            Msg::ExpectedNewline => "E0103",
            Msg::ExpectedIndent => "E0104",
            Msg::ExpectedExpr => "E0105",
            Msg::BlockNotClosed => "E0106",
            Msg::UndefinedVariable(_) => "E0201",
            Msg::ImmutableReassign(_) => "E0202",
            Msg::NotNegatable => "E0203",
            Msg::TypeMismatch => "E0204",
            Msg::DivByZero => "E0205",
            Msg::ModByZero => "E0206",
            Msg::BadCall(_) => "E0207",
            Msg::FieldNotImplemented => "E0208",
            Msg::IndexNotImplemented => "E0209",
            Msg::ForNotSupported(_) => "E0210",
            Msg::ArgCount(..) => "E0211",
            Msg::RangeNotInt => "E0212",
            Msg::RangeArgCount => "E0213",
            Msg::BadCondition => "E0214",
            Msg::BoolOrderCmp => "E0215",
            Msg::CmpMismatch => "E0216",
            Msg::InternalFnIndex => "E0217",
            Msg::Io(_) => "E0218",
        }
    }

    pub fn render(&self, lang: Lang) -> String {
        match lang {
            Lang::En => self.render_en(),
            Lang::Zh => self.render_zh(),
        }
    }

    fn render_en(&self) -> String {
        match self {
            Msg::TabNotAllowed => "tab characters are not allowed (GOALS D1)".into(),
            Msg::TabAtLineStart => {
                "tab at line start: mixing tabs and spaces is a compile error (GOALS D1)".into()
            }
            Msg::BadIndent => "indentation does not match any outer level".into(),
            Msg::UnterminatedString => "unterminated string literal".into(),
            Msg::IncompleteEscape => "incomplete escape sequence at end of string".into(),
            Msg::UnknownEscape(esc) => format!("unknown escape sequence `\\{}`", esc),
            Msg::StringAcrossLines => "string literal cannot span lines".into(),
            Msg::InvalidUtf8 => "string is not valid UTF-8".into(),
            Msg::BadFloat => "invalid float literal".into(),
            Msg::BadInt => "invalid integer literal (or out of i64 range; arbitrary-precision \
                 integers are not implemented yet)"
                .into(),
            Msg::UnexpectedBang => "unexpected `!`".into(),
            Msg::UnknownChar(c) => format!("unrecognized character `{}`", c),
            Msg::ExpectedToken(t) => format!("expected `{}`", t),
            Msg::ExpectedIdent(kind) => match kind {
                IdentKind::FnName => "expected function name".into(),
                IdentKind::VarName => "expected variable name".into(),
                IdentKind::ParamName => "expected parameter name".into(),
                IdentKind::TypeName => "expected type name".into(),
                IdentKind::FieldName => "expected field name".into(),
                IdentKind::LoopVar => "expected loop variable".into(),
            },
            Msg::ExpectedNewline => "expected end of statement (newline)".into(),
            Msg::ExpectedIndent => "expected an indented block".into(),
            Msg::ExpectedExpr => "expected expression".into(),
            Msg::BlockNotClosed => "unclosed block: expected DEDENT".into(),
            Msg::UndefinedVariable(name) => format!("undefined variable `{}`", name),
            Msg::ImmutableReassign(name) => format!(
                "cannot reassign immutable binding `{}` (declare it as `let mut`)",
                name
            ),
            Msg::NotNegatable => "unary `-` only supports numbers".into(),
            Msg::TypeMismatch => "operator type mismatch".into(),
            Msg::DivByZero => "integer division by zero".into(),
            Msg::ModByZero => "integer modulo by zero".into(),
            Msg::BadCall(v) => format!("cannot call {}", v),
            Msg::FieldNotImplemented => "field access is not implemented yet (M1)".into(),
            Msg::IndexNotImplemented => "index access is not implemented yet (M1)".into(),
            Msg::ForNotSupported(v) => format!(
                "`for` currently only supports Range (collections/channels: GOALS D6/§7); \
                 got {}",
                v
            ),
            Msg::ArgCount(name, expected, actual) => format!(
                "function `{}` expects {} arguments, got {}",
                name, expected, actual
            ),
            Msg::RangeNotInt => "`range` arguments must be integers".into(),
            Msg::RangeArgCount => "`range` expects 1 or 2 arguments".into(),
            Msg::BadCondition => "this value cannot be used as a condition".into(),
            Msg::BoolOrderCmp => "booleans only support equality comparison".into(),
            Msg::CmpMismatch => "comparison type mismatch".into(),
            Msg::InternalFnIndex => "internal error: invalid function index".into(),
            Msg::Io(e) => format!("I/O error: {}", e),
        }
    }

    fn render_zh(&self) -> String {
        match self {
            Msg::TabNotAllowed => "tab 字符不允许(混用 Tab/空格是编译错误,见 GOALS D1)".into(),
            Msg::TabAtLineStart => "行首出现 tab: 混用 Tab/空格是编译错误 (GOALS D1)".into(),
            Msg::BadIndent => "缩进层级不匹配".into(),
            Msg::UnterminatedString => "未闭合的字符串字面量".into(),
            Msg::IncompleteEscape => "字符串末尾的转义不完整".into(),
            Msg::UnknownEscape(esc) => format!("未知转义序列 \\{}", esc),
            Msg::StringAcrossLines => "字符串不能跨行(多行字符串未实现)".into(),
            Msg::InvalidUtf8 => "字符串不是合法 UTF-8".into(),
            Msg::BadFloat => "无效的浮点数字面量".into(),
            Msg::BadInt => "无效的整数面量(或超出 i64 范围;任意精度整数未实现)".into(),
            Msg::UnexpectedBang => "意外的 `!`".into(),
            Msg::UnknownChar(c) => format!("无法识别的字符 `{}`", c),
            Msg::ExpectedToken(t) => format!("期望 `{}`", t),
            Msg::ExpectedIdent(kind) => match kind {
                IdentKind::FnName => "期望函数名".into(),
                IdentKind::VarName => "期望变量名".into(),
                IdentKind::ParamName => "期望参数名".into(),
                IdentKind::TypeName => "期望类型名".into(),
                IdentKind::FieldName => "期望字段名".into(),
                IdentKind::LoopVar => "期望循环变量".into(),
            },
            Msg::ExpectedNewline => "期望语句结束(换行)".into(),
            Msg::ExpectedIndent => "期望缩进块".into(),
            Msg::ExpectedExpr => "期望表达式".into(),
            Msg::BlockNotClosed => "块未闭合: 期望 DEDENT".into(),
            Msg::UndefinedVariable(name) => format!("未定义变量 `{}`", name),
            Msg::ImmutableReassign(name) => {
                format!("不可变绑定 `{}` 不能重新赋值(声明为 `let mut`)", name)
            }
            Msg::NotNegatable => "一元 `-` 仅支持数值".into(),
            Msg::TypeMismatch => "运算符类型不匹配".into(),
            Msg::DivByZero => "整数除以零".into(),
            Msg::ModByZero => "整数取模除零".into(),
            Msg::BadCall(v) => format!("无法调用 {}", v),
            Msg::FieldNotImplemented => "字段访问尚未实现 (M1)".into(),
            Msg::IndexNotImplemented => "下标访问尚未实现 (M1)".into(),
            Msg::ForNotSupported(v) => {
                format!("`for` 暂仅支持 Range(集合/通道见 GOALS D6/§7);得到 {}", v)
            }
            Msg::ArgCount(name, expected, actual) => format!(
                "函数 `{}` 期望 {} 个参数,实际 {} 个",
                name, expected, actual
            ),
            Msg::RangeNotInt => "`range` 参数必须是整数".into(),
            Msg::RangeArgCount => "`range` 期望 1 或 2 个参数".into(),
            Msg::BadCondition => "该值不能用作条件".into(),
            Msg::BoolOrderCmp => "布尔值仅支持相等比较".into(),
            Msg::CmpMismatch => "比较运算符类型不匹配".into(),
            Msg::InternalFnIndex => "内部错误: 函数索引无效".into(),
            Msg::Io(e) => format!("I/O 错误: {}", e),
        }
    }
}

/// A structured error: stable code + parameters + source position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: &'static str,
    pub msg: Msg,
    pub line: usize,
    pub column: usize,
}

impl Diagnostic {
    pub fn new(msg: Msg, line: usize, column: usize) -> Self {
        Self {
            code: msg.code(),
            msg,
            line,
            column,
        }
    }

    /// Renders in the given language. Format:
    /// `line:col: [CODE] message` (position omitted when line == 0).
    pub fn render(&self, lang: Lang) -> String {
        if self.line == 0 {
            format!("[{}] {}", self.code, self.msg.render(lang))
        } else {
            format!(
                "{}:{}: [{}] {}",
                self.line,
                self.column,
                self.code,
                self.msg.render(lang)
            )
        }
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.render(Lang::current()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_codes() {
        assert_eq!(Msg::UndefinedVariable("x".into()).code(), "E0201");
        assert_eq!(Msg::TabNotAllowed.code(), "E0001");
        assert_eq!(Msg::BlockNotClosed.code(), "E0106");
    }

    #[test]
    fn bilingual_rendering() {
        let d = Diagnostic::new(Msg::UndefinedVariable("foo".into()), 3, 5);
        assert_eq!(d.render(Lang::En), "3:5: [E0201] undefined variable `foo`");
        assert_eq!(d.render(Lang::Zh), "3:5: [E0201] 未定义变量 `foo`");
    }

    #[test]
    fn no_position_for_line_zero() {
        let d = Diagnostic::new(Msg::DivByZero, 0, 0);
        assert_eq!(d.render(Lang::En), "[E0205] integer division by zero");
    }

    #[test]
    fn lang_parsing() {
        assert_eq!(Lang::parse("en").unwrap(), Lang::En);
        assert_eq!(Lang::parse("zh").unwrap(), Lang::Zh);
        assert_eq!(Lang::parse("fr"), None);
    }
}
