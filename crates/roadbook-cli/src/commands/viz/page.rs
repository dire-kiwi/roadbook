//! The HTML pages `roadbook viz` writes. A page is a shell: its data as JSON (read by the viewer
//! script, see `PageData` in `packages/viewer/src/project.ts`) and the viewer script, by
//! reference or inline. The viewer builds everything else.

use std::collections::HashMap;

use roadbook_compiler::Ir;
use roadbook_ir::{Effect, InvokeKind, Machine, Source, State, Trigger, TypeRef};
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

/// What a chart page holds besides its IR.
pub enum Extra<'a> {
    None,
    /// A trace's file name and text, which the page opens in replay mode.
    Trace(&'a str, &'a str),
    /// Traces (file name and text) whose coverage the page opens with.
    Coverage(Vec<(&'a str, &'a str)>),
}

/// The page of one entry chart: every machine of its IR. `index`: the href of the project
/// index.
pub fn chart_page(
    entry: &str,
    ir: &Ir,
    index: Option<&str>,
    extra: &Extra,
    script: &Script,
) -> String {
    let (mode, extra) = match extra {
        Extra::Trace(name, text) => (
            "replay",
            format!(",\"trace\":{}", json!({ "name": name, "text": text })),
        ),
        Extra::Coverage(traces) => (
            "coverage",
            format!(
                ",\"trace\":null,\"coverage\":{}",
                Value::Array(
                    traces
                        .iter()
                        .map(|(name, text)| json!({ "name": name, "text": text }))
                        .collect()
                )
            ),
        ),
        Extra::None => ("explore", ",\"trace\":null".to_string()),
    };
    page(
        &page_title(entry, ir),
        mode,
        entry,
        ir,
        index,
        &extra,
        script,
    )
}

/// The page of one entry chart of `roadbook viz --diff`: the merged IR (see `diff.rs`) and what
/// changed, `diff` (`ChartDiff` in `packages/viewer/src/diff.ts`).
pub fn diff_page(
    entry: &str,
    ir: &Ir,
    diff: &Value,
    index: Option<&str>,
    script: &Script,
) -> String {
    let title = if ir.machines.is_empty() {
        entry.to_string()
    } else {
        page_title(entry, ir)
    };
    let extra = format!(",\"trace\":null,\"diff\":{diff}");
    page(
        &format!("Changes · {title}"),
        "diff",
        entry,
        ir,
        index,
        &extra,
        script,
    )
}

/// `Checkout · examples/shop/checkout.road`: the entry file's first machine, not one it imports.
fn page_title(entry: &str, ir: &Ir) -> String {
    let name = ir
        .machines
        .iter()
        .find(|(_, m)| m.span.file == 0)
        .or_else(|| ir.machines.iter().next())
        .map_or("Roadbook", |(name, _)| name.as_str());
    format!("{name} · {entry}")
}

