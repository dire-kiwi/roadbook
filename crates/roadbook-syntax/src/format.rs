//! The canonical formatter. The rules are written down under "Formatting" in
//! `docs/design/language.md`.
//!
//! The formatter walks the tree and produces a list of lines. Each line is a row of cells; rows
//! of the same alignment group that follow each other with no blank or comment line between
//! them are aligned in a second pass. Comments are not in the tree: they are emitted from the
//! file's comment list, in source order, before the first row that starts after them, or at
//! the end of the row they follow on the same source line.

use crate::ast::*;
use crate::diagnostic::Diagnostic;
use crate::lexer::{Comment, Punct, Token, TokenKind, escape, lex};
use crate::parser::parse_file;
use crate::span::{FileId, Span};

/// Format a file. Fails, returning the diagnostics, if the file has syntax errors.
pub fn format(src: &str) -> Result<String, Vec<Diagnostic>> {
    let (file, diagnostics) = parse_file(FileId(0), src);
    if diagnostics.iter().any(Diagnostic::is_error) {
        return Err(diagnostics);
    }
    let tokens = lex(FileId(0), src).tokens;
    let mut f = Formatter {
        src,
        tokens,
        comments: &file.comments,
        next: 0,
        cursor: 0,
        at_block_start: true,
        lines: Vec::new(),
        carried: None,
    };
    f.file(&file);
    Ok(render(&f.lines))
}

const INDENT: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Group {
    /// One-line `type`, `enum`, `error` and `extern type` declarations.
    Data,
    /// One-line `fn`, `action` and `task` declarations.
    Callable,
    Var,
    Event,
    /// `children` lines: the machine, then `by` and the key.
    Children,
    /// One-line states.
    State,
    /// Transitions and invoke outcomes, including the lines of their branch blocks.
    Transition,
}

#[derive(Debug)]
enum Line {
    Blank,
    Row(Row),
}

#[derive(Debug)]
struct Row {
    indent: usize,
    /// `cells[0]` is the head; later cells are aligned columns. Empty cells are skipped.
    cells: Vec<String>,
    /// Appended after the cells and not aligned, such as ` {` on a block header.
    suffix: &'static str,
    comment: Option<String>,
    group: Option<Group>,
    /// Whether the row's head counts towards the group's column width.
    measured: bool,
}

struct Formatter<'a> {
    src: &'a str,
    tokens: Vec<Token>,
    comments: &'a [Comment],
    /// The next comment to emit.
    next: usize,
    /// The source offset up to which everything has been emitted.
    cursor: u32,
    at_block_start: bool,
    lines: Vec<Line>,
    /// A comment that ends the source line of a closing bracket, to put at the end of the next
    /// row: the row that starts with that bracket.
    carried: Option<String>,
}

fn off(s: Span) -> (u32, u32) {
    (s.start.offset, s.end.offset)
}

