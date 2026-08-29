use arc_swap::ArcSwapOption;
use fastrand::Rng;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use yrs::node::{DeepObservable, Observable, Path, PathSegment};
use yrs::test_utils::{RngExt, exchange_updates, run_scenario};
use yrs::updates::decoder::Decode;
use yrs::{
    Acquire, AcquireMut, Any, Cell, Delta, Doc, In, NodeRef, Out, StateVector, Transaction, Update,
    any,
};

#[test]
fn map_basic() {
    let mut d1 = Doc::with_client_id(1);
    let mut t1 = d1.transact_mut();
    let mut m1 = t1.node_mut("map").unwrap();

    let mut d2 = Doc::with_client_id(2);
    let mut t2 = d2.transact_mut();

    m1.insert_attr("number", 1);
    m1.insert_attr("string", "hello Y");
    m1.insert_attr("object", {
        let mut v = HashMap::new();
        v.insert("key2".to_owned(), "value");

        let mut map = HashMap::new();
        map.insert("key".to_owned(), v);
        map // { key: { key2: 'value' } }
    });
    m1.insert_attr("boolean1", true);
    m1.insert_attr("boolean0", false);

    //let m1m = t1.get_map("y-map");
    //let m1a = t1.get_text("y-text");
    //m1a.insert(&mut t1, 0, "a");
    //m1a.insert(&mut t1, 0, "b");
    //m1m.insert(&mut t1, "y-text".to_owned(), m1a);

    //TODO: YArray within YMap
    fn compare_all<D: std::ops::Deref<Target = Doc>>(m: &NodeRef<&Transaction<D>>) {
        assert_eq!(m.len(), 5);
        assert_eq!(m.attr("number"), Some(Out::from(1f64)));
        assert_eq!(m.attr("boolean0"), Some(Out::from(false)));
        assert_eq!(m.attr("boolean1"), Some(Out::from(true)));
        assert_eq!(m.attr("string"), Some(Out::from("hello Y")));
        assert_eq!(
            m.attr("object"),
            Some(Out::from(any!({
                "key": {
                    "key2": "value"
                }
            })))
        );
    }

    drop(m1);
    compare_all(&t1.node("map").unwrap());

    let update = t1.encode_state_as_update_v1(&StateVector::default());
    t2.apply_update(Update::decode_v1(update.as_slice()).unwrap())
        .unwrap();

    compare_all(&t2.node("map").unwrap());
}

#[test]
fn map_get_set() {
    let mut d1 = Doc::with_client_id(1);
    let mut t1 = d1.transact_mut();
    let mut m1 = t1.node_mut("map").unwrap();

    m1.insert_attr("stuff", "stuffy");
    m1.insert_attr("null", None as Option<String>);

    let update = t1.encode_state_as_update_v1(&StateVector::default());

    let mut d2 = Doc::with_client_id(2);
    let mut t2 = d2.transact_mut();

    t2.apply_update(Update::decode_v1(update.as_slice()).unwrap())
        .unwrap();

    let m2 = t2.node_mut("map").unwrap();

    assert_eq!(m2.attr("stuff"), Some(Out::from("stuffy")));
    assert_eq!(m2.attr("null"), Some(Out::Any(Any::Null)));
}

#[test]
fn map_get_set_sync_with_conflicts() {
    let mut d1 = Doc::with_client_id(1);
    let mut t1 = d1.transact_mut();
    t1.node_mut("map").unwrap().insert_attr("stuff", "c0");

    let mut d2 = Doc::with_client_id(2);
    let mut t2 = d2.transact_mut();
    t2.node_mut("map").unwrap().insert_attr("stuff", "c1");

    let u1 = t1.encode_state_as_update_v1(&StateVector::default());
    let u2 = t2.encode_state_as_update_v1(&StateVector::default());

    t1.apply_update(Update::decode_v1(u2.as_slice()).unwrap())
        .unwrap();
    t2.apply_update(Update::decode_v1(u1.as_slice()).unwrap())
        .unwrap();

    assert_eq!(t1.node("map").unwrap().attr("stuff"), Some(Out::from("c1")));
    assert_eq!(t2.node("map").unwrap().attr("stuff"), Some(Out::from("c1")));
}

