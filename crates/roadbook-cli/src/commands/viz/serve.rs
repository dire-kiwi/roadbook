//! `roadbook viz --watch`: rebuild when a chart changes, and serve the output directory on
//! `127.0.0.1` with automatic reload. Standard library only: the files are polled twice a
//! second, and each served HTML page polls `/__roadbook/version`, reloading when it changes.
//!
//! Only requests addressed to this server by a local name are answered (`Host` is
//! `localhost`, `127.0.0.1` or `[::1]` with its port), so a page on another site cannot read
//! the charts through a DNS name it points at 127.0.0.1 (DNS rebinding).

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use super::{Args, Built, build};
use crate::workspace::Workspace;
use crate::{Exit, report};

const VERSION_PATH: &str = "/__roadbook/version";

/// Added to every HTML page served, before `</body>`.
const RELOAD: &str = "<script>(function(){var v=null;setInterval(function(){fetch('/__roadbook/version',{cache:'no-store'}).then(function(r){return r.text()}).then(function(t){if(v!==null&&t!==v)location.reload();v=t}).catch(function(){})},700)})()</script>";

pub fn watch(ws: &Workspace, args: &Args, mut built: Built) -> Exit {
    if let Err(e) = std::fs::create_dir_all(&args.out) {
        return report::fatal(format!("cannot create `{}`: {e}", args.out.display()));
    }
    let listener = match TcpListener::bind(("127.0.0.1", args.port)) {
        Ok(l) => l,
        Err(e) => return report::fatal(format!("cannot listen on port {}: {e}", args.port)),
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(args.port);
    let version = Arc::new(AtomicU64::new(1));
    {
        let version = Arc::clone(&version);
        let root = args.out.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let version = Arc::clone(&version);
                let root = root.clone();
                std::thread::spawn(move || {
                    let _ = handle(stream, &root, &version, port);
                });
            }
        });
    }
    eprintln!(
        "serving `{}` at http://127.0.0.1:{port}/ (Ctrl-C to stop)",
        args.out.display()
    );

    let mut watcher = Watcher::new(ws, &args.paths, std::mem::take(&mut built.inputs));
    loop {
        std::thread::sleep(Duration::from_millis(500));
        let rebuilt = watcher.poll(ws, || {
            eprintln!("change detected, rebuilding");
            let options = super::BuildOptions {
                self_contained: args.self_contained,
                trace: args.trace.as_deref(),
            };
            let (_, b) = build(ws, &args.paths, &args.out, &options);
            (!b.inputs.is_empty()).then_some(b.inputs)
        });
        if rebuilt {
            version.fetch_add(1, Ordering::SeqCst);
        }
    }
}

type Snapshot = Vec<(PathBuf, Option<SystemTime>, u64)>;

/// What the charts looked like when the last build began.
struct Watcher {
    paths: Vec<PathBuf>,
    /// The files the last successful build read.
    inputs: Vec<PathBuf>,
    seen: Snapshot,
}

impl Watcher {
    fn new(ws: &Workspace, paths: &[PathBuf], inputs: Vec<PathBuf>) -> Watcher {
        let seen = snapshot(ws, paths, &inputs);
        Watcher {
            paths: paths.to_vec(),
            inputs,
            seen,
        }
    }

    /// Runs `build` if a chart changed since the last build began; it returns the files it
    /// read, or `None` if it failed. Returns whether it ran.
    fn poll(&mut self, ws: &Workspace, build: impl FnOnce() -> Option<Vec<PathBuf>>) -> bool {
        let now = snapshot(ws, &self.paths, &self.inputs);
        if now == self.seen {
            return false;
        }
        // Taken before building, so an edit made while the build runs is seen next time. If
        // the build reads new files (a new import), the next poll sees them as changed and
        // builds once more.
        self.seen = now;
        if let Some(inputs) = build() {
            self.inputs = inputs;
        }
        true
    }
}

/// The modification time and length of every chart: the files given and every file the last
/// build read.
fn snapshot(ws: &Workspace, paths: &[PathBuf], read: &[PathBuf]) -> Snapshot {
    let mut files: Vec<PathBuf> = ws
        .inputs(paths)
        .unwrap_or_default()
        .iter()
        .map(|p| ws.disk_path(p))
        .chain(read.iter().cloned())
        .collect();
    files.sort();
    files.dedup();
    files
        .into_iter()
        .map(|f| {
            let meta = std::fs::metadata(&f).ok();
            let modified = meta.as_ref().and_then(|m| m.modified().ok());
            let len = meta.map_or(0, |m| m.len());
            (f, modified, len)
        })
        .collect()
}

/// Whether a `Host` header names this server: a loopback name and the port it listens on
/// (which a browser leaves out only for port 80).
fn host_allowed(host: &str, port: u16) -> bool {
    let host = host.trim().to_ascii_lowercase();
    let (name, given) = match host.rsplit_once(':') {
        Some((name, p)) if !name.is_empty() && !p.contains(']') => (name, p.parse::<u16>().ok()),
        _ => (host.as_str(), Some(80)),
    };
    matches!(name, "localhost" | "127.0.0.1" | "[::1]") && given == Some(port)
}

