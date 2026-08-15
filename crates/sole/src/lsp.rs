//! Minimal LSP server (GOALS §9.1): JSON-RPC 2.0 over stdio.
//!
//! Supports full-sync document updates, publishDiagnostics (reusing the
//! type checker, bilingual via sole-diag), go-to-definition for functions /
//! types / top-level bindings, and basic completion. Hover and incremental
//! sync are intentionally not implemented yet.

use crate::typecheck;
use sole_diag::Lang;
use sole_parser::{parse, ElseBranch, Item, Stmt};
use std::collections::HashMap;
use std::io::{BufRead, Write};

/// An open document: uri → source text.
pub type Documents = HashMap<String, String>;

/// A symbol definition (1-based line/column, like the parser's spans).
pub type SymbolTable = HashMap<String, (usize, usize)>;

/// Runs the LSP server loop on stdin/stdout until `exit`.
pub fn run_server() -> Result<(), String> {
    let stdin = std::io::stdin();
    let mut docs: Documents = HashMap::new();
    let mut out = std::io::stdout();
    let mut shutdown = false;
    for msg in read_messages(&mut stdin.lock()) {
        let msg = msg?;
        let value: serde_json::Value =
            serde_json::from_str(&msg).map_err(|e| format!("invalid JSON-RPC message: {}", e))?;
        let method = value
            .get("method")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        let id = value.get("id").cloned();
        let params = value
            .get("params")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        match method.as_str() {
            "initialize" => {
                let result = serde_json::json!({
                    "capabilities": {
                        "textDocumentSync": 1,
                        "definitionProvider": true,
                        "completionProvider": {"triggerCharacters": ["."]},
                    },
                    "serverInfo": {"name": "sole-lsp", "version": env!("CARGO_PKG_VERSION")},
                });
                send(&mut out, id, &result)?;
            }
            "shutdown" => {
                send(&mut out, id, &serde_json::Value::Null)?;
                shutdown = true;
            }
            "exit" => break,
            "textDocument/didOpen" | "textDocument/didChange" => {
                let uri = params.pointer("/textDocument/uri").and_then(|u| u.as_str());
                // didOpen carries the text on textDocument; didChange carries
                // it on contentChanges[0].text (full sync).
                let text = params
                    .pointer("/textDocument/text")
                    .or_else(|| params.pointer("/contentChanges/0/text"))
                    .and_then(|t| t.as_str());
                if let (Some(uri), Some(text)) = (uri, text) {
                    docs.insert(uri.to_string(), text.to_string());
                    publish(&mut out, uri, &docs)?;
                }
            }
            "textDocument/definition" => {
                let result = definition(&params, &docs);
                send(&mut out, id, &result)?;
            }
            "textDocument/completion" => {
                let result = completion(&params, &docs);
                send(&mut out, id, &result)?;
            }
            _ => {
                if id.is_some() {
                    send(&mut out, id, &serde_json::Value::Null)?;
                }
            }
        }
        if shutdown && method == "exit" {
            break;
        }
    }
    Ok(())
}

/// Iterates framed LSP messages (Content-Length headers).
fn read_messages(stdin: &mut impl BufRead) -> impl Iterator<Item = Result<String, String>> + '_ {
    std::iter::from_fn(move || {
        let mut line = String::new();
        match stdin.read_line(&mut line) {
            Ok(0) => None,
            Ok(_) => {
                if let Some(len) = line.trim().strip_prefix("Content-Length:") {
                    let n: usize = len.trim().parse().unwrap_or(0);
                    let mut body = vec![0u8; n];
                    // Consume the blank line, then read the body.
                    let mut blank = String::new();
                    if stdin.read_line(&mut blank).is_err() {
                        return None;
                    }
                    if stdin.read_exact(&mut body).is_err() {
                        return None;
                    }
                    Some(
                        String::from_utf8(body)
                            .map_err(|e| format!("invalid UTF-8 in message: {}", e)),
                    )
                } else {
                    Some(Err("malformed LSP header".into()))
                }
            }
            Err(e) => Some(Err(format!("stdin error: {}", e))),
        }
    })
}

fn send(
    out: &mut impl Write,
    id: Option<serde_json::Value>,
    result: &serde_json::Value,
) -> Result<(), String> {
    let msg = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(serde_json::Value::Null),
        "result": result,
    });
    let body = serde_json::to_string(&msg).map_err(|e| e.to_string())?;
    write!(out, "Content-Length: {}\r\n\r\n{}", body.len(), body).map_err(|e| e.to_string())?;
    out.flush().map_err(|e| e.to_string())
}

fn notify(out: &mut impl Write, method: &str, params: &serde_json::Value) -> Result<(), String> {
    let msg = serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
    });
    let body = serde_json::to_string(&msg).map_err(|e| e.to_string())?;
    write!(out, "Content-Length: {}\r\n\r\n{}", body.len(), body).map_err(|e| e.to_string())?;
    out.flush().map_err(|e| e.to_string())
}

/// Runs parse + typecheck and publishes the diagnostics for one document.
fn publish(out: &mut impl Write, uri: &str, docs: &Documents) -> Result<(), String> {
    let diagnostics = diagnostics_for(docs.get(uri).map(String::as_str).unwrap_or(""));
    let params = serde_json::json!({
        "uri": uri,
        "diagnostics": diagnostics,
    });
    notify(out, "textDocument/publishDiagnostics", &params)
}

