//! End-to-end LSP tests: spawn the `sole lsp` binary and speak
//! JSON-RPC over its stdin/stdout.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};

struct Lsp {
    child: Child,
}

impl Lsp {
    fn start() -> Lsp {
        let child = Command::new(env!("CARGO_BIN_EXE_sole"))
            .arg("lsp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sole lsp");
        Lsp { child }
    }

    fn send(&mut self, msg: &serde_json::Value) {
        let body = serde_json::to_string(msg).unwrap();
        let stdin = self.child.stdin.as_mut().unwrap();
        write!(stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
        stdin.flush().unwrap();
    }

    fn recv(&mut self) -> serde_json::Value {
        let mut stdout = BufReader::new(self.child.stdout.take().unwrap());
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let n: usize = line
            .trim()
            .strip_prefix("Content-Length:")
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let mut blank = String::new();
        stdout.read_line(&mut blank).unwrap();
        let mut body = vec![0u8; n];
        stdout.read_exact(&mut body).unwrap();
        // Put the reader back for the next call.
        self.child.stdout = Some(stdout.into_inner());
        serde_json::from_slice(&body).unwrap()
    }

    fn initialize(&mut self) {
        self.send(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}
        }));
        let r = self.recv();
        assert_eq!(r["id"], 1);
        assert!(r["result"]["capabilities"]["definitionProvider"]
            .as_bool()
            .unwrap());
    }

    fn did_open(&mut self, uri: &str, text: &str) {
        self.send(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {"textDocument": {"uri": uri, "text": text}},
        }));
    }

    fn shutdown(&mut self) {
        self.send(&serde_json::json!({
            "jsonrpc": "2.0", "id": 99, "method": "shutdown", "params": {}
        }));
        let r = self.recv();
        assert_eq!(r["id"], 99);
        assert!(r["result"].is_null());
        self.send(&serde_json::json!({
            "jsonrpc": "2.0", "method": "exit", "params": {}
        }));
        assert!(self.child.wait().unwrap().success());
    }
}

#[test]
fn lsp_handshake_and_clean_shutdown() {
    let mut lsp = Lsp::start();
    lsp.initialize();
    lsp.shutdown();
}

#[test]
fn lsp_publishes_diagnostics_on_open() {
    let mut lsp = Lsp::start();
    lsp.initialize();
    lsp.did_open(
        "file:///diag.sole",
        "fn f(x: int) -> int:\n    return \"oops\"\n",
    );
    let r = lsp.recv();
    assert_eq!(r["method"], "textDocument/publishDiagnostics");
    let diags = &r["params"]["diagnostics"];
    assert_eq!(diags.as_array().unwrap().len(), 1);
    let d = &diags[0];
    assert_eq!(d["code"], "E0304");
    assert!(d["message"].as_str().unwrap().contains("E0304"));
    assert_eq!(d["range"]["start"]["line"], 1);
    lsp.shutdown();
}

#[test]
fn lsp_clean_diagnostics_are_empty() {
    let mut lsp = Lsp::start();
    lsp.initialize();
    lsp.did_open(
        "file:///clean.sole",
        "fn f(x: int) -> int:\n    return x\nprint(f(1))\n",
    );
    let r = lsp.recv();
    assert_eq!(r["method"], "textDocument/publishDiagnostics");
    assert!(r["params"]["diagnostics"].as_array().unwrap().is_empty());
    lsp.shutdown();
}

#[test]
fn lsp_definition_goes_to_function_and_let() {
    let mut lsp = Lsp::start();
    lsp.initialize();
    let src = "fn f(x: int) -> int:\n    return x + 1\nlet y = f(41)\nprint(y)\n";
    lsp.did_open("file:///def.sole", src);
    lsp.recv(); // diagnostics

    lsp.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": 2, "method": "textDocument/definition",
        "params": {"textDocument": {"uri": "file:///def.sole"},
                   "position": {"line": 2, "character": 9}},
    }));
    let r = lsp.recv();
    assert_eq!(r["id"], 2);
    let loc = &r["result"];
    assert_eq!(loc["range"]["start"]["line"], 0);
    assert_eq!(loc["range"]["start"]["character"], 0);

    lsp.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": 3, "method": "textDocument/definition",
        "params": {"textDocument": {"uri": "file:///def.sole"},
                   "position": {"line": 3, "character": 7}},
    }));
    let r = lsp.recv();
    let loc = &r["result"];
    assert_eq!(loc["range"]["start"]["line"], 2);
    assert_eq!(loc["range"]["start"]["character"], 0);
    lsp.shutdown();
}

#[test]
fn lsp_completion_includes_keywords_and_symbols() {
    let mut lsp = Lsp::start();
    lsp.initialize();
    lsp.did_open("file:///comp.sole", "fn my_func() -> int:\n    return 1\n");
    lsp.recv();
    lsp.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": 4, "method": "textDocument/completion",
        "params": {"textDocument": {"uri": "file:///comp.sole"},
                   "position": {"line": 0, "character": 0}},
    }));
    let r = lsp.recv();
    let items: Vec<String> = r["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["label"].as_str().map(String::from))
        .collect();
    assert!(items.contains(&"fn".to_string()));
    assert!(items.contains(&"my_func".to_string()));
    lsp.shutdown();
}

#[test]
fn lsp_diagnostics_update_after_change() {
    let mut lsp = Lsp::start();
    lsp.initialize();
    lsp.did_open("file:///chg.sole", "let x = 1\n");
    let r = lsp.recv();
    assert!(r["params"]["diagnostics"].as_array().unwrap().is_empty());

    // didChange with a type error
    lsp.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didChange",
        "params": {
            "textDocument": {"uri": "file:///chg.sole"},
            "contentChanges": [{"text": "let x: int = \"nope\"\n"}],
        },
    }));
    let r = lsp.recv();
    assert_eq!(r["method"], "textDocument/publishDiagnostics");
    assert_eq!(r["params"]["diagnostics"][0]["code"], "E0301");
    lsp.shutdown();
}
