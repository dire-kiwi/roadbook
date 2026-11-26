//! Types that refer to themselves (`tests/charts/recursive.road`): a `T?` that closes a cycle
//! of types is `Option<Box<T>>`, with the same JSON as `Option<T>`, through serde and through
//! a run.

#![deny(warnings)]

use serde_json::json;

roadbook::include!("tests/charts/recursive.road");

fn list(values: &[i64]) -> Option<Box<Node>> {
    values
        .iter()
        .rev()
        .fold(None, |next, &value| Some(Box::new(Node { value, next })))
}

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
fn a_record_that_refers_to_itself() {
    let node = *list(&[1, 2, 3]).unwrap();
    round_trip(
        &node,
        json!({"value": 1, "next": {"value": 2, "next": {"value": 3, "next": null}}}),
    );
    // A missing optional is `None`, as for any `Option`.
    let read: Node = serde_json::from_value(json!({"value": 1})).unwrap();
    assert_eq!(
        read,
        Node {
            value: 1,
            next: None
        }
    );
}

#[test]
fn records_that_refer_to_each_other() {
    let a = A {
        name: "a".into(),
        b: Some(Box::new(B {
            name: "b".into(),
            a: Some(Box::new(A {
                name: "inner".into(),
                b: None,
            })),
        })),
    };
    round_trip(
        &a,
        json!({"name": "a", "b": {"name": "b", "a": {"name": "inner", "b": null}}}),
    );
}

#[test]
fn an_error_that_refers_to_itself() {
    let chain = Chain {
        reason: "outer".into(),
        cause: Some(Box::new(Chain {
            reason: "inner".into(),
            cause: None,
        })),
    };
    round_trip(
        &chain,
        json!({
            "_tag": "Chain",
            "reason": "outer",
            "cause": {"_tag": "Chain", "reason": "inner", "cause": null},
        }),
    );
    // The `_tag` of the cause is checked too.
    let wrong = json!({"_tag": "Chain", "reason": "outer", "cause": {"reason": "inner"}});
    assert!(serde_json::from_value::<Chain>(wrong).is_err());
}

#[test]
fn a_record_that_refers_to_itself_through_an_alias_and_a_list() {
    let link = Link {
        value: 1,
        next: Some(Box::new(Link {
            value: 2,
            next: None,
        })),
    };
    let _: &MaybeLink = &link.next;
    round_trip(
        &link,
        json!({"value": 1, "next": {"value": 2, "next": null}}),
    );
    // A list holds its elements behind a pointer already.
    let tree = Tree {
        value: 1,
        children: vec![Tree {
            value: 2,
            children: vec![],
        }],
    };
    round_trip(
        &tree,
        json!({"value": 1, "children": [{"value": 2, "children": []}]}),
    );
    // An event is no type: its `Node?` needs no `Box`.
    let _ = grow::Event::Swap {
        node: list(&[1]).map(|n| *n),
    };
}

struct Host {
    fail: bool,
}

impl grow::Handlers for Host {
    async fn extend(
        &self,
        node: &Node,
        a: &A,
        link: &Link,
        tree: &Tree,
    ) -> Result<Node, ExtendError> {
        assert_eq!(a.b.as_ref().unwrap().name, "b");
        assert_eq!(link.next.as_ref().unwrap().value, 2);
        assert_eq!(tree.children[0].value, 2);
        if self.fail {
            return Err(Chain {
                reason: "outer".into(),
                cause: Some(Box::new(Chain {
                    reason: "inner".into(),
                    cause: None,
                })),
            }
            .into());
        }
        Ok(Node {
            value: 0,
            next: Some(Box::new(node.clone())),
        })
    }
}

#[tokio::test]
async fn values_that_refer_to_themselves_go_through_a_run() {
    let node = *list(&[1, 2]).unwrap();
    let a = A {
        name: "a".into(),
        b: Some(Box::new(B {
            name: "b".into(),
            a: None,
        })),
    };
    let link = Link {
        value: 1,
        next: Some(Box::new(Link {
            value: 2,
            next: None,
        })),
    };
    let tree = Tree {
        value: 1,
        children: vec![Tree {
            value: 2,
            children: vec![],
        }],
    };
    let host = Host { fail: false };
    let out = grow::run(&host, node.clone(), a.clone(), link.clone(), tree.clone()).await;
    assert_eq!(out, Ok(*list(&[0, 1, 2]).unwrap()));
    let host = Host { fail: true };
    let out = grow::run(&host, node, a, link, tree).await;
    let Err(GrowError::Chain(chain)) = out else {
        panic!("expected a Chain, got {out:?}");
    };
    assert_eq!(chain.cause.unwrap().reason, "inner");
}