impl Formatter<'_> {
    // ───────────── comments, blank lines and rows ─────────────

    /// A blank line if the source has one between what was last emitted and `start`.
    fn gap(&mut self, start: u32) {
        if self.at_block_start || start <= self.cursor {
            return;
        }
        let between = &self.src[self.cursor as usize..start as usize];
        if between.matches('\n').count() >= 2 {
            self.lines.push(Line::Blank);
        }
    }

    /// Emit, as their own lines, the comments that start before `before`.
    fn comments_before(&mut self, before: u32, indent: usize, keep_gaps: bool) {
        while let Some(c) = self.comments.get(self.next) {
            if c.span.start.offset >= before {
                break;
            }
            if keep_gaps {
                self.gap(c.span.start.offset);
            }
            self.push(indent, vec![c.text.clone()], "", None, true);
            self.cursor = self.cursor.max(c.span.end.offset);
            self.next += 1;
        }
    }

    fn push(
        &mut self,
        indent: usize,
        cells: Vec<String>,
        suffix: &'static str,
        group: Option<Group>,
        measured: bool,
    ) {
        self.lines.push(Line::Row(Row {
            indent,
            cells,
            suffix,
            comment: None,
            group,
            measured,
        }));
        self.at_block_start = false;
    }

    /// Emit a row for the source range `start..end`. Comments before it come first; comments
    /// inside it (it is printed on one line) are moved before it; a comment after it on the
    /// same source line stays at its end.
    fn row(
        &mut self,
        (start, end): (u32, u32),
        indent: usize,
        cells: Vec<String>,
        suffix: &'static str,
        group: Option<Group>,
    ) {
        self.row_with(start, end, indent, cells, suffix, group, true);
    }

    #[allow(clippy::too_many_arguments)]
    fn row_with(
        &mut self,
        start: u32,
        end: u32,
        indent: usize,
        cells: Vec<String>,
        suffix: &'static str,
        group: Option<Group>,
        measured: bool,
    ) {
        self.comments_before(start, indent, true);
        self.gap(start);
        self.comments_before(end, indent, false);
        self.push(indent, cells, suffix, group, measured);
        if let (Some(c), Some(Line::Row(row))) = (self.carried.take(), self.lines.last_mut()) {
            row.comment = Some(c);
        }
        self.cursor = self.cursor.max(end);
        self.trailing();
    }

    /// The comments after the closing bracket `close` of a multi-line list and before `end`,
    /// the end of the row that starts with the bracket, when that row also holds what follows
    /// it (`) {`, `) ! A | B`, `} from "…"`). Those comments are inside that row, and would
    /// move before it, into the list, and be indented as list lines by the next run. So a
    /// comment on the bracket's line is kept for the end of the row, and those on the lines
    /// after it go before the bracket now, at `indent`. Comments after `end` belong to what
    /// follows the row and are left alone.
    fn comments_after_close(&mut self, close: Token, end: u32, indent: usize) {
        if let Some(c) = self.comments.get(self.next)
            && c.span.start.offset >= close.span.end.offset
            && c.span.start.offset < end
            && c.span.start.line == close.span.end.line
        {
            self.carried = Some(c.text.clone());
            self.next += 1;
        }
        self.comments_before(end, indent, false);
    }

    fn trailing(&mut self) {
        let Some(c) = self.comments.get(self.next) else {
            return;
        };
        let start = c.span.start.offset;
        if start < self.cursor
            || matches!(self.lines.last(), Some(Line::Row(r)) if r.comment.is_some())
        {
            return;
        }
        let between = &self.src[self.cursor as usize..start as usize];
        if between
            .chars()
            .all(|ch| ch == ' ' || ch == '\t' || ch == ',')
        {
            if let Some(Line::Row(row)) = self.lines.last_mut() {
                row.comment = Some(c.text.clone());
            }
            self.cursor = c.span.end.offset;
            self.next += 1;
        }
    }

    fn open_block(&mut self) {
        self.at_block_start = true;
    }

    /// Emit the comments left in a block, then its closing line.
    fn close_block(
        &mut self,
        close: (u32, u32),
        indent: usize,
        text: String,
        group: Option<Group>,
    ) {
        self.comments_before(close.0, indent + INDENT, true);
        self.at_block_start = true; // never a blank line before `}`
        self.row_with(close.0, close.1, indent, vec![text], "", group, false);
    }

    /// The first token of kind `p` at or after `offset`.
    fn find(&self, offset: u32, p: Punct) -> Token {
        let i = self
            .tokens
            .partition_point(|t| t.span.start.offset < offset);
        *self.tokens[i..]
            .iter()
            .find(|t| t.kind == TokenKind::Punct(p))
            .expect("the parser checked the delimiter is there")
    }

    fn has_newline(&self, from: u32, to: u32) -> bool {
        from < to && self.src[from as usize..to as usize].contains('\n')
    }

    fn has_comment(&self, span: Span) -> bool {
        self.comments.iter().any(|c| {
            c.span.start.offset > span.start.offset && c.span.start.offset < span.end.offset
        })
    }

    // ───────────── file and declarations ─────────────

    fn file(&mut self, file: &File) {
        for item in &file.items {
            match item {
                TopItem::Import(i) => self.import(i),
                TopItem::Decl(d) => self.decl(d),
            }
        }
        self.comments_before(u32::MAX, 0, true);
    }

    fn import(&mut self, i: &Import) {
        let open = self.find(i.span.start.offset, Punct::LBrace);
        let first = i.names[0].span.start.offset;
        let from = format!("from {}", escape(&i.path.value));
        if !self.has_newline(open.span.end.offset, first) {
            let names: Vec<_> = i.names.iter().map(|n| n.name.clone()).collect();
            self.row(
                off(i.span),
                0,
                vec![format!("import {{ {} }} {from}", names.join(", "))],
                "",
                None,
            );
            return;
        }
        let close = self.find(
            i.names.last().map_or(first, |n| n.span.end.offset),
            Punct::RBrace,
        );
        self.row(
            (i.span.start.offset, open.span.end.offset),
            0,
            vec!["import {".into()],
            "",
            None,
        );
        let items: Vec<_> = i.names.iter().map(|n| (n.span, n.name.clone())).collect();
        self.list_lines(INDENT, &items);
        self.comments_before(close.span.start.offset, INDENT, true);
        self.comments_after_close(close, i.span.end.offset, INDENT);
        self.close_block(
            (close.span.start.offset, i.span.end.offset),
            0,
            format!("}} {from}"),
            None,
        );
    }

    /// The elements of a multi-line list, keeping the author's line breaks between them.
    fn list_lines(&mut self, indent: usize, items: &[(Span, String)]) {
        self.open_block();
        let mut lines: Vec<Vec<&(Span, String)>> = Vec::new();
        for (i, item) in items.iter().enumerate() {
            if i == 0 || item.0.start.line != items[i - 1].0.end.line {
                lines.push(Vec::new());
            }
            lines.last_mut().expect("pushed above").push(item);
        }
        let n = lines.len();
        for (i, line) in lines.into_iter().enumerate() {
            let mut text = line
                .iter()
                .map(|(_, s)| s.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            if i + 1 < n {
                text.push(',');
            }
            let span = (line[0].0.start.offset, line[line.len() - 1].0.end.offset);
            self.row(span, indent, vec![text], "", None);
        }
    }

    /// A brace list after `after`: one line (`{ a, b }`) or, when the source breaks the line
    /// after `{`, the header `head {`, the elements and `}`. Returns the one-line text if the
    /// list is on one line.
    fn brace_list(
        &mut self,
        after: u32,
        head: &str,
        items: &[(Span, String)],
        header_start: u32,
        indent: usize,
    ) -> Option<String> {
        let open = self.find(after, Punct::LBrace);
        let close = self.find(
            items
                .last()
                .map_or(open.span.end.offset, |i| i.0.end.offset),
            Punct::RBrace,
        );
        let first = items
            .first()
            .map_or(close.span.start.offset, |i| i.0.start.offset);
        if items.is_empty() || !self.has_newline(open.span.end.offset, first) {
            let texts: Vec<_> = items.iter().map(|i| i.1.as_str()).collect();
            return Some(if texts.is_empty() {
                "{}".into()
            } else {
                format!("{{ {} }}", texts.join(", "))
            });
        }
        self.row(
            (header_start, open.span.end.offset),
            indent,
            vec![format!("{head} {{")],
            "",
            None,
        );
        self.list_lines(indent + INDENT, items);
        self.close_block(off(close.span), indent, "}".into(), None);
        None
    }

    fn fields(fields: &[Field]) -> Vec<(Span, String)> {
        fields
            .iter()
            .map(|f| (f.span, format!("{}: {}", f.name.name, ty(&f.ty))))
            .collect()
    }

    /// The parameter list after `name`: `(a: T, …)` on one line, or a multi-line header. In the
    /// multi-line case the lines up to and including the `(` line are emitted and the returned
    /// head is `)`, for the caller's row from `start` to `end`.
    fn params(
        &mut self,
        name: &Ident,
        params: &[Field],
        head: String,
        (start, end): (u32, u32),
        indent: usize,
    ) -> String {
        let open = self.find(name.span.end.offset, Punct::LParen);
        let items = Self::fields(params);
        let multi = params
            .first()
            .is_some_and(|p| self.has_newline(open.span.end.offset, p.span.start.offset));
        if !multi {
            let texts: Vec<_> = items.iter().map(|i| i.1.as_str()).collect();
            return format!("{head}({})", texts.join(", "));
        }
        self.row(
            (start, open.span.end.offset),
            indent,
            vec![format!("{head}(")],
            "",
            None,
        );
        self.list_lines(indent + INDENT, &items);
        let close = self.find(
            items
                .last()
                .expect("multi-line lists are not empty")
                .0
                .end
                .offset,
            Punct::RParen,
        );
        self.comments_before(close.span.start.offset, indent + INDENT, true);
        self.comments_after_close(close, end, indent + INDENT);
        // The caller emits the line starting with `)`; it must not get a blank line before it.
        self.at_block_start = true;
        self.cursor = self.cursor.max(close.span.start.offset);
        ")".into()
    }

    fn decl_row(&mut self, whole: (u32, u32), cells: Vec<String>) {
        let group = decl_group(&cells[0]);
        self.row(whole, 0, cells, "", group);
    }

    fn decl(&mut self, d: &Decl) {
        let name = &d.name.name;
        let start = d.span.start.offset;
        let whole = off(d.span);
        let kw = d.kind.keyword();
        let head = format!("{kw} {name}");
        match &d.kind {
            DeclKind::Record { fields } | DeclKind::Error { fields }
                if !fields.is_empty() || kw == "type" =>
            {
                if let Some(text) = self.brace_list(
                    d.name.span.end.offset,
                    &head,
                    &Self::fields(fields),
                    start,
                    0,
                ) {
                    self.row(whole, 0, vec![head, text], "", Some(Group::Data));
                }
            }
            DeclKind::Enum { variants } => {
                let items: Vec<_> = variants.iter().map(|v| (v.span, v.name.clone())).collect();
                if let Some(text) = self.brace_list(d.name.span.end.offset, &head, &items, start, 0)
                {
                    self.row(whole, 0, vec![head, text], "", Some(Group::Data));
                }
            }
            DeclKind::Record { .. } | DeclKind::Error { .. } | DeclKind::Extern => {
                self.row(whole, 0, vec![head], "", Some(Group::Data))
            }
            DeclKind::Alias { target } => self.row(
                whole,
                0,
                vec![head, format!("= {}", ty(target))],
                "",
                Some(Group::Data),
            ),
            DeclKind::Fn { params, returns } => {
                let head = self.params(&d.name, params, head, whole, 0);
                self.decl_row(whole, vec![head, format!("-> {}", ty(returns))]);
            }
            DeclKind::Action { params } => {
                let head = self.params(&d.name, params, head, whole, 0);
                self.decl_row(whole, vec![head]);
            }
            DeclKind::Task {
                params,
                returns,
                errors,
            } => {
                let head = self.params(&d.name, params, head, whole, 0);
                let returns = returns
                    .as_ref()
                    .map_or(String::new(), |t| format!("-> {}", ty(t)));
                self.decl_row(whole, vec![head, returns, error_list(errors)]);
            }
            DeclKind::Machine(m) => {
                let header = (start, m.body.span.start.offset + 1);
                let mut head = self.params(&d.name, &m.params, head, header, 0);
                if let Some(t) = &m.returns {
                    head += &format!(" -> {}", ty(t));
                }
                if !m.errors.is_empty() {
                    head += &format!(" {}", error_list(&m.errors));
                }
                self.body(&m.body, header, head, 0);
            }
        }
    }

    // ───────────── bodies ─────────────

    /// `head {`, the items, `}`; or `head {}` for an empty body with no comments.
    fn body(&mut self, body: &Body, header: (u32, u32), head: String, indent: usize) {
        let close = (body.span.end.offset - 1, body.span.end.offset);
        if body.items.is_empty() && !self.has_comment(body.span) {
            self.row(
                (header.0, close.1),
                indent,
                vec![format!("{head} {{}}")],
                "",
                None,
            );
            return;
        }
        self.row(header, indent, vec![head], " {", None);
        self.open_block();
        for item in &body.items {
            self.item(item, indent + INDENT);
        }
        self.close_block(close, indent, "}".into(), None);
    }

    fn item(&mut self, item: &Item, indent: usize) {
        let whole = off(item.span());
        match item {
            Item::Var(v) => {
                let mut t = ty(&v.ty);
                if let Some(init) = &v.init {
                    t += &format!(" = {}", expr(init));
                }
                self.row(
                    whole,
                    indent,
                    vec![format!("var {}:", v.name.name), t],
                    "",
                    Some(Group::Var),
                );
            }
            Item::Children(c) => self.row(
                whole,
                indent,
                vec![
                    format!("children {}:", c.name.name),
                    c.machine.name.clone(),
                    format!("by {}", ty(&c.key)),
                ],
                "",
                Some(Group::Children),
            ),
            Item::Event(e) | Item::Emits(e) => {
                let kw = if let Item::Emits(_) = item {
                    "emits"
                } else {
                    "event"
                };
                let head = format!("{kw} {}", e.name.name);
                if e.fields.is_empty() {
                    self.row(whole, indent, vec![head], "", Some(Group::Event));
                } else if let Some(text) = self.brace_list(
                    e.name.span.end.offset,
                    &head,
                    &Self::fields(&e.fields),
                    whole.0,
                    indent,
                ) {
                    self.row(whole, indent, vec![head, text], "", Some(Group::Event));
                }
            }
            Item::State(s) => self.state(s, indent),
            Item::Invoke(inv) => {
                let mut args_end = inv.callee.span.end.offset;
                if let Some(a) = inv.args.last() {
                    args_end = a.span.end.offset;
                }
                let rparen = self.find(args_end, Punct::RParen);
                let open = self.find(rparen.span.end.offset, Punct::LBrace);
                let mut head = format!("invoke {}({})", inv.callee.name, exprs(&inv.args));
                if let Some(a) = &inv.alias {
                    head += &format!(" as {}", a.name);
                }
                self.row(
                    (whole.0, open.span.end.offset),
                    indent,
                    vec![head],
                    " {",
                    None,
                );
                self.open_block();
                for t in &inv.outcomes {
                    self.transition(t, indent + INDENT);
                }
                self.close_block((whole.1 - 1, whole.1), indent, "}".into(), None);
            }
            Item::Transition(t) => self.transition(t, indent),
            _ => {
                let text = inline_item(item).expect("every other item is one line");
                self.row(whole, indent, vec![text], "", None);
            }
        }
    }

    fn state(&mut self, s: &State, indent: usize) {
        let head = format!("{}state {}", state_prefix(s.kind), s.name.name);
        let one_line = match s.body.items.as_slice() {
            [] => Some("{}".to_string()),
            [item] => inline_item(item).map(|t| format!("{{ {t} }}")),
            _ => None,
        };
        match one_line {
            Some(text) if !self.has_comment(s.body.span) => self.row(
                off(s.span),
                indent,
                vec![head, text],
                "",
                Some(Group::State),
            ),
            _ => self.body(
                &s.body,
                (s.span.start.offset, s.body.span.start.offset + 1),
                head,
                indent,
            ),
        }
    }

    fn transition(&mut self, t: &Transition, indent: usize) {
        let trigger = trigger(&t.trigger);
        match &t.branches {
            Branches::Single(b) => {
                let mut cells = branch_cells(b);
                cells[0] = match &b.guard {
                    Some(g) => format!("{trigger} if {}", expr(g)),
                    None => trigger,
                };
                self.row(off(t.span), indent, cells, "", Some(Group::Transition));
            }
            Branches::Block { branches, span } => {
                self.row(
                    (t.span.start.offset, span.start.offset + 1),
                    indent,
                    vec![trigger],
                    " {",
                    Some(Group::Transition),
                );
                self.open_block();
                for b in branches {
                    let mut cells = branch_cells(b);
                    cells[0] = match &b.guard {
                        Some(g) => format!("if {}", expr(g)),
                        None => "else".into(),
                    };
                    self.row(
                        off(b.span),
                        indent + INDENT,
                        cells,
                        "",
                        Some(Group::Transition),
                    );
                }
                self.close_block(
                    (span.end.offset - 1, span.end.offset),
                    indent,
                    "}".into(),
                    Some(Group::Transition),
                );
            }
        }
    }
}

