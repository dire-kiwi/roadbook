//! The HTML pages `roadbook viz` writes. A page is a shell: its data as JSON (read by the viewer
//! script, see `PageData` in `packages/viewer/src/project.ts`) and the viewer script, by
//! reference or inline. The viewer builds everything else.

use std::collections::HashMap;

use roadbook_compiler::Ir;
use roadbook_ir::{InvokeKind, Machine, State};
use serde_json::{Value, json};

/// The project index's file name.
pub const INDEX: &str = "index.html";

/// How a page gets the viewer script.
pub enum Script<'a> {
    /// `<script src="…">`, a file next to the page.
    Shared(&'a str),
    /// The script's text, inside the page.
    Inline(&'a str),
}

/// The href of a page or file next to the current one: `./` and the name percent-encoded, so
/// whatever the file is called (`javascript:alert(1).html`, `a#b?.html`) it is a relative
/// path to that file.
pub fn href(name: &str) -> String {
    let mut out = String::from("./");
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The page of one entry chart: every machine of its IR. `index`: the href of the project
/// index.
pub fn chart_page(entry: &str, ir: &Ir, index: Option<&str>, script: &Script) -> String {
    // The entry file's first machine, not one it imports.
    let title = ir
        .machines
        .iter()
        .find(|(_, m)| m.span.file == 0)
        .or_else(|| ir.machines.iter().next())
        .map_or("Roadbook", |(name, _)| name.as_str());
    // Written by hand rather than through `serde_json::Value`, whose maps would sort the IR's
    // members: the viewer shows machines, events and declarations in source order.
    let data = format!(
        "{{\"kind\":\"chart\",\"mode\":\"explore\",\"entry\":{},\"index\":{},\"ir\":{}}}",
        Value::from(entry),
        index.map_or(Value::Null, Value::from),
        serde_json::to_string(ir).unwrap_or_default()
    );
    shell(&format!("{title} · {entry}"), &data, script)
}

/// The project index page.
pub fn index_page(title: &str, index: &Value, script: &Script) -> String {
    let data = json!({ "kind": "index", "index": index });
    shell(&format!("{title}: machines"), &data.to_string(), script)
}

fn shell(title: &str, data: &str, script: &Script) -> String {
    // In a <script> element, every `<` is written as the JSON escape for it (it occurs only
    // inside strings), so the JSON cannot close the element; JSON.parse reads it back.
    let json = data.replace('<', "\\u003c");
    let script = match script {
        Script::Shared(src) => format!("<script src=\"{}\"></script>", attr(src)),
        Script::Inline(js) => format!("<script>\n{}\n</script>", inline_js(js)),
    };
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <meta name=\"generator\" content=\"roadbook viz\">\n<title>{} · Roadbook</title>\n</head>\n<body>\n\
         <noscript>This page draws a Roadbook chart with JavaScript; turn it on to see the diagram.</noscript>\n\
         <script type=\"application/json\" id=\"rb-data\">{json}</script>\n{script}\n</body>\n</html>\n",
        text(title)
    )
}

/// Script text that cannot end its `<script>` element early.
fn inline_js(js: &str) -> String {
    js.replace("</script", "<\\/script")
        .replace("</SCRIPT", "<\\/SCRIPT")
        .replace("<!--", "<\\!--")
}

fn text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn attr(s: &str) -> String {
    text(s).replace('"', "&quot;")
}

/// File names for the pages of the entries (project paths): each path relative to the
/// directory the entries share, `/` replaced by `-`, `.road` by `.html`. Never `index.html`,
/// and unique.
pub fn file_names(entries: &[&str]) -> Vec<String> {
    let dirs: Vec<Vec<&str>> = entries
        .iter()
        .map(|e| {
            let mut parts: Vec<&str> = e.split('/').collect();
            parts.pop();
            parts
        })
        .collect();
    let common = dirs.first().map_or(0, |first| {
        (0..first.len())
            .take_while(|&i| dirs.iter().all(|d| d.get(i) == first.get(i)))
            .count()
    });
    let mut used: HashMap<String, usize> = HashMap::new();
    entries
        .iter()
        .map(|e| {
            let rel: Vec<&str> = e.split('/').skip(common).collect();
            let stem = rel.join("-");
            let stem = stem.strip_suffix(".road").unwrap_or(&stem).to_string();
            let mut name = format!("{stem}.html");
            if name == INDEX {
                name = format!("{stem}-chart.html");
            }
            let n = used.entry(name.clone()).or_insert(0);
            *n += 1;
            if *n > 1 {
                name = format!("{stem}-{n}.html");
            }
            name
        })
        .collect()
}

/// The data of the project index (`ProjectIndex` in `packages/viewer/src/project.ts`, which
/// builds the same from IRs): every machine once, identified by its name and source file,
/// with the first page that draws it, and which machines it invokes.
pub fn project_index(title: &str, charts: &[(&str, &Ir)], names: &[String]) -> Value {
    let key = |ir: &Ir, name: &str, m: &Machine| {
        let file = ir
            .sources
            .get(m.span.file as usize)
            .map_or("", |s| s.path.as_str());
        format!("{file}\u{0}{name}")
    };
    let mut keys: HashMap<String, usize> = HashMap::new();
    let mut machines: Vec<Value> = Vec::new();
    for (page, (_, ir)) in charts.iter().enumerate() {
        for (name, m) in &ir.machines {
            let k = key(ir, name, m);
            if keys.contains_key(&k) {
                continue;
            }
            keys.insert(k, machines.len());
            let source = ir
                .sources
                .get(m.span.file as usize)
                .map_or("?", |s| s.path.as_str());
            machines.push(json!({
                "name": name,
                "source": format!("{source}:{}", m.span.line),
                "doc": m.doc,
                "signature": signature(name, m),
                "states": count(&m.root),
                "page": page,
                "invokes": [],
            }));
        }
    }
    for (_, ir) in charts {
        for (name, m) in &ir.machines {
            let from = keys[&key(ir, name, m)];
            let mut invoked = Vec::new();
            invokes(&m.root, &mut invoked);
            for child in invoked {
                let Some(cm) = ir.machines.get(&child) else {
                    continue;
                };
                let Some(&to) = keys.get(&key(ir, &child, cm)) else {
                    continue;
                };
                if let Some(list) = machines[from]["invokes"].as_array_mut()
                    && !list.contains(&json!(to))
                {
                    list.push(json!(to));
                }
            }
        }
    }
    let pages: Vec<Value> = charts
        .iter()
        .zip(names)
        .map(|((entry, _), name)| json!({ "href": href(name), "entry": entry }))
        .collect();
    json!({ "title": title, "pages": pages, "machines": machines })
}

/// `machine Name(p: T) -> R ! E | F`
pub fn signature(name: &str, m: &Machine) -> String {
    let params: Vec<String> = m
        .params
        .iter()
        .map(|p| format!("{}: {}", p.name, p.ty))
        .collect();
    let mut s = format!("machine {name}({})", params.join(", "));
    if let Some(r) = &m.returns {
        s.push_str(&format!(" -> {r}"));
    }
    if !m.errors.is_empty() {
        s.push_str(&format!(" ! {}", m.errors.join(" | ")));
    }
    s
}

fn count(s: &State) -> usize {
    s.states.iter().map(|c| 1 + count(c)).sum()
}

fn invokes(s: &State, out: &mut Vec<String>) {
    if let Some(inv) = &s.invoke
        && inv.kind == InvokeKind::Machine
        && !out.contains(&inv.src)
    {
        out.push(inv.src.clone());
    }
    for c in &s.states {
        invokes(c, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_file_names() {
        assert_eq!(
            file_names(&["examples/shop/place_order.road"]),
            ["place_order.html"]
        );
        assert_eq!(
            file_names(&[
                "examples/shop/place_order.road",
                "examples/large/ticket.road"
            ]),
            ["shop-place_order.html", "large-ticket.html"]
        );
        assert_eq!(
            file_names(&["a/index.road", "b-c.road", "b/c.road"]),
            ["a-index.html", "b-c.html", "b-c-2.html"]
        );
        assert_eq!(file_names(&["index.road"]), ["index-chart.html"]);
    }

    #[test]
    fn hrefs_are_relative_and_percent_encoded() {
        assert_eq!(href("index.html"), "./index.html");
        assert_eq!(
            href("javascript:alert(1).html"),
            "./javascript%3Aalert%281%29.html"
        );
        assert_eq!(
            href("a b#c?d\"e<f>.html"),
            "./a%20b%23c%3Fd%22e%3Cf%3E.html"
        );
        assert_eq!(href("ü.html"), "./%C3%BC.html");
    }

    #[test]
    fn inline_scripts_cannot_close_early() {
        assert_eq!(inline_js("a</script>b<!--c"), "a<\\/script>b<\\!--c");
    }
}