#[test]
fn map_len_remove() {
    let mut d1 = Doc::with_client_id(1);
    let mut t1 = d1.transact_mut();
    let mut m1 = t1.node_mut("map").unwrap();

    let key1 = "stuff";
    let key2 = "other-stuff";

    m1.insert_attr(key1, "c0");
    m1.insert_attr(key2, "c1");
    assert_eq!(m1.attr_len(), 2);

    // TODO(unified-api): `remove_attr` doesn't return the removed value
    assert_eq!(m1.remove_attr(&key1), Some(Out::from("c0")));
    assert_eq!(m1.remove_attr(&key1), None);
    assert_eq!(m1.remove_attr(&key2), Some(Out::from("c1")));

    // remove 'stuff'
    assert_eq!(m1.remove_attr(key1), Some(Out::from("c0")));
    assert_eq!(m1.attr_len(), 1);

    // remove 'stuff' again - nothing should happen
    assert_eq!(m1.remove_attr(key1), None);
    assert_eq!(m1.attr_len(), 1);

    // remove 'other-stuff'
    assert_eq!(m1.remove_attr(key2), Some(Out::from("c1")));
    assert_eq!(m1.attr_len(), 0);
}

#[test]
fn map_clear() {
    let mut d1 = Doc::with_client_id(1);
    let mut t1 = d1.transact_mut();
    {
        let mut m1 = t1.node_mut("map").unwrap();
        m1.insert_attr("key1", "c0");
        m1.insert_attr("key2", "c1");
        m1.clear_attrs();

        assert_eq!(m1.attr_len(), 0);
        assert_eq!(m1.attr("key1"), None);
        assert_eq!(m1.attr("key2"), None);
    }

    let mut d2 = Doc::with_client_id(2);
    let mut t2 = d2.transact_mut();

    let u1 = t1.encode_state_as_update_v1(&StateVector::default());
    t2.apply_update(Update::decode_v1(u1.as_slice()).unwrap())
        .unwrap();

    let m2 = t2.node("map").unwrap();
    assert_eq!(m2.attr_len(), 0);
    assert_eq!(m2.attr("key1"), None);
    assert_eq!(m2.attr("key2"), None);
}

#[test]
fn map_clear_sync() {
    let mut d1 = Doc::with_client_id(1);
    let mut d2 = Doc::with_client_id(2);
    let mut d3 = Doc::with_client_id(3);
    let mut d4 = Doc::with_client_id(4);

    {
        let mut t1 = d1.transact_mut();
        let mut t2 = d2.transact_mut();
        let mut t3 = d3.transact_mut();

        t1.node_mut("map").unwrap().insert_attr("key1", "c0");
        t2.node_mut("map").unwrap().insert_attr("key1", "c1");
        t2.node_mut("map").unwrap().insert_attr("key1", "c2");
        t3.node_mut("map").unwrap().insert_attr("key1", "c3");
    }

    exchange_updates(&mut [&mut d1, &mut d2, &mut d3, &mut d4]);

    {
        let mut t1 = d1.transact_mut();
        let mut t2 = d2.transact_mut();
        let mut t3 = d3.transact_mut();

        t1.node_mut("map").unwrap().insert_attr("key2", "c0");
        t2.node_mut("map").unwrap().insert_attr("key2", "c1");
        t2.node_mut("map").unwrap().insert_attr("key2", "c2");
        t3.node_mut("map").unwrap().insert_attr("key2", "c3");
        t3.node_mut("map").unwrap().clear_attrs();
    }

    exchange_updates(&mut [&mut d1, &mut d2, &mut d3, &mut d4]);

    for doc in [d1, d2, d3, d4] {
        let client_id = doc.client_id();
        let txn = doc.transact();
        let map = txn.node("map").unwrap();

        assert_eq!(
            map.attr("key1"),
            None,
            "'key1' entry for peer {} should be removed",
            client_id
        );
        assert_eq!(
            map.attr("key2"),
            None,
            "'key2' entry for peer {} should be removed",
            client_id
        );
        assert_eq!(
            map.attr_len(),
            0,
            "all entries for peer {} should be removed",
            client_id
        );
    }
}

