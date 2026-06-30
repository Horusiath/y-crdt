use arc_swap::ArcSwapOption;
use fastrand::Rng;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use yrs::updates::decoder::Decode;
use yrs::{Any, Doc, Out, StateVector, Update, any};

#[test]
fn map_basic() {
    let mut d1 = Doc::with_client_id(1);
    let mut t1 = d1.transact_mut();
    let mut m1 = t1.node_mut("map");

    let mut d2 = Doc::with_client_id(2);
    let mut t2 = d2.transact_mut();
    let mut m2 = t2.node_mut("map");

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
    fn compare_all<D: std::ops::Deref<Target = Doc>>(m: &MapRef, txn: &Transaction<D>) {
        assert_eq!(m.len(txn), 5);
        assert_eq!(m.get(txn, &"number".to_owned()), Some(Out::from(1f64)));
        assert_eq!(m.get(txn, &"boolean0".to_owned()), Some(Out::from(false)));
        assert_eq!(m.get(txn, &"boolean1".to_owned()), Some(Out::from(true)));
        assert_eq!(m.get(txn, &"string".to_owned()), Some(Out::from("hello Y")));
        assert_eq!(
            m.get(txn, &"object".to_owned()),
            Some(Out::from(any!({
                "key": {
                    "key2": "value"
                }
            })))
        );
    }

    compare_all(&m1, &t1);

    let update = t1.encode_state_as_update_v1(&StateVector::default());
    t2.apply_update(Update::decode_v1(update.as_slice()).unwrap())
        .unwrap();

    compare_all(&m2, &t2);
}

#[test]
fn map_get_set() {
    let mut d1 = Doc::with_client_id(1);
    let mut t1 = d1.transact_mut();
    let mut m1 = t1.node_mut("map");

    m1.insert_attr("stuff", "stuffy");
    m1.insert_attr("null", None as Option<String>);

    let update = t1.encode_state_as_update_v1(&StateVector::default());

    let mut d2 = Doc::with_client_id(2);
    let m2 = d2.get_or_insert_map("map");
    let mut t2 = d2.transact_mut();

    t2.apply_update(Update::decode_v1(update.as_slice()).unwrap())
        .unwrap();

    assert_eq!(m2.get(&t2, &"stuff".to_owned()), Some(Out::from("stuffy")));
    assert_eq!(m2.get(&t2, &"null".to_owned()), Some(Out::Any(Any::Null)));
}

#[test]
fn map_get_set_sync_with_conflicts() {
    let mut d1 = Doc::with_client_id(1);
    let m1 = d1.get_or_insert_map("map");
    let mut t1 = d1.transact_mut();

    let mut d2 = Doc::with_client_id(2);
    let m2 = d2.get_or_insert_map("map");
    let mut t2 = d2.transact_mut();

    m1.insert(&mut t1, "stuff".to_owned(), "c0");
    m2.insert(&mut t2, "stuff".to_owned(), "c1");

    let u1 = t1.encode_state_as_update_v1(&StateVector::default());
    let u2 = t2.encode_state_as_update_v1(&StateVector::default());

    t1.apply_update(Update::decode_v1(u2.as_slice()).unwrap())
        .unwrap();
    t2.apply_update(Update::decode_v1(u1.as_slice()).unwrap())
        .unwrap();

    assert_eq!(m1.get(&t1, &"stuff".to_owned()), Some(Out::from("c1")));
    assert_eq!(m2.get(&t2, &"stuff".to_owned()), Some(Out::from("c1")));
}

