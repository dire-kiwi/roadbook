//! Positions and file names as the protocol has them: lines and columns counted in UTF-16 or
//! UTF-8 code units, and `file://` URIs.

use std::path::{Path, PathBuf};
use std::str::FromStr;

use lsp_types::{Position, Range, Uri};
use roadbook_compiler::Span;

/// What a column counts: UTF-16 code units (the protocol's default) or bytes, if the client
/// offers `utf-8`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16,
}

/// A text with the byte offset of each line's start.
pub struct Lines<'a> {
    text: &'a str,
    starts: Vec<usize>,
}

impl<'a> Lines<'a> {
    pub fn new(text: &'a str) -> Lines<'a> {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        Lines { text, starts }
    }

    /// The position of a byte offset.
    pub fn position(&self, offset: usize, enc: Encoding) -> Position {
        let mut offset = offset.min(self.text.len());
        while !self.text.is_char_boundary(offset) {
            offset -= 1;
        }
        let line = self.starts.partition_point(|&s| s <= offset) - 1;
        let before = &self.text[self.starts[line]..offset];
        let character = match enc {
            Encoding::Utf8 => before.len(),
            Encoding::Utf16 => before.encode_utf16().count(),
        };
        Position::new(line as u32, character as u32)
    }

    /// The byte offset of a position. A column past the end of its line is the end of the
    /// line, and a line past the end is the end of the text.
    pub fn offset(&self, pos: Position, enc: Encoding) -> usize {
        let Some(&start) = self.starts.get(pos.line as usize) else {
            return self.text.len();
        };
        let line = &self.text[start..];
        let line = &line[..line.find('\n').unwrap_or(line.len())];
        let line = line.strip_suffix('\r').unwrap_or(line);
        let mut units = 0usize;
        for (i, c) in line.char_indices() {
            units += match enc {
                Encoding::Utf8 => c.len_utf8(),
                Encoding::Utf16 => c.len_utf16(),
            };
            // A column inside a character means that character.
            if units > pos.character as usize {
                return start + i;
            }
        }
        start + line.len()
    }

    pub fn range(&self, span: Span, enc: Encoding) -> Range {
        Range::new(
            self.position(span.start.offset as usize, enc),
            self.position(span.end.offset as usize, enc),
        )
    }

    /// The range of the whole text.
    pub fn all(&self, enc: Encoding) -> Range {
        Range::new(Position::new(0, 0), self.position(self.text.len(), enc))
    }
}

/// The file a `file://` URI names, if it names one.
pub fn uri_path(uri: &Uri) -> Option<PathBuf> {
    let s = uri.as_str();
    let rest = s.strip_prefix("file://")?;
    // `file:///path` or `file://localhost/path`; another host is not a local file.
    let path = if rest.starts_with('/') {
        rest
    } else {
        rest.strip_prefix("localhost")?
    };
    let path = path.split(['?', '#']).next().unwrap_or("");
    let decoded = percent_decode(path)?;
    if cfg!(windows) {
        // `/c:/x` is `c:/x`.
        let bytes = decoded.as_bytes();
        if bytes.len() >= 3 && bytes[0] == b'/' && bytes[2] == b':' {
            return Some(PathBuf::from(&decoded[1..]));
        }
    }
    Some(PathBuf::from(decoded))
}

/// The `file://` URI of an absolute path.
pub fn path_uri(path: &Path) -> Uri {
    let mut s = String::from("file://");
    let text = path.to_string_lossy().replace('\\', "/");
    if !text.starts_with('/') {
        s.push('/');
    }
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~!$&'()*+,;=:@".contains(&b) {
            s.push(b as char);
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    Uri::from_str(&s).unwrap_or_else(|_| Uri::from_str("file:///").expect("a valid URI"))
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_and_utf8_columns() {
        // `é` is 2 bytes and 1 UTF-16 unit; `𝄞` is 4 bytes and 2 units.
        let text = "ab\n\u{e9}\u{1d11e}x\r\nlast";
        let lines = Lines::new(text);
        let x = text.find('x').unwrap();
        assert_eq!(lines.position(x, Encoding::Utf16), Position::new(1, 3));
        assert_eq!(lines.position(x, Encoding::Utf8), Position::new(1, 6));
        assert_eq!(lines.offset(Position::new(1, 3), Encoding::Utf16), x);
        assert_eq!(lines.offset(Position::new(1, 6), Encoding::Utf8), x);
        // Inside a surrogate pair: the character it is part of.
        assert_eq!(lines.offset(Position::new(1, 2), Encoding::Utf16), x - 4);
        // Past the end of a line (before `\r\n`), and past the last line.
        assert_eq!(lines.offset(Position::new(1, 99), Encoding::Utf16), x + 1);
        assert_eq!(
            lines.offset(Position::new(9, 0), Encoding::Utf16),
            text.len()
        );
        assert_eq!(lines.all(Encoding::Utf16).end, Position::new(2, 4));
    }

    #[test]
    fn file_uris() {
        let p = Path::new("/tmp/a b/ü.road");
        let uri = path_uri(p);
        assert_eq!(uri.as_str(), "file:///tmp/a%20b/%C3%BC.road");
        assert_eq!(uri_path(&uri).unwrap(), p);
        let other = Uri::from_str("file://localhost/x/y.road").unwrap();
        assert_eq!(uri_path(&other).unwrap(), Path::new("/x/y.road"));
        assert!(uri_path(&Uri::from_str("untitled:1").unwrap()).is_none());
    }
}