fn handle(
    mut stream: TcpStream,
    root: &Path,
    version: &AtomicU64,
    port: u16,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request = String::new();
    reader.read_line(&mut request)?;
    // Read the headers to the blank line; only `Host` is needed.
    let mut host = None;
    let mut line = String::new();
    while reader.read_line(&mut line)? > 2 {
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("host")
        {
            host = Some(value.trim().to_string());
        }
        line.clear();
    }
    let mut parts = request.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or("/"));
    if !host.is_some_and(|h| host_allowed(&h, port)) {
        return respond(
            &mut stream,
            "403 Forbidden",
            "text/plain",
            b"this server answers only to localhost\n",
            method,
        );
    }
    if method != "GET" && method != "HEAD" {
        return respond(
            &mut stream,
            "405 Method Not Allowed",
            "text/plain",
            b"GET only\n",
            method,
        );
    }
    let path = target.split(['?', '#']).next().unwrap_or("/");
    if path == VERSION_PATH {
        let v = version.load(Ordering::SeqCst).to_string();
        return respond(&mut stream, "200 OK", "text/plain", v.as_bytes(), method);
    }
    let Some(file) = resolve(root, path) else {
        return respond(
            &mut stream,
            "404 Not Found",
            "text/plain",
            b"not found\n",
            method,
        );
    };
    let Ok(mut body) = std::fs::read(&file) else {
        return respond(
            &mut stream,
            "404 Not Found",
            "text/plain",
            b"not found\n",
            method,
        );
    };
    let kind = match file.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        _ => "application/octet-stream",
    };
    if kind.starts_with("text/html") {
        let html = String::from_utf8_lossy(&body);
        let html = match html.rfind("</body>") {
            Some(at) => format!("{}{RELOAD}\n{}", &html[..at], &html[at..]),
            None => format!("{html}{RELOAD}"),
        };
        body = html.into_bytes();
    }
    respond(&mut stream, "200 OK", kind, &body, method)
}

/// The file a URL path names inside `root`, refusing anything that leaves it.
fn resolve(root: &Path, path: &str) -> Option<PathBuf> {
    let decoded = percent_decode(path)?;
    let mut file = root.to_path_buf();
    for seg in decoded.split('/').filter(|s| !s.is_empty()) {
        if seg == "." || seg == ".." || seg.contains('\\') {
            return None;
        }
        file.push(seg);
    }
    if file.is_dir() {
        file.push(super::page::INDEX);
    }
    file.is_file().then_some(file)
}

fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn respond(
    stream: &mut TcpStream,
    status: &str,
    kind: &str,
    body: &[u8],
    method: &str,
) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    if method != "HEAD" {
        stream.write_all(body)?;
    }
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_local_hosts_with_the_port_are_served() {
        for ok in [
            "localhost:7357",
            "127.0.0.1:7357",
            "[::1]:7357",
            "LocalHost:7357",
        ] {
            assert!(host_allowed(ok, 7357), "{ok}");
        }
        for bad in [
            "evil.example:7357",
            "localhost.evil.example:7357",
            "localhost:80",
            "localhost",
            "127.0.0.1:1",
            "[::1]",
            "::1",
            "",
        ] {
            assert!(!host_allowed(bad, 7357), "{bad}");
        }
        assert!(host_allowed("localhost", 80));
        assert!(host_allowed("[::1]", 80));
    }

    #[test]
    fn an_edit_during_a_build_is_not_missed() {
        let dir = std::env::temp_dir().join(format!("roadbook-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("roadbook.toml"), "").unwrap();
        let chart = dir.join("m.road");
        std::fs::write(&chart, "a").unwrap();
        let ws = Workspace::find(Some(&dir)).unwrap();
        let mut w = Watcher::new(&ws, &[], vec![chart.clone()]);
        assert!(!w.poll(&ws, || panic!("nothing changed")));

        std::fs::write(&chart, "ab").unwrap();
        // The build reads the file, then the user saves again before it finishes.
        assert!(w.poll(&ws, || {
            std::fs::write(&chart, "abc").unwrap();
            Some(vec![chart.clone()])
        }));
        assert!(w.poll(&ws, || Some(vec![chart.clone()])), "the second edit");
        assert!(!w.poll(&ws, || panic!("nothing changed")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn paths_stay_inside_the_root() {
        let root = std::env::temp_dir().join(format!("roadbook-serve-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("index.html"), "x").unwrap();
        std::fs::write(root.join("a b.html"), "x").unwrap();
        assert_eq!(resolve(&root, "/"), Some(root.join("index.html")));
        assert_eq!(resolve(&root, "/a%20b.html"), Some(root.join("a b.html")));
        assert_eq!(resolve(&root, "/../index.html"), None);
        assert_eq!(resolve(&root, "/%2e%2e/index.html"), None);
        assert_eq!(resolve(&root, "/missing.html"), None);
    }
}
