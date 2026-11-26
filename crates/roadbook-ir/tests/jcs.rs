//! RFC 8785 canonical JSON against the RFC's examples and against
//! ECMAScript number formatting.

use roadbook_ir::jcs::{format_number, to_canonical_string};
use serde_json::{Value, json};

/// JSON text written with `%` for a backslash, to keep escapes readable here.
fn bs(s: &str) -> String {
    s.replace('%', "\\")
}

fn canon(json_text: &str) -> String {
    let v: Value = serde_json::from_str(json_text).unwrap();
    to_canonical_string(&v)
}

/// RFC 8785 section 3.2.2 (whitespace, numbers, strings and literals) and
/// 3.2.3 (sorting).
#[test]
fn rfc_8785_section_3_2_2() {
    let input = bs(r#"{
          "numbers": [333333333.33333329, 1E30, 4.50, 2e-3, 0.000000000000000000000000001],
          "string": "%u20ac$%u000F%u000aA'%u0042%u0022%u005c%%%"%/",
          "literals": [null, true, false]
        }"#);
    let expected = bs(
        r#"{"literals":[null,true,false],"numbers":[333333333.3333333,1e+30,4.5,0.002,1e-27],"string":"EURO$%u000f%nA'B%"%%%%%"/"}"#,
    )
    .replace("EURO", "\u{20ac}");
    assert_eq!(canon(&input), expected);
}

#[test]
fn rfc_8785_section_3_2_3_sorting() {
    let mut map = serde_json::Map::new();
    for (key, name) in [
        ('\u{20ac}', "Euro Sign"),
        ('\r', "Carriage Return"),
        ('\u{fb33}', "Hebrew Letter Dalet With Dagesh"),
        ('1', "One"),
        ('\u{1f600}', "Emoji: Grinning Face"),
        ('\u{80}', "Control"),
        ('\u{f6}', "Latin Small Letter O With Diaeresis"),
    ] {
        map.insert(key.to_string(), Value::from(name));
    }
    let out = to_canonical_string(&Value::Object(map));
    // UTF-16 order puts the emoji's surrogate pair (U+D83D…) before U+FB33,
    // unlike code point order.
    let expected = bs(&format!(
        r#"{{"%r":"Carriage Return","1":"One","{}":"Control","{}":"Latin Small Letter O With Diaeresis","{}":"Euro Sign","{}":"Emoji: Grinning Face","{}":"Hebrew Letter Dalet With Dagesh"}}"#,
        '\u{80}', '\u{f6}', '\u{20ac}', '\u{1f600}', '\u{fb33}'
    ));
    assert_eq!(out, expected);
}

/// The nested structure example from the RFC's reference test data.
#[test]
fn nested_structures() {
    let input = bs(
        r#"{"1": {"f": {"f": "hi","F": 5} ,"%n": 56.0}, "10": { }, "": "empty", "a": { }, "111": [ {"e": "yes","E": "no" } ], "A": { }}"#,
    );
    let expected = bs(
        r#"{"":"empty","1":{"%n":56,"f":{"F":5,"f":"hi"}},"10":{},"111":[{"E":"no","e":"yes"}],"A":{},"a":{}}"#,
    );
    assert_eq!(canon(&input), expected);
}

#[test]
fn string_escapes() {
    let s: String = [1u8, 8, 9, 10, 12, 13, 31, 32, 0x7f]
        .iter()
        .map(|&b| char::from(b))
        .chain("\"\\/<\u{2028}".chars())
        .collect();
    let out = to_canonical_string(&json!([s]));
    assert_eq!(
        out,
        bs("[\"%u0001%b%t%n%f%r%u001f \u{7f}%\"%%/<\u{2028}\"]")
    );
}

/// RFC 8785 appendix B: IEEE 754 bit patterns and their canonical form.
#[test]
fn rfc_8785_appendix_b_numbers() {
    let vectors = [
        (0x0000000000000000u64, "0"),
        (0x8000000000000000, "0"),
        (0x0000000000000001, "5e-324"),
        (0x8000000000000001, "-5e-324"),
        (0x7fefffffffffffff, "1.7976931348623157e+308"),
        (0xffefffffffffffff, "-1.7976931348623157e+308"),
        (0x4340000000000000, "9007199254740992"),
        (0xc340000000000000, "-9007199254740992"),
        (0x4430000000000000, "295147905179352830000"),
        (0x44b52d02c7e14af5, "9.999999999999997e+22"),
        (0x44b52d02c7e14af6, "1e+23"),
        (0x44b52d02c7e14af7, "1.0000000000000001e+23"),
        (0x444b1ae4d6e2ef4e, "999999999999999700000"),
        (0x444b1ae4d6e2ef4f, "999999999999999900000"),
        (0x444b1ae4d6e2ef50, "1e+21"),
        (0x3eb0c6f7a0b5ed8c, "9.999999999999997e-7"),
        (0x3eb0c6f7a0b5ed8d, "0.000001"),
        (0x41b3de4355555553, "333333333.3333332"),
        (0x41b3de4355555554, "333333333.33333325"),
        (0x41b3de4355555555, "333333333.3333333"),
        (0x41b3de4355555556, "333333333.3333334"),
        (0x41b3de4355555557, "333333333.33333343"),
        (0xbecbf647612f3696, "-0.0000033333333333333333"),
        (0x43143ff3c1cb0959, "1424953923781206.2"),
    ];
    for (bits, expected) in vectors {
        let f = f64::from_bits(bits);
        assert_eq!(format_number(f).as_deref(), Some(expected), "{bits:016x}");
        // And through a JSON value, as the canonicaliser sees it.
        assert_eq!(to_canonical_string(&json!(f)), expected, "{bits:016x}");
    }
    assert_eq!(format_number(f64::NAN), None);
    assert_eq!(format_number(f64::INFINITY), None);
}

/// Integers and floats canonicalise alike, and parsing then canonicalising
/// agrees with JavaScript's `JSON.stringify(JSON.parse(text))`.
#[test]
fn integers_and_parsed_numbers() {
    assert_eq!(
        canon("[2.0, 2, -0, -0.0, 1e2, 100, 9007199254740993]"),
        "[2,2,0,0,100,100,9007199254740992]"
    );
    assert_eq!(
        canon("[1.0e-7, 0.1, 123456789012345678901234567890]"),
        "[1e-7,0.1,1.2345678901234568e+29]"
    );
    assert_eq!(
        canon(r#"{"b":1,"a":{"d":[],"c":{}}}"#),
        r#"{"a":{"c":{},"d":[]},"b":1}"#
    );
}

/// A spread of doubles formatted by node's `JSON.stringify`, generated once
/// and committed (see the file's header).
#[test]
fn matches_ecmascript_number_formatting() {
    let data = include_str!("data/es_numbers.txt");
    let mut n = 0;
    for line in data.lines().filter(|l| !l.starts_with('#')) {
        let (hex, expected) = line.split_once(' ').unwrap();
        let f = f64::from_bits(u64::from_str_radix(hex, 16).unwrap());
        assert_eq!(format_number(f).as_deref(), Some(expected), "bits {hex}");
        // Parsing JavaScript's output gives back the same double.
        let parsed: Value = serde_json::from_str(expected).unwrap();
        assert_eq!(parsed.as_f64(), Some(f), "parse {expected}");
        n += 1;
    }
    assert!(n > 1000);
}
