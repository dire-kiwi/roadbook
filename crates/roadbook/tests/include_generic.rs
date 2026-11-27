//! Maps and generic types across the generated code (`tests/charts/generic.road`): a map is a
//! `BTreeMap`, a generic type a generic struct or alias, and values inside them keep the JSON
//! form of their type: a `duration` is milliseconds and an `int` is read from any integral
//! number, in a map and in a type argument alike. A `T?` that closes a cycle through a type
//! argument is boxed.

#![deny(warnings)]

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::json;

mod generic {
    /// The chart's `extern type Money`: cents.
    #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
    pub struct Money(pub i64);

    roadbook::include!("tests/charts/generic.road");
}

use generic::*;

/// Encodes `value`, checks its JSON, and checks it decodes back to itself.
fn round_trip<T>(value: &T, expected: serde_json::Value)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let encoded = serde_json::to_value(value).unwrap();
    assert_eq!(encoded, expected);
    assert_eq!(&serde_json::from_value::<T>(encoded).unwrap(), value);
}

#[test]
fn generic_records_keep_the_json_form_of_their_arguments() {
    round_trip(
        &Timed {
            value: Duration::from_millis(1500),
            wait: Duration::from_secs(2),
        },
        json!({"value": 1500, "wait": 2000}),
    );
    round_trip(
        &Page {
            items: vec![Pair {
                first: "a".to_string(),
                second: 1i64,
            }],
            next: None,
        },
        json!({"items": [{"first": "a", "second": 1}], "next": null}),
    );
    // An `int` in a type argument reads any integral number.
    let page: Page<i64> =
        serde_json::from_value(json!({"items": [2.0, 3e0], "next": "n"})).unwrap();
    assert_eq!(page.items, vec![2, 3]);
    assert!(serde_json::from_value::<Page<i64>>(json!({"items": [2.5], "next": null})).is_err());
    // A non-finite float does not encode.
    assert!(serde_json::to_value(Wrap { inner: f64::NAN }).is_err());
}

#[test]
fn maps_are_btreemaps_of_the_value_form() {
    let waits: Dict<Duration> = BTreeMap::from([
        ("a".to_string(), Duration::from_millis(5)),
        ("b".to_string(), Duration::from_secs(1)),
    ]);
    round_trip(
        &Wrap { inner: waits },
        json!({"inner": {"a": 5, "b": 1000}}),
    );
    let prices: BTreeMap<String, Money> = BTreeMap::from([("x".to_string(), Money(250))]);
    round_trip(&Wrap { inner: prices }, json!({"inner": {"x": 250}}));
}

#[test]
fn cycles_through_type_arguments_are_boxed() {
    let leaf = |label: &str| Node {
        label: label.into(),
        kids: Page {
            items: vec![],
            next: None,
        },
        wrapped: Wrap { inner: None },
    };
    // `Page` holds its items in a `Vec`, so `kids` needs no box; `Wrap` holds `inner` in
    // place, so the `Node?` in `Wrap<Node?>` is boxed.
    let node = Node {
        label: "root".into(),
        kids: Page {
            items: vec![leaf("kid")],
            next: Some("2".into()),
        },
        wrapped: Wrap {
            inner: Some(Box::new(leaf("inner"))),
        },
    };
    round_trip(
        &node,
        json!({
            "label": "root",
            "kids": {"items": [{"label": "kid", "kids": {"items": [], "next": null}, "wrapped": {"inner": null}}], "next": "2"},
            "wrapped": {"inner": {"label": "inner", "kids": {"items": [], "next": null}, "wrapped": {"inner": null}}}
        }),
    );
    let link = Link {
        value: Duration::from_millis(1),
        next: Some(Box::new(Link {
            value: Duration::from_millis(2),
            next: None,
        })),
    };
    round_trip(
        &link,
        json!({"value": 1, "next": {"value": 2, "next": null}}),
    );
}

#[test]
fn a_question_mark_inside_a_generic_type_is_boxed() {
    let leaf = Self1 {
        label: "leaf".into(),
        o: Opt { v: None },
        m: None,
        g: Gen {
            w: Wrap { inner: None },
        },
    };
    let node = Self1 {
        label: "node".into(),
        o: Opt {
            v: Some(Box::new(leaf.clone())),
        },
        m: Some(Box::new(leaf.clone())),
        g: Gen {
            w: Wrap {
                inner: Some(Box::new(leaf)),
            },
        },
    };
    let leaf_json =
        json!({"label": "leaf", "o": {"v": null}, "m": null, "g": {"w": {"inner": null}}});
    round_trip(
        &node,
        json!({"label": "node", "o": {"v": leaf_json}, "m": leaf_json, "g": {"w": {"inner": leaf_json}}}),
    );
}

struct Host;

impl gather::Handlers for Host {
    fn waits(&self, names: &[UserId]) -> BTreeMap<UserId, Duration> {
        names
            .iter()
            .enumerate()
            .map(|(i, n)| (n.clone(), Duration::from_millis(100 * (i as u64 + 1))))
            .collect()
    }

    fn longest(&self, waits: &Dict<Duration>) -> Timed<String> {
        let (name, wait) = waits.iter().max_by_key(|(_, d)| **d).unwrap();
        Timed {
            value: name.clone(),
            wait: *wait,
        }
    }

    async fn fetch(&self, page: &Page<i64>) -> Page<Pair<String, i64>> {
        Page {
            items: page
                .items
                .iter()
                .map(|n| Pair {
                    first: n.to_string(),
                    second: *n,
                })
                .collect(),
            next: None,
        }
    }
}

impl shapes::Handlers for Host {}

impl twice::Handlers for Host {
    fn twice(&self, n: i64) -> i64 {
        n * 2
    }
}

impl price::Handlers for Host {
    fn total(&self, prices: &BTreeMap<String, Money>) -> Wrap<Money> {
        Wrap {
            inner: Money(prices.values().map(|m| m.0).sum()),
        }
    }
}

#[tokio::test]
async fn machines_take_and_give_maps_and_generic_values() {
    let first = Page {
        items: vec![1, 2],
        next: None,
    };
    let out = gather::run(&Host, vec!["a".into(), "b".into()], first).await;
    assert_eq!(
        out,
        Timed {
            value: "b".to_string(),
            wait: Duration::from_millis(200)
        }
    );

    let tree = Tree {
        value: 1,
        children: vec![Tree {
            value: 2,
            children: vec![],
        }],
    };
    let link = Link {
        value: Duration::from_millis(7),
        next: None,
    };
    let node = Node {
        label: "n".into(),
        kids: Page {
            items: vec![],
            next: None,
        },
        wrapped: Wrap { inner: None },
    };
    let out = shapes::run(&Host, tree.clone(), link.clone(), node).await;
    assert_eq!(
        out,
        Pair {
            first: tree,
            second: link
        }
    );

    let prices = BTreeMap::from([("a".to_string(), Money(1)), ("b".to_string(), Money(2))]);
    assert_eq!(price::run(&Host, prices).await, Wrap { inner: Money(3) });
    assert_eq!(twice::run(&Host, 4).await, 8);
}

#[test]
fn a_parameter_named_like_a_rust_keyword() {
    round_trip(&Odd::<i64> { me: 1 }, json!({"me": 1}));
}