fn page(
    title: &str,
    mode: &str,
    entry: &str,
    ir: &Ir,
    index: Option<&str>,
    extra: &str,
    script: &Script,
) -> String {
    // Written by hand rather than through `serde_json::Value`, whose maps would sort the IR's
    // members: the viewer shows machines, events and declarations in source order.
    let data = format!(
        "{{\"kind\":\"chart\",\"mode\":\"{mode}\",\"entry\":{},\"index\":{}{extra},\"ir\":{}}}",
        Value::from(entry),
        index.map_or(Value::Null, Value::from),
        serde_json::to_string(ir).unwrap_or_default()
    );
    shell(title, &data, script)
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

/// A source's path; a file of another package as imports write it: `inventory/inventory.road`.
fn source_path(src: Option<&Source>) -> String {
    match src {
        None => "?".into(),
        Some(Source {
            path,
            package: Some(p),
        }) => format!("{p}/{path}"),
        Some(s) => s.path.clone(),
    }
}

/// The data of the project index (`ProjectIndex` in `packages/viewer/src/project.ts`, which
/// builds the same from IRs): every machine once, identified by its name and source file,
/// with the first page that draws it, which machines it invokes and spawns, and which it talks
/// to: the machines it holds a `ref` to, the events it sends, the emitted events it handles, and
/// the calls it makes (`invoke inventory.reserve(…)`), each as `{"to": i, "calls": ["reserve"]}`.
pub fn project_index(title: &str, charts: &[(&str, &Ir)], names: &[String]) -> Value {
    let key = |ir: &Ir, name: &str, m: &Machine| {
        let file = source_path(ir.sources.get(m.span.file as usize));
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
            let source = source_path(ir.sources.get(m.span.file as usize));
            machines.push(json!({
                "name": name,
                "source": format!("{source}:{}", m.span.line),
                "doc": m.doc,
                "signature": signature(name, m),
                "states": count(&m.root),
                "page": page,
                "invokes": [],
                "spawns": [],
                "refs": [],
                "sends": [],
                "receives": [],
                "calls": [],
                "ends": [],
            }));
        }
    }
    for (page, (_, ir)) in charts.iter().enumerate() {
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
            // Each machine is listed once, from the first page that has it: its relations too.
            if machines[from]["page"] != json!(page) {
                continue;
            }
            let at = |machine: &str| {
                let other = ir.machines.get(machine)?;
                keys.get(&key(ir, machine, other)).copied()
            };
            let t = talks(ir, m);
            let entry = &mut machines[from];
            for (machine, names) in t.spawns {
                if let Some(to) = at(&machine) {
                    push(entry, "spawns", json!({ "to": to, "names": names }));
                }
            }
            for (machine, names) in t.refs {
                if let Some(to) = at(&machine) {
                    push(entry, "refs", json!({ "to": to, "names": names }));
                }
            }
            for (machine, events) in t.sends {
                if let Some(to) = at(&machine) {
                    push(entry, "sends", json!({ "to": to, "events": events }));
                }
            }
            for (machine, events) in t.receives {
                if let Some(sender) = at(&machine) {
                    push(
                        entry,
                        "receives",
                        json!({ "from": sender, "events": events }),
                    );
                }
            }
            for (machine, calls) in t.calls {
                if let Some(to) = at(&machine) {
                    push(entry, "calls", json!({ "to": to, "calls": calls }));
                }
            }
            for (machine, names) in t.ends {
                if let Some(to) = at(&machine) {
                    push(entry, "ends", json!({ "to": to, "names": names }));
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

/// `name: T`, or `sensitive name: T`: a field or parameter as written.
pub fn field_text(f: &roadbook_ir::Field) -> String {
    let sensitive = if f.sensitive { "sensitive " } else { "" };
    format!("{sensitive}{}: {}", f.name, f.ty)
}

/// `[strict] machine Name(p: T) -> R ! E | F`
pub fn signature(name: &str, m: &Machine) -> String {
    let params: Vec<String> = m.params.iter().map(field_text).collect();
    let strict = if m.strict { "strict " } else { "" };
    let mut s = format!("{strict}machine {name}({})", params.join(", "));
    if let Some(r) = &m.returns {
        s.push_str(&format!(" -> {r}"));
    }
    if !m.errors.is_empty() {
        s.push_str(&format!(" ! {}", m.errors.join(" | ")));
    }
    s
}

fn push(entry: &mut Value, list: &str, item: Value) {
    if let Some(list) = entry[list].as_array_mut() {
        list.push(item);
    }
}

/// Values grouped by machine, in first-seen order, each list without repeats.
type Grouped = Vec<(String, Vec<String>)>;

fn group(list: &mut Grouped, machine: &str, value: &str) {
    let at = match list.iter().position(|(m, _)| m == machine) {
        Some(at) => at,
        None => {
            list.push((machine.to_string(), Vec::new()));
            list.len() - 1
        }
    };
    let values = &mut list[at].1;
    if !values.iter().any(|v| v == value) {
        values.push(value.to_string());
    }
}

/// Which machines a machine talks to (`talks` in `packages/viewer/src/model.ts`).
struct Talks {
    /// Machines it spawns, with the collections of children holding them.
    spawns: Grouped,
    /// Machines it holds a `ref` to, with the parameters and variables holding one.
    refs: Grouped,
    /// Events it sends, by receiving machine.
    sends: Grouped,
    /// Events of other machines' `emits` it handles, by sending machine.
    receives: Grouped,
    /// Calls it makes, by called machine.
    calls: Grouped,
    /// Peers whose end it hears (`on name.ended`), by peer machine, with the names it watches
    /// them through.
    ends: Grouped,
}

fn talks(ir: &Ir, m: &Machine) -> Talks {
    let mut t = Talks {
        spawns: Vec::new(),
        refs: Vec::new(),
        sends: Vec::new(),
        receives: Vec::new(),
        calls: Vec::new(),
        ends: Vec::new(),
    };
    for (name, c) in &m.children {
        group(&mut t.spawns, &c.src, name);
    }
    for (name, ty) in m
        .params
        .iter()
        .map(|p| (&p.name, &p.ty))
        .chain(m.vars.iter().map(|v| (&v.name, &v.ty)))
    {
        if let Some(to) = ref_machine(ir, ty) {
            group(&mut t.refs, &to, name);
        }
    }
    messages(&m.root, &mut t);
    t
}

/// The machine a type is a handle to: `ref M`, with any suffixes, or an alias of one.
fn ref_machine(ir: &Ir, ty: &TypeRef) -> Option<String> {
    let mut ty = ty.clone();
    for _ in 0..32 {
        match ir.expand_top(ty.base()).into_owned() {
            TypeRef::Ref(m) => return Some(m),
            next @ (TypeRef::List(_) | TypeRef::Optional(_)) => ty = next,
            _ => return None,
        }
    }
    None
}

/// The sends, handled emitted events, calls and peers' ends heard of `s` and the states inside
/// it, in document order: each state's call invoke, `entry`, `exit`, then each transition's
/// trigger and effects.
fn messages(s: &State, t: &mut Talks) {
    fn effects(list: &[Effect], t: &mut Talks) {
        for e in list {
            if let Effect::Send { to, event, .. } = e {
                group(&mut t.sends, to.peer(), event);
            }
        }
    }
    if let Some(inv) = &s.invoke
        && inv.kind == InvokeKind::Call
        && let Some(to) = &inv.to
    {
        group(&mut t.calls, to.peer(), &inv.src);
    }
    effects(&s.entry, t);
    effects(&s.exit, t);
    for tr in &s.transitions {
        match &tr.trigger {
            Trigger::Emitted { from, event } => group(&mut t.receives, from.peer(), event),
            Trigger::Ended { from } => group(&mut t.ends, from.peer(), &from.name),
            _ => {}
        }
        effects(&tr.effects, t);
    }
    for c in &s.states {
        messages(c, t);
    }
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

    /// A call is an edge from the caller to the callee, with the calls made: `calls` beside
    /// `sends` (`IndexMachine` in `packages/viewer/src/project.ts`).
    #[test]
    fn the_index_lists_the_calls_each_machine_makes() {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../spec/conformance/charts/calls.road"),
        )
        .unwrap();
        let out = roadbook_compiler::compile_source("calls.road", &src, &Default::default());
        assert!(!out.has_errors(), "{}", out.render());
        let ir = out.ir.unwrap();
        let index = project_index("t", &[("calls.road", &ir)], &["calls.html".into()]);
        let machines = index["machines"].as_array().unwrap();
        let names: Vec<&str> = machines
            .iter()
            .map(|m| m["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["Inventory", "Order", "Counter"]);
        assert_eq!(machines[0]["calls"], json!([]));
        assert_eq!(
            machines[1]["calls"].to_string(),
            r#"[{"calls":["reserve"],"to":0}]"#
        );
        assert_eq!(
            machines[2]["calls"],
            json!([{ "to": 0, "calls": ["count"] }])
        );
        // A call is neither an invoke of a machine nor a send.
        for m in machines {
            assert_eq!(m["invokes"], json!([]), "{m}");
            assert_eq!(m["sends"], json!([]), "{m}");
        }
        assert_eq!(
            machines[1]["refs"],
            json!([{ "to": 0, "names": ["inventory"] }])
        );
    }

    /// A peer's end heard (`on name.ended`) is an edge from the holder to the peer, with the
    /// names it watches it through: `ends` beside `refs` (`IndexMachine` in
    /// `packages/viewer/src/project.ts`). A holder with no `on name.ended` has none.
    #[test]
    fn the_index_lists_the_peers_whose_end_each_machine_hears() {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../spec/conformance/charts/ended.road"),
        )
        .unwrap();
        let out = roadbook_compiler::compile_source("ended.road", &src, &Default::default());
        assert!(!out.has_errors(), "{}", out.render());
        let ir = out.ir.unwrap();
        let index = project_index("t", &[("ended.road", &ir)], &["ended.html".into()]);
        let machines = index["machines"].as_array().unwrap();
        let at = |name: &str| machines.iter().position(|m| m["name"] == name).unwrap();
        let inventory = at("Inventory");
        let ends = |name: &str| machines[at(name)]["ends"].clone();
        assert_eq!(
            ends("Order"),
            json!([{ "to": inventory, "names": ["inventory"] }])
        );
        assert_eq!(
            ends("Picker"),
            json!([{ "to": inventory, "names": ["inventory"] }])
        );
        assert_eq!(
            ends("Switcher"),
            json!([{ "to": inventory, "names": ["current"] }])
        );
        assert_eq!(ends("Clerk"), json!([]));
        assert_eq!(ends("Inventory"), json!([]));
        // It holds the ref all the same.
        assert_eq!(
            machines[at("Clerk")]["refs"],
            json!([{ "to": inventory, "names": ["inventory"] }])
        );
    }

    #[test]
    fn inline_scripts_cannot_close_early() {
        assert_eq!(inline_js("a</script>b<!--c"), "a<\\/script>b<\\!--c");
    }

    /// Spawned children are a relation of their own, and what is sent to them or received
    /// from them counts as for any other machine (`projectIndex` in the viewer builds the same).
    #[test]
    fn the_index_records_spawns_and_messages_with_spawned_children() {
        let ir = Ir::from_json(include_str!(
            "../../../../../spec/conformance/charts/hub.ir.json"
        ))
        .unwrap();
        let index = project_index("hub", &[("hub.road", &ir)], &["hub.html".into()]);
        let machines = index["machines"].as_array().unwrap();
        let at = |name: &str| machines.iter().position(|m| m["name"] == name).unwrap();
        let (hub, pool, session) = (&machines[at("Hub")], &machines[at("Pool")], at("Session"));
        assert_eq!(
            hub["spawns"],
            json!([{ "to": session, "names": ["sessions"] }])
        );
        assert_eq!(
            pool["spawns"],
            json!([{ "to": session, "names": ["slots"] }])
        );
        assert_eq!(hub["invokes"], json!([]));
        assert_eq!(
            hub["sends"],
            json!([{ "to": session, "events": ["Echo", "Notify", "Leave", "Drop", "Crash"] }])
        );
        assert_eq!(
            hub["receives"],
            json!([{ "from": session, "events": ["Said"] }])
        );
        assert_eq!(machines[session]["spawns"], json!([]));
    }
}