/// A declaration whose parameter list was broken over lines is not aligned with others.
fn decl_group(head: &str) -> Option<Group> {
    if head.starts_with(')') {
        None
    } else {
        Some(Group::Callable)
    }
}

fn state_prefix(kind: StateKind) -> &'static str {
    match kind {
        StateKind::Normal => "",
        StateKind::Final => "final ",
        StateKind::Parallel => "parallel ",
    }
}

/// The text of an item that fits on one line, or `None` for states, invokes and branch blocks.
fn inline_item(item: &Item) -> Option<String> {
    Some(match item {
        Item::Initial(n) => format!("initial {}", n.name),
        Item::Entry(h) => format!("entry {}", effects(&h.effects)),
        Item::Exit(h) => format!("exit {}", effects(&h.effects)),
        Item::Output(v) => format!("output {}", expr(&v.value)),
        Item::Fail(v) => format!("fail {}", expr(&v.value)),
        Item::Transition(Transition {
            trigger: tr,
            branches: Branches::Single(b),
            ..
        }) => {
            let mut cells = branch_cells(b);
            cells[0] = match &b.guard {
                Some(g) => format!("{} if {}", trigger(tr), expr(g)),
                None => trigger(tr),
            };
            join_cells(Some(Group::Transition), &cells)
        }
        Item::Var(_)
        | Item::Event(_)
        | Item::Emits(_)
        | Item::Children(_)
        | Item::State(_)
        | Item::Invoke(_)
        | Item::Transition(_) => {
            return None;
        }
    })
}

