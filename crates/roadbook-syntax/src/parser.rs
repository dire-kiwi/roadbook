//! A hand-written recursive descent parser with error recovery.
//!
//! Each parse function reports its own error and returns `Err(Fail)`. The loops over items
//! (top level, machine and state bodies, invoke outcomes, branch blocks) catch the failure,
//! skip to the next token that can start an item at the same nesting depth, and carry on, so
//! one file can report several errors and still produce a partial tree.

use crate::ast::*;
use crate::codes;
use crate::diagnostic::Diagnostic;
use crate::lexer::{
    Comment, CommentKind, Keyword as K, Punct as P, Token, TokenKind as T, lex, unescape,
};
use crate::span::{FileId, Pos, Span};

/// Parse one file. Always returns a tree; after syntax errors it is partial.
pub fn parse_file(file: FileId, src: &str) -> (File, Vec<Diagnostic>) {
    let lexed = lex(file, src);
    let mut p = Parser {
        src,
        tokens: lexed.tokens,
        pos: 0,
        doc_used: vec![false; lexed.comments.len()],
        comments: lexed.comments,
        diags: lexed.diagnostics,
        depth: 0,
        too_deep: false,
    };
    let file = p.file();
    p.unattached_docs();
    let mut diags = p.diags;
    diags.sort_by_key(|d| (d.span.start.offset, d.code));
    (file, diags)
}

/// The failure marker. The diagnostic has already been recorded.
struct Fail;
type PResult<T> = Result<T, Fail>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum BodyKind {
    Machine,
    State,
}

struct Parser<'a> {
    src: &'a str,
    tokens: Vec<Token>,
    pos: usize,
    comments: Vec<Comment>,
    doc_used: Vec<bool>,
    diags: Vec<Diagnostic>,
    /// How deeply the tree being built is nested here: states, expressions (each operator
    /// and parenthesis), field accesses and type suffixes. Bounded by [`MAX_DEPTH`]. No
    /// closures here: in a debug build every frame on the recursive path counts.
    depth: usize,
    /// Whether the nesting limit has been reported. Once per file is enough: the recovery
    /// after it may run into the same deep structure again.
    too_deep: bool,
}

/// The deepest nesting accepted. The parser and every pass over the tree recurse once per
/// level, so this bound keeps them all within the stack. In a debug build the parser uses
/// about 23 KB of stack per level, and 64 levels fit in the 2 MB of a test thread with room
/// to spare. Real charts nest a handful of levels.
pub const MAX_DEPTH: usize = 64;

/// Keywords that start a top-level declaration. Seeing one inside a block means the block was
/// not closed.
fn is_decl_start(k: T) -> bool {
    matches!(
        k,
        T::Keyword(
            K::Import
                | K::Type
                | K::Enum
                | K::Error
                | K::Extern
                | K::Fn
                | K::Action
                | K::Task
                | K::Machine
                | K::Strict
        )
    )
}

/// Keywords that start a machine or state item.
fn is_item_start(k: T) -> bool {
    matches!(
        k,
        T::Keyword(
            K::Var
                | K::Event
                | K::Emits
                | K::Call
                | K::Initial
                | K::State
                | K::Final
                | K::Parallel
                | K::Entry
                | K::Exit
                | K::On
                | K::After
                | K::Always
                | K::Done
                | K::Invoke
                | K::Output
                | K::Fail
        )
    )
}

fn cmp_op(k: T) -> Option<BinaryOp> {
    Some(match k {
        T::Punct(P::EqEq) => BinaryOp::Eq,
        T::Punct(P::NotEq) => BinaryOp::Ne,
        T::Punct(P::Lt) => BinaryOp::Lt,
        T::Punct(P::Le) => BinaryOp::Le,
        T::Punct(P::Gt) => BinaryOp::Gt,
        T::Punct(P::Ge) => BinaryOp::Ge,
        _ => return None,
    })
}

