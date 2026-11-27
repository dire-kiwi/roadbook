//! The lexer: source text to tokens and comments.
//!
//! Comments are not tokens. They are returned separately, in source order, so the parser can
//! attach `///` comments to declarations and the formatter can put every comment back.

use serde::Serialize;

use crate::codes;
use crate::diagnostic::Diagnostic;
use crate::span::{FileId, LineIndex, Span};

/// The reserved words. `from`, `as`, `ref` and the built-in type names are not reserved: they
/// are only meaningful in one position (after an import list, after an invoke's arguments, and
/// as a type), where no name can appear.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Keyword {
    Import,
    Type,
    Enum,
    Error,
    Extern,
    Fn,
    Action,
    Task,
    Machine,
    Strict,
    Var,
    Event,
    Emits,
    Call,
    Initial,
    State,
    Final,
    Parallel,
    Entry,
    Exit,
    On,
    After,
    Always,
    Done,
    Invoke,
    Output,
    Fail,
    If,
    Else,
    Set,
    Do,
    Send,
    Emit,
    Ignore,
    Reply,
    And,
    Or,
    Not,
    True,
    False,
}

impl Keyword {
    pub const ALL: [Keyword; 40] = [
        Keyword::Import,
        Keyword::Type,
        Keyword::Enum,
        Keyword::Error,
        Keyword::Extern,
        Keyword::Fn,
        Keyword::Action,
        Keyword::Task,
        Keyword::Machine,
        Keyword::Strict,
        Keyword::Var,
        Keyword::Event,
        Keyword::Emits,
        Keyword::Call,
        Keyword::Initial,
        Keyword::State,
        Keyword::Final,
        Keyword::Parallel,
        Keyword::Entry,
        Keyword::Exit,
        Keyword::On,
        Keyword::After,
        Keyword::Always,
        Keyword::Done,
        Keyword::Invoke,
        Keyword::Output,
        Keyword::Fail,
        Keyword::If,
        Keyword::Else,
        Keyword::Set,
        Keyword::Do,
        Keyword::Send,
        Keyword::Emit,
        Keyword::Ignore,
        Keyword::Reply,
        Keyword::And,
        Keyword::Or,
        Keyword::Not,
        Keyword::True,
        Keyword::False,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Keyword::Import => "import",
            Keyword::Type => "type",
            Keyword::Enum => "enum",
            Keyword::Error => "error",
            Keyword::Extern => "extern",
            Keyword::Fn => "fn",
            Keyword::Action => "action",
            Keyword::Task => "task",
            Keyword::Machine => "machine",
            Keyword::Strict => "strict",
            Keyword::Var => "var",
            Keyword::Event => "event",
            Keyword::Emits => "emits",
            Keyword::Call => "call",
            Keyword::Initial => "initial",
            Keyword::State => "state",
            Keyword::Final => "final",
            Keyword::Parallel => "parallel",
            Keyword::Entry => "entry",
            Keyword::Exit => "exit",
            Keyword::On => "on",
            Keyword::After => "after",
            Keyword::Always => "always",
            Keyword::Done => "done",
            Keyword::Invoke => "invoke",
            Keyword::Output => "output",
            Keyword::Fail => "fail",
            Keyword::If => "if",
            Keyword::Else => "else",
            Keyword::Set => "set",
            Keyword::Do => "do",
            Keyword::Send => "send",
            Keyword::Emit => "emit",
            Keyword::Ignore => "ignore",
            Keyword::Reply => "reply",
            Keyword::And => "and",
            Keyword::Or => "or",
            Keyword::Not => "not",
            Keyword::True => "true",
            Keyword::False => "false",
        }
    }

    pub fn lookup(s: &str) -> Option<Keyword> {
        Keyword::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// Punctuation and operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Punct {
    LBrace,
    RBrace,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
    Colon,
    Dot,
    Eq,
    EqEq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,
    Plus,
    Minus,
    Star,
    Bang,
    Pipe,
    Question,
    Arrow,
}

