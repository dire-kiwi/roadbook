//! `roadbook lsp`, started as an editor starts it, talking JSON-RPC over standard input and
//! output.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Server {
    fn start() -> Server {
        let mut child = Command::new(env!("CARGO_BIN_EXE_roadbook"))
            .args(["lsp", "--stdio"])
            // The current directory means nothing to the server: each file finds its project.
            .current_dir(std::env::temp_dir())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Server {
            child,
            stdin,
            stdout,
        }
    }

    fn send(&mut self, msg: Value) {
        let body = msg.to_string();
        write!(self.stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        self.stdin.flush().unwrap();
    }

    fn recv(&mut self) -> Value {
        let mut length = 0;
        loop {
            let mut line = String::new();
            self.stdout.read_line(&mut line).unwrap();
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some(n) = line.strip_prefix("Content-Length: ") {
                length = n.parse().unwrap();
            }
        }
        let mut body = vec![0; length];
        self.stdout.read_exact(&mut body).unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    /// Sends several messages in one write, as a client that falls behind would.
    fn send_all(&mut self, msgs: &[Value]) {
        let mut out = String::new();
        for m in msgs {
            let body = m.to_string();
            out.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
        }
        self.stdin.write_all(out.as_bytes()).unwrap();
        self.stdin.flush().unwrap();
    }

    /// The answer to request `id`, skipping notifications.
    fn answer(&mut self, id: i64) -> Value {
        loop {
            let m = self.recv();
            if m["id"] == id {
                return m;
            }
        }
    }

    /// The next diagnostics published for `uri`.
    fn diagnostics(&mut self, uri: &str) -> Vec<Value> {
        loop {
            let m = self.recv();
            if m["method"] == "textDocument/publishDiagnostics" && m["params"]["uri"] == uri {
                return m["params"]["diagnostics"].as_array().unwrap().clone();
            }
        }
    }
}

fn uri(p: &Path) -> String {
    format!("file://{}", p.display())
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

#[test]
fn an_editor_session() {
    let dir = std::env::temp_dir().join(format!("roadbook-cli-lsp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("roadbook.toml"), "").unwrap();
    let dir = dir.canonicalize().unwrap();
    let file = dir.join("m.road");
    let bad = "machine M() {\n  initial A\n  state A { always -> Dne }\n}\n";
    std::fs::write(&file, bad).unwrap();
    let file_uri = uri(&file);

    let mut s = Server::start();
    s.send(json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "capabilities": {} } }));
    let init = s.answer(1);
    assert_eq!(init["result"]["serverInfo"]["name"], "roadbook");
    assert_eq!(
        init["result"]["capabilities"]["renameProvider"]["prepareProvider"],
        true
    );
    assert_eq!(
        init["result"]["capabilities"]["completionProvider"]["triggerCharacters"],
        json!(["."])
    );
    s.send(json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }));

    s.send(json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
        "textDocument": { "uri": file_uri, "languageId": "roadbook", "version": 1, "text": bad } } }));
    let diags = s.diagnostics(&file_uri);
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0]["code"], "RB0201");
    assert_eq!(
        diags[0]["range"]["start"],
        json!({ "line": 2, "character": 22 })
    );

    let good = "machine M() {\n  initial A\n  state A { always -> B }\n  final state B {}\n}\n";
    s.send(json!({ "jsonrpc": "2.0", "method": "textDocument/didChange", "params": {
        "textDocument": { "uri": file_uri, "version": 2 }, "contentChanges": [{ "text": good }] } }));
    assert_eq!(s.diagnostics(&file_uri), Vec::<Value>::new());

    // A chart of the repository's project, which imports a package by name through
    // `[packages]` in its `roadbook.toml`: the definition is in the package's file.
    let orders = repo().join("examples/orders/orders.road");
    let text = std::fs::read_to_string(&orders).unwrap();
    let orders_uri = uri(&orders);
    s.send(json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
        "textDocument": { "uri": orders_uri, "languageId": "roadbook", "version": 1, "text": text } } }));
    assert_eq!(s.diagnostics(&orders_uri), Vec::<Value>::new());
    let (line, l) = text
        .lines()
        .enumerate()
        .find(|(_, l)| l.contains("ref Inventory"))
        .unwrap();
    let character = l.find("ref Inventory").unwrap() + 4;
    s.send(json!({ "jsonrpc": "2.0", "id": 2, "method": "textDocument/definition", "params": {
        "textDocument": { "uri": orders_uri }, "position": { "line": line, "character": character } } }));
    let loc = s.answer(2);
    assert_eq!(
        loc["result"]["uri"],
        uri(&repo().join("examples/inventory/inventory.road"))
    );

    // While `on inventory.` is being written, `ended` completes it: `inventory` holds a
    // `ref` to the other package's machine.
    let typing = text.replacen(
        "  initial Reserving\n",
        "  initial Reserving\n  on inventory.\n",
        1,
    );
    assert_ne!(typing, text);
    s.send(json!({ "jsonrpc": "2.0", "method": "textDocument/didChange", "params": {
        "textDocument": { "uri": orders_uri, "version": 2 }, "contentChanges": [{ "text": typing }] } }));
    let _ = s.diagnostics(&orders_uri);
    let line = typing.lines().position(|l| l == "  on inventory.").unwrap();
    s.send(
        json!({ "jsonrpc": "2.0", "id": 4, "method": "textDocument/completion", "params": {
        "textDocument": { "uri": orders_uri }, "position": { "line": line, "character": 15 } } }),
    );
    let items = s.answer(4);
    let labels: Vec<&str> = items["result"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["label"].as_str().unwrap())
        .collect();
    assert_eq!(labels, ["ended"], "{items}");

    s.send(json!({ "jsonrpc": "2.0", "id": 3, "method": "shutdown" }));
    assert_eq!(s.answer(3)["result"], Value::Null);
    s.send(json!({ "jsonrpc": "2.0", "method": "exit" }));
    assert!(s.child.wait().unwrap().success());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rapid_changes_are_compiled_together() {
    // A copy of the large example, so that a compilation takes a while.
    let dir = std::env::temp_dir().join(format!("roadbook-cli-lsp-rapid-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("roadbook.toml"), "").unwrap();
    let dir = dir.canonicalize().unwrap();
    for f in ["types.road", "ticket.road", "survey.road"] {
        std::fs::copy(repo().join("examples/large").join(f), dir.join(f)).unwrap();
    }
    let file = dir.join("ticket.road");
    let text = std::fs::read_to_string(&file).unwrap();
    let file_uri = uri(&file);

    let mut s = Server::start();
    s.send(json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "capabilities": {} } }));
    s.answer(1);
    s.send(json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }));
    s.send(json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
        "textDocument": { "uri": file_uri, "languageId": "roadbook", "version": 1, "text": text } } }));
    assert_eq!(s.diagnostics(&file_uri), Vec::<Value>::new());

    // Typing: each change a whole text, as the client sends them; the last one is broken.
    const CHANGES: i64 = 60;
    let changes: Vec<Value> = (2..=CHANGES + 1)
        .map(|version| {
            let text = if version == CHANGES + 1 {
                format!(
                    "{text}\nmachine Extra() {{\n  initial A\n  state A {{ always -> Dne }}\n}}\n"
                )
            } else {
                format!("{text}\n// {version}\n")
            };
            json!({ "jsonrpc": "2.0", "method": "textDocument/didChange", "params": {
                "textDocument": { "uri": file_uri, "version": version },
                "contentChanges": [{ "text": text }] } })
        })
        .collect();
    s.send_all(&changes);
    let mut compiled = 0;
    let last = loop {
        let m = s.recv();
        if m["method"] != "textDocument/publishDiagnostics" || m["params"]["uri"] != file_uri {
            continue;
        }
        compiled += 1;
        if m["params"]["version"] == CHANGES + 1 {
            break m["params"]["diagnostics"].as_array().unwrap().clone();
        }
    };
    assert!(
        compiled * 3 < CHANGES,
        "{compiled} compilations for {CHANGES} changes sent at once"
    );
    let codes: Vec<&str> = last.iter().map(|d| d["code"].as_str().unwrap()).collect();
    assert_eq!(codes, ["RB0201"], "{last:#?}");

    s.send(json!({ "jsonrpc": "2.0", "id": 2, "method": "shutdown" }));
    assert_eq!(s.answer(2)["result"], Value::Null);
    s.send(json!({ "jsonrpc": "2.0", "method": "exit" }));
    assert!(s.child.wait().unwrap().success());
    let _ = std::fs::remove_dir_all(&dir);
}