/// `[head, target, effects]`, with the head left for the caller to fill in.
fn branch_cells(b: &Branch) -> Vec<String> {
    let target = b.target.as_ref().map_or(String::new(), |t| {
        let path: Vec<_> = t.path.iter().map(|p| p.name.as_str()).collect();
        format!("-> {}", path.join("."))
    });
    vec![String::new(), target, effects(&b.effects)]
}

fn trigger(t: &Trigger) -> String {
    let pattern = |p: &Pattern| match p {
        Pattern::Name(n) => n.name.clone(),
        Pattern::Any { .. } => "*".into(),
    };
    match t {
        Trigger::On(p) => format!("on {}", pattern(p)),
        Trigger::Receive { from, event } => format!("on {}.{}", from.name, event.name),
        Trigger::ChildDone { from } => format!("on {}.done", from.name),
        Trigger::ChildFail { from, error } => format!("on {}.fail {}", from.name, pattern(error)),
        Trigger::After(e) if bare_record(e) => format!("after ({})", expr(e)),
        Trigger::After(e) => format!("after {}", expr(e)),
        Trigger::Always => "always".into(),
        Trigger::Done => "done".into(),
        Trigger::Fail(p) => format!("fail {}", pattern(p)),
    }
}

/// Whether an `after` delay has a record value outside any parentheses, which would read as
/// the start of a branch block.
fn bare_record(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Record { fields, .. } => !fields.is_empty(),
        ExprKind::Field(e, _) | ExprKind::Unary(_, e) => bare_record(e),
        ExprKind::Binary(_, a, b) => bare_record(a) || bare_record(b),
        _ => false,
    }
}

