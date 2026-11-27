//! What the compiler's symbol index has no symbol for, read from the text: the built-in enum
//! `Ended` and its variants, `event` and `event.status` in an `on name.ended` transition and
//! the word `ended` there; and completion after a dot (`on name.`, `Ended.`, `event.`).
//!
//! Like the outline, this works from the syntax tree, so it answers while a line is still
//! being written and the file does not compile.

use lsp_types::{CompletionItem, CompletionItemKind, Documentation, MarkupContent, MarkupKind};
use roadbook_syntax::ast::{Body, DeclKind, File, Item, Machine, TypeKind, TypeRef};
use roadbook_syntax::lexer::{Punct, TokenKind, lex};
use roadbook_syntax::{FileId, parse_file};

/// The variants of the built-in enum `Ended`, with what each says about the peer's run.
pub const VARIANTS: [(&str, &str); 5] = [
    ("Done", "it finished, with its output"),
    ("Failed", "it failed with one of its errors"),
    ("Cancelled", "it was cancelled"),
    ("Defect", "it ended with a defect"),
    (
        "Suspended",
        "it was suspended: saved and stopped, to go on as another run no holder can reach",
    ),
];

const ENDED_DOC: &str = "How a peer's run ended: the type of `event.status` in `on name.ended`. Built in, so nothing can be declared with its name.";