impl Parser<'_> {
    // ───────────── token access ─────────────

    fn tok(&self) -> Token {
        self.tokens[self.pos]
    }

    fn peek(&self) -> T {
        self.tok().kind
    }

    fn nth(&self, n: usize) -> T {
        self.nth_tok(n).kind
    }

    fn nth_tok(&self, n: usize) -> Token {
        self.tokens[(self.pos + n).min(self.tokens.len() - 1)]
    }

    fn bump(&mut self) -> Token {
        let t = self.tok();
        if t.kind != T::Eof {
            self.pos += 1;
        }
        t
    }

    fn at(&self, k: T) -> bool {
        self.peek() == k
    }

    fn at_p(&self, p: P) -> bool {
        self.at(T::Punct(p))
    }

    fn at_kw(&self, k: K) -> bool {
        self.at(T::Keyword(k))
    }

    fn eat_p(&mut self, p: P) -> Option<Token> {
        self.at_p(p).then(|| self.bump())
    }

    fn eat_kw(&mut self, k: K) -> Option<Token> {
        self.at_kw(k).then(|| self.bump())
    }

    fn text(&self, t: Token) -> &str {
        &self.src[t.span.range()]
    }

    /// The span of the last consumed token.
    fn prev(&self) -> Span {
        self.tokens[self.pos.saturating_sub(1)].span
    }

    fn since(&self, start: Span) -> Span {
        start.to(self.prev())
    }

    fn found(&self) -> String {
        let t = self.tok();
        match t.kind {
            T::Eof => "end of file".into(),
            T::Keyword(_) => format!("keyword `{}`", self.text(t)),
            _ => format!("`{}`", self.text(t)),
        }
    }

    // ───────────── diagnostics ─────────────

    fn error(&mut self, d: Diagnostic) {
        self.diags.push(d);
    }

    /// Report "expected X, found Y" at the current token.
    fn unexpected(&mut self, expected: &str) -> Fail {
        let msg = format!("expected {expected}, found {}", self.found());
        let d = Diagnostic::error(codes::UNEXPECTED_TOKEN, self.tok().span, msg);
        self.error(d);
        Fail
    }

    fn unclosed(&mut self, open: Token, what: &str) -> Fail {
        let close = match open.kind {
            T::Punct(P::LParen) => ")",
            T::Punct(P::LBracket) => "]",
            _ => "}",
        };
        let opener = self.text(open).to_string();
        let msg = format!("expected `{close}` to close {what}, found {}", self.found());
        let d = Diagnostic::error(codes::UNCLOSED_DELIMITER, self.tok().span, msg)
            .with_secondary(open.span, format!("unclosed `{opener}`"));
        self.error(d);
        Fail
    }

    /// One more level of nesting, or an error at the current token if that is too deep. A
    /// function that goes deeper restores the depth when it succeeds; after a failure, the
    /// loop that recovers restores it.
    fn deeper(&mut self) -> PResult<()> {
        self.depth += 1;
        if self.depth <= MAX_DEPTH {
            return Ok(());
        }
        if self.too_deep {
            return Err(Fail);
        }
        self.too_deep = true;
        let d = Diagnostic::error(
            codes::NESTED_TOO_DEEPLY,
            self.tok().span,
            format!("this is nested more than {MAX_DEPTH} levels deep"),
        )
        .with_help("states, parentheses, operators and type suffixes nest; move part of it into a `fn` or a separate machine");
        self.error(d);
        Err(Fail)
    }

    fn expect_p(&mut self, p: P, context: &str) -> PResult<Token> {
        match self.eat_p(p) {
            Some(t) => Ok(t),
            None => Err(self.unexpected(&format!("`{}`{context}", p.as_str()))),
        }
    }

    // ───────────── names ─────────────

    fn bump_ident(&mut self) -> Ident {
        let t = self.bump();
        self.ident(t)
    }

    fn ident(&self, t: Token) -> Ident {
        Ident {
            name: self.text(t).to_string(),
            span: t.span,
        }
    }

    /// A keyword where a name is required is consumed as the name (with an error) when it is on
    /// the same line as the tokens around it, which is when the author most likely meant it as
    /// one. Otherwise the name is probably missing and the keyword starts the next item.
    /// A keyword followed by `:` is always meant as a name (a field, parameter or variable),
    /// since no keyword is followed by `:`.
    fn keyword_as_name(&mut self, what: &str) -> PResult<Ident> {
        let t = self.tok();
        let next = self.tokens[(self.pos + 1).min(self.tokens.len() - 1)];
        let same_line =
            next.span.start.line == t.span.end.line && self.prev().end.line == t.span.start.line;
        let named = next.kind == T::Punct(P::Colon);
        if !named && (next.kind == T::Eof || !same_line || is_decl_start(t.kind)) {
            return Err(self.unexpected(what));
        }
        let word = self.text(t).to_string();
        let instead = match word.as_str() {
            "type" => "kind".to_string(),
            "error" => "failure".to_string(),
            "event" => "message".to_string(),
            "state" => "status".to_string(),
            w => format!("{w}Name"),
        };
        let d = Diagnostic::error(
            codes::RESERVED_NAME,
            t.span,
            format!("`{word}` is a keyword and cannot be used as {what}"),
        )
        .with_help(format!(
            "keywords are reserved; rename it, for example to `{instead}`"
        ));
        self.error(d);
        self.bump();
        Ok(Ident {
            name: word,
            span: t.span,
        })
    }

    /// An upper-case name. A lower-case name is accepted with an error.
    fn upper(&mut self, what: &str) -> PResult<Ident> {
        match self.peek() {
            T::Upper => Ok(self.bump_ident()),
            T::Lower => {
                let t = self.bump();
                let id = self.ident(t);
                let fixed = capitalize(&id.name, true);
                let d = Diagnostic::error(codes::WRONG_NAME_CASE, t.span, format!("{what} must start with an upper-case letter: `{}`", id.name))
                    .with_help("types, errors, events, states and machines are upper-case; variables, fields, functions, actions and tasks are lower-case")
                    .with_fix(format!("rename to `{fixed}`"), t.span, fixed);
                self.error(d);
                Ok(id)
            }
            T::Keyword(_) => self.keyword_as_name(what),
            _ => Err(self.unexpected(what)),
        }
    }

    /// A lower-case name. An upper-case name is accepted with an error.
    fn lower(&mut self, what: &str) -> PResult<Ident> {
        match self.peek() {
            T::Lower => Ok(self.bump_ident()),
            T::Upper => {
                let t = self.bump();
                let id = self.ident(t);
                let fixed = capitalize(&id.name, false);
                let d = Diagnostic::error(codes::WRONG_NAME_CASE, t.span, format!("{what} must start with a lower-case letter: `{}`", id.name))
                    .with_help("variables, fields, functions, actions and tasks are lower-case; types, errors, events, states and machines are upper-case")
                    .with_fix(format!("rename to `{fixed}`"), t.span, fixed);
                self.error(d);
                Ok(id)
            }
            T::Keyword(_) => self.keyword_as_name(what),
            _ => Err(self.unexpected(what)),
        }
    }

    /// A variable or parameter name: lower-case, and not the implicit name `result`.
    /// Whether the current token is the word `sensitive` used as a modifier: followed by
    /// `next` (a field or parameter name, or `var`).
    fn at_sensitive(&self, next: T) -> bool {
        self.at(T::Lower) && self.text(self.tok()) == "sensitive" && self.nth(1) == next
    }

    fn binding(&mut self, what: &str) -> PResult<Ident> {
        let id = self.lower(what)?;
        if id.name == "result" {
            let d = Diagnostic::error(
                codes::RESERVED_NAME,
                id.span,
                format!("`result` is reserved and cannot be used as {what}"),
            )
            .with_help("`result` names an invoke's result inside `done`; choose another name");
            self.error(d);
        }
        Ok(id)
    }

    // ───────────── doc comments ─────────────

    /// The `///` comments between the previous token and the current one.
    fn take_doc(&mut self) -> Option<Doc> {
        let lo = if self.pos == 0 {
            0
        } else {
            self.prev().end.offset
        };
        let hi = self.tok().span.start.offset;
        let first = self.comments.partition_point(|c| c.span.start.offset < lo);
        let mut lines = Vec::new();
        let mut span: Option<Span> = None;
        for i in first..self.comments.len() {
            let c = &self.comments[i];
            if c.span.start.offset >= hi {
                break;
            }
            if c.kind == CommentKind::Doc {
                self.doc_used[i] = true;
                let line = &c.text[3..];
                lines.push(line.strip_prefix(' ').unwrap_or(line).to_string());
                span = Some(span.map_or(c.span, |s| s.to(c.span)));
            }
        }
        span.map(|span| Doc {
            text: lines.join("\n"),
            span,
        })
    }

    fn unattached_docs(&mut self) {
        for (c, used) in self.comments.iter().zip(&self.doc_used) {
            if c.kind == CommentKind::Doc && !used {
                let slashes = Span {
                    end: crate::span::Pos {
                        offset: c.span.start.offset + 3,
                        line: c.span.start.line,
                        col: c.span.start.col + 3,
                    },
                    ..c.span
                };
                self.diags.push(
                    Diagnostic::error(codes::DOC_COMMENT_DOCUMENTS_NOTHING, c.span, "this doc comment does not document anything")
                        .with_help("`///` documents the declaration, machine, state, history state, event, variable or check that follows it; use `//` for other comments")
                        .with_fix("make it a plain comment", slashes, "//"),
                );
            }
        }
    }

    // ───────────── recovery ─────────────

    /// Skip tokens until one satisfying `stop`, a declaration keyword or an unmatched `}` at
    /// the current depth. At the top level (`in_block == false`) unmatched `}` are skipped.
    fn recover(&mut self, start: usize, in_block: bool, stop: impl Fn(T) -> bool) {
        if self.pos == start {
            self.bump();
        }
        let mut depth = 0usize;
        loop {
            let k = self.peek();
            match k {
                T::Eof => return,
                T::Punct(P::LBrace | P::LParen | P::LBracket) => depth += 1,
                T::Punct(P::RBrace | P::RParen | P::RBracket) => {
                    if depth > 0 {
                        depth -= 1;
                    } else if in_block && k == T::Punct(P::RBrace) {
                        return;
                    }
                }
                _ if depth == 0 && (stop(k) || is_decl_start(k)) => return,
                // `check` is not a keyword, but one at the start of a line in a body starts the
                // next item: a run of checks recovers check by check.
                T::Lower if depth == 0 && in_block && self.at_check() && self.starts_line() => {
                    return;
                }
                _ => {}
            }
            self.bump();
        }
    }

    // ───────────── file and declarations ─────────────

    fn file(&mut self) -> File {
        let mut items = Vec::new();
        while !self.at(T::Eof) {
            let start = self.pos;
            let item = match self.peek() {
                T::Keyword(K::Import) => self.import().map(TopItem::Import),
                k if is_decl_start(k) => self.decl().map(TopItem::Decl),
                _ if self.at_check() => {
                    let d = Diagnostic::error(codes::NOT_AT_TOP_LEVEL, self.tok().span, "`check` must be inside a machine")
                        .with_help("a `check` is about one machine's states: write it directly in that machine's body");
                    self.error(d);
                    Err(Fail)
                }
                k if is_item_start(k) => {
                    let word = self.text(self.tok()).to_string();
                    let d = Diagnostic::error(codes::NOT_AT_TOP_LEVEL, self.tok().span, format!("`{word}` must be inside a machine"))
                        .with_help("the top level of a file holds imports and declarations (type, enum, error, extern, fn, action, task, machine)");
                    self.error(d);
                    Err(Fail)
                }
                _ => Err(self.unexpected("a declaration (`type`, `enum`, `error`, `extern`, `fn`, `action`, `task`, `machine`, `strict machine`) or `import`")),
            };
            match item {
                Ok(i) => items.push(i),
                Err(Fail) => {
                    self.depth = 0;
                    self.recover(start, false, |_| false)
                }
            }
        }
        let eof = self.tok().span;
        let start = self.tokens[0].span;
        File {
            items,
            comments: self.comments.clone(),
            span: Span {
                start: crate::span::Pos::default(),
                ..start
            }
            .to(eof),
        }
    }

    fn import(&mut self) -> PResult<Import> {
        let start = self.bump().span;
        let (names, _) = self.list(P::LBrace, "the import list", |p| match p.peek() {
            T::Upper | T::Lower => Ok(p.bump_ident()),
            T::Keyword(_) => p.keyword_as_name("a name to import"),
            _ => Err(p.unexpected("a name to import")),
        })?;
        if names.is_empty() {
            let d = Diagnostic::error(
                codes::UNEXPECTED_TOKEN,
                self.prev(),
                "an import list names at least one declaration",
            );
            self.error(d);
        }
        if !(self.at(T::Lower) && self.text(self.tok()) == "from") {
            return Err(self.unexpected("`from`"));
        }
        self.bump();
        if !self.at(T::String) {
            return Err(self.unexpected("the path of the file to import, as a string"));
        }
        let t = self.bump();
        let path = StringLit {
            value: unescape(self.text(t)),
            span: t.span,
        };
        Ok(Import {
            names,
            path,
            span: self.since(start),
        })
    }

    fn decl(&mut self) -> PResult<Decl> {
        let doc = self.take_doc();
        let kw = self.bump();
        let start = kw.span;
        let T::Keyword(k) = kw.kind else {
            unreachable!("decl() is called at a declaration keyword")
        };
        let (name, kind) = self.decl_kind(k)?;
        self.not_unanswered(&name);
        Ok(Decl {
            doc,
            name,
            kind,
            span: self.since(start),
        })
    }

    /// Reports a declaration, event or emitted event named `Unanswered`, the built-in error
    /// every call can fail with, or `Ended`, the built-in enum of how a peer's run ended.
    fn not_unanswered(&mut self, name: &Ident) {
        let what = match name.name.as_str() {
            "Unanswered" => "the built-in error of a call that is not answered",
            "Ended" => {
                "the built-in enum of how a peer's run ended (`event.status` in `on name.ended`)"
            }
            _ => return,
        };
        let d = Diagnostic::error(
            codes::RESERVED_NAME,
            name.span,
            format!("`{}` is {what}, and cannot be declared", name.name),
        )
        .with_help("choose another name");
        self.error(d);
    }

    /// Reports `on from.word` with a lower-case `word` that is none of `done`, `fail` and
    /// `ended`: after the dot comes an event, which is upper-case, or one of those.
    fn unknown_lifecycle(&mut self, from: &Ident, word: &Ident) {
        let mut d = Diagnostic::error(
            codes::UNKNOWN_LIFECYCLE_TRIGGER,
            word.span,
            format!(
                "`on {}.{}`: `{}` is not an event name, `done`, `fail` or `ended`",
                from.name, word.name, word.name
            ),
        );
        // A misspelling, or a word for a run's end.
        let ends = [
            "closed",
            "stopped",
            "finished",
            "exited",
            "terminated",
            "died",
        ];
        let close = if word.name.starts_with("end") || ends.contains(&word.name.as_str()) {
            Some("ended")
        } else {
            ["done", "fail", "ended"]
                .into_iter()
                .find(|c| edits(&word.name, c) <= 1)
        };
        d = match close {
            Some(c) => d.with_help(format!("did you mean `{c}`?")).with_fix(
                format!("replace with `{c}`"),
                word.span,
                c,
            ),
            None => {
                let fixed = capitalize(&word.name, true);
                d.with_help(format!(
                    "events are upper-case: `on {}.{fixed}` is taken for an event `{}` emits; `done` and `fail` take a spawned child's outcome, and `ended` the end of the run a `ref` holds",
                    from.name, from.name
                ))
                .with_fix(format!("rename to `{fixed}`"), word.span, fixed)
            }
        };
        self.error(d);
    }

    /// The name and kind of the declaration after keyword `k`.
    fn decl_kind(&mut self, k: K) -> PResult<(Ident, DeclKind)> {
        Ok(match k {
            K::Type => {
                let name = self.upper("a type name")?;
                let params = if self.at_p(P::Lt) {
                    self.angle_list("the type parameters", |p| p.upper("a type parameter"))?
                } else {
                    Vec::new()
                };
                let kind = if self.eat_p(P::Eq).is_some() {
                    DeclKind::Alias {
                        params,
                        target: self.type_ref()?,
                    }
                } else if self.at_p(P::LBrace) {
                    DeclKind::Record {
                        params,
                        fields: self.fields("the fields of `type`")?,
                    }
                } else {
                    let after = if params.is_empty() { "" } else { "<…>" };
                    return Err(
                        self.unexpected(&format!("`=` or `{{` after `type {}{after}`", name.name))
                    );
                };
                (name, kind)
            }
            K::Enum => {
                let name = self.upper("an enum name")?;
                let (variants, _) = self.list(P::LBrace, "the enum's variants", |p| {
                    p.upper("an enum variant")
                })?;
                if variants.is_empty() {
                    let d = Diagnostic::error(
                        codes::UNEXPECTED_TOKEN,
                        self.prev(),
                        format!("enum `{}` needs at least one variant", name.name),
                    );
                    self.error(d);
                }
                (name, DeclKind::Enum { variants })
            }
            K::Error => {
                let name = self.upper("an error name")?;
                let fields = if self.at_p(P::LBrace) {
                    self.fields("the fields of `error`")?
                } else {
                    Vec::new()
                };
                (name, DeclKind::Error { fields })
            }
            K::Extern => {
                if self.eat_kw(K::Type).is_none() {
                    return Err(self.unexpected("`type` after `extern`"));
                }
                (self.upper("a type name")?, DeclKind::Extern)
            }
            K::Fn => {
                let name = self.lower("a function name")?;
                let params = self.params()?;
                self.expect_p(P::Arrow, " and the return type: a `fn` returns a value")?;
                (
                    name,
                    DeclKind::Fn {
                        params,
                        returns: self.type_ref()?,
                    },
                )
            }
            K::Action => {
                let name = self.lower("an action name")?;
                (
                    name,
                    DeclKind::Action {
                        params: self.params()?,
                    },
                )
            }
            K::Task => {
                let name = self.lower("a task name")?;
                let params = self.params()?;
                let returns = if self.eat_p(P::Arrow).is_some() {
                    Some(self.type_ref()?)
                } else {
                    None
                };
                let errors = self.errors()?;
                let mut emits = Vec::new();
                while self.at_kw(K::Emits) {
                    emits.push(self.event()?);
                }
                (
                    name,
                    DeclKind::Task {
                        params,
                        returns,
                        errors,
                        emits,
                    },
                )
            }
            K::Machine | K::Strict => {
                let strict = k == K::Strict;
                if strict && self.eat_kw(K::Machine).is_none() {
                    return Err(self.unexpected("`machine` after `strict`"));
                }
                let name = self.upper("a machine name")?;
                let params = self.params()?;
                let returns = if self.eat_p(P::Arrow).is_some() {
                    Some(self.type_ref()?)
                } else {
                    None
                };
                let errors = self.errors()?;
                let body = self.body(BodyKind::Machine, &format!("machine `{}`", name.name))?;
                (
                    name,
                    DeclKind::Machine(Machine {
                        strict,
                        params,
                        returns,
                        errors,
                        body,
                    }),
                )
            }
            K::Import => unreachable!("imports are parsed by import()"),
            _ => unreachable!("decl() is called at a declaration keyword"),
        })
    }

    /// Whether the current token starts a top-level declaration. `error` does so only before an
    /// upper-case name: elsewhere it is the implicit name of an invoke's `fail` outcome, as in
    /// `do log(error)`. A declaration keyword followed by `:` is a misused name.
    fn at_decl_start(&self) -> bool {
        is_decl_start(self.peek())
            && (self.peek() != T::Keyword(K::Error) || self.nth(1) == T::Upper)
            && self.nth(1) != T::Punct(P::Colon)
    }

    /// A comma-separated list between `open` and its closer, trailing comma allowed. Returns
    /// the elements and the span from the opener to the closer.
    fn list<E>(
        &mut self,
        open: P,
        what: &str,
        mut elem: impl FnMut(&mut Self) -> PResult<E>,
    ) -> PResult<(Vec<E>, Span)> {
        let close = match open {
            P::LParen => P::RParen,
            _ => P::RBrace,
        };
        let open_tok = self.expect_p(open, &format!(" to start {what}"))?;
        let mut out = Vec::new();
        loop {
            if self.at_p(close) {
                break;
            }
            if self.at(T::Eof) || self.at_decl_start() {
                return Err(self.unclosed(open_tok, what));
            }
            out.push(elem(self)?);
            if self.eat_p(P::Comma).is_none() {
                if self.at_p(close) {
                    break;
                }
                if self.at(T::Eof) || self.at_decl_start() {
                    return Err(self.unclosed(open_tok, what));
                }
                return Err(self.unexpected(&format!("`,` or `{}`", close.as_str())));
            }
        }
        self.bump();
        Ok((out, open_tok.span.to(self.prev())))
    }

    fn fields(&mut self, what: &str) -> PResult<Vec<Field>> {
        Ok(self.list(P::LBrace, what, |p| p.field(false))?.0)
    }

    fn params(&mut self) -> PResult<Vec<Field>> {
        Ok(self
            .list(P::LParen, "the parameter list", |p| p.field(true))?
            .0)
    }

    fn field(&mut self, param: bool) -> PResult<Field> {
        let start = self.tok().span;
        let sensitive = self.at_sensitive(T::Lower);
        if sensitive {
            self.bump();
        }
        let name = if param {
            self.binding("a parameter name")?
        } else {
            self.lower("a field name")?
        };
        self.expect_p(P::Colon, &format!(" and a type after `{}`", name.name))?;
        let ty = self.type_ref()?;
        Ok(Field {
            span: start.to(ty.span),
            sensitive,
            name,
            ty,
        })
    }

    /// A non-empty comma-separated list between `<` and `>`, trailing comma allowed. A `>=`
    /// where the `>` is expected is split in two, so `var m: map<string, int>= f()` reads as
    /// written.
    fn angle_list<E>(
        &mut self,
        what: &str,
        mut elem: impl FnMut(&mut Self) -> PResult<E>,
    ) -> PResult<Vec<E>> {
        let open = self.expect_p(P::Lt, &format!(" to start {what}"))?;
        let mut out = Vec::new();
        loop {
            if !out.is_empty() && self.close_angle() {
                break;
            }
            if self.at(T::Eof) || self.at_decl_start() {
                return Err(self.unclosed(open, what));
            }
            out.push(elem(self)?);
            if self.eat_p(P::Comma).is_none() {
                if self.close_angle() {
                    break;
                }
                return Err(self.unexpected("`,` or `>`"));
            }
        }
        Ok(out)
    }

    /// Takes a `>`, or the `>` of a `>=` (leaving its `=`), if the next token starts with one.
    fn close_angle(&mut self) -> bool {
        if self.eat_p(P::Gt).is_some() {
            return true;
        }
        if self.at_p(P::Ge) {
            let t = self.tok();
            let mid = Pos {
                offset: t.span.start.offset + 1,
                line: t.span.start.line,
                col: t.span.start.col + 1,
            };
            self.tokens[self.pos] = Token {
                kind: T::Punct(P::Eq),
                span: Span {
                    start: mid,
                    ..t.span
                },
            };
            self.tokens.insert(
                self.pos,
                Token {
                    kind: T::Punct(P::Gt),
                    span: Span { end: mid, ..t.span },
                },
            );
            self.bump();
            return true;
        }
        false
    }

    /// A type argument: a type one level deeper.
    fn type_arg(&mut self) -> PResult<TypeRef> {
        let base = self.depth;
        self.deeper()?;
        let t = self.type_ref();
        self.depth = base;
        t
    }

    fn type_ref(&mut self) -> PResult<TypeRef> {
        let t = self.tok();
        let kind = match t.kind {
            T::Upper => {
                let name = self.bump_ident();
                if self.at_p(P::Lt) {
                    TypeKind::Generic(
                        name,
                        self.angle_list("the type arguments", |p| p.type_arg())?,
                    )
                } else {
                    TypeKind::Named(name)
                }
            }
            T::Lower if self.text(t) == "map" && self.nth(1) == T::Punct(P::Lt) => {
                self.bump();
                let open = self.tok();
                let mut args =
                    self.angle_list("the key and value types of `map`", |p| p.type_arg())?;
                if args.len() != 2 {
                    let d = Diagnostic::error(
                        codes::UNEXPECTED_TOKEN,
                        open.span.to(self.prev()),
                        format!(
                            "`map` takes a key type and a value type, not {}",
                            args.len()
                        ),
                    )
                    .with_help("write `map<string, T>`");
                    self.error(d);
                    return Err(Fail);
                }
                let v = args.pop().expect("two");
                let k = args.pop().expect("two");
                TypeKind::Map(Box::new(k), Box::new(v))
            }
            T::Lower if self.text(t) == "ref" && self.nth(1) == T::Upper => {
                self.bump();
                TypeKind::Ref(self.bump_ident())
            }
            T::Lower if self.text(t) == "ref" => {
                self.bump();
                return Err(self.unexpected("the name of a machine after `ref`"));
            }
            T::Keyword(K::Call) if self.nth(1) == T::Lower => {
                self.bump();
                TypeKind::Call(self.bump_ident())
            }
            T::Keyword(K::Call) => {
                self.bump();
                return Err(self.unexpected("the name of one of the machine's calls after `call`"));
            }
            T::Lower => {
                self.bump();
                let id = self.ident(t);
                match Builtin::lookup(&id.name) {
                    Some(b) => TypeKind::Builtin(b),
                    None => {
                        let d = Diagnostic::error(codes::UNKNOWN_BUILTIN_TYPE, t.span, format!("`{}` is not a built-in type", id.name))
                            .with_help("the built-in types are `string`, `int`, `float`, `bool`, `duration` and `json`; declared types start with an upper-case letter");
                        self.error(d);
                        TypeKind::Named(id)
                    }
                }
            }
            _ => return Err(self.unexpected("a type")),
        };
        let mut ty = TypeRef {
            kind,
            span: t.span.to(self.prev()),
        };
        let base = self.depth;
        loop {
            if matches!(self.peek(), T::Punct(P::LBracket | P::Question)) {
                self.deeper()?;
            }
            if self.eat_p(P::LBracket).is_some() {
                self.expect_p(P::RBracket, " after `[` in a list type `T[]`")?;
                ty = TypeRef {
                    span: ty.span.to(self.prev()),
                    kind: TypeKind::List(Box::new(ty)),
                };
            } else if self.eat_p(P::Question).is_some() {
                ty = TypeRef {
                    span: ty.span.to(self.prev()),
                    kind: TypeKind::Optional(Box::new(ty)),
                };
            } else {
                self.depth = base;
                return Ok(ty);
            }
        }
    }

    fn errors(&mut self) -> PResult<Vec<Ident>> {
        let mut out = Vec::new();
        if self.eat_p(P::Bang).is_some() {
            out.push(self.upper("an error name")?);
            while self.eat_p(P::Pipe).is_some() {
                out.push(self.upper("an error name")?);
            }
        }
        Ok(out)
    }

    // ───────────── machine and state bodies ─────────────

    fn body(&mut self, kind: BodyKind, owner: &str) -> PResult<Body> {
        let open = self.expect_p(P::LBrace, &format!(" to start the body of {owner}"))?;
        let mut items = Vec::new();
        loop {
            let k = self.peek();
            if k == T::Punct(P::RBrace) {
                self.bump();
                break;
            }
            if k == T::Eof || is_decl_start(k) {
                // Keep what was parsed: the tree stays useful to an editor.
                self.unclosed(open, owner);
                break;
            }
            let (start, depth) = (self.pos, self.depth);
            match self.item(kind) {
                Ok(Some(item)) => items.push(item),
                Ok(None) => {}
                Err(Fail) => {
                    self.depth = depth;
                    self.recover(start, true, is_item_start)
                }
            }
        }
        Ok(Body {
            items,
            span: open.span.to(self.prev()),
        })
    }

    /// One body item. `Ok(None)` is a well-formed item in the wrong kind of body, reported and
    /// left out of the tree.
    fn item(&mut self, kind: BodyKind) -> PResult<Option<Item>> {
        // Nested states recurse through here, so a state is parsed before entering the large
        // frame of `other_item`: the deepest nesting allowed must fit a thread's stack in a
        // debug build.
        if matches!(self.peek(), T::Keyword(K::State | K::Final | K::Parallel)) {
            return Ok(Some(Item::State(self.state()?)));
        }
        self.other_item(kind)
    }

    /// A body item other than a state.
    #[inline(never)]
    fn other_item(&mut self, kind: BodyKind) -> PResult<Option<Item>> {
        if let Some(deep) = self.at_history() {
            let h = self.history(deep)?;
            if kind == BodyKind::Machine {
                let word = if deep { "deep history" } else { "history" };
                let d = Diagnostic::error(
                    codes::NOT_IN_MACHINE_BODY,
                    h.span,
                    format!("`{word}` is only allowed in a state with child states, not directly in a machine body"),
                )
                .with_help("the machine is never left, so it has nothing to return to; put the history state in the state it returns to");
                self.error(d);
                return Ok(None);
            }
            return Ok(Some(Item::History(h)));
        }
        let mut t = self.tok();
        if self.at_check() {
            return self.check_item(kind);
        }
        if self.at_internal_event() {
            let decl = self.internal_event()?;
            if kind == BodyKind::State {
                let d = Diagnostic::error(
                    codes::NOT_IN_STATE,
                    t.span,
                    "`internal event` is only allowed directly in a machine body",
                )
                .with_help("move the event to the top of the machine, with its other events");
                self.error(d);
                return Ok(None);
            }
            return Ok(Some(Item::InternalEvent(decl)));
        }
        if self.at_children() {
            let decl = self.children()?;
            if kind == BodyKind::State {
                let d = Diagnostic::error(codes::NOT_IN_STATE, t.span, "`children` is only allowed directly in a machine body")
                    .with_help("move the `children` to the top of the machine; a collection belongs to the machine, not to a state");
                self.error(d);
                return Ok(None);
            }
            return Ok(Some(Item::Children(decl)));
        }
        // `sensitive var`: `sensitive` is not reserved, so it is a modifier only here.
        if self.at_sensitive(T::Keyword(K::Var)) {
            t = self.nth_tok(1);
        }
        let T::Keyword(k) = t.kind else {
            let expected = match kind {
                BodyKind::Machine => {
                    "a machine item (`var`, `event`, `internal event`, `emits`, `call`, `children`, `initial`, `state`, `entry`, `exit`, `on`, `after`, `always`, `check`) or `}`"
                }
                BodyKind::State => {
                    "a state item (`var`, `initial`, `state`, `history`, `entry`, `exit`, `on`, `after`, `always`, `done`, `invoke`, `output`, `fail`) or `}`"
                }
            };
            return Err(self.unexpected(expected));
        };
        let machine_only = matches!(k, K::Event | K::Emits | K::Call);
        let state_only = matches!(k, K::Done | K::Invoke | K::Output | K::Fail);
        let item = match k {
            K::Var => Item::Var(self.var()?),
            K::Event => Item::Event(self.event()?),
            K::Emits => Item::Emits(self.event()?),
            K::Call => Item::Call(self.call_decl()?),
            K::Initial => {
                self.bump();
                Item::Initial(self.upper("the name of the initial state")?)
            }
            K::State | K::Final | K::Parallel => Item::State(self.state()?),
            K::Entry | K::Exit => {
                self.bump();
                let effects = self.effects()?;
                if effects.is_empty() {
                    let expected = format!(
                        "`set`, `do`, `send`, `emit`, `reply`, `spawn`, `stop`, `raise` or `cancel` after `{}`",
                        self.text(t)
                    );
                    return Err(self.unexpected(&expected));
                }
                let hook = Hook {
                    effects,
                    span: self.since(t.span),
                };
                if k == K::Entry {
                    Item::Entry(hook)
                } else {
                    Item::Exit(hook)
                }
            }
            K::On => {
                self.bump();
                let trigger = if self.at(T::Lower) && self.nth(1) == T::Punct(P::Dot) {
                    let from = self.bump_ident();
                    self.bump();
                    if self.eat_kw(K::Done).is_some() {
                        Trigger::ChildDone { from }
                    } else if self.eat_kw(K::Fail).is_some() {
                        let error = self.pattern("an error name or `*`")?;
                        Trigger::ChildFail { from, error }
                    } else if self.at(T::Lower) && self.text(self.tok()) == "ended" {
                        self.bump();
                        Trigger::Ended { from }
                    } else if self.at(T::Lower) {
                        let word = self.bump_ident();
                        self.unknown_lifecycle(&from, &word);
                        Trigger::Receive { from, event: word }
                    } else {
                        let event = self.upper(&format!(
                            "the name of an event `{}` emits, `done`, `fail` or `ended`",
                            from.name
                        ))?;
                        Trigger::Receive { from, event }
                    }
                } else if self.at(T::Lower) {
                    Trigger::Call(self.bump_ident())
                } else {
                    Trigger::On(
                        self.pattern("an event name, `*`, `name.Event` or the name of a call")?,
                    )
                };
                Item::Transition(self.transition(t.span, trigger)?)
            }
            K::After => {
                self.bump();
                let delay = self.expr(true)?;
                self.check_record_after(&delay)?;
                Item::Transition(self.transition(t.span, Trigger::After(delay))?)
            }
            K::Always => {
                self.bump();
                Item::Transition(self.transition(t.span, Trigger::Always)?)
            }
            K::Done => {
                self.bump();
                Item::Transition(self.transition(t.span, Trigger::Done)?)
            }
            K::Invoke => Item::Invoke(self.invoke()?),
            K::Output => {
                self.bump();
                let value = self.expr(false)?;
                Item::Output(FinalValue {
                    span: self.since(t.span),
                    value,
                })
            }
            K::Fail => {
                self.bump();
                let outcome_like = match self.peek() {
                    T::Punct(P::Star) => true,
                    T::Upper => {
                        matches!(
                            self.nth(1),
                            T::Punct(P::Arrow)
                                | T::Keyword(K::If | K::Set | K::Do | K::Send | K::Emit | K::Reply)
                        ) || self.nth(1) == T::Punct(P::LBrace)
                            && matches!(self.nth(2), T::Keyword(K::If | K::Else))
                    }
                    _ => false,
                };
                if outcome_like {
                    let d = Diagnostic::error(codes::OUTCOME_OUTSIDE_INVOKE, t.span.to(self.tok().span), "`fail Error -> …` is an invoke outcome and belongs inside `invoke { }`")
                        .with_help("in a final state, `fail expr` (with no transition) is the machine's failure");
                    self.error(d);
                    return Err(Fail);
                }
                let value = self.expr(false)?;
                Item::Fail(FinalValue {
                    span: self.since(t.span),
                    value,
                })
            }
            _ => {
                let d = match k {
                    K::If | K::Else => Diagnostic::error(codes::UNEXPECTED_TOKEN, t.span, format!("`{}` must follow a trigger", self.text(t)))
                        .with_help("guarded branches are written `on Event if guard -> Target`, or in a block `on Event { if … else … }`"),
                    _ => {
                        self.unexpected("a machine or state item");
                        return Err(Fail);
                    }
                };
                self.error(d);
                return Err(Fail);
            }
        };
        let word = self.text(t).to_string();
        if kind == BodyKind::State && machine_only {
            let d = Diagnostic::error(codes::NOT_IN_STATE, t.span, format!("`{word}` is only allowed directly in a machine body"))
                .with_help(format!("move the `{word}` to the top of the machine; a machine's events and calls belong to all its states"));
            self.error(d);
            return Ok(None);
        }
        if kind == BodyKind::Machine && state_only {
            let d = Diagnostic::error(
                codes::NOT_IN_MACHINE_BODY,
                t.span,
                format!("`{word}` is only allowed in a state, not directly in a machine body"),
            )
            .with_help(format!("put the `{word}` in a state"));
            self.error(d);
            return Ok(None);
        }
        Ok(Some(item))
    }

    /// `check` is not reserved: it starts a check only followed by a name (`never`,
    /// `reachable`, `can` or a state), which no other item can begin with.
    fn at_check(&self) -> bool {
        self.at(T::Lower)
            && self.text(self.tok()) == "check"
            && matches!(self.nth(1), T::Lower | T::Upper)
    }

    /// Whether the current token is the first on its line.
    fn starts_line(&self) -> bool {
        self.pos == 0 || self.prev().end.line < self.tok().span.start.line
    }

    /// Whether the current token is the lower-case word `word`.
    fn at_word(&self, word: &str) -> bool {
        self.at(T::Lower) && self.text(self.tok()) == word
    }

    /// A check, reported and left out in a state. Not inlined: `item` is on the recursive
    /// path of nested states, and a larger frame there lowers how deep they fit in the stack.
    #[inline(never)]
    fn check_item(&mut self, kind: BodyKind) -> PResult<Option<Item>> {
        let t = self.tok();
        let c = self.check()?;
        if kind == BodyKind::State {
            let d = Diagnostic::error(codes::NOT_IN_STATE, t.span, "`check` is only allowed directly in a machine body")
                .with_help("move the `check` into the machine's body; its state paths are written from there, like a target at machine level");
            self.error(d);
            return Ok(None);
        }
        Ok(Some(Item::Check(c)))
    }

    /// `check property`, where `property` is `never A { and B }`, `reachable A`,
    /// `A only after B` or `can finish`. All the words but `and` and `after` are contextual.
    fn check(&mut self) -> PResult<Check> {
        let doc = self.take_doc();
        let start = self.bump().span;
        let property = if self.at_word("never") {
            self.bump();
            let mut states = vec![self.state_path()?];
            while self.eat_kw(K::And).is_some() {
                states.push(self.state_path()?);
            }
            Property::Never(states)
        } else if self.at_word("reachable") {
            self.bump();
            Property::Reachable(self.state_path()?)
        } else if self.at_word("can") {
            self.bump();
            if !self.at_word("finish") {
                return Err(self.unexpected("`finish` after `check can`"));
            }
            self.bump();
            Property::CanFinish
        } else if self.at(T::Upper) {
            let state = self.state_path()?;
            if self.at_kw(K::After) {
                // `check A after B`: the `only` is missing. Reported, and parsed as meant, so
                // the `after` does not start a transition.
                let at = self.tok().span;
                let d = Diagnostic::error(codes::UNEXPECTED_TOKEN, at, "expected `only after` after the state of a `check`, found keyword `after`")
                    .with_help("a check that one state is entered only after another is written `check A only after B`")
                    .with_fix("insert `only`", Span { end: at.start, ..at }, "only ");
                self.error(d);
            } else if self.at_word("only") {
                self.bump();
                if !self.at_kw(K::After) {
                    return Err(self.unexpected("`after` after `only`"));
                }
            } else {
                return Err(self.unexpected("`only after` after the state of a `check`"));
            }
            self.bump();
            let after = self.state_path()?;
            Property::OnlyAfter { state, after }
        } else {
            return Err(
                self.unexpected("`never`, `reachable`, `can finish` or a state after `check`")
            );
        };
        Ok(Check {
            doc,
            property,
            span: self.since(start),
        })
    }

    /// `Name { . Name }`: a state named in a `check`.
    fn state_path(&mut self) -> PResult<Target> {
        // A lower-case name on the next line is the next item, not a misspelt state.
        if !self.at(T::Upper) && self.starts_line() {
            return Err(self.unexpected("a state"));
        }
        let mut path = vec![self.upper("a state name")?];
        while self.eat_p(P::Dot).is_some() {
            path.push(self.upper("a state name")?);
        }
        let span = path[0].span.to(path[path.len() - 1].span);
        let args = vec![None; path.len()];
        Ok(Target {
            dot: false,
            path,
            args,
            span,
        })
    }

    /// `children` is not reserved: it starts a collection only as `children name:`.
    fn at_children(&self) -> bool {
        self.at(T::Lower)
            && self.text(self.tok()) == "children"
            && self.nth(1) == T::Lower
            && self.nth(2) == T::Punct(P::Colon)
    }

    /// `children name: Machine by Key` or `children name: task by Key`
    fn children(&mut self) -> PResult<ChildrenDecl> {
        let doc = self.take_doc();
        let start = self.bump().span;
        let name = self.binding("the name of a children collection")?;
        self.expect_p(
            P::Colon,
            &format!(" and a machine or task after `children {}`", name.name),
        )?;
        // A lower-case name is a task, an upper-case one a machine. `by` is not reserved, so
        // `children x: by …` names no task: it is the key word with the task left out.
        let what = "the machine or task of the collection's children";
        let src = match self.peek() {
            T::Lower if self.text(self.tok()) == "by" => return Err(self.unexpected(what)),
            T::Upper | T::Lower => self.bump_ident(),
            T::Keyword(_) => self.keyword_as_name(what)?,
            _ => return Err(self.unexpected(what)),
        };
        if !(self.at(T::Lower) && self.text(self.tok()) == "by") {
            return Err(self.unexpected(&format!(
                "`by` and the key type after `children {}: {}`",
                name.name, src.name
            )));
        }
        self.bump();
        let key = self.type_ref()?;
        Ok(ChildrenDecl {
            doc,
            name,
            src,
            key,
            span: self.since(start),
        })
    }

    /// `Some(deep)` at the start of `history Name` or `deep history`. Both words are
    /// contextual: no name can start an item, so they are not reserved.
    fn at_history(&self) -> Option<bool> {
        if !self.at(T::Lower) {
            return None;
        }
        match self.text(self.tok()) {
            "history" if self.nth(1) == T::Upper => Some(false),
            "deep" if matches!(self.nth(1), T::Lower | T::Upper) => Some(true),
            _ => None,
        }
    }

    fn history(&mut self, deep: bool) -> PResult<History> {
        let doc = self.take_doc();
        let start = self.bump().span;
        if deep {
            if !(self.at(T::Lower) && self.text(self.tok()) == "history") {
                return Err(self.unexpected("`history` after `deep`"));
            }
            self.bump();
        }
        let name = self.upper("the name of the history state")?;
        Ok(History {
            doc,
            deep,
            name,
            span: self.since(start),
        })
    }

    fn var(&mut self) -> PResult<Var> {
        let doc = self.take_doc();
        let start = self.tok().span;
        let sensitive = self.at_sensitive(T::Keyword(K::Var));
        if sensitive {
            self.bump();
        }
        self.bump();
        let name = self.binding("a variable name")?;
        self.expect_p(P::Colon, &format!(" and a type after `var {}`", name.name))?;
        let ty = self.type_ref()?;
        let init = if self.eat_p(P::Eq).is_some() {
            Some(self.expr(false)?)
        } else {
            None
        };
        Ok(Var {
            doc,
            sensitive,
            name,
            ty,
            init,
            span: self.since(start),
        })
    }

    /// `internal` is not reserved: it starts an item only as `internal event`.
    fn at_internal_event(&self) -> bool {
        self.at(T::Lower)
            && self.text(self.tok()) == "internal"
            && self.nth(1) == T::Keyword(K::Event)
    }

    /// `internal event Name [{ fields }]`
    fn internal_event(&mut self) -> PResult<EventDecl> {
        let doc = self.take_doc();
        let start = self.bump().span;
        self.bump();
        self.event_rest(doc, start)
    }

    fn event(&mut self) -> PResult<EventDecl> {
        let doc = self.take_doc();
        let start = self.bump().span;
        self.event_rest(doc, start)
    }

    fn event_rest(&mut self, doc: Option<Doc>, start: Span) -> PResult<EventDecl> {
        let name = self.upper("an event name")?;
        self.not_unanswered(&name);
        let fields = if self.at_p(P::LBrace) {
            self.fields("the event's fields")?
        } else {
            Vec::new()
        };
        Ok(EventDecl {
            doc,
            name,
            fields,
            span: self.since(start),
        })
    }

    fn state(&mut self) -> PResult<State> {
        let doc = self.take_doc();
        self.deeper()?;
        let start = self.tok().span;
        let kind = if self.eat_kw(K::Final).is_some() {
            StateKind::Final
        } else if self.eat_kw(K::Parallel).is_some() {
            StateKind::Parallel
        } else {
            StateKind::Normal
        };
        if self.eat_kw(K::State).is_none() {
            return Err(self.unexpected("`state`"));
        }
        let name = self.upper("a state name")?;
        let params = if self.at_p(P::LParen) {
            self.params()?
        } else {
            Vec::new()
        };
        let body = self.body(BodyKind::State, &format!("state `{}`", name.name))?;
        self.depth -= 1;
        Ok(State {
            doc,
            kind,
            name,
            params,
            body,
            span: self.since(start),
        })
    }

    fn pattern(&mut self, what: &str) -> PResult<Pattern> {
        match self.eat_p(P::Star) {
            Some(t) => Ok(Pattern::Any { span: t.span }),
            None => Ok(Pattern::Name(self.upper(what)?)),
        }
    }

    /// `after X { … }` is a branch block, so a record value as the delay needs parentheses.
    fn check_record_after(&mut self, delay: &Expr) -> PResult<()> {
        if let ExprKind::Record { ty, fields } = &delay.kind {
            if fields.is_empty()
                && self.at_p(P::LBrace)
                && self.nth(1) == T::Lower
                && self.nth(2) == T::Punct(P::Colon)
            {
                let d = Diagnostic::error(codes::RECORD_IN_AFTER, delay.span.to(self.tok().span), "a record value in an `after` delay must be parenthesised")
                    .with_help(format!("write `after ({} {{ … }}) …`: a `{{` after the delay starts a block of branches", ty.name));
                self.error(d);
                return Err(Fail);
            }
        }
        Ok(())
    }

    fn transition(&mut self, start: Span, trigger: Trigger) -> PResult<Transition> {
        let trigger_span = self.since(start);
        let ignore = matches!(
            trigger,
            Trigger::On(_) | Trigger::Receive { .. } | Trigger::Ended { .. }
        );
        let branches = self.branches(trigger_span, ignore)?;
        Ok(Transition {
            trigger,
            branches,
            trigger_span,
            span: self.since(start),
        })
    }

    /// `ignore` says whether a branch may be `ignore`: only in an `on` transition.
    fn branches(&mut self, trigger: Span, ignore: bool) -> PResult<Branches> {
        if !self.at_p(P::LBrace) {
            let start = self.tok().span;
            let guard = if self.eat_kw(K::If).is_some() {
                Some(self.expr(false)?)
            } else {
                None
            };
            return Ok(Branches::Single(
                self.action(start, guard, trigger, ignore)?,
            ));
        }
        let open = self.bump();
        let mut branches: Vec<Branch> = Vec::new();
        let mut seen_else = false;
        let errors_before = self.diags.len();
        loop {
            let k = self.peek();
            if k == T::Punct(P::RBrace) {
                self.bump();
                break;
            }
            if k == T::Eof || is_decl_start(k) {
                return Err(self.unclosed(open, "the block of branches"));
            }
            let (start, depth) = (self.pos, self.depth);
            let t = self.tok();
            let result = match k {
                T::Keyword(K::If | K::Else) if seen_else => {
                    let d = Diagnostic::error(codes::MALFORMED_BRANCH_BLOCK, t.span, "`else` must be the last branch of a block")
                        .with_help("branches are tried in order, so a branch after `else` could never be taken");
                    self.error(d);
                    Err(Fail)
                }
                T::Keyword(K::If) => {
                    self.bump();
                    match self.expr(false) {
                        Ok(guard) => self.action(t.span, Some(guard), t.span, ignore),
                        Err(f) => Err(f),
                    }
                }
                T::Keyword(K::Else) if branches.is_empty() => {
                    let d = Diagnostic::error(
                        codes::MALFORMED_BRANCH_BLOCK,
                        t.span,
                        "a block of branches starts with `if`",
                    )
                    .with_help("for a single unguarded branch, drop the braces");
                    self.error(d);
                    Err(Fail)
                }
                T::Keyword(K::Else) => {
                    self.bump();
                    seen_else = true;
                    self.action(t.span, None, t.span, ignore)
                }
                T::Punct(P::Arrow)
                | T::Keyword(K::Set | K::Do | K::Send | K::Emit | K::Reply | K::Ignore) => {
                    let d = Diagnostic::error(
                        codes::MALFORMED_BRANCH_BLOCK,
                        t.span,
                        "every branch in a block starts with `if` or `else`",
                    )
                    .with_help("for a single unguarded branch, drop the braces");
                    self.error(d);
                    Err(Fail)
                }
                _ => Err(self.unexpected("`if`, `else` or `}` in a block of branches")),
            };
            match result {
                Ok(b) => branches.push(b),
                Err(Fail) => {
                    self.depth = depth;
                    self.recover(start, true, |k| matches!(k, T::Keyword(K::If | K::Else)))
                }
            }
        }
        if branches.is_empty() && self.diags.len() == errors_before {
            let d = Diagnostic::error(
                codes::MALFORMED_BRANCH_BLOCK,
                open.span.to(self.prev()),
                "a block of branches needs at least one `if` branch",
            );
            self.error(d);
        }
        Ok(Branches::Block {
            branches,
            span: open.span.to(self.prev()),
        })
    }

    fn action(
        &mut self,
        start: Span,
        guard: Option<Expr>,
        trigger: Span,
        ignore: bool,
    ) -> PResult<Branch> {
        if let Some(kw) = self.eat_kw(K::Ignore) {
            if !ignore {
                let d = Diagnostic::error(
                    codes::MISPLACED_IGNORE,
                    kw.span,
                    "only an event's `on` transition can `ignore` it",
                )
                .with_secondary(trigger, "this transition")
                .with_help("`ignore` takes an event and does nothing; a call is answered with `reply`, and a timer, `always` or `done` with nothing to do can be removed");
                self.error(d);
                return Err(Fail);
            }
            if self.at_p(P::Arrow)
                || self.at_kw(K::Set)
                || self.at_kw(K::Do)
                || self.at_kw(K::Send)
                || self.at_kw(K::Emit)
            {
                let d = Diagnostic::error(
                    codes::MISPLACED_IGNORE,
                    kw.span.to(self.tok().span),
                    "`ignore` stands alone: a branch that ignores its event has no target and no effects",
                )
                .with_help("remove `ignore`, or the target and effects");
                self.error(d);
                return Err(Fail);
            }
            return Ok(Branch {
                guard,
                targets: Vec::new(),
                effects: Vec::new(),
                ignore: true,
                span: self.since(start),
            });
        }
        let mut targets = Vec::new();
        if self.eat_p(P::Arrow).is_some() {
            targets.push(self.target()?);
            while self.eat_p(P::Comma).is_some() {
                targets.push(self.target()?);
            }
        }
        let effects = self.effects()?;
        if (!targets.is_empty() || !effects.is_empty()) && self.at_kw(K::Ignore) {
            let d = Diagnostic::error(
                codes::MISPLACED_IGNORE,
                self.tok().span,
                "`ignore` stands alone: a branch that ignores its event has no target and no effects",
            )
            .with_help("remove `ignore`, or the target and effects");
            self.error(d);
            return Err(Fail);
        }
        if targets.is_empty() && effects.is_empty() {
            let (expected, help) = if ignore {
                (
                    "`-> Target`, `set`, `do`, `send`, `emit`, `reply`, `spawn`, `stop`, `raise`, `cancel` or `ignore`",
                    "give it a target (`-> State`), assignments (`set x = …`), actions (`do f(…)`), messages (`send child.Event`, `emit Event`, `raise Event`) or a reply (`reply call done`), or write `ignore` to take the event and do nothing",
                )
            } else {
                (
                    "`-> Target`, `set`, `do`, `send`, `emit`, `reply`, `spawn`, `stop`, `raise` or `cancel`",
                    "give it a target (`-> State`), assignments (`set x = …`), actions (`do f(…)`), messages (`send child.Event`, `emit Event`, `raise Event`) or a reply (`reply call done`)",
                )
            };
            let msg = format!(
                "a transition must do something: expected {expected}, found {}",
                self.found()
            );
            let d = Diagnostic::error(codes::TRANSITION_DOES_NOTHING, self.tok().span, msg)
                .with_secondary(trigger, "this transition")
                .with_help(help);
            self.error(d);
            return Err(Fail);
        }
        Ok(Branch {
            guard,
            targets,
            effects,
            ignore: false,
            span: self.since(start),
        })
    }

    /// `[ . ] segment { . segment }`, each segment `Upper [ ( args ) ]`: a leading dot names a
    /// child of the transition's own state, and the arguments are those of the state the
    /// segment names.
    fn target(&mut self) -> PResult<Target> {
        let dot = self.eat_p(P::Dot);
        let mut path = vec![self.upper(if dot.is_some() {
            "a child state after `.`"
        } else {
            "a target state"
        })?];
        let mut args = vec![self.target_args()?];
        while self.eat_p(P::Dot).is_some() {
            path.push(self.upper("a state name")?);
            args.push(self.target_args()?);
        }
        let first = dot.map_or(path[0].span, |d| d.span);
        let last = match &args[args.len() - 1] {
            Some(a) => a.span,
            None => path[path.len() - 1].span,
        };
        Ok(Target {
            dot: dot.is_some(),
            path,
            args,
            span: first.to(last),
        })
    }

    /// The arguments after a target's segment, `( expr, … )`, if a `(` follows it: effects
    /// start with a keyword, so a `(` there always starts them.
    fn target_args(&mut self) -> PResult<Option<TargetArgs>> {
        if !self.at_p(P::LParen) {
            return Ok(None);
        }
        let (args, span) = self.list(P::LParen, "the argument list", |p| p.expr(false))?;
        Ok(Some(TargetArgs { args, span }))
    }

    /// Zero or more `set …`, `do …`, `send …`, `emit …`, `reply …`, `spawn …`, `stop …`,
    /// `raise …` and `cancel …` clauses.
    fn effects(&mut self) -> PResult<Vec<Effect>> {
        let mut out = Vec::new();
        loop {
            if let Some(kw) = self.eat_kw(K::Reply) {
                out.push(Effect::Reply(self.reply(kw.span)?));
                continue;
            }
            if let Some(kw) = self.eat_kw(K::Send) {
                let to = self.lower("the name of the machine to send to")?;
                let key = if self.at_p(P::LBracket) {
                    Some(SendKey::Key(self.key()?))
                } else if self.at_p(P::Dot) && self.nth(1) == T::Punct(P::Star) {
                    self.bump();
                    let star = self.bump();
                    Some(SendKey::All { span: star.span })
                } else {
                    None
                };
                self.expect_p(
                    P::Dot,
                    &format!(" and the event to send after `send {}`", to.name),
                )?;
                let event = self.upper("an event name")?;
                let mut message = self.message(kw.span, Some(to), event)?;
                message.key = key;
                out.push(Effect::Send(message));
                continue;
            }
            if self.at_contextual("spawn") {
                let kw = self.bump();
                let children = self.lower("the children collection to spawn into")?;
                if !self.at_p(P::LBracket) {
                    return Err(self
                        .unexpected(&format!("`[` and the key after `spawn {}`", children.name)));
                }
                let key = self.key()?;
                self.expect_p(P::Eq, &format!(" after `spawn {}[…]`", children.name))?;
                // A lower-case name is a task, an upper-case one a machine.
                let src = match self.peek() {
                    T::Upper | T::Lower => self.bump_ident(),
                    T::Keyword(_) => self.keyword_as_name("the machine or task to spawn")?,
                    _ => return Err(self.unexpected("the machine or task to spawn")),
                };
                if !self.at_p(P::LParen) {
                    return Err(
                        self.unexpected(&format!("`(` and the arguments of `{}`", src.name))
                    );
                }
                let (args, _) = self.list(P::LParen, "the argument list", |p| p.expr(false))?;
                out.push(Effect::Spawn(Spawn {
                    children,
                    key,
                    src,
                    args,
                    span: self.since(kw.span),
                }));
                continue;
            }
            if self.at_contextual("stop") {
                let kw = self.bump();
                let children = self.lower("the children collection to stop a child of")?;
                if !self.at_p(P::LBracket) {
                    return Err(
                        self.unexpected(&format!("`[` and the key after `stop {}`", children.name))
                    );
                }
                let key = self.key()?;
                out.push(Effect::Stop(Stop {
                    children,
                    key,
                    span: self.since(kw.span),
                }));
                continue;
            }
            if self.at(T::Lower) && self.text(self.tok()) == "raise" && self.nth(1) == T::Upper {
                let kw = self.bump();
                let event = self.upper("the event to raise")?;
                let message = self.message(kw.span, None, event)?;
                // `after` goes on with the raise only on the line its event ends on: on a
                // later line it starts an `after` transition.
                let line = self.prev().end.line;
                let delay = if self.at_kw(K::After) && self.tok().span.start.line == line {
                    self.bump();
                    Some(self.expr(false)?)
                } else {
                    None
                };
                let name = if self.at(T::Lower)
                    && self.text(self.tok()) == "as"
                    && self.nth(1) == T::Lower
                {
                    let as_kw = self.bump();
                    let name = self.binding("the name of a delayed raise")?;
                    if delay.is_none() {
                        let d = Diagnostic::error(
                            codes::RAISE_NAME_WITHOUT_DELAY,
                            as_kw.span.to(name.span),
                            format!("only a delayed raise has a name: `raise {} as {}` has no `after`", message.event.name, name.name),
                        )
                        .with_help("add a delay (`raise E after 5s as name`), or remove `as name`: an immediate raise is taken before the step ends, so there is nothing to cancel");
                        self.error(d);
                    }
                    Some(name)
                } else {
                    None
                };
                out.push(Effect::Raise(Raise {
                    message,
                    delay,
                    name,
                    span: self.since(kw.span),
                }));
                continue;
            }
            if self.at_contextual("cancel") {
                let kw = self.bump();
                let name = self.lower("the name of the delayed raise to cancel")?;
                out.push(Effect::Cancel(Cancel {
                    span: self.since(kw.span),
                    name,
                }));
                continue;
            }
            if let Some(kw) = self.eat_kw(K::Emit) {
                let event = self.upper("the name of an event the machine emits")?;
                out.push(Effect::Emit(self.message(kw.span, None, event)?));
                continue;
            }
            if self.eat_kw(K::Set).is_some() {
                loop {
                    let var = self.lower("a variable name")?;
                    self.expect_p(P::Eq, &format!(" after `set {}`", var.name))?;
                    let value = self.expr(false)?;
                    out.push(Effect::Set(Assign {
                        span: var.span.to(value.span),
                        var,
                        value,
                    }));
                    if !self.comma_continues() {
                        break;
                    }
                }
            } else if self.eat_kw(K::Do).is_some() {
                loop {
                    let call = self.call("an action name")?;
                    out.push(Effect::Do(call));
                    if !self.comma_continues() {
                        break;
                    }
                }
            } else {
                return Ok(out);
            }
        }
    }

    /// The rest of `reply name done [value]` or `reply name fail error`. The value after
    /// `done` is optional, so it is parsed only if it starts on the same line as `done`:
    /// `reply pending done` at the end of a line is followed by the next item, not a value.
    fn reply(&mut self, start: Span) -> PResult<Reply> {
        let call = match self.peek() {
            T::Keyword(K::Call) => {
                let t = self.bump();
                self.ident(t)
            }
            T::Lower => self.bump_ident(),
            _ => {
                return Err(
                    self.unexpected("the call to reply to: `call` or a variable holding one")
                );
            }
        };
        let (outcome, value) = if let Some(done) = self.eat_kw(K::Done) {
            let next = self.tok();
            let value = if next.span.start.line == done.span.end.line && starts_expr(next.kind) {
                Some(self.expr(false)?)
            } else {
                None
            };
            (ReplyOutcome::Done, value)
        } else if self.eat_kw(K::Fail).is_some() {
            (ReplyOutcome::Fail, Some(self.expr(false)?))
        } else {
            return Err(self.unexpected(&format!("`done` or `fail` after `reply {}`", call.name)));
        };
        Ok(Reply {
            call,
            outcome,
            value,
            span: self.since(start),
        })
    }

    /// `call name(params) [-> T] [! E | F]` in a machine body.
    fn call_decl(&mut self) -> PResult<CallDecl> {
        let doc = self.take_doc();
        let start = self.bump().span;
        let name = self.lower("a call name")?;
        let params = self.params()?;
        let returns = if self.eat_p(P::Arrow).is_some() {
            Some(self.type_ref()?)
        } else {
            None
        };
        let errors = self.errors()?;
        Ok(CallDecl {
            doc,
            name,
            params,
            returns,
            errors,
            span: self.since(start),
        })
    }

    /// `spawn`, `stop` and `cancel` are not reserved: they start an effect only where an
    /// effect can start, followed by a name (`spawn name[`, `stop name[`, `cancel name`), as
    /// `raise` does followed by an event (`raise Name`). No expression or other item
    /// can go on with a lower-case name there, so a missing `[` is reported as such.
    fn at_contextual(&self, word: &str) -> bool {
        self.at(T::Lower) && self.text(self.tok()) == word && self.nth(1) == T::Lower
    }

    /// `[expr]`: the key of a spawned child.
    fn key(&mut self) -> PResult<Expr> {
        let open = self.bump();
        self.deeper()?;
        let key = self.expr(false)?;
        self.depth -= 1;
        if self.eat_p(P::RBracket).is_none() {
            if self.at(T::Eof) {
                return Err(self.unclosed(open, "the key"));
            }
            return Err(self.unexpected("`]` after the key"));
        }
        Ok(key)
    }

    /// The rest of a `send` or `emit`: the optional record body after the event's name.
    fn message(&mut self, start: Span, to: Option<Ident>, event: Ident) -> PResult<Message> {
        let mut fields = Vec::new();
        if self.at_p(P::LBrace) {
            let what = format!("the fields of `{}`", event.name);
            fields = self.list(P::LBrace, &what, |p| p.field_value())?.0;
        }
        Ok(Message {
            to,
            key: None,
            value_span: event.span.to(self.prev()),
            event,
            fields,
            span: self.since(start),
        })
    }

    fn field_value(&mut self) -> PResult<FieldValue> {
        let name = self.lower("a field name")?;
        self.expect_p(P::Colon, &format!(" after the field name `{}`", name.name))?;
        let value = self.expr(false)?;
        Ok(FieldValue {
            span: name.span.to(value.span),
            name,
            value,
        })
    }

    /// In `set` and `do` lists: eat a comma, and say whether another element follows (a
    /// trailing comma is allowed).
    fn comma_continues(&mut self) -> bool {
        if self.eat_p(P::Comma).is_none() {
            return false;
        }
        matches!(self.peek(), T::Lower | T::Upper)
    }

    fn call(&mut self, what: &str) -> PResult<Call> {
        let name = self.lower(what)?;
        if !self.at_p(P::LParen) {
            return Err(self.unexpected(&format!("`(` and the arguments of `{}`", name.name)));
        }
        let (args, span) = self.list(P::LParen, "the argument list", |p| p.expr(false))?;
        Ok(Call {
            span: name.span.to(span),
            name,
            args,
        })
    }

    fn invoke(&mut self) -> PResult<Invoke> {
        let start = self.bump().span;
        let to = if self.at(T::Lower) && self.nth(1) == T::Punct(P::Dot) {
            let to = self.bump_ident();
            self.bump();
            Some(to)
        } else {
            None
        };
        if let Some(to) = &to {
            let what = format!("the name of a call of the machine `{}` holds", to.name);
            let callee = self.lower(&what)?;
            return self.invoke_rest(start, Some(to.clone()), callee);
        }
        let callee = match self.peek() {
            T::Upper | T::Lower => self.bump_ident(),
            T::Keyword(_) => self.keyword_as_name("the task or machine to invoke")?,
            _ => return Err(self.unexpected("the task or machine to invoke")),
        };
        self.invoke_rest(start, None, callee)
    }

    /// An invoke after its callee: the arguments, `as`, and the outcomes.
    fn invoke_rest(&mut self, start: Span, to: Option<Ident>, callee: Ident) -> PResult<Invoke> {
        if !self.at_p(P::LParen) {
            return Err(self.unexpected(&format!("`(` and the arguments of `{}`", callee.name)));
        }
        let (args, _) = self.list(P::LParen, "the argument list", |p| p.expr(false))?;
        let alias = if self.at(T::Lower) && self.text(self.tok()) == "as" {
            self.bump();
            Some(self.binding("the name of the invoked machine")?)
        } else {
            None
        };
        let retry = if self.at(T::Lower) && self.text(self.tok()) == "retry" {
            Some(self.retry()?)
        } else {
            None
        };
        let mut outcomes = Vec::new();
        // An invoke may leave out its block: `invoke ticks(1s) as clock`.
        let Some(open) = self.eat_p(P::LBrace) else {
            return Ok(Invoke {
                to,
                callee,
                args,
                alias,
                retry,
                outcomes,
                span: self.since(start),
            });
        };
        let errors_before = self.diags.len();
        loop {
            let k = self.peek();
            if k == T::Punct(P::RBrace) {
                self.bump();
                break;
            }
            if k == T::Eof || is_decl_start(k) {
                self.unclosed(open, &format!("`invoke {}`", callee.name));
                break;
            }
            let (pos, depth) = (self.pos, self.depth);
            match self.outcome() {
                Ok(t) => outcomes.push(t),
                Err(Fail) => {
                    self.depth = depth;
                    self.recover(pos, true, |k| matches!(k, T::Keyword(K::Done | K::Fail)))
                }
            }
        }
        if outcomes.is_empty() && self.diags.len() == errors_before {
            let d = Diagnostic::error(
                codes::UNEXPECTED_TOKEN,
                open.span.to(self.prev()),
                "an `invoke` handles at least one outcome",
            )
            .with_help("add `done -> <State>`");
            self.error(d);
        }
        Ok(Invoke {
            to,
            callee,
            args,
            alias,
            retry,
            outcomes,
            span: self.since(start),
        })
    }

    /// `retry N [on E | …] [after delay]`, at `retry`.
    fn retry(&mut self) -> PResult<Retry> {
        let start = self.bump().span;
        if !self.at(T::Int) {
            return Err(self.unexpected("the number of retries after `retry`"));
        }
        let t = self.bump();
        let count = self.text(t).parse().unwrap_or(i64::MAX);
        let mut errors = Vec::new();
        if self.eat_kw(K::On).is_some() {
            errors.push(self.upper("an error name")?);
            while self.eat_p(P::Pipe).is_some() {
                errors.push(self.upper("an error name")?);
            }
        }
        let delay = if self.eat_kw(K::After).is_some() {
            let delay = self.expr(true)?;
            self.check_record_after(&delay)?;
            Some(delay)
        } else {
            None
        };
        Ok(Retry {
            count,
            count_span: t.span,
            errors,
            delay,
            span: self.since(start),
        })
    }

    fn outcome(&mut self) -> PResult<Transition> {
        let t = self.tok();
        match t.kind {
            T::Keyword(K::Done) => {
                self.bump();
                self.transition(t.span, Trigger::Done)
            }
            T::Keyword(K::Fail) => {
                self.bump();
                if !matches!(self.peek(), T::Upper | T::Punct(P::Star)) {
                    let d = Diagnostic::error(codes::NOT_AN_OUTCOME, t.span.to(self.tok().span), format!("expected an error name or `*` after `fail`, found {}", self.found()))
                        .with_help("inside `invoke { }`, `fail Error -> …` handles one of the task's errors; `fail expr` belongs in a final state");
                    self.error(d);
                    return Err(Fail);
                }
                let pattern = self.pattern("an error name or `*`")?;
                self.transition(t.span, Trigger::Fail(pattern))
            }
            _ => {
                let msg = format!(
                    "expected `done`, `fail` or `}}` inside `invoke`, found {}",
                    self.found()
                );
                let mut d = Diagnostic::error(codes::NOT_AN_OUTCOME, t.span, msg);
                if is_item_start(t.kind) {
                    d = d.with_help("an `invoke` block holds only its outcomes; put other items in the enclosing state");
                }
                self.error(d);
                Err(Fail)
            }
        }
    }

    // ───────────── expressions ─────────────

    /// `no_record` forbids a bare `Upper { … }` at this level (for `after` delays).
    fn expr(&mut self, no_record: bool) -> PResult<Expr> {
        let base = self.depth;
        self.deeper()?;
        let mut a = self.and_expr(no_record)?;
        while self.eat_kw(K::Or).is_some() {
            self.deeper()?;
            let b = self.and_expr(no_record)?;
            a = binary(BinaryOp::Or, a, b);
        }
        self.depth = base;
        Ok(a)
    }

    fn and_expr(&mut self, nr: bool) -> PResult<Expr> {
        let base = self.depth;
        let mut a = self.not_expr(nr)?;
        while self.eat_kw(K::And).is_some() {
            self.deeper()?;
            let b = self.not_expr(nr)?;
            a = binary(BinaryOp::And, a, b);
        }
        self.depth = base;
        Ok(a)
    }

    fn not_expr(&mut self, nr: bool) -> PResult<Expr> {
        match self.eat_kw(K::Not) {
            Some(t) => {
                let e = self.cmp_expr(nr)?;
                Ok(Expr {
                    span: t.span.to(e.span),
                    kind: ExprKind::Unary(UnaryOp::Not, Box::new(e)),
                })
            }
            None => self.cmp_expr(nr),
        }
    }

    fn cmp_expr(&mut self, nr: bool) -> PResult<Expr> {
        let a = self.add_expr(nr)?;
        let Some(op) = cmp_op(self.peek()) else {
            return Ok(a);
        };
        self.bump();
        let b = self.add_expr(nr)?;
        let e = binary(op, a, b);
        if cmp_op(self.peek()).is_some() {
            let d = Diagnostic::error(
                codes::CHAINED_COMPARISON,
                e.span.to(self.tok().span),
                "comparisons cannot be chained",
            )
            .with_help("use `and`, as in `a < b and b < c`, or parentheses");
            self.error(d);
            return Err(Fail);
        }
        Ok(e)
    }

    fn add_expr(&mut self, nr: bool) -> PResult<Expr> {
        let base = self.depth;
        let mut a = self.mul_expr(nr)?;
        loop {
            let op = match self.peek() {
                T::Punct(P::Plus) => BinaryOp::Add,
                T::Punct(P::Minus) => BinaryOp::Sub,
                _ => break,
            };
            self.deeper()?;
            self.bump();
            let b = self.mul_expr(nr)?;
            a = binary(op, a, b);
        }
        self.depth = base;
        Ok(a)
    }

    fn mul_expr(&mut self, nr: bool) -> PResult<Expr> {
        let base = self.depth;
        let mut a = self.unary(nr)?;
        while self.at_p(P::Star) {
            self.deeper()?;
            self.bump();
            let b = self.unary(nr)?;
            a = binary(BinaryOp::Mul, a, b);
        }
        self.depth = base;
        Ok(a)
    }

    fn unary(&mut self, nr: bool) -> PResult<Expr> {
        match self.eat_p(P::Minus) {
            Some(t) => {
                let e = self.postfix(nr)?;
                Ok(Expr {
                    span: t.span.to(e.span),
                    kind: ExprKind::Unary(UnaryOp::Neg, Box::new(e)),
                })
            }
            None => self.postfix(nr),
        }
    }

    fn postfix(&mut self, nr: bool) -> PResult<Expr> {
        let base = self.depth;
        let mut e = self.primary(nr)?;
        while self.at_p(P::Dot) {
            self.deeper()?;
            self.bump();
            let field = self.lower("a field name")?;
            e = Expr {
                span: e.span.to(field.span),
                kind: ExprKind::Field(Box::new(e), field),
            };
        }
        self.depth = base;
        Ok(e)
    }

    fn primary(&mut self, nr: bool) -> PResult<Expr> {
        let t = self.tok();
        let text = self.text(t);
        let kind = match t.kind {
            T::Int => ExprKind::Int(text.parse().unwrap_or(i64::MAX)),
            T::Float => ExprKind::Float(text.parse().unwrap_or(0.0)),
            T::Duration => {
                let digits = text.trim_end_matches(|c: char| c.is_ascii_alphabetic());
                let unit = match &text[digits.len()..] {
                    "ms" => DurationUnit::Ms,
                    "s" => DurationUnit::S,
                    "m" => DurationUnit::M,
                    _ => DurationUnit::H,
                };
                ExprKind::Duration(Duration {
                    value: digits.parse().unwrap_or(u64::MAX),
                    unit,
                })
            }
            T::String => ExprKind::String(unescape(text)),
            T::Keyword(K::True) => ExprKind::Bool(true),
            T::Keyword(K::False) => ExprKind::Bool(false),
            // `event`, `error` and `call` are declaration keywords and also implicit names.
            T::Keyword(K::Event | K::Error | K::Call) => ExprKind::Var(self.ident(t)),
            T::Lower if self.nth(1) == T::Punct(P::LParen) => {
                let c = self.call("a function name")?;
                return Ok(Expr {
                    span: c.span,
                    kind: ExprKind::Call(c),
                });
            }
            T::Lower => ExprKind::Var(self.ident(t)),
            T::Upper => {
                self.bump();
                let ty = self.ident(t);
                if self.at_p(P::Dot) && self.nth(1) == T::Upper {
                    self.bump();
                    let variant = self.bump_ident();
                    return Ok(Expr {
                        span: t.span.to(variant.span),
                        kind: ExprKind::Enum(ty, variant),
                    });
                }
                if nr || !self.at_p(P::LBrace) {
                    return Ok(Expr {
                        span: t.span,
                        kind: ExprKind::Record {
                            ty,
                            fields: Vec::new(),
                        },
                    });
                }
                let (fields, span) =
                    self.list(P::LBrace, &format!("the record value `{}`", ty.name), |p| {
                        p.field_value()
                    })?;
                return Ok(Expr {
                    span: t.span.to(span),
                    kind: ExprKind::Record { ty, fields },
                });
            }
            T::Punct(P::LParen) => {
                self.bump();
                let inner = self.expr(false)?;
                self.expect_p(P::RParen, " to close the parenthesis")?;
                return Ok(Expr {
                    span: t.span.to(self.prev()),
                    kind: inner.kind,
                });
            }
            T::Keyword(K::Not) => {
                let d = Diagnostic::error(
                    codes::UNEXPECTED_TOKEN,
                    t.span,
                    "`not` cannot be used here without parentheses",
                )
                .with_help(
                    "`not` binds more loosely than comparison and arithmetic; write `(not …)`",
                );
                self.error(d);
                return Err(Fail);
            }
            _ => return Err(self.unexpected("an expression")),
        };
        self.bump();
        Ok(Expr { kind, span: t.span })
    }
}

/// Whether a token can start an expression.
fn starts_expr(k: T) -> bool {
    matches!(
        k,
        T::Int
            | T::Float
            | T::Duration
            | T::String
            | T::Lower
            | T::Upper
            | T::Punct(P::LParen | P::Minus)
            | T::Keyword(K::True | K::False | K::Not | K::Event | K::Error | K::Call)
    )
}

fn binary(op: BinaryOp, a: Expr, b: Expr) -> Expr {
    Expr {
        span: a.span.to(b.span),
        kind: ExprKind::Binary(op, Box::new(a), Box::new(b)),
    }
}

fn capitalize(name: &str, upper: bool) -> String {
    let mut c = name.chars();
    match c.next() {
        Some(f) if upper => f.to_uppercase().chain(c).collect(),
        Some(f) => f.to_lowercase().chain(c).collect(),
        None => String::new(),
    }
}

/// Levenshtein distance between two short words, counting a swap of two adjacent characters as
/// one edit.
fn edits(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut v = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                v = v.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = v;
        }
    }
    d[a.len()][b.len()]
}