#[test]
fn map_get_set_with_3_way_conflicts() {
    let mut d1 = Doc::with_client_id(1);
    let mut d2 = Doc::with_client_id(2);
    let mut d3 = Doc::with_client_id(3);

    {
        let mut t1 = d1.transact_mut();
        let mut t2 = d2.transact_mut();
        let mut t3 = d3.transact_mut();

        t1.node_mut("map").unwrap().insert_attr("stuff", "c0");
        t2.node_mut("map").unwrap().insert_attr("stuff", "c1");
        t2.node_mut("map").unwrap().insert_attr("stuff", "c2");
        t3.node_mut("map").unwrap().insert_attr("stuff", "c3");
    }

    exchange_updates(&mut [&mut d1, &mut d2, &mut d3]);

    for doc in [d1, d2, d3] {
        let client_id = doc.client_id();
        let txn = doc.transact();
        let map = txn.node("map").unwrap();

        assert_eq!(
            map.attr("stuff"),
            Some(Out::from("c3")),
            "peer {} - map entry resolved to unexpected value",
            client_id
        );
    }
}

#[test]
fn map_get_set_remove_with_3_way_conflicts() {
    let mut d1 = Doc::with_client_id(1);
    let mut d2 = Doc::with_client_id(2);
    let mut d3 = Doc::with_client_id(3);
    let mut d4 = Doc::with_client_id(4);

    {
        let mut t1 = d1.transact_mut();
        let mut t2 = d2.transact_mut();
        let mut t3 = d3.transact_mut();

        t1.node_mut("map").unwrap().insert_attr("key1", "c0");
        t2.node_mut("map").unwrap().insert_attr("key1", "c1");
        t2.node_mut("map").unwrap().insert_attr("key1", "c2");
        t3.node_mut("map").unwrap().insert_attr("key1", "c3");
    }

    exchange_updates(&mut [&mut d1, &mut d2, &mut d3, &mut d4]);

    {
        let mut t1 = d1.transact_mut();
        let mut t2 = d2.transact_mut();
        let mut t3 = d3.transact_mut();
        let mut t4 = d4.transact_mut();

        t1.node_mut("map").unwrap().insert_attr("key1", "deleteme");
        t2.node_mut("map").unwrap().insert_attr("key1", "c1");
        t3.node_mut("map").unwrap().insert_attr("key1", "c2");
        t4.node_mut("map").unwrap().insert_attr("key1", "c3");
        t4.node_mut("map").unwrap().remove_attr("key1");
    }

    exchange_updates(&mut [&mut d1, &mut d2, &mut d3, &mut d4]);

    for doc in [d1, d2, d3, d4] {
        let client_id = doc.client_id();
        let txn = doc.transact();
        let map = txn.node("map").unwrap();

        assert_eq!(
            map.attr("key1"),
            None,
            "entry 'key1' on peer {} should be removed",
            client_id
        );
    }
}

#[test]
fn insert_and_remove_events() {
    let mut d1 = Doc::with_client_id(1);
    let delta = Cell::new(Delta::out());
    let delta1 = delta.clone();
    let delta2 = delta.clone();
    let delta = || delta.acquire().clone();
    let _sub = {
        let mut txn = d1.transact_mut();
        txn.node_mut("map")
            .unwrap()
            .observe(move |e| *delta1.acquire_mut() = e.delta(false))
    };

    // insert new entry
    {
        let mut txn = d1.transact_mut();
        txn.node_mut("map").unwrap().insert_attr("a", 1);
    }
    assert_eq!(delta(), Delta::out().insert_attr("a", 1));

    // update existing entry once
    {
        let mut txn = d1.transact_mut();
        txn.node_mut("map").unwrap().insert_attr("a", 2);
    }
    assert_eq!(delta(), Delta::out().insert_attr("a", 2));

    // update existing entry twice
    {
        let mut txn = d1.transact_mut();
        let mut m1 = txn.node_mut("map").unwrap();
        m1.insert_attr("a", 3);
        m1.insert_attr("a", 4);
    }
    assert_eq!(delta(), Delta::out().insert_attr("a", 4));

    // remove existing entry
    {
        let mut txn = d1.transact_mut();
        txn.node_mut("map").unwrap().remove_attr("a");
    }
    assert_eq!(delta(), Delta::out().remove_attr("a"));

    // add another entry and update it
    {
        let mut txn = d1.transact_mut();
        let mut m1 = txn.node_mut("map").unwrap();
        m1.insert_attr("b", 1);
        m1.insert_attr("b", 2);
    }
    assert_eq!(delta(), Delta::out().insert_attr("b", 2));

    // add and remove an entry
    {
        let mut txn = d1.transact_mut();
        let mut m1 = txn.node_mut("map").unwrap();
        m1.insert_attr("c", 1);
        m1.remove_attr("c");
    }
    assert_eq!(delta(), Delta::out()); // insert->remove == no-op

    // copy updates over
    let mut d2 = Doc::with_client_id(2);
    let _sub = {
        let mut txn = d2.transact_mut();
        txn.node_mut("map")
            .unwrap()
            .observe(move |e| *delta2.acquire_mut() = e.delta(false))
    };

    {
        let t1 = d1.transact();
        let mut t2 = d2.transact_mut();

        let sv = t2.state_vector();
        let update = t1.encode_diff_v1(&sv);
        t2.apply_update(Update::decode_v1(update.as_slice()).unwrap())
            .unwrap();
    }
    assert_eq!(delta(), Delta::out().insert_attr("b", 2));
}