/// Type-checks a source and returns LSP diagnostics (0-based positions).
pub fn diagnostics_for(source: &str) -> Vec<serde_json::Value> {
    let lang = Lang::current();
    let diag = match parse(source) {
        Ok(program) => match typecheck::check(&program) {
            Ok(()) => return Vec::new(),
            Err(e) => Some(e.diag),
        },
        Err(e) => Some(e.diag),
    };
    diag.map(|d| {
        serde_json::json!({
            "range": {
                "start": {"line": d.line.saturating_sub(1), "character": d.column.saturating_sub(1)},
                "end": {"line": d.line.saturating_sub(1), "character": d.column.saturating_sub(1)},
            },
            "severity": 1,
            "code": d.code,
            "source": "sole",
            "message": d.render(lang),
        })
    })
    .into_iter()
    .collect()
}

/// Builds a symbol table (functions, types, test blocks, and all `let`
/// bindings with their 1-based positions). Later bindings shadow earlier
/// ones of the same name.
pub fn symbols_for(source: &str) -> SymbolTable {
    let mut symbols: SymbolTable = HashMap::new();
    let Ok(program) = parse(source) else {
        return symbols;
    };
    fn collect_stmt(stmt: &Stmt, symbols: &mut SymbolTable) {
        match stmt {
            Stmt::Let { name, span, .. } => {
                symbols.insert(name.clone(), (span.line, span.column));
            }
            Stmt::If {
                then_block,
                else_block,
                ..
            } => {
                for s in &then_block.stmts {
                    collect_stmt(s, symbols);
                }
                if let Some(ElseBranch::Block(b)) = else_block {
                    for s in &b.stmts {
                        collect_stmt(s, symbols);
                    }
                }
                if let Some(ElseBranch::If(s)) = else_block {
                    collect_stmt(s, symbols);
                }
            }
            Stmt::While { body, .. } | Stmt::For { body, .. } | Stmt::TaskGroup { body, .. } => {
                for s in &body.stmts {
                    collect_stmt(s, symbols);
                }
            }
            _ => {}
        }
    }
    for item in &program.items {
        match item {
            Item::Fn(f) => {
                symbols.insert(f.name.clone(), (f.span.line, f.span.column));
            }
            Item::Test(t) => {
                symbols.insert(t.name.clone(), (t.span.line, t.span.column));
            }
            Item::Struct(s) => {
                symbols.insert(s.name.clone(), (s.span.line, s.span.column));
            }
            Item::Interface(i) => {
                symbols.insert(i.name.clone(), (i.span.line, i.span.column));
            }
            Item::Stmt(stmt) => collect_stmt(stmt, &mut symbols),
            _ => {}
        }
    }
    symbols
}

/// Extracts the identifier touching a 0-based position in a line.
fn ident_at(line: &str, character: usize) -> Option<String> {
    let bytes: Vec<char> = line.chars().collect();
    let mut pos = character.min(bytes.len());
    // Cursors commonly sit just after the token; step back onto it.
    if pos > 0
        && (pos >= bytes.len() || !is_ident_char(bytes[pos]))
        && is_ident_char(bytes[pos - 1])
    {
        pos -= 1;
    }
    if pos >= bytes.len() || !is_ident_char(bytes[pos]) {
        return None;
    }
    let mut start = pos;
    while start > 0 && is_ident_char(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = pos + 1;
    while end < bytes.len() && is_ident_char(bytes[end]) {
        end += 1;
    }
    Some(bytes[start..end].iter().collect())
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Handles `textDocument/definition`.
pub fn definition(params: &serde_json::Value, docs: &Documents) -> serde_json::Value {
    let Some(uri) = params.pointer("/textDocument/uri").and_then(|u| u.as_str()) else {
        return serde_json::Value::Null;
    };
    let Some(position) = params.get("position") else {
        return serde_json::Value::Null;
    };
    let line = position.get("line").and_then(|l| l.as_u64()).unwrap_or(0) as usize;
    let character = position
        .get("character")
        .and_then(|c| c.as_u64())
        .unwrap_or(0) as usize;
    let Some(source) = docs.get(uri) else {
        return serde_json::Value::Null;
    };
    let Some(line_text) = source.lines().nth(line) else {
        return serde_json::Value::Null;
    };
    let Some(name) = ident_at(line_text, character) else {
        return serde_json::Value::Null;
    };
    let symbols = symbols_for(source);
    let Some((sline, scol)) = symbols.get(&name) else {
        return serde_json::Value::Null;
    };
    serde_json::json!({
        "uri": uri,
        "range": {
            "start": {"line": sline - 1, "character": scol - 1},
            "end": {"line": sline - 1, "character": scol - 1 + name.len()},
        },
    })
}

/// Handles `textDocument/completion`: keywords plus known symbols.
pub fn completion(params: &serde_json::Value, docs: &Documents) -> serde_json::Value {
    let keywords = [
        "fn",
        "let",
        "mut",
        "if",
        "else",
        "while",
        "for",
        "in",
        "return",
        "true",
        "false",
        "ref",
        "break",
        "continue",
        "struct",
        "interface",
        "impl",
        "test",
        "yield",
        "task_group",
        "go",
        "and",
        "or",
        "not",
        "assert",
        "import",
        "from",
        "Some",
        "None",
        "Ok",
        "Err",
        "print",
        "range",
        "read_to_str",
        "write",
        "clock",
        "sleep",
        "abs",
        "floor",
        "ceil",
        "round",
        "sqrt",
        "pow",
        "json_encode",
        "json_decode",
    ];
    let mut items: Vec<serde_json::Value> = keywords
        .iter()
        .map(|k| serde_json::json!({"label": k, "kind": 14}))
        .collect();
    if let Some(uri) = params.pointer("/textDocument/uri").and_then(|u| u.as_str()) {
        if let Some(source) = docs.get(uri) {
            for name in symbols_for(source).keys() {
                items.push(serde_json::json!({"label": name, "kind": 3}));
            }
        }
    }
    serde_json::json!({"isIncomplete": false, "items": items})
}