/// Consecutive `set`s, and consecutive `do`s, share a clause; clauses are separated by two
/// spaces. Every `set` comes first, then the `do`, `send`, `emit`, `spawn` and `stop` effects,
/// each in the order written: the order they run in. A `send`, `emit`, `spawn` or `stop` is a
/// clause of its own.
fn effects(effects: &[Effect]) -> String {
    let mut out = String::new();
    let mut last: Option<&'static str> = None;
    let sets = effects.iter().filter(|e| matches!(e, Effect::Set(_)));
    let rest = effects.iter().filter(|e| !matches!(e, Effect::Set(_)));
    for e in sets.chain(rest) {
        let kw = match e {
            Effect::Set(_) => "set",
            Effect::Do(_) => "do",
            Effect::Send(_) => "send",
            Effect::Emit(_) => "emit",
            Effect::Spawn(_) => "spawn",
            Effect::Stop(_) => "stop",
        };
        if last == Some(kw) && matches!(kw, "set" | "do") {
            out.push_str(", ");
        } else {
            if last.is_some() {
                out.push_str("  ");
            }
            out.push_str(kw);
            out.push(' ');
        }
        match e {
            Effect::Set(a) => out.push_str(&format!("{} = {}", a.var.name, expr(&a.value))),
            Effect::Do(c) => out.push_str(&call(c)),
            Effect::Send(m) | Effect::Emit(m) => out.push_str(&message(m)),
            Effect::Spawn(sp) => out.push_str(&format!(
                "{}[{}] = {}({})",
                sp.children.name,
                expr(&sp.key),
                sp.machine.name,
                exprs(&sp.args)
            )),
            Effect::Stop(st) => out.push_str(&format!("{}[{}]", st.children.name, expr(&st.key))),
        }
        last = Some(kw);
    }
    out
}

