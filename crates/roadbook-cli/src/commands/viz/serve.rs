//! `roadbook viz --watch`: rebuild when a chart changes, and serve the output directory on
//! `127.0.0.1` with automatic reload. Standard library only: the files are polled twice a
//! second, and each served HTML page polls `/__roadbook/version`, reloading when it changes.

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
                    let _ = handle(stream, &root, &version);
                });
            }
        });
    }
    eprintln!(
        "serving `{}` at http://127.0.0.1:{port}/ (Ctrl-C to stop)",
        args.out.display()
    );

    let mut seen = snapshot(ws, &args.paths, &built.inputs);
    loop {
        std::thread::sleep(Duration::from_millis(500));
        let now = snapshot(ws, &args.paths, &built.inputs);
        if now == seen {
            continue;
        }
        seen = now;
        eprintln!("change detected, rebuilding");
        let (_, b) = build(ws, &args.paths, &args.out, args.self_contained);
        if !b.inputs.is_empty() {
            built = b;
            // The files read may have changed (a new import): watch them from now on.
            seen = snapshot(ws, &args.paths, &built.inputs);
        }
        version.fetch_add(1, Ordering::SeqCst);
    }
}

/// The modification time and length of every chart: the files given and every file the last
/// build read.
fn snapshot(
    ws: &Workspace,
    paths: &[PathBuf],
    read: &[PathBuf],
) -> Vec<(PathBuf, Option<SystemTime>, u64)> {
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

fn handle(mut stream: TcpStream, root: &Path, version: &AtomicU64) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request = String::new();
    reader.read_line(&mut request)?;
    // Read the headers to the blank line; none are needed.
    let mut line = String::new();
    while reader.read_line(&mut line)? > 2 {
        line.clear();
    }
    let mut parts = request.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or("/"));
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
