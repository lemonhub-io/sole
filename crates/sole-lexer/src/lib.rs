//! Token definitions and a hand-rolled lexer for the Sole language.
//!
//! M1 scope: keywords, identifiers, numbers (with `_` separators), strings,
//! operators, and Python-style INDENT/DEDENT handling. Tabs are rejected
//! anywhere in the source (GOALS D1: mixing tabs and spaces is a compile
//! error).

use sole_diag::{Diagnostic, Lang, Msg};
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // Keywords
    Let,
    Mut,
    Fn,
    If,
    Else,
    While,
    For,
    In,
    Return,
    True,
    False,
    Ref,
    Break,
    Continue,
    Struct,
    Interface,
    Impl,
    Test,
    With,
    Yield,
    TaskGroup,
    Go,
    And,
    Or,
    Not,
    Assert,
    // Layout tokens
    Newline,
    Indent,
    Dedent,
    Eof,
    // Literals & identifiers
    Ident(String),
    Int(i64),
    Float(f64),
    Str(String),
    // Operators & punctuation
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    EqEq,
    BangEq,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Arrow,
    Colon,
    Comma,
    Dot,
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub line: usize,
    pub column: usize,
}

impl Token {
    pub fn new(kind: TokenKind, line: usize, column: usize) -> Self {
        Self { kind, line, column }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LexError {
    pub diag: Diagnostic,
}

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.diag.render(Lang::current()))
    }
}

impl std::error::Error for LexError {}

/// Lexes a whole source file into a token stream ending with `Eof`.
pub fn lex(source: &str) -> Result<Vec<Token>, LexError> {
    Lexer::new(source).run()
}

struct Lexer<'a> {
    src: &'a [u8],
    pos: usize,
    line: usize,
    col: usize,
    tokens: Vec<Token>,
    indent_stack: Vec<usize>,
    paren_depth: usize,
    at_line_start: bool,
}

