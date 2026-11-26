//! RFC 8785 canonical JSON against the RFC's examples and against
//! ECMAScript number formatting.

use roadbook_ir::jcs::{format_number, to_canonical, to_canonical_string};
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

/// `to_canonical` writes straight from `Serialize`; it must give what the `Value` path
/// gives.
fn same_as_value_path<T: serde::Serialize>(x: &T) {
    let via_value = to_canonical_string(&serde_json::to_value(x).unwrap());
    assert_eq!(to_canonical(x).unwrap(), via_value);
}

/// Every JSON document in the spec, each line of a `.jsonl` one: traces, cases, snapshots,
/// schemas, IR.
#[test]
fn direct_writer_matches_on_the_spec_documents() {
    fn walk(dir: &std::path::Path, n: &mut usize) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, n);
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let docs: Vec<&str> = match path.extension().and_then(|e| e.to_str()) {
                Some("json") => vec![text.as_str()],
                Some("jsonl") => text.lines().filter(|l| !l.trim().is_empty()).collect(),
                _ => continue,
            };
            for d in docs {
                let v: Value = serde_json::from_str(d).unwrap();
                same_as_value_path(&v);
                *n += 1;
            }
        }
    }
    let mut n = 0;
    walk(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec"),
        &mut n,
    );
    assert!(n > 1000, "only {n} documents");
}

#[test]
fn direct_writer_matches_on_every_serde_shape() {
    use std::collections::BTreeMap;

    use serde::Serialize;

    #[derive(Serialize)]
    struct Unit;

    #[derive(Serialize)]
    struct Newtype(Vec<u8>);

    #[derive(Serialize)]
    struct Tuple(i32, &'static str);

    #[derive(Serialize)]
    enum External {
        Unit,
        Newtype(u64),
        Tuple(i8, bool),
        Struct { z: u8, a: Option<f64> },
    }

    #[derive(Serialize)]
    #[serde(tag = "kind")]
    enum Internal {
        #[serde(rename = "b.second")]
        Second {
            zeta: String,
            alpha: Value,
        },
        Empty,
    }

    /// Field order that is not canonical order, names past U+E000 and with escapes.
    #[derive(Serialize)]
    struct Fields {
        z: u32,
        #[serde(rename = "\u{fb33}")]
        dalet: u8,
        #[serde(rename = "\u{1f600}")]
        emoji: u8,
        #[serde(rename = "a\"b")]
        quoted: u8,
        a: i128,
        b: u128,
        c: f32,
        nan: f64,
        inf: f64,
        neg_zero: f64,
        ch: char,
        bytes: Newtype,
        unit: Unit,
        tuple: Tuple,
        externals: Vec<External>,
        internals: Vec<Internal>,
        ints: BTreeMap<i64, &'static str>,
        escaped_keys: BTreeMap<String, u8>,
        none: Option<u8>,
    }

    #[derive(Serialize)]
    struct Flattened<'a> {
        t: u64,
        run: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        parent: Option<&'a str>,
        #[serde(flatten)]
        record: &'a Internal,
    }

    let fields = Fields {
        z: 1,
        dalet: 2,
        emoji: 3,
        quoted: 4,
        a: -(1 << 60),
        b: 1 << 63,
        c: 0.1,
        nan: f64::NAN,
        inf: f64::INFINITY,
        neg_zero: -0.0,
        ch: '\n',
        bytes: Newtype(vec![0, 255]),
        unit: Unit,
        tuple: Tuple(-7, "x\u{1}"),
        externals: vec![
            External::Unit,
            External::Newtype(u64::MAX),
            External::Tuple(-1, true),
            External::Struct { z: 0, a: None },
            External::Struct {
                z: 9,
                a: Some(1e21),
            },
        ],
        internals: vec![
            Internal::Second {
                zeta: "z".into(),
                alpha: json!({ "y": [1, { "b": 2, "a": 1 }], "x": null }),
            },
            Internal::Empty,
        ],
        ints: BTreeMap::from([(10, "ten"), (-2, "minus two"), (9, "nine")]),
        escaped_keys: BTreeMap::from([
            ("b\\".to_owned(), 1),
            ("a\n".to_owned(), 2),
            ("a".to_owned(), 3),
            ("\u{1f600}x".to_owned(), 4),
            ("\u{fb33}".to_owned(), 5),
        ]),
        none: None,
    };
    same_as_value_path(&fields);
    let record = Internal::Second {
        zeta: "q".into(),
        alpha: json!(1.5),
    };
    for parent in [None, Some("r1")] {
        same_as_value_path(&Flattened {
            t: 12,
            run: "r1.2",
            parent,
            record: &record,
        });
    }
    same_as_value_path(&Flattened {
        t: 0,
        run: "r",
        parent: None,
        record: &Internal::Empty,
    });
}

#[test]
fn direct_writer_refuses_what_to_value_refuses() {
    use std::collections::BTreeMap;
    let bad = BTreeMap::from([(vec![1u8], 1)]);
    assert!(serde_json::to_value(&bad).is_err());
    assert!(to_canonical(&bad).is_err());
}

#[test]
fn direct_writer_keeps_the_last_of_a_repeated_member() {
    #[derive(serde::Serialize)]
    struct Inner {
        a: u8,
        b: u8,
    }
    #[derive(serde::Serialize)]
    struct Outer {
        b: &'static str,
        #[serde(flatten)]
        inner: Inner,
        c: u8,
    }
    let x = Outer {
        b: "first",
        inner: Inner { a: 1, b: 2 },
        c: 3,
    };
    same_as_value_path(&x);
    assert_eq!(to_canonical(&x).unwrap(), r#"{"a":1,"b":2,"c":3}"#);
}

/// What serde_json's `Number` serialises as with its `arbitrary_precision` feature, which
/// this workspace does not enable but a host may: written as the number it holds.
#[test]
fn direct_writer_reads_serde_json_private_numbers() {
    use serde::ser::SerializeStruct;
    struct Precise(&'static str);
    impl serde::Serialize for Precise {
        fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            let mut st = s.serialize_struct("$serde_json::private::Number", 1)?;
            st.serialize_field("$serde_json::private::Number", self.0)?;
            st.end()
        }
    }
    assert_eq!(
        to_canonical(&json!({ "n": [1] })).unwrap(),
        to_canonical(&std::collections::BTreeMap::from([("n", [Precise("1")])])).unwrap()
    );
    assert_eq!(to_canonical(&Precise("2.50")).unwrap(), "2.5");
    assert_eq!(to_canonical(&Precise("1e21")).unwrap(), "1e+21");
}

/// What serde_json's `RawValue` serialises as (its `raw_value` feature, enabled in some
/// builds of this workspace): written as the JSON it holds, canonically.
#[test]
fn direct_writer_reads_raw_values() {
    use serde::ser::SerializeStruct;
    struct Raw(&'static str);
    impl serde::Serialize for Raw {
        fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            let mut st = s.serialize_struct("$serde_json::private::RawValue", 1)?;
            st.serialize_field("$serde_json::private::RawValue", self.0)?;
            st.end()
        }
    }
    assert_eq!(
        to_canonical(&[Raw(r#"{ "b": 1.0, "a": "x\u0041" }"#)]).unwrap(),
        r#"[{"a":"xA","b":1}]"#
    );
}