#[test]
fn map_len_remove() {
    let mut d1 = Doc::with_client_id(1);
    let m1 = d1.get_or_insert_map("map");
    let mut t1 = d1.transact_mut();

    let key1 = "stuff".to_owned();
    let key2 = "other-stuff".to_owned();

    m1.insert(&mut t1, key1.clone(), "c0");
    m1.insert(&mut t1, key2.clone(), "c1");
    assert_eq!(m1.len(&t1), 2);

    // remove 'stuff'
    assert_eq!(m1.remove(&mut t1, &key1), Some(Out::from("c0")));
    assert_eq!(m1.len(&t1), 1);

    // remove 'stuff' again - nothing should happen
    assert_eq!(m1.remove(&mut t1, &key1), None);
    assert_eq!(m1.len(&t1), 1);

    // remove 'other-stuff'
    assert_eq!(m1.remove(&mut t1, &key2), Some(Out::from("c1")));
    assert_eq!(m1.len(&t1), 0);
}

#[test]
fn map_clear() {
    let mut d1 = Doc::with_client_id(1);
    let m1 = d1.get_or_insert_map("map");
    let mut t1 = d1.transact_mut();

    m1.insert(&mut t1, "key1".to_owned(), "c0");
    m1.insert(&mut t1, "key2".to_owned(), "c1");
    m1.clear(&mut t1);

    assert_eq!(m1.len(&t1), 0);
    assert_eq!(m1.get(&t1, &"key1".to_owned()), None);
    assert_eq!(m1.get(&t1, &"key2".to_owned()), None);

    let mut d2 = Doc::with_client_id(2);
    let m2 = d2.get_or_insert_map("map");
    let mut t2 = d2.transact_mut();

    let u1 = t1.encode_state_as_update_v1(&StateVector::default());
    t2.apply_update(Update::decode_v1(u1.as_slice()).unwrap())
        .unwrap();

    assert_eq!(m2.len(&t2), 0);
    assert_eq!(m2.get(&t2, &"key1".to_owned()), None);
    assert_eq!(m2.get(&t2, &"key2".to_owned()), None);
}

#[test]
fn map_clear_sync() {
    let mut d1 = Doc::with_client_id(1);
    let mut d2 = Doc::with_client_id(2);
    let mut d3 = Doc::with_client_id(3);
    let mut d4 = Doc::with_client_id(4);

    {
        let m1 = d1.get_or_insert_map("map");
        let m2 = d2.get_or_insert_map("map");
        let m3 = d3.get_or_insert_map("map");

        let mut t1 = d1.transact_mut();
        let mut t2 = d2.transact_mut();
        let mut t3 = d3.transact_mut();

        m1.insert(&mut t1, "key1".to_owned(), "c0");
        m2.insert(&mut t2, "key1".to_owned(), "c1");
        m2.insert(&mut t2, "key1".to_owned(), "c2");
        m3.insert(&mut t3, "key1".to_owned(), "c3");
    }

    exchange_updates(&mut [&mut d1, &mut d2, &mut d3, &mut d4]);

    {
        let m1 = d1.get_or_insert_map("map");
        let m2 = d2.get_or_insert_map("map");
        let m3 = d3.get_or_insert_map("map");

        let mut t1 = d1.transact_mut();
        let mut t2 = d2.transact_mut();
        let mut t3 = d3.transact_mut();

        m1.insert(&mut t1, "key2".to_owned(), "c0");
        m2.insert(&mut t2, "key2".to_owned(), "c1");
        m2.insert(&mut t2, "key2".to_owned(), "c2");
        m3.insert(&mut t3, "key2".to_owned(), "c3");
        m3.clear(&mut t3);
    }

    exchange_updates(&mut [&mut d1, &mut d2, &mut d3, &mut d4]);

    for mut doc in [d1, d2, d3, d4] {
        let map = doc.get_or_insert_map("map");

        assert_eq!(
            map.get(&doc.transact(), &"key1".to_owned()),
            None,
            "'key1' entry for peer {} should be removed",
            doc.client_id()
        );
        assert_eq!(
            map.get(&doc.transact(), &"key2".to_owned()),
            None,
            "'key2' entry for peer {} should be removed",
            doc.client_id()
        );
        assert_eq!(
            map.len(&doc.transact()),
            0,
            "all entries for peer {} should be removed",
            doc.client_id()
        );
    }
}