fn map_transactions() -> [Box<dyn Fn(&mut Doc, &mut Rng)>; 3] {
    fn set(doc: &mut Doc, rng: &mut Rng) {
        let key = rng.choice(["one", "two"]).unwrap();
        let value: String = rng.random_string();
        doc.transact_mut()
            .node_mut("map")
            .unwrap()
            .insert_attr(key, value);
    }

    fn set_type(doc: &mut Doc, rng: &mut Rng) {
        let key = rng.choice(["one", "two", "three"]).unwrap();
        let (a, b) = (rng.f32(), rng.f32());
        let mut txn = doc.transact_mut();
        let mut map = txn.node_mut("map").unwrap();
        if a <= 0.33 {
            let arr = Delta::new().insert(1).insert(2).insert(3).insert(4);
            map.insert_attr(key, In::Node(arr));
        } else if b <= 0.33 {
            map.insert_attr(key, In::Node(Delta::new().insert_text("deeptext")));
        } else {
            let nested = Delta::new().insert_attr("deepkey", "deepvalue");
            map.insert_attr(key, In::Node(nested));
        }
    }

    fn delete(doc: &mut Doc, rng: &mut Rng) {
        let key = rng.choice(["one", "two"]).unwrap();
        doc.transact_mut().node_mut("map").unwrap().remove_attr(key);
    }
    [Box::new(set), Box::new(set_type), Box::new(delete)]
}

fn fuzzy(iterations: usize) {
    run_scenario(0, &map_transactions(), 5, iterations)
}

#[test]
fn fuzzy_test_6() {
    fuzzy(6)
}

#[test]
fn observe_deep() {
    let mut doc = Doc::with_client_id(1);
    let paths = Arc::new(Mutex::new(vec![]));
    let calls = Arc::new(AtomicU32::new(0));
    let paths_copy = paths.clone();
    let calls_copy = calls.clone();
    let _sub = {
        let mut txn = doc.transact_mut();
        txn.node_mut("map").unwrap().observe_deep(move |e| {
            let path: Vec<Path> = e.iter().map(|e| e.path()).collect();
            paths_copy.lock().unwrap().push(path);
            calls_copy.fetch_add(1, Ordering::Relaxed);
        })
    };

    let nested = {
        let mut txn = doc.transact_mut();
        let out = txn
            .node_mut("map")
            .unwrap()
            .insert_attr("map", In::Node(Delta::new()));
        let Out::Node(nested) = out else {
            panic!("expected a nested node")
        };
        nested.id
    };
    let nested2 = {
        let mut txn = doc.transact_mut();
        let out = txn
            .node_mut(nested.clone())
            .unwrap()
            .insert_attr("array", In::Node(Delta::new()));
        let Out::Node(nested2) = out else {
            panic!("expected a nested node")
        };
        nested2.id
    };
    {
        let mut txn = doc.transact_mut();
        txn.node_mut(nested2).unwrap().insert(0, "content");
    }

    let nested_text = {
        let mut txn = doc.transact_mut();
        let out = txn
            .node_mut(nested.clone())
            .unwrap()
            .insert_attr("text", In::Node(Delta::new().insert_text("text")));
        let Out::Node(nested_text) = out else {
            panic!("expected a nested node")
        };
        nested_text.id
    };
    {
        let mut txn = doc.transact_mut();
        txn.node_mut(nested_text).unwrap().push_text("!");
    }

    assert_eq!(calls.load(Ordering::Relaxed), 5);
    let actual = paths.lock().unwrap();
    assert_eq!(
        actual.as_slice(),
        &[
            vec![Path::from(vec![])],
            vec![Path::from(vec![PathSegment::Key("map".into())])],
            vec![Path::from(vec![
                PathSegment::Key("map".into()),
                PathSegment::Key("array".into())
            ])],
            vec![Path::from(vec![PathSegment::Key("map".into()),])],
            vec![Path::from(vec![
                PathSegment::Key("map".into()),
                PathSegment::Key("text".into()),
            ])],
        ]
    );
}