/// `to.Event { field: value, … }`, or `Event` with no fields.
fn message(m: &Message) -> String {
    let mut out = String::new();
    if let Some(to) = &m.to {
        out.push_str(&to.name);
        match &m.key {
            Some(SendKey::Key(k)) => out.push_str(&format!("[{}]", expr(k))),
            Some(SendKey::All { .. }) => out.push_str(".*"),
            None => {}
        }
        out.push('.');
    }
    out.push_str(&message_value(m));
    out
}

/// The event value of a `send` or `emit` as the formatter writes it, without the receiver:
/// `Reserve { orderId: id }`, or `Pause` with no fields. This is the `text` of its IR value.
pub fn message_value(m: &Message) -> String {
    if m.fields.is_empty() {
        return m.event.name.clone();
    }
    let fs: Vec<_> = m
        .fields
        .iter()
        .map(|f| format!("{}: {}", f.name.name, expr(&f.value)))
        .collect();
    format!("{} {{ {} }}", m.event.name, fs.join(", "))
}

fn error_list(errors: &[Ident]) -> String {
    if errors.is_empty() {
        return String::new();
    }
    let names: Vec<_> = errors.iter().map(|e| e.name.as_str()).collect();
    format!("! {}", names.join(" | "))
}

fn ty(t: &TypeRef) -> String {
    match &t.kind {
        TypeKind::Builtin(b) => b.as_str().into(),
        TypeKind::Named(n) => n.name.clone(),
        TypeKind::List(t) => format!("{}[]", ty(t)),
        TypeKind::Optional(t) => format!("{}?", ty(t)),
        TypeKind::Ref(n) => format!("ref {}", n.name),
    }
}