/// A hover's text, in Markdown, and the byte range of the word it is about.
pub struct Hover {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

fn markdown(signature: &str, what: &str, doc: &str) -> String {
    format!("```roadbook\n{signature}\n```\n\n{what}\n\n---\n\n{doc}")
}

fn enum_signature() -> String {
    let names: Vec<&str> = VARIANTS.iter().map(|(v, _)| *v).collect();
    format!("enum Ended {{ {} }}", names.join(", "))
}

/// The hover for a built-in name at byte `offset` of `text`, if there is one there.
pub fn hover(text: &str, offset: usize) -> Option<Hover> {
    let tokens = lex(FileId(0), text).tokens;
    let i = tokens.iter().position(|t| {
        matches!(
            t.kind,
            TokenKind::Upper | TokenKind::Lower | TokenKind::Keyword(_)
        ) && t.span.start.offset as usize <= offset
            && offset <= t.span.end.offset as usize
    })?;
    let word = |k: usize| -> &str {
        let t = &tokens[k];
        &text[t.span.start.offset as usize..t.span.end.offset as usize]
    };
    let dot = |k: usize| tokens[k].kind == TokenKind::Punct(Punct::Dot);
    let at = |text: String| {
        Some(Hover {
            start: tokens[i].span.start.offset as usize,
            end: tokens[i].span.end.offset as usize,
            text,
        })
    };
    let this = word(i);
    // `Ended.Done`: a variant of the built-in enum.
    if i >= 2 && dot(i - 1) && word(i - 2) == "Ended" {
        let (v, what) = VARIANTS.iter().find(|(v, _)| *v == this)?;
        return at(markdown(
            &format!("Ended.{v}"),
            "variant of the built-in enum `Ended`",
            &format!("The peer's run ended this way: {what}."),
        ));
    }
    if this == "Ended" && tokens[i].kind == TokenKind::Upper {
        return at(markdown(&enum_signature(), "built-in enum", ENDED_DOC));
    }
    // `on name.ended`: the trigger.
    if this == "ended"
        && i >= 3
        && dot(i - 1)
        && tokens[i - 2].kind == TokenKind::Lower
        && tokens[i - 3].kind == TokenKind::Keyword(roadbook_syntax::lexer::Keyword::On)
    {
        let name = word(i - 2);
        return at(markdown(
            &format!("on {name}.ended"),
            "a peer's end",
            &format!(
                "Taken once, when the run `{name}` holds ends, however it ends: it finished, failed, was cancelled, ended with a defect or was suspended. `event.status` says which. Writing it is the subscription: a holder with no `on {name}.ended` is not told."
            ),
        ));
    }
    // `event` and `event.status` in an `on name.ended` transition.
    let (event, status) = if this == "event" && i + 2 < tokens.len() && dot(i + 1) {
        (i, i + 2)
    } else if this == "status" && i >= 2 && dot(i - 1) {
        (i - 2, i)
    } else {
        return None;
    };
    if word(event) != "event" || word(status) != "status" {
        return None;
    }
    let (file, _) = parse_file(FileId(0), text);
    let from = ended_from(&file, offset)?;
    if i == event {
        at(markdown(
            "event: { status: Ended }",
            &format!("the end of the run `{from}` holds"),
            "Read only as `event.status`: nothing else comes with a peer's end, not its output, its error or its defect.",
        ))
    } else {
        at(markdown(
            "event.status: Ended",
            &format!("how the run `{from}` holds ended"),
            &format!(
                "A variant of the built-in enum `Ended`: {}.",
                VARIANTS
                    .iter()
                    .map(|(v, _)| format!("`Ended.{v}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ))
    }
}

/// The name of the peer whose end the `on name.ended` transition around `offset` takes.
fn ended_from(file: &File, offset: usize) -> Option<String> {
    fn body(b: &Body, offset: usize) -> Option<String> {
        let covers = |s: &roadbook_syntax::Span| {
            s.start.offset as usize <= offset && offset <= s.end.offset as usize
        };
        for item in &b.items {
            match item {
                Item::Transition(t) if covers(&t.span) => {
                    return match &t.trigger {
                        roadbook_syntax::ast::Trigger::Ended { from } => Some(from.name.clone()),
                        _ => None,
                    };
                }
                Item::State(s) if covers(&s.span) => return body(&s.body, offset),
                _ => {}
            }
        }
        None
    }
    file.machines()
        .find(|(d, _)| {
            d.span.start.offset as usize <= offset && offset <= d.span.end.offset as usize
        })
        .and_then(|(_, m)| body(&m.body, offset))
}

/// The machine a `ref` type is a handle to: `ref M`, `ref M?`, `ref M[]`, or an alias of one
/// declared in the file.
fn ref_machine(file: &File, ty: &TypeRef) -> Option<String> {
    let mut ty = ty;
    for _ in 0..32 {
        match &ty.kind {
            TypeKind::Ref(m) => return Some(m.name.clone()),
            TypeKind::Optional(e) | TypeKind::List(e) => match &e.kind {
                TypeKind::Ref(m) => return Some(m.name.clone()),
                TypeKind::Named(n) => ty = alias(file, &n.name)?,
                _ => return None,
            },
            TypeKind::Named(n) => ty = alias(file, &n.name)?,
            _ => return None,
        }
    }
    None
}

fn alias<'f>(file: &'f File, name: &str) -> Option<&'f TypeRef> {
    file.decls().find_map(|d| match &d.kind {
        DeclKind::Alias { params, target } if d.name.name == name && params.is_empty() => {
            Some(target)
        }
        _ => None,
    })
}

/// The machine the `ref` parameter or variable `name` of `m` is a handle to.
fn peer_machine(file: &File, m: &Machine, name: &str) -> Option<String> {
    m.params
        .iter()
        .map(|p| (&p.name, &p.ty))
        .chain(m.body.vars().map(|v| (&v.name, &v.ty)))
        .find(|(n, _)| n.name == name)
        .and_then(|(_, ty)| ref_machine(file, ty))
}

fn is_name(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn item(label: &str, kind: CompletionItemKind, detail: String, doc: String) -> CompletionItem {
    CompletionItem {
        label: label.into(),
        kind: Some(kind),
        detail: Some(detail),
        documentation: Some(Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: doc,
        })),
        ..CompletionItem::default()
    }
}

/// What can follow the dot before byte `offset` of `text`: `ended` (and the peer's emitted
/// events, which `emitted` lists for a machine) after `on name.` for a `ref`, the variants
/// after `Ended.`, and `status` after `event.` in an `on name.ended` transition.
pub fn complete(
    text: &str,
    offset: usize,
    emitted: impl Fn(&str) -> Vec<String>,
) -> Vec<CompletionItem> {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let before = &text[..offset];
    let start = before.trim_end_matches(is_name).len();
    let Some(head) = before[..start].strip_suffix('.') else {
        return Vec::new();
    };
    let name = &head[head.trim_end_matches(is_name).len()..];
    // The text as it will be once the word is written, so that it parses.
    let written = |word: &str| format!("{}{word}{}", &text[..start], &text[offset..]);
    match name {
        "Ended" => VARIANTS
            .iter()
            .map(|(v, what)| {
                item(
                    v,
                    CompletionItemKind::ENUM_MEMBER,
                    "Ended".into(),
                    format!("The peer's run ended this way: {what}."),
                )
            })
            .collect(),
        "event" => {
            let (file, _) = parse_file(FileId(0), &written("status"));
            match ended_from(&file, start) {
                Some(from) => vec![item(
                    "status",
                    CompletionItemKind::FIELD,
                    "Ended".into(),
                    format!("How the run `{from}` holds ended."),
                )],
                None => Vec::new(),
            }
        }
        "" => Vec::new(),
        _ => {
            let rest = head[..head.len() - name.len()].trim_end();
            let on = rest
                .strip_suffix("on")
                .is_some_and(|r| !r.ends_with(is_name));
            if !on {
                return Vec::new();
            }
            let (file, _) = parse_file(FileId(0), &written("ended"));
            let Some(machine) = file
                .machines()
                .find(|(d, _)| {
                    d.span.start.offset as usize <= start && start <= d.span.end.offset as usize
                })
                .and_then(|(_, m)| peer_machine(&file, m, name))
            else {
                return Vec::new();
            };
            let mut out = vec![item(
                "ended",
                CompletionItemKind::KEYWORD,
                format!("the run `{name}` holds ended"),
                format!(
                    "`on {name}.ended` is taken once, when the run `{name}` holds ends, however it ends; `event.status` (`Ended`) says how."
                ),
            )];
            // The peer's `emits`: from this file's text if it is declared here, else from the
            // last compilation (the index has nothing for a file that does not parse).
            let here = file
                .machines()
                .find(|(d, _)| d.name.name == machine)
                .map(|(_, m)| {
                    m.body
                        .items
                        .iter()
                        .filter_map(|i| match i {
                            Item::Emits(e) => Some(e.name.name.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                });
            for e in here.unwrap_or_else(|| emitted(&machine)) {
                out.push(item(
                    &e,
                    CompletionItemKind::EVENT,
                    format!("emits {e}"),
                    format!("An event `{machine}` emits."),
                ));
            }
            out
        }
    }
}