#[test]
fn try_update() {
    let mut doc = Doc::new();
    let mut txn = doc.transact_mut();
    let mut map = txn.node_mut("map").unwrap();

    assert!(map.try_update("key", 1), "new entry");
    assert_eq!(map.attr("key"), Some(Out::from(1)));

    assert!(
        !map.try_update(&mut txn, "key", 1),
        "unchanged entry shouldn't trigger update"
    );
    assert_eq!(map.attr("key"), Some(Out::from(1)));

    assert!(map.try_update("key", 2), "entry should change");
    assert_eq!(map.attr("key"), Some(Out::from(2)));

    map.remove_attr("key");
    assert!(
        map.try_update("key", 2),
        "removed entry should trigger update"
    );
    assert_eq!(map.attr("key"), Some(Out::from(2)));
}

#[test]
fn get_as() {
    use serde::Deserialize;
    // TODO(unified-api): Map::get_as (serde deserialization) has no NodeRef equivalent

    #[derive(Debug, PartialEq, Deserialize)]
    struct Order {
        shipment_address: String,
        items: HashMap<String, OrderItem>,
        #[serde(default)]
        comment: Option<String>,
    }

    #[derive(Debug, PartialEq, Deserialize)]
    struct OrderItem {
        name: String,
        price: f64,
        quantity: u32,
    }

    let mut doc = Doc::new();
    let mut txn = doc.transact_mut();
    let mut map = txn.node_mut("map").unwrap();

    map.insert_attr(
        "orders",
        Delta::new().insert(In::from(
            Delta::new()
                .insert_attr("shipment_address", In::from("123 Main St"))
                .insert_attr(
                    "items",
                    In::from(
                        Delta::new()
                            .insert_attr(
                                "item1",
                                In::from(
                                    Delta::new()
                                        .insert_attr("name", In::from("item1"))
                                        .insert_attr("price", In::from(1.99))
                                        .insert_attr("quantity", In::from(2)),
                                ),
                            )
                            .insert_attr(
                                "item2",
                                In::from(
                                    Delta::new()
                                        .insert_attr("name", In::from("item2"))
                                        .insert_attr("price", In::from(2.99))
                                        .insert_attr("quantity", In::from(1)),
                                ),
                            ),
                    ),
                ),
        )),
    );

    let expected = Order {
        comment: None,
        shipment_address: "123 Main St".to_string(),
        items: HashMap::from([
            (
                "item1".to_string(),
                OrderItem {
                    name: "item1".to_string(),
                    price: 1.99,
                    quantity: 2,
                },
            ),
            (
                "item2".to_string(),
                OrderItem {
                    name: "item2".to_string(),
                    price: 2.99,
                    quantity: 1,
                },
            ),
        ]),
    };

    let actual: Vec<Order> = map.get_as(&txn, "orders").unwrap();
    assert_eq!(actual, vec![expected]);
}

