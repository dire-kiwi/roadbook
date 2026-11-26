//! A client that drives the server in-process over JSON-RPC messages, and throwaway projects.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use std::time::Duration;

use lsp_server::{Connection, Message, Notification, Request, RequestId};
use roadbook_compiler::FsLoader;
use roadbook_lsp::{FindProject, ProjectConfig};
use serde_json::{Value, json};

/// A directory of files that is removed when dropped.
pub struct Project {
    pub root: PathBuf,
}

impl Project {
    /// A project (with a `roadbook.toml`) holding `files`.
    pub fn new(name: &str, files: &[(&str, &str)]) -> Project {
        let root = std::env::temp_dir().join(format!("roadbook-lsp-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("roadbook.toml"), "").unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        for (path, text) in files {
            let p = root.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        Project { root }
    }

    /// A project holding a copy of the `.road` files of a directory of the repository.
    pub fn copy(name: &str, dir: &str) -> Project {
        let src = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(dir);
        let mut files = Vec::new();
        for e in std::fs::read_dir(src).unwrap() {
            let p = e.unwrap().path();
            let file = p.file_name().unwrap().to_string_lossy().into_owned();
            if file.ends_with(".road") {
                files.push((file, std::fs::read_to_string(&p).unwrap()));
            }
        }
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        Project::new(name, &refs)
    }

    pub fn path(&self, file: &str) -> PathBuf {
        self.root.join(file)
    }

    pub fn uri(&self, file: &str) -> String {
        roadbook_lsp::text::path_uri(&self.path(file))
            .as_str()
            .to_string()
    }

    pub fn read(&self, file: &str) -> String {
        std::fs::read_to_string(self.path(file)).unwrap()
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Finds projects as the command line does, simplified: the nearest `roadbook.toml`, every
/// `.road` file under it outside `node_modules`, and packages in `node_modules`.
pub fn finder() -> FindProject {
    Box::new(|file: &Path| {
        let mut dir = file.parent();
        while let Some(d) = dir {
            if d.join("roadbook.toml").is_file() {
                let mut files = Vec::new();
                walk(d, d, &mut files);
                files.sort();
                return Ok(ProjectConfig {
                    root: d.to_path_buf(),
                    loader: FsLoader::new(d).with_node_modules(true),
                    options: Default::default(),
                    files,
                });
            }
            dir = d.parent();
        }
        Ok(ProjectConfig::standalone(file.parent().unwrap()))
    })
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            if !p.ends_with("node_modules") {
                walk(root, &p, out);
            }
        } else if p.extension().is_some_and(|x| x == "road") {
            out.push(
                p.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
}

pub struct Client {
    conn: Connection,
    server: Option<JoinHandle<()>>,
    next: i32,
    /// Notifications received while waiting for something else.
    queue: VecDeque<Notification>,
    pub init: Value,
}

const TIMEOUT: Duration = Duration::from_secs(60);

impl Client {
    /// Starts a server and initializes it with these client capabilities.
    pub fn start(capabilities: Value) -> Client {
        let (server, client) = Connection::memory();
        let handle = std::thread::spawn(move || {
            roadbook_lsp::run(server, finder()).expect("the server runs");
        });
        let mut c = Client {
            conn: client,
            server: Some(handle),
            next: 0,
            queue: VecDeque::new(),
            init: Value::Null,
        };
        c.init = c
            .request("initialize", json!({ "capabilities": capabilities }))
            .unwrap();
        c.notify("initialized", json!({}));
        c
    }

    pub fn notify(&self, method: &str, params: Value) {
        self.conn
            .sender
            .send(Notification::new(method.into(), params).into())
            .unwrap();
    }

    /// Sends a request and waits for its answer: the result, or the error's message.
    pub fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next += 1;
        let id = RequestId::from(self.next);
        self.conn
            .sender
            .send(Request::new(id.clone(), method.into(), params).into())
            .unwrap();
        loop {
            match self.conn.receiver.recv_timeout(TIMEOUT).expect("an answer") {
                Message::Response(r) if r.id == id => {
                    return r.response_result.map_err(|e| e.message);
                }
                Message::Notification(n) => self.queue.push_back(n),
                _ => {}
            }
        }
    }

    pub fn open(&self, uri: &str, text: &str) {
        self.notify(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": uri, "languageId": "roadbook", "version": 1, "text": text } }),
        );
    }

    pub fn change(&self, uri: &str, version: i32, text: &str) {
        self.notify(
            "textDocument/didChange",
            json!({ "textDocument": { "uri": uri, "version": version }, "contentChanges": [{ "text": text }] }),
        );
    }

    /// The next diagnostics published for `uri`.
    pub fn diagnostics(&mut self, uri: &str) -> Vec<Value> {
        let is_for = |n: &Notification| {
            n.method == "textDocument/publishDiagnostics" && n.params["uri"] == uri
        };
        if let Some(i) = self.queue.iter().position(is_for) {
            let n = self.queue.remove(i).unwrap();
            return n.params["diagnostics"].as_array().unwrap().clone();
        }
        loop {
            match self
                .conn
                .receiver
                .recv_timeout(TIMEOUT)
                .expect("diagnostics")
            {
                Message::Notification(n) if is_for(&n) => {
                    return n.params["diagnostics"].as_array().unwrap().clone();
                }
                Message::Notification(n) => self.queue.push_back(n),
                _ => {}
            }
        }
    }

    /// Drops the notifications received so far.
    pub fn forget(&mut self) {
        self.queue.clear();
        while self.conn.receiver.try_recv().is_ok() {}
    }

    pub fn shutdown(mut self) {
        self.request("shutdown", Value::Null).unwrap();
        self.notify("exit", Value::Null);
        self.server.take().unwrap().join().unwrap();
    }
}

/// `textDocument` and `position` for the first `needle` in `text`, at its `^` (or start),
/// in UTF-16 code units.
pub fn at(uri: &str, text: &str, needle: &str) -> Value {
    json!({ "textDocument": { "uri": uri }, "position": position(text, needle) })
}

pub fn position(text: &str, needle: &str) -> Value {
    let (before, after) = needle.split_once('^').unwrap_or(("", needle));
    let offset = text
        .find(&format!("{before}{after}"))
        .unwrap_or_else(|| panic!("no `{needle}`"))
        + before.len();
    let line_start = text[..offset].rfind('\n').map_or(0, |i| i + 1);
    let line = text[..offset].matches('\n').count();
    let character = text[line_start..offset].encode_utf16().count();
    json!({ "line": line, "character": character })
}

/// The text a range covers, the range in UTF-16 code units.
pub fn slice<'a>(text: &'a str, range: &Value) -> &'a str {
    let a = offset(text, &range["start"]);
    let b = offset(text, &range["end"]);
    &text[a..b]
}

pub fn offset(text: &str, pos: &Value) -> usize {
    let line = pos["line"].as_u64().unwrap() as usize;
    let character = pos["character"].as_u64().unwrap() as usize;
    let start: usize = text.split_inclusive('\n').take(line).map(str::len).sum();
    let mut units = 0;
    for (i, c) in text[start..].char_indices() {
        if units >= character || c == '\n' {
            return start + i;
        }
        units += c.len_utf16();
    }
    text.len()
}

/// Applies the edits of a `WorkspaceEdit` for one file to its text.
pub fn apply(text: &str, edits: &Value) -> String {
    let mut edits: Vec<(usize, usize, String)> = edits
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                offset(text, &e["range"]["start"]),
                offset(text, &e["range"]["end"]),
                e["newText"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    edits.sort_by_key(|e| std::cmp::Reverse(e.0));
    let mut out = text.to_string();
    for (a, b, t) in edits {
        out.replace_range(a..b, &t);
    }
    out
}