#[test]
fn map_get_set_with_3_way_conflicts() {
    let mut d1 = Doc::with_client_id(1);
    let mut d2 = Doc::with_client_id(2);
    let mut d3 = Doc::with_client_id(3);

    {
        let m1 = d1.get_or_insert_map("map");
        let m2 = d2.get_or_insert_map("map");
        let m3 = d3.get_or_insert_map("map");

        let mut t1 = d1.transact_mut();
        let mut t2 = d2.transact_mut();
        let mut t3 = d3.transact_mut();

        m1.insert(&mut t1, "stuff".to_owned(), "c0");
        m2.insert(&mut t2, "stuff".to_owned(), "c1");
        m2.insert(&mut t2, "stuff".to_owned(), "c2");
        m3.insert(&mut t3, "stuff".to_owned(), "c3");
    }

    exchange_updates(&mut [&mut d1, &mut d2, &mut d3]);

    for mut doc in [d1, d2, d3] {
        let map = doc.get_or_insert_map("map");

        assert_eq!(
            map.get(&doc.transact(), &"stuff".to_owned()),
            Some(Out::from("c3")),
            "peer {} - map entry resolved to unexpected value",
            doc.client_id()
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
        let m1 = d1.get_or_insert_map("map");
        let m2 = d2.get_or_insert_map("map");
        let m3 = d3.get_or_insert_map("map");

        let mut t1 = d1.transact_mut();
        let mut t2 = d2.transact_mut();
        let mut t3 = d3.transact_mut();

        m1.insert(&mut t1, "key1".to_owned(), "c0");
        m2.insert(&mut t2, "key1".to_owned(), "c1");
        m2.insert(&mut t2, "key1".to_owned(), "c2");
        m3.insert(&mut t3, "key1".to_owned(), "c3");
    }

    exchange_updates(&mut [&mut d1, &mut d2, &mut d3, &mut d4]);

    {
        let m1 = d1.get_or_insert_map("map");
        let m2 = d2.get_or_insert_map("map");
        let m3 = d3.get_or_insert_map("map");
        let m4 = d4.get_or_insert_map("map");

        let mut t1 = d1.transact_mut();
        let mut t2 = d2.transact_mut();
        let mut t3 = d3.transact_mut();
        let mut t4 = d4.transact_mut();

        m1.insert(&mut t1, "key1".to_owned(), "deleteme");
        m2.insert(&mut t2, "key1".to_owned(), "c1");
        m3.insert(&mut t3, "key1".to_owned(), "c2");
        m4.insert(&mut t4, "key1".to_owned(), "c3");
        m4.remove(&mut t4, &"key1".to_owned());
    }

    exchange_updates(&mut [&mut d1, &mut d2, &mut d3, &mut d4]);

    for mut doc in [d1, d2, d3, d4] {
        let map = doc.get_or_insert_map("map");

        assert_eq!(
            map.get(&doc.transact(), &"key1".to_owned()),
            None,
            "entry 'key1' on peer {} should be removed",
            doc.client_id()
        );
    }
}

#[test]
fn insert_and_remove_events() {
    let mut d1 = Doc::with_client_id(1);
    let m1 = d1.get_or_insert_map("map");

    let entries = Arc::new(ArcSwapOption::default());
    let entries_c = entries.clone();
    let _sub = m1.observe(move |txn, e| {
        let keys = e.keys(txn);
        entries_c.store(Some(Arc::new(keys.clone())));
    });

    // insert new entry
    {
        let mut txn = d1.transact_mut();
        m1.insert(&mut txn, "a", 1);
        // txn is committed at the end of this scope
    }
    assert_eq!(
        entries.swap(None),
        Some(Arc::new(HashMap::from([(
            "a".into(),
            EntryChange::Inserted(Any::Number(1.0).into())
        )])))
    );

    // update existing entry once
    {
        let mut txn = d1.transact_mut();
        m1.insert(&mut txn, "a", 2);
    }
    assert_eq!(
        entries.swap(None),
        Some(Arc::new(HashMap::from([(
            "a".into(),
            EntryChange::Updated(Any::Number(1.0).into(), Any::Number(2.0).into())
        )])))
    );

    // update existing entry twice
    {
        let mut txn = d1.transact_mut();
        m1.insert(&mut txn, "a", 3);
        m1.insert(&mut txn, "a", 4);
    }
    assert_eq!(
        entries.swap(None),
        Some(Arc::new(HashMap::from([(
            "a".into(),
            EntryChange::Updated(Any::Number(2.0).into(), Any::Number(4.0).into())
        )])))
    );

    // remove existing entry
    {
        let mut txn = d1.transact_mut();
        m1.remove(&mut txn, "a");
    }
    assert_eq!(
        entries.swap(None),
        Some(Arc::new(HashMap::from([(
            "a".into(),
            EntryChange::Removed(Any::Number(4.0).into())
        )])))
    );

    // add another entry and update it
    {
        let mut txn = d1.transact_mut();
        m1.insert(&mut txn, "b", 1);
        m1.insert(&mut txn, "b", 2);
    }
    assert_eq!(
        entries.swap(None),
        Some(Arc::new(HashMap::from([(
            "b".into(),
            EntryChange::Inserted(Any::Number(2.0).into())
        )])))
    );

    // add and remove an entry
    {
        let mut txn = d1.transact_mut();
        m1.insert(&mut txn, "c", 1);
        m1.remove(&mut txn, "c");
    }
    assert_eq!(entries.swap(None), Some(HashMap::new().into()));

    // copy updates over
    let mut d2 = Doc::with_client_id(2);
    let m2 = d2.get_or_insert_map("map");

    let entries = Arc::new(ArcSwapOption::default());
    let entries_c = entries.clone();
    let _sub = m2.observe(move |txn, e| {
        let keys = e.keys(txn);
        entries_c.store(Some(Arc::new(keys.clone())));
    });

    {
        let t1 = d1.transact_mut();
        let mut t2 = d2.transact_mut();

        let sv = t2.state_vector();
        let mut encoder = EncoderV1::new();
        t1.encode_diff(&sv, &mut encoder);
        t2.apply_update(Update::decode_v1(encoder.to_vec().as_slice()).unwrap())
            .unwrap();
    }
    assert_eq!(
        entries.swap(None),
        Some(Arc::new(HashMap::from([(
            "b".into(),
            EntryChange::Inserted(Any::Number(2.0).into())
        )])))
    );
}

fn map_transactions() -> [Box<dyn Fn(&mut Doc, &mut Rng)>; 3] {
    fn set(doc: &mut Doc, rng: &mut Rng) {
        let map = doc.get_or_insert_map("map");
        let mut txn = doc.transact_mut();
        let key = rng.choice(["one", "two"]).unwrap();
        let value: String = rng.random_string();
        map.insert(&mut txn, key.to_string(), value);
    }

    fn set_type(doc: &mut Doc, rng: &mut Rng) {
        let map = doc.get_or_insert_map("map");
        let mut txn = doc.transact_mut();
        let key = rng.choice(["one", "two", "three"]).unwrap();
        if rng.f32() <= 0.33 {
            map.insert(
                &mut txn,
                key.to_string(),
                ArrayPrelim::from(vec![1, 2, 3, 4]),
            );
        } else if rng.f32() <= 0.33 {
            map.insert(&mut txn, key.to_string(), TextPrelim::new("deeptext"));
        } else {
            map.insert(
                &mut txn,
                key.to_string(),
                MapPrelim::from([("deepkey".to_owned(), "deepvalue")]),
            );
        }
    }

    fn delete(doc: &mut Doc, rng: &mut Rng) {
        let map = doc.get_or_insert_map("map");
        let mut txn = doc.transact_mut();
        let key = rng.choice(["one", "two"]).unwrap();
        map.remove(&mut txn, key);
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
    let map = doc.get_or_insert_map("map");

    let paths = Arc::new(Mutex::new(vec![]));
    let calls = Arc::new(AtomicU32::new(0));
    let paths_copy = paths.clone();
    let calls_copy = calls.clone();
    let _sub = map.observe_deep(move |_txn, e| {
        let path: Vec<Path> = e.iter().map(Event::path).collect();
        paths_copy.lock().unwrap().push(path);
        calls_copy.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    });

    let nested = map.insert(&mut doc.transact_mut(), "map", MapPrelim::default());
    nested.insert(
        &mut doc.transact_mut(),
        "array",
        ArrayPrelim::from(Vec::<String>::default()),
    );
    let nested2 = nested
        .get(&doc.transact(), "array")
        .unwrap()
        .cast::<ArrayRef>()
        .unwrap();
    nested2.insert(&mut doc.transact_mut(), 0, "content");

    let nested_text = nested.insert(&mut doc.transact_mut(), "text", TextPrelim::new("text"));
    nested_text.push(&mut doc.transact_mut(), "!");

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
fn get_or_init() {
    let mut doc = Doc::with_client_id(1);
    let mut txn = doc.transact_mut();
    let map = txn.get_or_insert_map("map");

    let m: MapRef = map.get_or_init(&mut txn, "nested");
    m.insert(&mut txn, "key", 1);
    let m: MapRef = map.get_or_init(&mut txn, "nested");
    assert_eq!(m.get(&txn, "key"), Some(Out::from(1)));

    let m: ArrayRef = map.get_or_init(&mut txn, "nested");
    m.insert(&mut txn, 0, 1);
    let m: ArrayRef = map.get_or_init(&mut txn, "nested");
    assert_eq!(m.get(&txn, 0), Some(Out::from(1)));

    let m: TextRef = map.get_or_init(&mut txn, "nested");
    m.insert(&mut txn, 0, "a");
    let m: TextRef = map.get_or_init(&mut txn, "nested");
    assert_eq!(m.get_string(&txn), "a".to_string());

    let m: XmlFragmentRef = map.get_or_init(&mut txn, "nested");
    m.insert(&mut txn, 0, XmlTextPrelim::new("b"));
    let m: XmlFragmentRef = map.get_or_init(&mut txn, "nested");
    assert_eq!(m.get_string(&txn), "b".to_string());

    let m: XmlTextRef = map.get_or_init(&mut txn, "nested");
    m.insert(&mut txn, 0, "c");
    let m: XmlTextRef = map.get_or_init(&mut txn, "nested");
    assert_eq!(m.get_string(&txn), "c".to_string());
}

#[test]
fn try_update() {
    let mut doc = Doc::new();
    let mut txn = doc.transact_mut();
    let map = txn.get_or_insert_map("map");

    assert!(map.try_update(&mut txn, "key", 1), "new entry");
    assert_eq!(map.get(&txn, "key"), Some(Out::from(1)));

    assert!(
        !map.try_update(&mut txn, "key", 1),
        "unchanged entry shouldn't trigger update"
    );
    assert_eq!(map.get(&txn, "key"), Some(Out::from(1)));

    assert!(map.try_update(&mut txn, "key", 2), "entry should change");
    assert_eq!(map.get(&txn, "key"), Some(Out::from(2)));

    map.remove(&mut txn, "key");
    assert!(
        map.try_update(&mut txn, "key", 2),
        "removed entry should trigger update"
    );
    assert_eq!(map.get(&txn, "key"), Some(Out::from(2)));
}

#[test]
fn get_as() {
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
    let map = txn.get_or_insert_map("map");

    map.insert(
        &mut txn,
        "orders",
        ArrayPrelim::from([In::from(MapPrelim::from([
            ("shipment_address", In::from("123 Main St")),
            (
                "items",
                In::from(MapPrelim::from([
                    (
                        "item1",
                        In::from(MapPrelim::from([
                            ("name", In::from("item1")),
                            ("price", In::from(1.99)),
                            ("quantity", In::from(2)),
                        ])),
                    ),
                    (
                        "item2",
                        In::from(MapPrelim::from([
                            ("name", In::from("item2")),
                            ("price", In::from(2.99)),
                            ("quantity", In::from(1)),
                        ])),
                    ),
                ])),
            ),
        ]))]),
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

    let doc = Arc::new(RwLock::new(Doc::with_client_id(1)));

    let d2 = doc.clone();
    let h2 = spawn(move || {
        for _ in 0..10 {
            let millis = fastrand::u64(1..20);
            sleep(Duration::from_millis(millis));

            let mut doc = d2.write().unwrap();
            let map = doc.get_or_insert_map("test");
            let mut txn = doc.transact_mut();
            map.insert(&mut txn, "key", 1);
        }
    });

    let d3 = doc.clone();
    let h3 = spawn(move || {
        for _ in 0..10 {
            let millis = fastrand::u64(1..20);
            sleep(Duration::from_millis(millis));

            let mut doc = d3.write().unwrap();
            let map = doc.get_or_insert_map("test");
            let mut txn = doc.transact_mut();
            map.insert(&mut txn, "key", 2);
        }
    });

    h3.join().unwrap();
    h2.join().unwrap();

    let mut doc = doc.write().unwrap();
    let map = doc.get_or_insert_map("test");
    let txn = doc.transact();
    let value = map.get(&txn, "key").unwrap().to_json(&txn);

    assert!(value == 1.into() || value == 2.into())
}

#[test]
fn test_delete_not_applied_map() {
    // -- Setup: Doc A creates initial state, Doc B clones via update --
    let mut doc_a = Doc::new();
    let root_a = doc_a.get_or_insert_map("root");
    let mut doc_b = Doc::new();
    let root_b = doc_b.get_or_insert_map("root");

    // Doc A: create root Map with nested sub-Map
    {
        let root_a = doc_a.get_or_insert_map("root");
        let mut txn = doc_a.transact_mut();
        root_a.insert(&mut txn, "sub", MapPrelim::default()); // { sub: {} }
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
        let root_b = doc_b.get_or_insert_map("root");
        let mut txn = doc_b.transact_mut();
        let sub: MapRef = root_b.get(&txn, "sub").unwrap().cast().unwrap();
        sub.insert(&mut txn, "x", 1i64); // { sub: { x: 1 } }
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
        let root_b = doc_b.get_or_insert_map("root");
        let mut txn = doc_b.transact_mut();
        root_b.insert(&mut txn, "key", "value"); // { sub: { x: 1 }, key: 'value' }
        root_b.remove(&mut txn, "sub"); // { key: 'value' }
    }

    doc_a
        .transact_mut()
        .apply_update(Update::decode_v1(&doc_b.transact().encode_diff_v1(&sv_a)).unwrap())
        .unwrap();

    // At this point both docs should agree: root = {"key": "value"}
    {
        let tx_a = doc_a.transact();
        let tx_b = doc_b.transact();
        let keys_a: Vec<_> = root_a.keys(&tx_a).collect();
        let keys_b: Vec<_> = root_b.keys(&tx_b).collect();
        assert_eq!(keys_a, vec!["key"]);
        assert_eq!(keys_b, vec!["key"]);
    }

    // -- Step 3: Doc A deletes the key, syncs to Doc B --
    // Use sv_b_saved (from before step 2) to match the Python reproduction exactly:
    // Python uses sv_b captured after step 1, before step 2 operations.
    {
        let root_a = doc_a.get_or_insert_map("root");
        let mut txn = doc_a.transact_mut();
        root_a.remove(&mut txn, "key");
    }

    doc_b
        .transact_mut()
        .apply_update(Update::decode_v1(&doc_a.transact().encode_diff_v1(&sv_b_saved)).unwrap())
        .unwrap();

    // -- Verify convergence --
    let tx_a = doc_a.transact();
    let tx_b = doc_b.transact();
    let keys_a: Vec<_> = root_a.keys(&tx_a).collect();
    let keys_b: Vec<_> = root_b.keys(&tx_b).collect();
    assert_eq!(keys_a, keys_b);
}
