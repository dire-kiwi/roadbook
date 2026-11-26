//! Source positions: file ids, spans and a line index.

use serde::Serialize;

/// Identifies a source file. The caller decides what the number means, usually an index into
/// its list of loaded files (the IR's `sources` array uses the same numbering).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct FileId(pub u32);

/// A position in a source file: a byte offset plus the 1-based line and column it falls on.
/// Columns count Unicode scalar values, not bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Pos {
    pub offset: u32,
    pub line: u32,
    pub col: u32,
}

/// A half-open range `start..end` of a source file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Span {
    pub file: FileId,
    pub start: Pos,
    pub end: Pos,
}

impl Span {
    /// The span from the start of `self` to the end of `other`.
    pub fn to(self, other: Span) -> Span {
        Span {
            file: self.file,
            start: self.start,
            end: other.end,
        }
    }

    /// The byte range of the span, for slicing the source.
    pub fn range(self) -> std::ops::Range<usize> {
        self.start.offset as usize..self.end.offset as usize
    }

    /// An empty span at the start of `self`.
    pub fn shrink_to_start(self) -> Span {
        Span {
            end: self.start,
            ..self
        }
    }

    /// An empty span at the end of `self`.
    pub fn shrink_to_end(self) -> Span {
        Span {
            start: self.end,
            ..self
        }
    }
}

/// Serialized as `{ "file", "start", "end", "line", "col", "endLine", "endCol" }`, the
/// offsets in bytes and lines and columns 1-based.
impl Serialize for Span {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("Span", 7)?;
        st.serialize_field("file", &self.file)?;
        st.serialize_field("start", &self.start.offset)?;
        st.serialize_field("end", &self.end.offset)?;
        st.serialize_field("line", &self.start.line)?;
        st.serialize_field("col", &self.start.col)?;
        st.serialize_field("endLine", &self.end.line)?;
        st.serialize_field("endCol", &self.end.col)?;
        st.end()
    }
}

/// Maps byte offsets to lines and columns, and lines back to their text.
#[derive(Clone, Debug)]
pub struct LineIndex {
    /// Byte offset of the start of each line.
    starts: Vec<u32>,
}

impl LineIndex {
    pub fn new(src: &str) -> LineIndex {
        let mut starts = vec![0];
        starts.extend(src.match_indices('\n').map(|(i, _)| i as u32 + 1));
        LineIndex { starts }
    }

    /// The number of lines. A file ending in a newline has an empty last line.
    pub fn line_count(&self) -> usize {
        self.starts.len()
    }

    /// The position of a byte offset. Offsets past the end are clamped to the end.
    pub fn pos(&self, src: &str, offset: usize) -> Pos {
        let offset = offset.min(src.len());
        let line = self.starts.partition_point(|&s| s as usize <= offset) - 1;
        let start = self.starts[line] as usize;
        let col = src
            .get(start..offset)
            .map_or(offset - start, |s| s.chars().count())
            + 1;
        Pos {
            offset: offset as u32,
            line: line as u32 + 1,
            col: col as u32,
        }
    }

    /// A span of `file` covering the byte range `start..end`.
    pub fn span(&self, file: FileId, src: &str, start: usize, end: usize) -> Span {
        Span {
            file,
            start: self.pos(src, start),
            end: self.pos(src, end),
        }
    }

    /// The text of a 1-based line, without its line terminator.
    pub fn line_text<'a>(&self, src: &'a str, line: u32) -> &'a str {
        let i = (line as usize).saturating_sub(1).min(self.starts.len() - 1);
        let start = self.starts[i] as usize;
        let end = self.starts.get(i + 1).map_or(src.len(), |&e| e as usize);
        src[start..end].trim_end_matches(['\n', '\r'])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_are_one_based_and_count_chars() {
        let src = "ab\n\u{e9}x\n";
        let idx = LineIndex::new(src);
        assert_eq!(idx.line_count(), 3);
        assert_eq!(
            idx.pos(src, 0),
            Pos {
                offset: 0,
                line: 1,
                col: 1
            }
        );
        assert_eq!(
            idx.pos(src, 2),
            Pos {
                offset: 2,
                line: 1,
                col: 3
            }
        );
        assert_eq!(
            idx.pos(src, 3),
            Pos {
                offset: 3,
                line: 2,
                col: 1
            }
        );
        // `é` is two bytes but one column.
        assert_eq!(
            idx.pos(src, 5),
            Pos {
                offset: 5,
                line: 2,
                col: 2
            }
        );
        assert_eq!(idx.line_text(src, 2), "\u{e9}x");
        assert_eq!(idx.line_text(src, 3), "");
    }
}
