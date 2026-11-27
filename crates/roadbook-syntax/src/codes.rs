//! The syntax diagnostic codes, `RB0001` to `RB00xx`. Each is listed with an example in the
//! "Diagnostic codes" table of `docs/design/language.md`.

use crate::diagnostic::Code;

/// A character that cannot start any token.
pub const UNEXPECTED_CHARACTER: Code = Code(1);
/// A string literal with no closing quote on its line.
pub const UNTERMINATED_STRING: Code = Code(2);
/// A backslash escape other than `\"`, `\\`, `\n` and `\t`.
pub const UNKNOWN_ESCAPE: Code = Code(3);
/// A number followed by letters that are not a duration unit, such as `5min` or `1.5s`.
pub const MALFORMED_NUMBER: Code = Code(4);
/// A number too large for its type: an integer beyond a 64-bit signed integer, a duration
/// whose number does not fit in 64 bits, or a float too large to be a finite 64-bit float.
pub const NUMBER_TOO_LARGE: Code = Code(5);
/// A name containing `_`. Names are camelCase or PascalCase.
pub const UNDERSCORE_IN_NAME: Code = Code(6);
/// The parser found a token it did not expect.
pub const UNEXPECTED_TOKEN: Code = Code(10);
/// A `{` or `(` that is never closed.
pub const UNCLOSED_DELIMITER: Code = Code(11);
/// A lower-case name where an upper-case one is required, or the reverse.
pub const WRONG_NAME_CASE: Code = Code(12);
/// A keyword, or the reserved name `result`, used as a name.
pub const RESERVED_NAME: Code = Code(13);
/// A state-only item (`done`, `invoke`, `output`, `fail`) directly in a machine body.
pub const NOT_IN_MACHINE_BODY: Code = Code(14);
/// A machine-only item (`var`, `event`, `emits`, `children`, `check`) inside a state.
pub const NOT_IN_STATE: Code = Code(15);
/// A machine or state item at the top level of a file.
pub const NOT_AT_TOP_LEVEL: Code = Code(16);
/// A transition with no target and no effects.
pub const TRANSITION_DOES_NOTHING: Code = Code(17);
/// A record value written directly as an `after` delay.
pub const RECORD_IN_AFTER: Code = Code(18);
/// `fail Error` with branches outside an `invoke` block.
pub const OUTCOME_OUTSIDE_INVOKE: Code = Code(19);
/// Something other than a `done` or `fail Error` outcome inside an `invoke` block.
pub const NOT_AN_OUTCOME: Code = Code(20);
/// A branch block that does not start with `if`, or has a branch after `else`.
pub const MALFORMED_BRANCH_BLOCK: Code = Code(21);
/// Two comparisons chained without parentheses, such as `a < b < c`.
pub const CHAINED_COMPARISON: Code = Code(22);
/// A `///` comment that is not followed by something it can document.
pub const DOC_COMMENT_DOCUMENTS_NOTHING: Code = Code(23);
/// A lower-case name in a type position that is not a built-in type.
pub const UNKNOWN_BUILTIN_TYPE: Code = Code(24);
/// States, expressions or types nested more than [`crate::MAX_DEPTH`] levels deep.
pub const NESTED_TOO_DEEPLY: Code = Code(25);
/// `ignore` outside an `on` transition, or together with a target or effects.
pub const MISPLACED_IGNORE: Code = Code(26);
/// `on name.word` with a lower-case `word` other than `done`, `fail` and `ended`.
pub const UNKNOWN_LIFECYCLE_TRIGGER: Code = Code(27);

#[cfg(test)]
mod tests {
    /// Every code defined here is in the table in `language.md`, and the table has no other
    /// syntax codes.
    #[test]
    fn codes_are_documented() {
        let defined: Vec<String> = include_str!("codes.rs")
            .lines()
            .filter(|l| l.starts_with("pub const"))
            .filter_map(|l| {
                l.split_once("= Code(")
                    .map(|(_, n)| n.trim_end_matches(");").parse::<u16>().unwrap())
            })
            .map(|n| format!("RB{n:04}"))
            .collect();
        let doc = include_str!("../../../docs/design/language.md");
        let table: Vec<String> = doc
            .lines()
            .filter_map(|l| l.strip_prefix("| `RB"))
            .map(|l| format!("RB{}", &l[..4]))
            .filter(|c| c.starts_with("RB00"))
            .collect();
        assert_eq!(defined, table);
    }
}