// ───────────── expressions ─────────────

fn exprs(es: &[Expr]) -> String {
    es.iter().map(expr).collect::<Vec<_>>().join(", ")
}

fn call(c: &Call) -> String {
    format!("{}({})", c.name.name, exprs(&c.args))
}

fn precedence(e: &Expr) -> u8 {
    match &e.kind {
        ExprKind::Binary(op, ..) => op.precedence(),
        ExprKind::Unary(UnaryOp::Not, _) => 3,
        ExprKind::Unary(UnaryOp::Neg, _) => 7,
        _ => 8,
    }
}

/// The canonical source text of an expression, as the formatter writes it: only the
/// parentheses precedence needs, plus `not (…)` around a binary operand of `not`. This is the
/// `text` of an IR expression node.
pub fn expr_text(e: &Expr) -> String {
    expr(e)
}

/// An expression with the parentheses its context needs: those required by precedence, plus
/// `not (…)` around any binary operand of `not`.
pub(crate) fn expr(e: &Expr) -> String {
    match &e.kind {
        ExprKind::Int(n) => n.to_string(),
        ExprKind::Float(f) => {
            let s = f.to_string();
            if s.contains('.') { s } else { format!("{s}.0") }
        }
        ExprKind::Duration(d) => format!("{}{}", d.value, d.unit.as_str()),
        ExprKind::String(s) => escape(s),
        ExprKind::Bool(b) => b.to_string(),
        ExprKind::Var(n) => n.name.clone(),
        ExprKind::Field(x, f) => format!("{}.{}", operand(x, 8), f.name),
        ExprKind::Call(c) => call(c),
        ExprKind::Enum(t, v) => format!("{}.{}", t.name, v.name),
        ExprKind::Record { ty, fields } if fields.is_empty() => ty.name.clone(),
        ExprKind::Record { ty, fields } => {
            let fs: Vec<_> = fields
                .iter()
                .map(|f| format!("{}: {}", f.name.name, expr(&f.value)))
                .collect();
            format!("{} {{ {} }}", ty.name, fs.join(", "))
        }
        ExprKind::Unary(UnaryOp::Not, x) if matches!(x.kind, ExprKind::Binary(..)) => {
            format!("not ({})", expr(x))
        }
        ExprKind::Unary(UnaryOp::Not, x) => format!("not {}", operand(x, 4)),
        ExprKind::Unary(UnaryOp::Neg, x) => format!("-{}", operand(x, 8)),
        ExprKind::Binary(op, a, b) => {
            let p = op.precedence();
            // Comparisons do not chain, so both sides bind tighter; the rest associate left.
            let left = if p == 4 { 5 } else { p };
            format!("{} {} {}", operand(a, left), op.as_str(), operand(b, p + 1))
        }
    }
}