impl Punct {
    /// Longest first, so `->` wins over `-`.
    const ALL: [(&'static str, Punct); 23] = [
        ("->", Punct::Arrow),
        ("==", Punct::EqEq),
        ("!=", Punct::NotEq),
        ("<=", Punct::Le),
        (">=", Punct::Ge),
        ("{", Punct::LBrace),
        ("}", Punct::RBrace),
        ("(", Punct::LParen),
        (")", Punct::RParen),
        ("[", Punct::LBracket),
        ("]", Punct::RBracket),
        (",", Punct::Comma),
        (":", Punct::Colon),
        (".", Punct::Dot),
        ("=", Punct::Eq),
        ("<", Punct::Lt),
        (">", Punct::Gt),
        ("+", Punct::Plus),
        ("-", Punct::Minus),
        ("*", Punct::Star),
        ("!", Punct::Bang),
        ("|", Punct::Pipe),
        ("?", Punct::Question),
    ];

    pub fn as_str(self) -> &'static str {
        Punct::ALL
            .iter()
            .find(|(_, p)| *p == self)
            .map(|(s, _)| *s)
            .unwrap_or("?")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TokenKind {
    /// A name starting with an upper-case letter.
    Upper,
    /// A name starting with a lower-case letter that is not a keyword.
    Lower,
    Keyword(Keyword),
    Int,
    Float,
    Duration,
    /// A string literal, quotes included. Use [`unescape`] for its value.
    String,
    Punct(Punct),
    Eof,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CommentKind {
    /// `// …`, including `//// …`.
    Line,
    /// `/// …`
    Doc,
}

/// A comment, with its full text (`//` included, trailing whitespace removed).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Comment {
    pub kind: CommentKind,
    pub text: String,
    #[serde(skip)]
    pub span: Span,
}

/// The output of [`lex`].
#[derive(Clone, Debug)]
pub struct Lexed {
    /// The tokens, ending with one [`TokenKind::Eof`].
    pub tokens: Vec<Token>,
    pub comments: Vec<Comment>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Split `src` into tokens and comments. Never fails: bad input produces a diagnostic and the
/// lexer carries on.
pub fn lex(file: FileId, src: &str) -> Lexed {
    let index = LineIndex::new(src);
    let mut lx = Lexer {
        file,
        src,
        index,
        pos: 0,
        out: Lexed {
            tokens: Vec::new(),
            comments: Vec::new(),
            diagnostics: Vec::new(),
        },
    };
    lx.run();
    lx.out
}

struct Lexer<'a> {
    file: FileId,
    src: &'a str,
    index: LineIndex,
    pos: usize,
    out: Lexed,
}

fn is_name_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

impl Lexer<'_> {
    fn span(&self, start: usize, end: usize) -> Span {
        self.index.span(self.file, self.src, start, end)
    }

    fn push(&mut self, kind: TokenKind, start: usize) {
        let span = self.span(start, self.pos);
        self.out.tokens.push(Token { kind, span });
    }

    fn error(&mut self, code: crate::diagnostic::Code, start: usize, end: usize, message: String) {
        let span = self.span(start, end);
        self.out
            .diagnostics
            .push(Diagnostic::error(code, span, message));
    }

    fn run(&mut self) {
        let bytes = self.src.as_bytes();
        while self.pos < bytes.len() {
            let start = self.pos;
            let c = bytes[start];
            match c {
                b' ' | b'\t' | b'\r' | b'\n' => self.pos += 1,
                b'/' if bytes.get(start + 1) == Some(&b'/') => {
                    let end = self.src[start..]
                        .find('\n')
                        .map_or(self.src.len(), |i| start + i);
                    let text = self.src[start..end].trim_end();
                    let doc = text.starts_with("///") && !text.starts_with("////");
                    let kind = if doc {
                        CommentKind::Doc
                    } else {
                        CommentKind::Line
                    };
                    let span = self.span(start, start + text.len());
                    self.out.comments.push(Comment {
                        kind,
                        text: text.to_string(),
                        span,
                    });
                    self.pos = end;
                }
                b'0'..=b'9' => self.number(),
                b'a'..=b'z' | b'A'..=b'Z' | b'_' => self.name(),
                b'"' => self.string(),
                _ => {
                    let rest = &self.src[start..];
                    if let Some((s, p)) = Punct::ALL.iter().find(|(s, _)| rest.starts_with(s)) {
                        self.pos += s.len();
                        self.push(TokenKind::Punct(*p), start);
                    } else {
                        let ch = rest.chars().next().unwrap_or('?');
                        self.pos += ch.len_utf8();
                        self.error(
                            codes::UNEXPECTED_CHARACTER,
                            start,
                            self.pos,
                            format!("unexpected character `{}`", ch.escape_debug()),
                        );
                    }
                }
            }
        }
        self.push(TokenKind::Eof, self.pos);
    }

    fn name(&mut self) {
        let bytes = self.src.as_bytes();
        let start = self.pos;
        while self.pos < bytes.len() && is_name_char(bytes[self.pos]) {
            self.pos += 1;
        }
        let text = &self.src[start..self.pos];
        if text.contains('_') {
            let suggestion = camel_case(text);
            let span = self.span(start, self.pos);
            self.out.diagnostics.push(
                Diagnostic::error(
                    codes::UNDERSCORE_IN_NAME,
                    span,
                    format!("names cannot contain `_`: `{text}`"),
                )
                .with_help(format!(
                    "names are camelCase or PascalCase, such as `{suggestion}`"
                ))
                .with_fix(format!("rename to `{suggestion}`"), span, suggestion),
            );
        }
        let first = text.trim_start_matches('_').bytes().next().unwrap_or(b'a');
        let kind = if first.is_ascii_uppercase() {
            TokenKind::Upper
        } else if let Some(k) = Keyword::lookup(text) {
            TokenKind::Keyword(k)
        } else {
            TokenKind::Lower
        };
        self.push(kind, start);
    }

    fn number(&mut self) {
        let bytes = self.src.as_bytes();
        let start = self.pos;
        let digits = |pos: &mut usize| {
            while *pos < bytes.len() && bytes[*pos].is_ascii_digit() {
                *pos += 1;
            }
        };
        digits(&mut self.pos);
        let mut kind = TokenKind::Int;
        if bytes.get(self.pos) == Some(&b'.')
            && bytes.get(self.pos + 1).is_some_and(u8::is_ascii_digit)
        {
            self.pos += 1;
            digits(&mut self.pos);
            kind = TokenKind::Float;
        }
        let suffix_start = self.pos;
        while self.pos < bytes.len() && is_name_char(bytes[self.pos]) {
            self.pos += 1;
        }
        let suffix = &self.src[suffix_start..self.pos];
        if !suffix.is_empty() {
            if kind == TokenKind::Int && matches!(suffix, "ms" | "s" | "m" | "h") {
                kind = TokenKind::Duration;
            } else {
                let text = &self.src[start..self.pos];
                let msg = if kind == TokenKind::Float && matches!(suffix, "ms" | "s" | "m" | "h") {
                    format!(
                        "malformed duration `{text}`: durations are whole numbers, such as `1500ms`"
                    )
                } else {
                    format!("malformed number `{text}`: durations end in `ms`, `s`, `m` or `h`")
                };
                self.error(codes::MALFORMED_NUMBER, start, self.pos, msg);
            }
        }
        let text = &self.src[start..suffix_start];
        if kind == TokenKind::Int && text.parse::<i64>().is_err() {
            let text = text.to_string();
            self.error(
                codes::NUMBER_TOO_LARGE,
                start,
                self.pos,
                format!("integer `{text}` does not fit in an `int` (64-bit signed)"),
            );
        }
        if kind == TokenKind::Duration && text.parse::<u64>().is_err() {
            let text = self.src[start..self.pos].to_string();
            self.error(
                codes::NUMBER_TOO_LARGE,
                start,
                self.pos,
                format!("duration `{text}` is too large: its number does not fit in 64 bits"),
            );
        }
        if kind == TokenKind::Float && !text.parse::<f64>().is_ok_and(f64::is_finite) {
            let text = text.to_string();
            self.error(
                codes::NUMBER_TOO_LARGE,
                start,
                self.pos,
                format!("float `{text}` is too large to be a finite `float` (64-bit)"),
            );
        }
        self.push(kind, start);
    }

    fn string(&mut self) {
        let bytes = self.src.as_bytes();
        let start = self.pos;
        self.pos += 1;
        loop {
            match bytes.get(self.pos) {
                None | Some(b'\n') => {
                    self.error(
                        codes::UNTERMINATED_STRING,
                        start,
                        self.pos,
                        "unterminated string: expected `\"` before the end of the line".into(),
                    );
                    break;
                }
                Some(b'"') => {
                    self.pos += 1;
                    break;
                }
                Some(b'\\') => {
                    match bytes.get(self.pos + 1) {
                        Some(b'"' | b'\\' | b'n' | b't') => {}
                        _ => {
                            let len = self.src[self.pos + 1..]
                                .chars()
                                .next()
                                .filter(|c| *c != '\n')
                                .map_or(0, char::len_utf8);
                            let esc = self.src[self.pos..self.pos + 1 + len].to_string();
                            self.error(codes::UNKNOWN_ESCAPE, self.pos, self.pos + 1 + len, format!("unknown escape `{esc}`: strings allow `\\\"`, `\\\\`, `\\n` and `\\t`"));
                        }
                    }
                    self.pos += 1;
                    if self.pos < bytes.len() && bytes[self.pos] != b'\n' {
                        self.pos += self.src[self.pos..]
                            .chars()
                            .next()
                            .map_or(1, char::len_utf8);
                    }
                }
                Some(_) => {
                    self.pos += self.src[self.pos..]
                        .chars()
                        .next()
                        .map_or(1, char::len_utf8)
                }
            }
        }
        self.push(TokenKind::String, start);
    }
}

fn camel_case(text: &str) -> String {
    let mut out = String::new();
    let mut upper = false;
    for c in text.chars() {
        if c == '_' {
            upper = !out.is_empty();
        } else if upper {
            out.extend(c.to_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// The value of a string literal token's text (quotes included). Unknown escapes, already
/// reported by the lexer, are kept as written.
pub fn unescape(text: &str) -> String {
    let inner = text.strip_prefix('"').unwrap_or(text);
    let inner = inner.strip_suffix('"').unwrap_or(inner);
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(c @ ('"' | '\\')) => out.push(c),
            Some(c) => {
                out.push('\\');
                out.push(c);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// A string value written as a literal, with the canonical escapes.
pub fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<(TokenKind, &str)> {
        let lexed = lex(FileId(0), src);
        assert!(lexed.diagnostics.is_empty(), "{:?}", lexed.diagnostics);
        lexed
            .tokens
            .iter()
            .map(|t| (t.kind, &src[t.span.range()]))
            .collect()
    }

    fn errors(src: &str) -> Vec<String> {
        lex(FileId(0), src)
            .diagnostics
            .iter()
            .map(|d| format!("{} {}", d.code, d.message))
            .collect()
    }

    #[test]
    fn names_and_keywords() {
        use TokenKind::*;
        assert_eq!(
            kinds("state Foo bar event from string"),
            vec![
                (Keyword(self::Keyword::State), "state"),
                (Upper, "Foo"),
                (Lower, "bar"),
                (Keyword(self::Keyword::Event), "event"),
                (Lower, "from"),
                (Lower, "string"),
                (Eof, "")
            ]
        );
    }

    #[test]
    fn numbers_and_durations() {
        use TokenKind::*;
        assert_eq!(
            kinds("3 1.5 200ms 30s 5m 1h 2.x"),
            vec![
                (Int, "3"),
                (Float, "1.5"),
                (Duration, "200ms"),
                (Duration, "30s"),
                (Duration, "5m"),
                (Duration, "1h"),
                (Int, "2"),
                (Punct(self::Punct::Dot), "."),
                (Lower, "x"),
                (Eof, "")
            ]
        );
    }

    #[test]
    fn punctuation_longest_match() {
        let got: Vec<_> = kinds("-> - == = != ! <= < >= > [] ? | * + , : . ( ) { }")
            .into_iter()
            .map(|(_, s)| s)
            .collect();
        assert_eq!(
            got,
            [
                "->", "-", "==", "=", "!=", "!", "<=", "<", ">=", ">", "[", "]", "?", "|", "*",
                "+", ",", ":", ".", "(", ")", "{", "}", ""
            ]
        );
    }

    #[test]
    fn strings_and_escapes() {
        let src = r#""a \"b\" \\ \n \t""#;
        assert_eq!(kinds(src)[0].0, TokenKind::String);
        assert_eq!(unescape(src), "a \"b\" \\ \n \t");
        assert_eq!(escape("a \"b\" \\ \n \t"), src);
    }

    #[test]
    fn comments_are_kept_apart() {
        let lexed = lex(FileId(0), "// plain  \n/// doc\n//// not doc\nstate");
        let got: Vec<_> = lexed
            .comments
            .iter()
            .map(|c| (c.kind, c.text.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                (CommentKind::Line, "// plain"),
                (CommentKind::Doc, "/// doc"),
                (CommentKind::Line, "//// not doc")
            ]
        );
        assert_eq!(lexed.tokens.len(), 2);
    }

    #[test]
    fn spans_have_lines_and_columns() {
        let lexed = lex(FileId(3), "a\n  Bc");
        let t = lexed.tokens[1];
        assert_eq!(
            (
                t.span.file,
                t.span.start.line,
                t.span.start.col,
                t.span.end.col
            ),
            (FileId(3), 2, 3, 5)
        );
    }

    #[test]
    fn lexer_errors() {
        assert_eq!(errors("a # b"), ["RB0001 unexpected character `#`"]);
        assert_eq!(
            errors("\"abc\nx"),
            ["RB0002 unterminated string: expected `\"` before the end of the line"]
        );
        assert_eq!(
            errors(r#""\q""#),
            ["RB0003 unknown escape `\\q`: strings allow `\\\"`, `\\\\`, `\\n` and `\\t`"]
        );
        assert_eq!(
            errors("5min"),
            ["RB0004 malformed number `5min`: durations end in `ms`, `s`, `m` or `h`"]
        );
        assert_eq!(
            errors("1.5s"),
            ["RB0004 malformed duration `1.5s`: durations are whole numbers, such as `1500ms`"]
        );
        assert_eq!(
            errors("99999999999999999999"),
            ["RB0005 integer `99999999999999999999` does not fit in an `int` (64-bit signed)"]
        );
        let huge = format!("1{}.0", "0".repeat(400));
        assert_eq!(
            errors(&huge),
            [format!(
                "RB0005 float `{huge}` is too large to be a finite `float` (64-bit)"
            )]
        );
        assert!(errors(&format!("1{}.0", "0".repeat(300))).is_empty());
        assert_eq!(
            errors("99999999999999999999s"),
            [
                "RB0005 duration `99999999999999999999s` is too large: its number does not fit in 64 bits"
            ]
        );
        assert_eq!(
            errors("snake_case"),
            ["RB0006 names cannot contain `_`: `snake_case`"]
        );
    }
}