#[test]
#[cfg(feature = "sync")]
fn multi_threading() {
    use std::sync::{Arc, RwLock};
    use std::thread::{sleep, spawn};
    use std::time::Duration;

    let doc = Arc::new(RwLock::new(Doc::with_client_id(1)));

    let d2 = doc.clone();
    let h2 = spawn(move || {
        for _ in 0..10 {
            let millis = fastrand::u64(1..20);
            sleep(Duration::from_millis(millis));

            let mut doc = d2.write().unwrap();
            let mut txn = doc.transact_mut();
            txn.node_mut("test").unwrap().insert_attr("key", 1);
        }
    });

    let d3 = doc.clone();
    let h3 = spawn(move || {
        for _ in 0..10 {
            let millis = fastrand::u64(1..20);
            sleep(Duration::from_millis(millis));

            let mut doc = d3.write().unwrap();
            let mut txn = doc.transact_mut();
            txn.node_mut("test").unwrap().insert_attr("key", 2);
        }
    });

    h3.join().unwrap();
    h2.join().unwrap();

    let doc = doc.write().unwrap();
    let txn = doc.transact();
    let value = txn.node("test").unwrap().attr("key").unwrap();

    assert!(value == Out::from(1) || value == Out::from(2))
}

#[test]
fn test_delete_not_applied_map() {
    // -- Setup: Doc A creates initial state, Doc B clones via update --
    let mut doc_a = Doc::new();
    let mut doc_b = Doc::new();

    // Doc A: create root Map with nested sub-Map
    {
        let mut txn = doc_a.transact_mut();
        let mut root_a = txn.node_mut("root").unwrap();
        root_a.insert_attr("sub", In::Node(Delta::new())); // { sub: {} }
    }

    // Clone to Doc B
    doc_b
        .transact_mut()
        .apply_update(
            Update::decode_v1(&doc_a.transact().encode_diff_v1(&StateVector::default())).unwrap(),
        )
        .unwrap();

    let sv_a = doc_a.transact().state_vector();
    let sv_b = doc_b.transact().state_vector();

    // -- Step 1: Doc B writes into the sub-Map, syncs to Doc A --
    {
        let mut txn = doc_b.transact_mut();
        let Out::Node(sub) = txn.node("root").unwrap().attr("sub").unwrap() else {
            panic!("expected a nested node")
        };
        txn.node_mut(sub.id).unwrap().insert_attr("x", 1i64); // { sub: { x: 1 } }
    }

    doc_a
        .transact_mut()
        .apply_update(Update::decode_v1(&doc_b.transact().encode_diff_v1(&sv_a)).unwrap())
        .unwrap();
    let sv_a = doc_a.transact().state_vector();
    // Save sv_b before it gets shadowed — we need it later for step 3
    let sv_b_saved = sv_b;

    // -- Step 2: Doc B adds a new key AND deletes the sub-Map --
    {
        let mut txn = doc_b.transact_mut();
        let mut root_b = txn.node_mut("root").unwrap();
        root_b.insert_attr("key", "value"); // { sub: { x: 1 }, key: 'value' }
        root_b.remove_attr("sub"); // { key: 'value' }
    }

    doc_a
        .transact_mut()
        .apply_update(Update::decode_v1(&doc_b.transact().encode_diff_v1(&sv_a)).unwrap())
        .unwrap();

    // At this point both docs should agree: root = {"key": "value"}
    {
        let tx_a = doc_a.transact();
        let tx_b = doc_b.transact();
        let (root_a, root_b) = (tx_a.node("root").unwrap(), tx_b.node("root").unwrap());
        let keys_a: Vec<_> = root_a.attr_keys().collect();
        let keys_b: Vec<_> = root_b.attr_keys().collect();
        assert_eq!(keys_a, vec!["key"]);
        assert_eq!(keys_b, vec!["key"]);
    }

    // -- Step 3: Doc A deletes the key, syncs to Doc B --
    // Use sv_b_saved (from before step 2) to match the Python reproduction exactly:
    // Python uses sv_b captured after step 1, before step 2 operations.
    {
        let mut txn = doc_a.transact_mut();
        txn.node_mut("root").unwrap().remove_attr("key");
    }

    doc_b
        .transact_mut()
        .apply_update(Update::decode_v1(&doc_a.transact().encode_diff_v1(&sv_b_saved)).unwrap())
        .unwrap();

    // -- Verify convergence --
    let tx_a = doc_a.transact();
    let tx_b = doc_b.transact();
    let (root_a, root_b) = (tx_a.node("root").unwrap(), tx_b.node("root").unwrap());
    let keys_a: Vec<_> = root_a.attr_keys().collect();
    let keys_b: Vec<_> = root_b.attr_keys().collect();
    assert_eq!(keys_a, keys_b);
}
