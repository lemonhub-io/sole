//! AST definitions and a recursive-descent parser for Sole.
//!
//! M1 scope: functions, top-level statements, let bindings, assignment,
//! if/while/for, returns, and expressions. Grammar follows GOALS D1-D7.

use sole_diag::{Diagnostic, IdentKind, Lang, Msg};
use sole_lexer::{Token, TokenKind};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub line: usize,
    pub column: usize,
}

impl Span {
    pub fn new(line: usize, column: usize) -> Self {
        Self { line, column }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Fn(FnDef),
    Struct(StructDef),
    Interface(InterfaceDef),
    Impl(ImplDef),
    Stmt(Stmt),
}

#[derive(Debug, Clone, PartialEq)]
pub struct FnDef {
    pub name: String,
    pub params: Vec<Param>,
    pub ret: Option<Type>,
    pub body: Block,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StructDef {
    pub name: String,
    pub fields: Vec<(String, Type)>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InterfaceDef {
    pub name: String,
    pub methods: Vec<MethodSig>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MethodSig {
    pub name: String,
    pub params: Vec<Param>,
    pub ret: Option<Type>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImplDef {
    pub ty: String,
    pub interface: Option<String>,
    pub methods: Vec<FnDef>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub is_mut: bool,
    pub ty: Type,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    Named(String, Vec<Type>),
    Ref(Box<Type>),
    MutRef(Box<Type>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    Let {
        name: String,
        is_mut: bool,
        ty: Option<Type>,
        value: Expr,
        span: Span,
    },
    Assign {
        name: String,
        value: Expr,
        span: Span,
    },
    FieldAssign {
        obj: String,
        field: String,
        value: Expr,
        span: Span,
    },
    Expr(Expr),
    Return {
        value: Option<Expr>,
        span: Span,
    },
    If {
        cond: Expr,
        then_block: Block,
        else_block: Option<ElseBranch>,
        span: Span,
    },
    While {
        cond: Expr,
        body: Block,
        span: Span,
    },
    For {
        var: String,
        is_mut: bool,
        mode: IterMode,
        iterable: Expr,
        body: Block,
        span: Span,
    },
    Break {
        span: Span,
    },
    Continue {
        span: Span,
    },
}

impl Stmt {
    pub fn span(&self) -> Span {
        match self {
            Stmt::Let { span, .. }
            | Stmt::Assign { span, .. }
            | Stmt::FieldAssign { span, .. }
            | Stmt::Return { span, .. }
            | Stmt::If { span, .. }
            | Stmt::While { span, .. }
            | Stmt::For { span, .. }
            | Stmt::Break { span }
            | Stmt::Continue { span } => *span,
            Stmt::Expr(e) => e.span(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ElseBranch {
    If(Box<Stmt>),
    Block(Block),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum IterMode {
    Move,
    Borrow,
    MutBorrow,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Int(i64, Span),
    Float(f64, Span),
    Str(String, Span),
    Bool(bool, Span),
    Ident(String, Span),
    List(Vec<Expr>, Span),
    Unary {
        op: UnOp,
        expr: Box<Expr>,
        span: Span,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        span: Span,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
        span: Span,
    },
    Field {
        obj: Box<Expr>,
        name: String,
        span: Span,
    },
    Index {
        obj: Box<Expr>,
        index: Box<Expr>,
        span: Span,
    },
    Borrow {
        mutable: bool,
        expr: Box<Expr>,
        span: Span,
    },
}

impl Expr {
    pub fn span(&self) -> Span {
        match self {
            Expr::Int(_, s)
            | Expr::Float(_, s)
            | Expr::Str(_, s)
            | Expr::Bool(_, s)
            | Expr::Ident(_, s)
            | Expr::List(_, s) => *s,
            Expr::Unary { span, .. }
            | Expr::Binary { span, .. }
            | Expr::Call { span, .. }
            | Expr::Field { span, .. }
            | Expr::Index { span, .. }
            | Expr::Borrow { span, .. } => *span,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UnOp {
    Neg,
    Not,
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

#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub diag: Diagnostic,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.diag.render(Lang::current()))
    }
}

impl std::error::Error for ParseError {}

/// Lexes and parses a whole source file into a `Program`.
pub fn parse(source: &str) -> Result<Program, ParseError> {
    let tokens = sole_lexer::lex(source).map_err(|e| ParseError { diag: e.diag })?;
    Parser::new(&tokens).parse_program()
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(tokens: &'a [Token]) -> Self {
        Self { tokens, pos: 0 }
    }

    fn parse_program(&mut self) -> Result<Program, ParseError> {
        let mut items = Vec::new();
        loop {
            self.skip_newlines();
            if self.at(&TokenKind::Eof) {
                break;
            }
            items.push(self.parse_item()?);
        }
        Ok(Program { items })
    }

    fn parse_item(&mut self) -> Result<Item, ParseError> {
        if self.at(&TokenKind::Fn) {
            return self.parse_fn().map(Item::Fn);
        }
        if self.at(&TokenKind::Struct) {
            return self.parse_struct().map(Item::Struct);
        }
        if self.at(&TokenKind::Interface) {
            return self.parse_interface().map(Item::Interface);
        }
        if self.at(&TokenKind::Impl) {
            return self.parse_impl().map(Item::Impl);
        }
        self.parse_stmt().map(Item::Stmt)
    }

    fn parse_struct(&mut self) -> Result<StructDef, ParseError> {
        let span = self.here_span();
        self.expect(&TokenKind::Struct, "struct")?;
        let name = self.expect_ident(IdentKind::TypeName)?;
        self.expect(&TokenKind::Colon, ":")?;
        self.expect_newline()?;
        if !self.at(&TokenKind::Indent) {
            return Err(self.err_here(Msg::ExpectedIndent));
        }
        self.advance();
        let mut fields = Vec::new();
        while !self.at(&TokenKind::Dedent) {
            if self.at(&TokenKind::Eof) {
                return Err(self.err_here(Msg::BlockNotClosed));
            }
            let field_name = self.expect_ident(IdentKind::FieldName)?;
            self.expect(&TokenKind::Colon, ":")?;
            let ty = self.parse_type()?;
            self.expect_stmt_end()?;
            fields.push((field_name, ty));
        }
        self.advance(); // consume Dedent
        Ok(StructDef { name, fields, span })
    }

    fn parse_interface(&mut self) -> Result<InterfaceDef, ParseError> {
        let span = self.here_span();
        self.expect(&TokenKind::Interface, "interface")?;
        let name = self.expect_ident(IdentKind::TypeName)?;
        self.expect(&TokenKind::Colon, ":")?;
        self.expect_newline()?;
        if !self.at(&TokenKind::Indent) {
            return Err(self.err_here(Msg::ExpectedIndent));
        }
        self.advance();
        let mut methods = Vec::new();
        while !self.at(&TokenKind::Dedent) {
            if self.at(&TokenKind::Eof) {
                return Err(self.err_here(Msg::BlockNotClosed));
            }
            if !self.at(&TokenKind::Fn) {
                return Err(self.err_here(Msg::ExpectedToken("fn".into())));
            }
            self.advance();
            let method_name = self.expect_ident(IdentKind::FnName)?;
            self.expect(&TokenKind::LParen, "(")?;
            let mut params = Vec::new();
            if !self.at(&TokenKind::RParen) {
                loop {
                    params.push(self.parse_param()?);
                    if self.at(&TokenKind::Comma) {
                        self.advance();
                        continue;
                    }
                    break;
                }
            }
            self.expect(&TokenKind::RParen, ")")?;
            let ret = if self.at(&TokenKind::Arrow) {
                self.advance();
                Some(self.parse_type()?)
            } else {
                None
            };
            self.expect_stmt_end()?;
            methods.push(MethodSig {
                name: method_name,
                params,
                ret,
            });
        }
        self.advance(); // consume Dedent
        Ok(InterfaceDef {
            name,
            methods,
            span,
        })
    }

    fn parse_impl(&mut self) -> Result<ImplDef, ParseError> {
        let span = self.here_span();
        self.expect(&TokenKind::Impl, "impl")?;
        let ty = self.expect_ident(IdentKind::TypeName)?;
        let interface = if self.at(&TokenKind::Colon) && self.peek2() != Some(&TokenKind::Newline) {
            self.advance();
            Some(self.expect_ident(IdentKind::TypeName)?)
        } else {
            None
        };
        self.expect(&TokenKind::Colon, ":")?;
        self.expect_newline()?;
        if !self.at(&TokenKind::Indent) {
            return Err(self.err_here(Msg::ExpectedIndent));
        }
        self.advance();
        let mut methods = Vec::new();
        while !self.at(&TokenKind::Dedent) {
            if self.at(&TokenKind::Eof) {
                return Err(self.err_here(Msg::BlockNotClosed));
            }
            methods.push(self.parse_fn()?);
        }
        self.advance(); // consume Dedent
        Ok(ImplDef {
            ty,
            interface,
            methods,
            span,
        })
    }

    fn parse_fn(&mut self) -> Result<FnDef, ParseError> {
        self.expect(&TokenKind::Fn, "fn")?;
        let name = self.expect_ident(IdentKind::FnName)?;
        self.expect(&TokenKind::LParen, "(")?;
        let mut params = Vec::new();
        if !self.at(&TokenKind::RParen) {
            loop {
                params.push(self.parse_param()?);
                if self.at(&TokenKind::Comma) {
                    self.advance();
                    continue;
                }
                break;
            }
        }
        self.expect(&TokenKind::RParen, ")")?;
        let ret = if self.at(&TokenKind::Arrow) {
            self.advance();
            Some(self.parse_type()?)
        } else {
            None
        };
        self.expect(&TokenKind::Colon, ":")?;
        self.expect_newline()?;
        let body = self.parse_block()?;
        Ok(FnDef {
            name,
            params,
            ret,
            body,
        })
    }

    fn parse_param(&mut self) -> Result<Param, ParseError> {
        let is_mut = self.at(&TokenKind::Mut);
        if is_mut {
            self.advance();
        }
        let name = self.expect_ident(IdentKind::ParamName)?;
        self.expect(&TokenKind::Colon, ":")?;
        let ty = self.parse_type()?;
        Ok(Param { name, is_mut, ty })
    }

    fn parse_type(&mut self) -> Result<Type, ParseError> {
        if self.at(&TokenKind::Mut) && self.peek2() == Some(&TokenKind::Ref) {
            self.advance();
            self.advance();
            let inner = self.parse_type_inner()?;
            return Ok(Type::MutRef(Box::new(inner)));
        }
        if self.at(&TokenKind::Ref) {
            self.advance();
            let inner = self.parse_type_inner()?;
            return Ok(Type::Ref(Box::new(inner)));
        }
        self.parse_type_inner()
    }

    fn parse_type_inner(&mut self) -> Result<Type, ParseError> {
        let name = self.expect_ident(IdentKind::TypeName)?;
        let mut args = Vec::new();
        if self.at(&TokenKind::LBracket) {
            self.advance();
            loop {
                args.push(self.parse_type()?);
                if self.at(&TokenKind::Comma) {
                    self.advance();
                    continue;
                }
                break;
            }
            self.expect(&TokenKind::RBracket, "]")?;
        }
        Ok(Type::Named(name, args))
    }

    fn parse_stmt(&mut self) -> Result<Stmt, ParseError> {
        match self.peek_kind() {
            Some(TokenKind::Let) => self.parse_let(),
            Some(TokenKind::Ident(_)) if self.peek2() == Some(&TokenKind::Eq) => {
                self.parse_assign()
            }
            Some(TokenKind::Ident(_))
                if self.peek2() == Some(&TokenKind::Dot)
                    && matches!(self.peek3(), Some(TokenKind::Ident(_)))
                    && self.peek4() == Some(&TokenKind::Eq) =>
            {
                self.parse_field_assign()
            }
            Some(TokenKind::Return) => self.parse_return(),
            Some(TokenKind::If) => self.parse_if(),
            Some(TokenKind::While) => self.parse_while(),
            Some(TokenKind::For) => self.parse_for(),
            Some(TokenKind::Break) => {
                let span = self.here_span();
                self.advance();
                self.expect_stmt_end()?;
                Ok(Stmt::Break { span })
            }
            Some(TokenKind::Continue) => {
                let span = self.here_span();
                self.advance();
                self.expect_stmt_end()?;
                Ok(Stmt::Continue { span })
            }
            _ => {
                let expr = self.parse_expr()?;
                self.expect_stmt_end()?;
                Ok(Stmt::Expr(expr))
            }
        }
    }

    fn parse_let(&mut self) -> Result<Stmt, ParseError> {
        let span = self.here_span();
        self.expect(&TokenKind::Let, "let")?;
        let is_mut = self.at(&TokenKind::Mut);
        if is_mut {
            self.advance();
        }
        let name = self.expect_ident(IdentKind::VarName)?;
        let ty = if self.at(&TokenKind::Colon) {
            self.advance();
            Some(self.parse_type()?)
        } else {
            None
        };
        self.expect(&TokenKind::Eq, "=")?;
        let value = self.parse_expr()?;
        self.expect_stmt_end()?;
        Ok(Stmt::Let {
            name,
            is_mut,
            ty,
            value,
            span,
        })
    }

    fn parse_assign(&mut self) -> Result<Stmt, ParseError> {
        let span = self.here_span();
        let name = self.expect_ident(IdentKind::VarName)?;
        self.expect(&TokenKind::Eq, "=")?;
        let value = self.parse_expr()?;
        self.expect_stmt_end()?;
        Ok(Stmt::Assign { name, value, span })
    }

    fn parse_field_assign(&mut self) -> Result<Stmt, ParseError> {
        let span = self.here_span();
        let obj = self.expect_ident(IdentKind::VarName)?;
        self.expect(&TokenKind::Dot, ".")?;
        let field = self.expect_ident(IdentKind::FieldName)?;
        self.expect(&TokenKind::Eq, "=")?;
        let value = self.parse_expr()?;
        self.expect_stmt_end()?;
        Ok(Stmt::FieldAssign {
            obj,
            field,
            value,
            span,
        })
    }

    fn parse_return(&mut self) -> Result<Stmt, ParseError> {
        let span = self.here_span();
        self.expect(&TokenKind::Return, "return")?;
        let value = match self.peek_kind() {
            Some(TokenKind::Newline) | Some(TokenKind::Eof) => None,
            _ => Some(self.parse_expr()?),
        };
        self.expect_stmt_end()?;
        Ok(Stmt::Return { value, span })
    }

    fn parse_if(&mut self) -> Result<Stmt, ParseError> {
        let span = self.here_span();
        self.expect(&TokenKind::If, "if")?;
        let cond = self.parse_expr()?;
        self.expect(&TokenKind::Colon, ":")?;
        self.expect_newline()?;
        let then_block = self.parse_block()?;
        let else_block = if self.at(&TokenKind::Else) {
            self.advance();
            if self.at(&TokenKind::If) {
                Some(ElseBranch::If(Box::new(self.parse_if()?)))
            } else {
                self.expect(&TokenKind::Colon, ":")?;
                self.expect_newline()?;
                let block = self.parse_block()?;
                Some(ElseBranch::Block(block))
            }
        } else {
            None
        };
        Ok(Stmt::If {
            cond,
            then_block,
            else_block,
            span,
        })
    }

    fn parse_while(&mut self) -> Result<Stmt, ParseError> {
        let span = self.here_span();
        self.expect(&TokenKind::While, "while")?;
        let cond = self.parse_expr()?;
        self.expect(&TokenKind::Colon, ":")?;
        self.expect_newline()?;
        let body = self.parse_block()?;
        Ok(Stmt::While { cond, body, span })
    }

    fn parse_for(&mut self) -> Result<Stmt, ParseError> {
        let span = self.here_span();
        self.expect(&TokenKind::For, "for")?;
        let is_mut = self.at(&TokenKind::Mut);
        if is_mut {
            self.advance();
        }
        let var = self.expect_ident(IdentKind::LoopVar)?;
        self.expect(&TokenKind::In, "in")?;
        let mode = if self.at(&TokenKind::Mut) && self.peek2() == Some(&TokenKind::Ref) {
            self.advance();
            self.advance();
            IterMode::MutBorrow
        } else if self.at(&TokenKind::Ref) {
            self.advance();
            IterMode::Borrow
        } else {
            IterMode::Move
        };
        let iterable = self.parse_expr()?;
        self.expect(&TokenKind::Colon, ":")?;
        self.expect_newline()?;
        let body = self.parse_block()?;
        Ok(Stmt::For {
            var,
            is_mut,
            mode,
            iterable,
            body,
            span,
        })
    }

    /// Parses an indented block. The caller must already have consumed
    /// the `:` and the newline.
    fn parse_block(&mut self) -> Result<Block, ParseError> {
        if self.at(&TokenKind::Indent) {
            self.advance();
        } else {
            return Err(self.err_here(Msg::ExpectedIndent));
        }
        let mut stmts = Vec::new();
        while !self.at(&TokenKind::Dedent) {
            if self.at(&TokenKind::Eof) {
                return Err(self.err_here(Msg::BlockNotClosed));
            }
            stmts.push(self.parse_stmt()?);
        }
        self.advance(); // consume Dedent
        Ok(Block { stmts })
    }

    fn expect_stmt_end(&mut self) -> Result<(), ParseError> {
        match self.peek_kind() {
            Some(TokenKind::Newline) => {
                self.advance();
                Ok(())
            }
            Some(TokenKind::Eof) => Ok(()),
            _ => Err(self.err_here(Msg::ExpectedNewline)),
        }
    }

    // Expression parsing, lowest precedence first.

    fn parse_expr(&mut self) -> Result<Expr, ParseError> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_and()?;
        while self.at(&TokenKind::Or) {
            let span = self.here_span();
            self.advance();
            let rhs = self.parse_and()?;
            lhs = Expr::Binary {
                op: BinOp::Or,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }
        Ok(lhs)
    }

    fn parse_and(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_cmp()?;
        while self.at(&TokenKind::And) {
            let span = self.here_span();
            self.advance();
            let rhs = self.parse_cmp()?;
            lhs = Expr::Binary {
                op: BinOp::And,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }
        Ok(lhs)
    }

    fn parse_cmp(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_add()?;
        loop {
            let op = match self.peek_kind() {
                Some(TokenKind::EqEq) => BinOp::Eq,
                Some(TokenKind::BangEq) => BinOp::Ne,
                Some(TokenKind::Lt) => BinOp::Lt,
                Some(TokenKind::Le) => BinOp::Le,
                Some(TokenKind::Gt) => BinOp::Gt,
                Some(TokenKind::Ge) => BinOp::Ge,
                _ => break,
            };
            let span = self.here_span();
            self.advance();
            let rhs = self.parse_add()?;
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }
        Ok(lhs)
    }

    fn parse_add(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_mul()?;
        loop {
            let op = match self.peek_kind() {
                Some(TokenKind::Plus) => BinOp::Add,
                Some(TokenKind::Minus) => BinOp::Sub,
                _ => break,
            };
            let span = self.here_span();
            self.advance();
            let rhs = self.parse_mul()?;
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }
        Ok(lhs)
    }

    fn parse_mul(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_unary()?;
        loop {
            let op = match self.peek_kind() {
                Some(TokenKind::Star) => BinOp::Mul,
                Some(TokenKind::Slash) => BinOp::Div,
                Some(TokenKind::Percent) => BinOp::Mod,
                _ => break,
            };
            let span = self.here_span();
            self.advance();
            let rhs = self.parse_unary()?;
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> Result<Expr, ParseError> {
        match self.peek_kind() {
            Some(TokenKind::Minus) => {
                let span = self.here_span();
                self.advance();
                let expr = self.parse_unary()?;
                Ok(Expr::Unary {
                    op: UnOp::Neg,
                    expr: Box::new(expr),
                    span,
                })
            }
            Some(TokenKind::Not) => {
                let span = self.here_span();
                self.advance();
                let expr = self.parse_unary()?;
                Ok(Expr::Unary {
                    op: UnOp::Not,
                    expr: Box::new(expr),
                    span,
                })
            }
            Some(TokenKind::Mut) if self.peek2() == Some(&TokenKind::Ref) => {
                let span = self.here_span();
                self.advance();
                self.advance();
                let expr = self.parse_unary()?;
                Ok(Expr::Borrow {
                    mutable: true,
                    expr: Box::new(expr),
                    span,
                })
            }
            Some(TokenKind::Ref) => {
                let span = self.here_span();
                self.advance();
                let expr = self.parse_unary()?;
                Ok(Expr::Borrow {
                    mutable: false,
                    expr: Box::new(expr),
                    span,
                })
            }
            _ => self.parse_postfix(),
        }
    }

    fn parse_postfix(&mut self) -> Result<Expr, ParseError> {
        let mut expr = self.parse_primary()?;
        loop {
            match self.peek_kind() {
                Some(TokenKind::LParen) => {
                    let span = self.here_span();
                    self.advance();
                    let mut args = Vec::new();
                    if !self.at(&TokenKind::RParen) {
                        loop {
                            args.push(self.parse_expr()?);
                            if self.at(&TokenKind::Comma) {
                                self.advance();
                                continue;
                            }
                            break;
                        }
                    }
                    self.expect(&TokenKind::RParen, ")")?;
                    expr = Expr::Call {
                        callee: Box::new(expr),
                        args,
                        span,
                    };
                }
                Some(TokenKind::LBracket) => {
                    let span = self.here_span();
                    self.advance();
                    let index = self.parse_expr()?;
                    self.expect(&TokenKind::RBracket, "]")?;
                    expr = Expr::Index {
                        obj: Box::new(expr),
                        index: Box::new(index),
                        span,
                    };
                }
                Some(TokenKind::Dot) => {
                    let span = self.here_span();
                    self.advance();
                    let name = self.expect_ident(IdentKind::FieldName)?;
                    expr = Expr::Field {
                        obj: Box::new(expr),
                        name,
                        span,
                    };
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    fn parse_primary(&mut self) -> Result<Expr, ParseError> {
        match self.peek_kind() {
            Some(TokenKind::Int(n)) => {
                let span = self.here_span();
                self.advance();
                Ok(Expr::Int(n, span))
            }
            Some(TokenKind::Float(f)) => {
                let span = self.here_span();
                self.advance();
                Ok(Expr::Float(f, span))
            }
            Some(TokenKind::Str(s)) => {
                let span = self.here_span();
                let s = s.clone();
                self.advance();
                Ok(Expr::Str(s, span))
            }
            Some(TokenKind::True) => {
                let span = self.here_span();
                self.advance();
                Ok(Expr::Bool(true, span))
            }
            Some(TokenKind::False) => {
                let span = self.here_span();
                self.advance();
                Ok(Expr::Bool(false, span))
            }
            Some(TokenKind::Ident(name)) => {
                let span = self.here_span();
                let name = name.clone();
                self.advance();
                Ok(Expr::Ident(name, span))
            }
            Some(TokenKind::LParen) => {
                self.advance();
                let inner = self.parse_expr()?;
                self.expect(&TokenKind::RParen, ")")?;
                Ok(inner)
            }
            Some(TokenKind::LBracket) => {
                let span = self.here_span();
                self.advance();
                let mut items = Vec::new();
                if !self.at(&TokenKind::RBracket) {
                    loop {
                        items.push(self.parse_expr()?);
                        if self.at(&TokenKind::Comma) {
                            self.advance();
                            continue;
                        }
                        break;
                    }
                }
                self.expect(&TokenKind::RBracket, "]")?;
                Ok(Expr::List(items, span))
            }
            _ => Err(self.err_here(Msg::ExpectedExpr)),
        }
    }

    // Token stream helpers.

    fn peek_kind(&self) -> Option<TokenKind> {
        self.tokens.get(self.pos).map(|t| t.kind.clone())
    }

    fn at(&self, kind: &TokenKind) -> bool {
        self.peek_kind().as_ref() == Some(kind)
    }

    fn peek2(&self) -> Option<&TokenKind> {
        self.tokens.get(self.pos + 1).map(|t| &t.kind)
    }

    fn peek3(&self) -> Option<&TokenKind> {
        self.tokens.get(self.pos + 2).map(|t| &t.kind)
    }

    fn peek4(&self) -> Option<&TokenKind> {
        self.tokens.get(self.pos + 3).map(|t| &t.kind)
    }

    fn advance(&mut self) {
        if self.pos < self.tokens.len() {
            self.pos += 1;
        }
    }

    fn expect(&mut self, kind: &TokenKind, token: &str) -> Result<(), ParseError> {
        if self.at(kind) {
            self.advance();
            Ok(())
        } else {
            Err(self.err_here(Msg::ExpectedToken(token.to_string())))
        }
    }

    fn expect_newline(&mut self) -> Result<(), ParseError> {
        if self.at(&TokenKind::Newline) {
            self.advance();
            Ok(())
        } else {
            Err(self.err_here(Msg::ExpectedNewline))
        }
    }

    fn expect_ident(&mut self, kind: IdentKind) -> Result<String, ParseError> {
        match self.peek_kind() {
            Some(TokenKind::Ident(name)) => {
                let name = name.clone();
                self.advance();
                Ok(name)
            }
            _ => Err(self.err_here(Msg::ExpectedIdent(kind))),
        }
    }

    fn skip_newlines(&mut self) {
        while self.at(&TokenKind::Newline) {
            self.advance();
        }
    }

    fn err_here(&self, msg: Msg) -> ParseError {
        let (line, column) = match self.tokens.get(self.pos) {
            Some(t) => (t.line, t.column),
            None => (0, 0),
        };
        ParseError {
            diag: Diagnostic::new(msg, line, column),
        }
    }

    fn here_span(&self) -> Span {
        match self.tokens.get(self.pos) {
            Some(t) => Span::new(t.line, t.column),
            None => Span::new(0, 0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fn_with_types_and_generics() {
        let p = parse("fn add(a: int, b: List[int]) -> int:\n    return a\n").expect("parse");
        assert_eq!(p.items.len(), 1);
        let Item::Fn(f) = &p.items[0] else {
            panic!("expected fn item");
        };
        assert_eq!(f.name, "add");
        assert_eq!(f.params.len(), 2);
        assert_eq!(f.ret, Some(Type::Named("int".into(), vec![])));
    }

    #[test]
    fn parses_ref_types() {
        let p = parse("fn peek(x: ref List[int]) -> int:\n    return 0\n").expect("parse");
        let Item::Fn(f) = &p.items[0] else {
            panic!("expected fn item");
        };
        assert_eq!(
            f.params[0].ty,
            Type::Ref(Box::new(Type::Named(
                "List".into(),
                vec![Type::Named("int".into(), vec![])]
            )))
        );
    }

    #[test]
    fn parses_top_level_statements_and_control_flow() {
        let src = r#"
let mut total = 0
for i in range(5):
    total = total + i
while total > 0:
    total = total - 1
print(total)
"#;
        let p = parse(src).expect("parse");
        assert!(p.items.len() >= 4);
    }

    #[test]
    fn parses_if_else_chain() {
        let src = r#"
fn sign(n: int) -> str:
    if n > 0:
        return "pos"
    else if n < 0:
        return "neg"
    else:
        return "zero"
"#;
        let p = parse(src).expect("parse");
        let Item::Fn(f) = &p.items[0] else {
            panic!("expected fn item");
        };
        assert_eq!(f.name, "sign");
    }

    #[test]
    fn rejects_missing_indent() {
        assert!(parse("fn f():\nreturn 1\n").is_err());
    }

    #[test]
    fn assignment_requires_target_ident() {
        assert!(parse("x + 1 = 5\n").is_err());
    }
}