fn operand(e: &Expr, min: u8) -> String {
    if precedence(e) < min {
        format!("({})", expr(e))
    } else {
        expr(e)
    }
}

// ───────────── alignment and output ─────────────

fn len(s: &str) -> usize {
    s.chars().count()
}

/// The space before cell `i`. In a transition the effects follow the target by two spaces.
fn sep(group: Option<Group>, i: usize, has_target: bool) -> usize {
    if group == Some(Group::Transition) && i == 2 && has_target {
        2
    } else {
        1
    }
}

fn join_cells(group: Option<Group>, cells: &[String]) -> String {
    let has_target = cells.get(1).is_some_and(|c| !c.is_empty());
    let mut out = cells[0].clone();
    for (i, c) in cells.iter().enumerate().skip(1) {
        if !c.is_empty() {
            out.push_str(&" ".repeat(sep(group, i, has_target)));
            out.push_str(c);
        }
    }
    out
}

fn render(lines: &[Line]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < lines.len() {
        let Line::Row(row) = &lines[i] else {
            out.push('\n');
            i += 1;
            continue;
        };
        // The run of rows in the same group.
        let mut j = i + 1;
        if row.group.is_some() {
            while let Some(Line::Row(r)) = lines.get(j) {
                if r.group != row.group {
                    break;
                }
                j += 1;
            }
        }
        let run: Vec<&Row> = lines[i..j]
            .iter()
            .filter_map(|l| if let Line::Row(r) = l { Some(r) } else { None })
            .collect();
        let columns = run.iter().map(|r| r.cells.len()).max().unwrap_or(1);
        let mut widths = vec![0; columns];
        for r in run.iter().filter(|r| r.measured) {
            widths[0] = widths[0].max(r.indent + len(&r.cells[0]));
            for (c, cell) in r.cells.iter().enumerate().skip(1) {
                widths[c] = widths[c].max(len(cell));
            }
        }
        let has_target = widths.get(1).is_some_and(|w| *w > 0);
        let mut starts = vec![0; columns];
        let mut pos = widths[0];
        for c in 1..columns {
            if widths[c] > 0 {
                pos += sep(row.group, c, has_target);
                starts[c] = pos;
                pos += widths[c];
            }
        }
        for r in run {
            let mut line = " ".repeat(r.indent) + &r.cells[0];
            for (c, cell) in r.cells.iter().enumerate().skip(1) {
                if !cell.is_empty() {
                    let width = len(&line);
                    line.push_str(&" ".repeat(starts[c].saturating_sub(width).max(1)));
                    line.push_str(cell);
                }
            }
            line.push_str(r.suffix);
            if let Some(comment) = &r.comment {
                line.push(' ');
                line.push_str(comment);
            }
            out.push_str(line.trim_end());
            out.push('\n');
        }
        i = j;
    }
    while out.ends_with("\n\n") {
        out.pop();
    }
    if out.trim().is_empty() {
        return String::new();
    }
    out
}