impl<'a> Lexer<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            src: source.as_bytes(),
            pos: 0,
            line: 1,
            col: 1,
            tokens: Vec::new(),
            indent_stack: vec![0],
            paren_depth: 0,
            at_line_start: true,
        }
    }

    fn run(mut self) -> Result<Vec<Token>, LexError> {
        while self.pos < self.src.len() {
            if self.at_line_start {
                self.handle_line_start()?;
            }
            match self.peek() {
                Some(b'\n') => {
                    let (sl, sc) = (self.line, self.col);
                    self.advance();
                    if self.paren_depth == 0 {
                        self.push(TokenKind::Newline, sl, sc);
                        self.at_line_start = true;
                    }
                }
                Some(b'\r') => {
                    self.advance();
                    if self.peek() != Some(b'\n') {
                        return Err(self.error(Msg::UnknownChar('\r')));
                    }
                }
                Some(b' ') => {
                    self.advance();
                }
                Some(b'\t') => {
                    return Err(self.error(Msg::TabNotAllowed));
                }
                Some(b'#') => {
                    while self.pos < self.src.len() && self.src[self.pos] != b'\n' {
                        self.advance();
                    }
                }
                Some(b'"') => self.lex_string()?,
                Some(c) if c.is_ascii_digit() => self.lex_number()?,
                Some(c) if is_ident_start(c) => self.lex_ident(),
                Some(c) => {
                    self.lex_operator(c)?;
                }
                None => break,
            }
        }
        // Close any open indentation levels, then EOF.
        while self.indent_stack.len() > 1 {
            self.indent_stack.pop();
            self.push(TokenKind::Dedent, self.line, self.col);
        }
        self.push(TokenKind::Eof, self.line, self.col);
        Ok(self.tokens)
    }

    /// Handles the start of a new logical line: counts indentation, emits
    /// INDENT/DEDENT, and skips blank or comment-only lines silently.
    fn handle_line_start(&mut self) -> Result<(), LexError> {
        let mut indent;
        loop {
            indent = 0;
            while self.pos < self.src.len() {
                match self.src[self.pos] {
                    b' ' => {
                        indent += 1;
                        self.advance();
                    }
                    b'\t' => {
                        return Err(self.error(Msg::TabAtLineStart));
                    }
                    _ => break,
                }
            }
            // Blank or comment-only lines do not affect indentation.
            match self.peek() {
                Some(b'\n') | Some(b'#') => {
                    while self.pos < self.src.len() && self.src[self.pos] != b'\n' {
                        self.advance();
                    }
                    if self.pos < self.src.len() {
                        self.advance(); // consume the newline silently
                    }
                    continue;
                }
                None => return Ok(()),
                _ => {}
            }
            break;
        }
        let top = *self.indent_stack.last().expect("indent stack never empty");
        if indent > top {
            self.indent_stack.push(indent);
            self.push(TokenKind::Indent, self.line, self.col);
        } else if indent < top {
            loop {
                self.indent_stack.pop();
                self.push(TokenKind::Dedent, self.line, self.col);
                let current = *self.indent_stack.last().expect("indent stack never empty");
                if indent == current {
                    break;
                }
                if indent > current {
                    return Err(self.error(Msg::BadIndent));
                }
            }
        }
        self.at_line_start = false;
        Ok(())
    }

    fn lex_number(&mut self) -> Result<(), LexError> {
        let (sl, sc) = (self.line, self.col);
        let start = self.pos;
        let mut is_float = false;
        loop {
            match self.peek() {
                Some(b'0'..=b'9') | Some(b'_') => {
                    self.advance();
                }
                Some(b'.') if self.peek2().is_some_and(|n| n.is_ascii_digit()) => {
                    is_float = true;
                    self.advance();
                }
                _ => break,
            }
        }
        let text = std::str::from_utf8(&self.src[start..self.pos])
            .expect("number text is ASCII")
            .replace('_', "");
        if is_float {
            let value = text.parse::<f64>().map_err(|_| self.error(Msg::BadFloat))?;
            self.push(TokenKind::Float(value), sl, sc);
        } else {
            let value = text.parse::<i64>().map_err(|_| self.error(Msg::BadInt))?;
            self.push(TokenKind::Int(value), sl, sc);
        }
        Ok(())
    }

    fn lex_ident(&mut self) {
        let (sl, sc) = (self.line, self.col);
        let start = self.pos;
        while let Some(&c) = self.src.get(self.pos) {
            if is_ident_continue(c) {
                self.advance();
            } else {
                break;
            }
        }
        let text = std::str::from_utf8(&self.src[start..self.pos]).expect("identifier is ASCII");
        let kind = match text {
            "let" => TokenKind::Let,
            "mut" => TokenKind::Mut,
            "fn" => TokenKind::Fn,
            "if" => TokenKind::If,
            "else" => TokenKind::Else,
            "while" => TokenKind::While,
            "for" => TokenKind::For,
            "in" => TokenKind::In,
            "return" => TokenKind::Return,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            "ref" => TokenKind::Ref,
            "break" => TokenKind::Break,
            "continue" => TokenKind::Continue,
            "struct" => TokenKind::Struct,
            "interface" => TokenKind::Interface,
            "impl" => TokenKind::Impl,
            "test" => TokenKind::Test,
            "assert" => TokenKind::Assert,
            "with" => TokenKind::With,
            "yield" => TokenKind::Yield,
            "task_group" => TokenKind::TaskGroup,
            "go" => TokenKind::Go,
            "and" => TokenKind::And,
            "or" => TokenKind::Or,
            "not" => TokenKind::Not,
            _ => TokenKind::Ident(text.to_string()),
        };
        self.push(kind, sl, sc);
    }

    fn lex_string(&mut self) -> Result<(), LexError> {
        let (sl, sc) = (self.line, self.col);
        self.advance(); // opening quote
        let mut buf = Vec::new();
        loop {
            let Some(&c) = self.src.get(self.pos) else {
                return Err(self.error(Msg::UnterminatedString));
            };
            match c {
                b'"' => {
                    self.advance();
                    break;
                }
                b'\\' => {
                    self.advance();
                    let Some(&esc) = self.src.get(self.pos) else {
                        return Err(self.error(Msg::IncompleteEscape));
                    };
                    let decoded = match esc {
                        b'n' => b'\n',
                        b't' => b'\t',
                        b'r' => b'\r',
                        b'0' => b'\0',
                        b'\\' => b'\\',
                        b'"' => b'"',
                        other => {
                            return Err(self.error(Msg::UnknownEscape((other as char).to_string())));
                        }
                    };
                    self.advance();
                    buf.push(decoded);
                }
                b'\n' => {
                    return Err(self.error(Msg::StringAcrossLines));
                }
                _ => {
                    buf.push(c);
                    self.advance();
                }
            }
        }
        let text = String::from_utf8(buf).map_err(|_| self.error(Msg::InvalidUtf8))?;
        self.push(TokenKind::Str(text), sl, sc);
        Ok(())
    }

    fn lex_operator(&mut self, c: u8) -> Result<(), LexError> {
        let (sl, sc) = (self.line, self.col);
        match c {
            b'+' => {
                self.advance();
                self.push(TokenKind::Plus, sl, sc);
            }
            b'-' => {
                self.advance();
                if self.peek() == Some(b'>') {
                    self.advance();
                    self.push(TokenKind::Arrow, sl, sc);
                } else {
                    self.push(TokenKind::Minus, sl, sc);
                }
            }
            b'*' => {
                self.advance();
                self.push(TokenKind::Star, sl, sc);
            }
            b'/' => {
                self.advance();
                self.push(TokenKind::Slash, sl, sc);
            }
            b'%' => {
                self.advance();
                self.push(TokenKind::Percent, sl, sc);
            }
            b'=' => {
                self.advance();
                if self.peek() == Some(b'=') {
                    self.advance();
                    self.push(TokenKind::EqEq, sl, sc);
                } else {
                    self.push(TokenKind::Eq, sl, sc);
                }
            }
            b'!' => {
                self.advance();
                if self.peek() == Some(b'=') {
                    self.advance();
                    self.push(TokenKind::BangEq, sl, sc);
                } else {
                    return Err(self.error(Msg::UnexpectedBang));
                }
            }
            b'<' => {
                self.advance();
                if self.peek() == Some(b'=') {
                    self.advance();
                    self.push(TokenKind::Le, sl, sc);
                } else {
                    self.push(TokenKind::Lt, sl, sc);
                }
            }
            b'>' => {
                self.advance();
                if self.peek() == Some(b'=') {
                    self.advance();
                    self.push(TokenKind::Ge, sl, sc);
                } else {
                    self.push(TokenKind::Gt, sl, sc);
                }
            }
            b':' => {
                self.advance();
                self.push(TokenKind::Colon, sl, sc);
            }
            b',' => {
                self.advance();
                self.push(TokenKind::Comma, sl, sc);
            }
            b'.' => {
                self.advance();
                self.push(TokenKind::Dot, sl, sc);
            }
            b'(' => {
                self.advance();
                self.paren_depth += 1;
                self.push(TokenKind::LParen, sl, sc);
            }
            b')' => {
                self.advance();
                self.paren_depth = self.paren_depth.saturating_sub(1);
                self.push(TokenKind::RParen, sl, sc);
            }
            b'[' => {
                self.advance();
                self.push(TokenKind::LBracket, sl, sc);
            }
            b']' => {
                self.advance();
                self.push(TokenKind::RBracket, sl, sc);
            }
            b'{' => {
                self.advance();
                self.push(TokenKind::LBrace, sl, sc);
            }
            b'}' => {
                self.advance();
                self.push(TokenKind::RBrace, sl, sc);
            }
            _ => {
                return Err(self.error(Msg::UnknownChar(c as char)));
            }
        }
        Ok(())
    }

    fn push(&mut self, kind: TokenKind, line: usize, column: usize) {
        self.tokens.push(Token::new(kind, line, column));
    }

    fn advance(&mut self) {
        let c = self.src[self.pos];
        self.pos += 1;
        if c == b'\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn peek2(&self) -> Option<u8> {
        self.src.get(self.pos + 1).copied()
    }

    fn error(&self, msg: Msg) -> LexError {
        LexError {
            diag: Diagnostic::new(msg, self.line, self.col),
        }
    }
}

