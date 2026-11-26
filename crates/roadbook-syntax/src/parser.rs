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
use crate::span::{FileId, Span};

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
                        .with_help("`///` documents the declaration, machine, state, history state, event or variable that follows it; use `//` for other comments")
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
                k if is_item_start(k) => {
                    let word = self.text(self.tok()).to_string();
                    let d = Diagnostic::error(codes::NOT_AT_TOP_LEVEL, self.tok().span, format!("`{word}` must be inside a machine"))
                        .with_help("the top level of a file holds imports and declarations (type, enum, error, extern, fn, action, task, machine)");
                    self.error(d);
                    Err(Fail)
                }
                _ => Err(self.unexpected("a declaration (`type`, `enum`, `error`, `extern`, `fn`, `action`, `task`, `machine`) or `import`")),
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
        let (name, kind) = match k {
            K::Type => {
                let name = self.upper("a type name")?;
                let kind = if self.eat_p(P::Eq).is_some() {
                    DeclKind::Alias {
                        target: self.type_ref()?,
                    }
                } else if self.at_p(P::LBrace) {
                    DeclKind::Record {
                        fields: self.fields("the fields of `type`")?,
                    }
                } else {
                    return Err(self.unexpected(&format!("`=` or `{{` after `type {}`", name.name)));
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
                (
                    name,
                    DeclKind::Task {
                        params,
                        returns,
                        errors,
                    },
                )
            }
            K::Machine => {
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
                        params,
                        returns,
                        errors,
                        body,
                    }),
                )
            }
            K::Import => unreachable!("imports are parsed by import()"),
            _ => unreachable!("decl() is called at a declaration keyword"),
        };
        Ok(Decl {
            doc,
            name,
            kind,
            span: self.since(start),
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

    fn type_ref(&mut self) -> PResult<TypeRef> {
        let t = self.tok();
        let kind = match t.kind {
            T::Upper => TypeKind::Named(self.bump_ident()),
            T::Lower if self.text(t) == "ref" && self.nth(1) == T::Upper => {
                self.bump();
                TypeKind::Ref(self.bump_ident())
            }
            T::Lower if self.text(t) == "ref" => {
                self.bump();
                return Err(self.unexpected("the name of a machine after `ref`"));
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
        // `sensitive var`: `sensitive` is not reserved, so it is a modifier only here.
        if self.at_sensitive(T::Keyword(K::Var)) {
            t = self.nth_tok(1);
        }
        let T::Keyword(k) = t.kind else {
            let expected = match kind {
                BodyKind::Machine => {
                    "a machine item (`var`, `event`, `emits`, `initial`, `state`, `entry`, `exit`, `on`, `after`, `always`) or `}`"
                }
                BodyKind::State => {
                    "a state item (`initial`, `state`, `history`, `entry`, `exit`, `on`, `after`, `always`, `done`, `invoke`, `output`, `fail`) or `}`"
                }
            };
            return Err(self.unexpected(expected));
        };
        let machine_only = matches!(k, K::Var | K::Event | K::Emits);
        let state_only = matches!(k, K::Done | K::Invoke | K::Output | K::Fail);
        let item = match k {
            K::Var => Item::Var(self.var()?),
            K::Event => Item::Event(self.event()?),
            K::Emits => Item::Emits(self.event()?),
            K::Initial => {
                self.bump();
                Item::Initial(self.upper("the name of the initial state")?)
            }
            K::State | K::Final | K::Parallel => Item::State(self.state()?),
            K::Entry | K::Exit => {
                self.bump();
                let effects = self.effects()?;
                if effects.is_empty() {
                    let expected =
                        format!("`set`, `do`, `send` or `emit` after `{}`", self.text(t));
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
                    let event =
                        self.upper(&format!("the name of an event `{}` emits", from.name))?;
                    Trigger::Receive { from, event }
                } else {
                    Trigger::On(self.pattern("an event name, `*` or `name.Event`")?)
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
                                | T::Keyword(K::If | K::Set | K::Do | K::Send | K::Emit)
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
                .with_help(format!("move the `{word}` to the top of the machine; a machine's variables and events belong to all its states"));
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

    fn event(&mut self) -> PResult<EventDecl> {
        let doc = self.take_doc();
        let start = self.bump().span;
        let name = self.upper("an event name")?;
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
        let body = self.body(BodyKind::State, &format!("state `{}`", name.name))?;
        self.depth -= 1;
        Ok(State {
            doc,
            kind,
            name,
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
        let branches = self.branches(trigger_span)?;
        Ok(Transition {
            trigger,
            branches,
            trigger_span,
            span: self.since(start),
        })
    }

    fn branches(&mut self, trigger: Span) -> PResult<Branches> {
        if !self.at_p(P::LBrace) {
            let start = self.tok().span;
            let guard = if self.eat_kw(K::If).is_some() {
                Some(self.expr(false)?)
            } else {
                None
            };
            return Ok(Branches::Single(self.action(start, guard, trigger)?));
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
                        Ok(guard) => self.action(t.span, Some(guard), t.span),
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
                    self.action(t.span, None, t.span)
                }
                T::Punct(P::Arrow) | T::Keyword(K::Set | K::Do | K::Send | K::Emit) => {
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

    fn action(&mut self, start: Span, guard: Option<Expr>, trigger: Span) -> PResult<Branch> {
        let target = if self.eat_p(P::Arrow).is_some() {
            Some(self.target()?)
        } else {
            None
        };
        let effects = self.effects()?;
        if target.is_none() && effects.is_empty() {
            let msg = format!(
                "a transition must do something: expected `-> Target`, `set`, `do`, `send` or `emit`, found {}",
                self.found()
            );
            let d = Diagnostic::error(codes::TRANSITION_DOES_NOTHING, self.tok().span, msg)
                .with_secondary(trigger, "this transition")
                .with_help("give it a target (`-> State`), assignments (`set x = …`), actions (`do f(…)`) or messages (`send child.Event`, `emit Event`)");
            self.error(d);
            return Err(Fail);
        }
        Ok(Branch {
            guard,
            target,
            effects,
            span: self.since(start),
        })
    }

    fn target(&mut self) -> PResult<Target> {
        let mut path = vec![self.upper("a target state")?];
        while self.eat_p(P::Dot).is_some() {
            path.push(self.upper("a state name")?);
        }
        let span = path[0].span.to(path[path.len() - 1].span);
        Ok(Target { path, span })
    }

    /// Zero or more `set …`, `do …`, `send …` and `emit …` clauses.
    fn effects(&mut self) -> PResult<Vec<Effect>> {
        let mut out = Vec::new();
        loop {
            if let Some(kw) = self.eat_kw(K::Send) {
                let to = self.lower("the name of the machine to send to")?;
                self.expect_p(
                    P::Dot,
                    &format!(" and the event to send after `send {}`", to.name),
                )?;
                let event = self.upper("an event name")?;
                out.push(Effect::Send(self.message(kw.span, Some(to), event)?));
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

    /// The rest of a `send` or `emit`: the optional record body after the event's name.
    fn message(&mut self, start: Span, to: Option<Ident>, event: Ident) -> PResult<Message> {
        let mut fields = Vec::new();
        if self.at_p(P::LBrace) {
            let what = format!("the fields of `{}`", event.name);
            fields = self.list(P::LBrace, &what, |p| p.field_value())?.0;
        }
        Ok(Message {
            to,
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
        let callee = match self.peek() {
            T::Upper | T::Lower => self.bump_ident(),
            T::Keyword(_) => self.keyword_as_name("the task or machine to invoke")?,
            _ => return Err(self.unexpected("the task or machine to invoke")),
        };
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
        let open = self.expect_p(P::LBrace, " and the invoke's outcomes (`done`, `fail`)")?;
        let mut outcomes = Vec::new();
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
            // `event` and `error` are declaration keywords and also implicit names.
            T::Keyword(K::Event | K::Error) => ExprKind::Var(self.ident(t)),
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
