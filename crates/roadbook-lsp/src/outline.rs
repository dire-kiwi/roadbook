//! The outline of a file: its declarations, the events a task emits, and in a machine its
//! parameters, variables, events (internal ones among them), `children` collections and
//! states (history states among them), nested as they are written. Built from the syntax
//! tree alone, so it is there even when the file does not compile.

use lsp_types::{DocumentSymbol, SymbolKind as Kind};
use roadbook_compiler::index::{declaration, type_text};
use roadbook_syntax::ast::{Body, DeclKind, EventDecl, Field, Item, State, StateKind};
use roadbook_syntax::{FileId, Span, parse_file};

use crate::text::{Encoding, Lines};

pub fn outline(text: &str, enc: Encoding) -> Vec<DocumentSymbol> {
    let (file, _) = parse_file(FileId(0), text);
    let o = Outline {
        lines: Lines::new(text),
        enc,
    };
    file.decls()
        .map(|d| {
            let (_, detail) = declaration(d);
            let (kind, children) = match &d.kind {
                DeclKind::Record { fields, .. } => (Kind::STRUCT, o.fields(fields)),
                DeclKind::Error { fields } => (Kind::STRUCT, o.fields(fields)),
                DeclKind::Alias { .. } => (Kind::TYPE_PARAMETER, Vec::new()),
                DeclKind::Enum { variants } => (
                    Kind::ENUM,
                    variants
                        .iter()
                        .map(|v| o.symbol(&v.name, Kind::ENUM_MEMBER, None, v.span, v.span, vec![]))
                        .collect(),
                ),
                DeclKind::Extern => (Kind::INTERFACE, Vec::new()),
                DeclKind::Fn { .. } => (Kind::FUNCTION, Vec::new()),
                // A task's emitted events are its children, as a machine's are.
                DeclKind::Task { emits, .. } => (
                    Kind::FUNCTION,
                    emits.iter().map(|e| o.event("emits", e)).collect(),
                ),
                DeclKind::Action { .. } => (Kind::METHOD, Vec::new()),
                DeclKind::Machine(m) => {
                    let mut children = o.fields(&m.params);
                    children.extend(o.body(&m.body));
                    (Kind::MODULE, children)
                }
            };
            o.symbol(
                &d.name.name,
                kind,
                Some(detail),
                d.span,
                d.name.span,
                children,
            )
        })
        .collect()
}

struct Outline<'a> {
    lines: Lines<'a>,
    enc: Encoding,
}

impl Outline<'_> {
    #[allow(deprecated)] // `DocumentSymbol::deprecated` must be given.
    fn symbol(
        &self,
        name: &str,
        kind: Kind,
        detail: Option<String>,
        span: Span,
        name_span: Span,
        children: Vec<DocumentSymbol>,
    ) -> DocumentSymbol {
        // The range must contain the selection range, even in a partial tree.
        let span = if span.start.offset <= name_span.start.offset
            && name_span.end.offset <= span.end.offset
        {
            span
        } else {
            name_span
        };
        DocumentSymbol {
            name: name.to_string(),
            detail,
            kind,
            tags: None,
            deprecated: None,
            range: self.lines.range(span, self.enc),
            selection_range: self.lines.range(name_span, self.enc),
            children: (!children.is_empty()).then_some(children),
        }
    }

    fn fields(&self, fields: &[Field]) -> Vec<DocumentSymbol> {
        fields
            .iter()
            .map(|f| {
                let detail = type_text(&f.ty);
                self.symbol(
                    &f.name.name,
                    Kind::FIELD,
                    Some(detail),
                    f.span,
                    f.name.span,
                    vec![],
                )
            })
            .collect()
    }

    fn body(&self, body: &Body) -> Vec<DocumentSymbol> {
        let mut out = Vec::new();
        for item in &body.items {
            match item {
                Item::Var(v) => out.push(self.symbol(
                    &v.name.name,
                    Kind::VARIABLE,
                    Some(type_text(&v.ty)),
                    v.span,
                    v.name.span,
                    vec![],
                )),
                Item::Event(e) => out.push(self.event("event", e)),
                Item::InternalEvent(e) => out.push(self.event("internal event", e)),
                Item::Emits(e) => out.push(self.event("emits", e)),
                Item::Call(c) => out.push(self.symbol(
                    &c.name.name,
                    Kind::METHOD,
                    Some("call".into()),
                    c.span,
                    c.name.span,
                    self.fields(&c.params),
                )),
                Item::Children(c) => out.push(self.symbol(
                    &c.name.name,
                    Kind::ARRAY,
                    Some(format!("{} by {}", c.src.name, type_text(&c.key))),
                    c.span,
                    c.name.span,
                    vec![],
                )),
                Item::State(s) => out.push(self.state(s)),
                // A pseudo-state, shown among the states it returns to.
                Item::History(h) => out.push(self.symbol(
                    &h.name.name,
                    Kind::OBJECT,
                    Some(if h.deep { "deep history" } else { "history" }.into()),
                    h.span,
                    h.name.span,
                    vec![],
                )),
                _ => {}
            }
        }
        out
    }

    fn event(&self, word: &str, e: &EventDecl) -> DocumentSymbol {
        self.symbol(
            &e.name.name,
            Kind::EVENT,
            Some(word.into()),
            e.span,
            e.name.span,
            self.fields(&e.fields),
        )
    }

    fn state(&self, s: &State) -> DocumentSymbol {
        let detail = match s.kind {
            StateKind::Normal => "state",
            StateKind::Final => "final state",
            StateKind::Parallel => "parallel state",
        };
        self.symbol(
            &s.name.name,
            Kind::OBJECT,
            Some(detail.into()),
            s.span,
            s.name.span,
            self.body(&s.body),
        )
    }
}