fn is_ident_start(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphabetic()
}

fn is_ident_continue(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(source: &str) -> Vec<TokenKind> {
        lex(source)
            .expect("lex ok")
            .into_iter()
            .map(|t| t.kind)
            .collect()
    }

    #[test]
    fn basic_tokens() {
        assert_eq!(
            kinds("let x = 42\n"),
            vec![
                TokenKind::Let,
                TokenKind::Ident("x".into()),
                TokenKind::Eq,
                TokenKind::Int(42),
                TokenKind::Newline,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn indentation() {
        let toks = kinds("fn f():\n    let x = 1\n");
        assert!(toks.contains(&TokenKind::Indent));
        assert!(toks.contains(&TokenKind::Dedent));
    }

    #[test]
    fn numbers_with_separators_and_floats() {
        assert_eq!(
            kinds("1_000_000 2.5\n"),
            vec![
                TokenKind::Int(1_000_000),
                TokenKind::Float(2.5),
                TokenKind::Newline,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn strings_and_escapes() {
        assert_eq!(
            kinds("\"a\\nb\"\n"),
            vec![
                TokenKind::Str("a\nb".into()),
                TokenKind::Newline,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn comments_and_blank_lines_are_skipped() {
        assert_eq!(
            kinds("# comment\n\nlet x = 1\n"),
            vec![
                TokenKind::Let,
                TokenKind::Ident("x".into()),
                TokenKind::Eq,
                TokenKind::Int(1),
                TokenKind::Newline,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn keywords_and_operators() {
        assert_eq!(
            kinds("fn f(a: int) -> int:\n    return a + 1\n"),
            vec![
                TokenKind::Fn,
                TokenKind::Ident("f".into()),
                TokenKind::LParen,
                TokenKind::Ident("a".into()),
                TokenKind::Colon,
                TokenKind::Ident("int".into()),
                TokenKind::RParen,
                TokenKind::Arrow,
                TokenKind::Ident("int".into()),
                TokenKind::Colon,
                TokenKind::Newline,
                TokenKind::Indent,
                TokenKind::Return,
                TokenKind::Ident("a".into()),
                TokenKind::Plus,
                TokenKind::Int(1),
                TokenKind::Newline,
                TokenKind::Dedent,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn tabs_are_rejected() {
        assert!(lex("let x = 1\n\tlet y = 2\n").is_err());
        assert!(lex("let\tx = 1\n").is_err());
    }

    #[test]
    fn error_has_stable_code() {
        let err = lex("let\tx = 1\n").unwrap_err();
        assert_eq!(err.diag.code, "E0001");
        assert!(err.to_string().contains("[E0001]"));
    }

    #[test]
    fn unterminated_string_is_an_error() {
        assert!(lex("print(\"oops\n").is_err());
    }

    #[test]
    fn multiline_call_in_parens_has_no_indent_tokens() {
        // D1: 表达式跨行仅在括号内允许 —— 括号内的续行不产生 Indent。
        let toks = kinds("print(1,\n  2)\n");
        assert!(!toks.contains(&TokenKind::Indent));
        assert!(!toks.contains(&TokenKind::Dedent));
        assert!(toks.contains(&TokenKind::Newline));
    }
}
