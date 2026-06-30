use std::collections::{HashMap, HashSet};
use std::iter::FromIterator;
use std::sync::{Arc, Mutex};

#[test]
fn push_back() {
    let mut doc = Doc::with_client_id(1);
    let mut txn = doc.transact_mut();
    let mut a = txn.node_mut("array").unwrap();

    a.push_back("a");
    a.push_back("b");
    a.push_back("c");

    let actual: Vec<_> = a.iter().collect();
    assert_eq!(actual, vec!["a".into(), "b".into(), "c".into()]);
}

#[test]
fn push_front() {
    let mut doc = Doc::with_client_id(1);
    let mut txn = doc.transact_mut();
    let mut a = txn.node_mut("array");

    a.push_front("c");
    a.push_front("b");
    a.push_front("a");

    let actual: Vec<_> = a.iter().collect();
    assert_eq!(actual, vec!["a".into(), "b".into(), "c".into()]);
}

#[test]
fn insert() {
    let mut doc = Doc::with_client_id(1);
    let mut txn = doc.transact_mut();
    let mut a = txn.node_mut("array").unwrap();

    a.insert(0, "a");
    a.insert(1, "c");
    a.insert(1, "b");

    let actual: Vec<_> = a.iter().collect();
    assert_eq!(actual, vec!["a".into(), "b".into(), "c".into()]);
}

#[test]
fn basic() {
    let mut d1 = Doc::with_client_id(1);
    let mut d2 = Doc::with_client_id(2);

    d1.transact_mut().node_mut("array").unwrap().insert(0, "Hi");

    let update = d1
        .transact()
        .encode_state_as_update_v1(&StateVector::default());

    let mut t2 = d2.transact_mut();
    t2.apply_update(Update::decode_v1(update.as_slice()).unwrap())
        .unwrap();
    let a2 = t2.node_mut("array").unwrap();
    let actual: Vec<_> = a2.iter().collect();

    assert_eq!(actual, vec!["Hi".into()]);
}

#[test]
fn len() {
    let mut d = Doc::with_client_id(1);

    {
        let mut txn = d.transact_mut();
        let mut a = txn.node_mut("array").unwrap();

        a.push_back(0); // len: 1
        a.push_back(1); // len: 2
        a.push_back(2); // len: 3
        a.push_back(3); // len: 4

        a.remove_range(0, 1); // len: 3
        a.insert(0, 0); // len: 4

        assert_eq!(a.len(), 4);
    }
    {
        let mut txn = d.transact_mut();
        let mut a = txn.node_mut("array").unwrap();
        a.remove_range(1, 1); // len: 3
        assert_eq!(a.len(), 3);

        a.insert(1, 1); // len: 4
        assert_eq!(a.len(), 4);

        a.remove_range(2, 1); // len: 3
        assert_eq!(a.len(), 3);

        a.insert(2, 2); // len: 4
        assert_eq!(a.len(), 4);
    }

    let mut txn = d.transact_mut();
    let mut a = txn.node_mut("array").unwrap();
    assert_eq!(a.len(), 4);

    a.remove_range(1, 1);
    assert_eq!(a.len(), 3);

    a.insert(1, 1);
    assert_eq!(a.len(), 4);
}

#[test]
fn remove_insert() {
    let mut d1 = Doc::with_client_id(1);
    let mut t1 = d1.transact_mut();
    let mut a1 = t1.node_mut("array").unwrap();

    a1.insert(0, "A");
    a1.remove(1, 0);
}

#[test]
fn insert_3_elements_try_re_get() {
    let mut d1 = Doc::with_client_id(1);
    let mut d2 = Doc::with_client_id(2);
    {
        let mut t1 = d1.transact_mut();
        let mut a1 = t1.node_mut("array").unwrap();

        a1.push_back(1);
        a1.push_back(true);
        a1.push_back(false);
        let actual: Vec<_> = a1.iter().collect();
        assert_eq!(
            actual,
            vec![Out::from(1.0), Out::from(true), Out::from(false)]
        );
    }

    exchange_updates(&mut [&mut d1, &mut d2]);

    let mut t2 = d2.transact();
    let mut a2 = t2.node_mut("array").unwrap();
    let actual: Vec<_> = a2.iter(&t2).collect();
    assert_eq!(
        actual,
        vec![Out::from(1.0), Out::from(true), Out::from(false)]
    );
}

#[test]
fn concurrent_insert_with_3_conflicts() {
    let mut d1 = Doc::with_client_id(1);
    let a = d1.get_or_insert_array("array");
    {
        let mut txn = d1.transact_mut();
        a.insert(&mut txn, 0, 0);
    }

    let mut d2 = Doc::with_client_id(2);
    {
        let mut txn = d1.transact_mut();
        a.insert(&mut txn, 0, 1);
    }

    let mut d3 = Doc::with_client_id(3);
    {
        let mut txn = d1.transact_mut();
        a.insert(&mut txn, 0, 2);
    }

    exchange_updates(&mut [&mut d1, &mut d2, &mut d3]);

    let a1 = to_array(&mut d1);
    let a2 = to_array(&mut d2);
    let a3 = to_array(&mut d3);

    assert_eq!(a1, a2, "Peer 1 and peer 2 states are different");
    assert_eq!(a2, a3, "Peer 2 and peer 3 states are different");
}

fn to_array(d: &mut Doc) -> Vec<Out> {
    let a = d.get_or_insert_array("array");
    a.iter(&d.transact()).collect()
}

#[test]
fn concurrent_insert_remove_with_3_conflicts() {
    let mut d1 = Doc::with_client_id(1);
    {
        let a = d1.get_or_insert_array("array");
        let mut txn = d1.transact_mut();
        a.insert_range(&mut txn, 0, ["x", "y", "z"]);
    }
    let mut d2 = Doc::with_client_id(2);
    let mut d3 = Doc::with_client_id(3);

    exchange_updates(&mut [&mut d1, &mut d2, &mut d3]);

    {
        // start state: [x,y,z]
        let a1 = d1.get_or_insert_array("array");
        let a2 = d2.get_or_insert_array("array");
        let a3 = d3.get_or_insert_array("array");
        let mut t1 = d1.transact_mut();
        let mut t2 = d2.transact_mut();
        let mut t3 = d3.transact_mut();

        a1.insert(&mut t1, 1, 0); // [x,0,y,z]
        a2.remove_range(&mut t2, 0, 1); // [y,z]
        a2.remove_range(&mut t2, 1, 1); // [y]
        a3.insert(&mut t3, 1, 2); // [x,2,y,z]
    }

    exchange_updates(&mut [&mut d1, &mut d2, &mut d3]);
    // after exchange expected: [0,2,y]

    let a1 = to_array(&mut d1);
    let a2 = to_array(&mut d2);
    let a3 = to_array(&mut d3);

    assert_eq!(a1, a2, "Peer 1 and peer 2 states are different");
    assert_eq!(a2, a3, "Peer 2 and peer 3 states are different");
}

#[test]
fn insertions_in_late_sync() {
    let mut d1 = Doc::with_client_id(1);
    {
        let a = d1.get_or_insert_array("array");
        let mut txn = d1.transact_mut();
        a.push_back(&mut txn, "x");
        a.push_back(&mut txn, "y");
    }
    let mut d2 = Doc::with_client_id(2);
    let mut d3 = Doc::with_client_id(3);

    exchange_updates(&mut [&mut d1, &mut d2, &mut d3]);

    {
        let a1 = d1.get_or_insert_array("array");
        let a2 = d2.get_or_insert_array("array");
        let a3 = d3.get_or_insert_array("array");
        let mut t1 = d1.transact_mut();
        let mut t2 = d2.transact_mut();
        let mut t3 = d3.transact_mut();

        a1.insert(&mut t1, 1, "user0");
        a2.insert(&mut t2, 1, "user1");
        a3.insert(&mut t3, 1, "user2");
    }

    exchange_updates(&mut [&mut d1, &mut d2, &mut d3]);

    let a1 = to_array(&mut d1);
    let a2 = to_array(&mut d2);
    let a3 = to_array(&mut d3);

    assert_eq!(a1, a2, "Peer 1 and peer 2 states are different");
    assert_eq!(a2, a3, "Peer 2 and peer 3 states are different");
}

#[test]
fn removals_in_late_sync() {
    let mut d1 = Doc::with_client_id(1);
    {
        let a = d1.get_or_insert_array("array");
        let mut txn = d1.transact_mut();
        a.push_back(&mut txn, "x");
        a.push_back(&mut txn, "y");
    }
    let mut d2 = Doc::with_client_id(2);

    exchange_updates(&mut [&mut d1, &mut d2]);

    {
        let a1 = d1.get_or_insert_array("array");
        let a2 = d2.get_or_insert_array("array");
        let mut t1 = d1.transact_mut();
        let mut t2 = d2.transact_mut();

        a2.remove_range(&mut t2, 1, 1);
        a1.remove_range(&mut t1, 0, 2);
    }

    exchange_updates(&mut [&mut d1, &mut d2]);

    let a1 = to_array(&mut d1);
    let a2 = to_array(&mut d2);

    assert_eq!(a1, a2, "Peer 1 and peer 2 states are different");
}

#[test]
fn insert_then_merge_delete_on_sync() {
    let mut d1 = Doc::with_client_id(1);
    {
        let a = d1.get_or_insert_array("array");
        let mut txn = d1.transact_mut();
        a.push_back(&mut txn, "x");
        a.push_back(&mut txn, "y");
        a.push_back(&mut txn, "z");
    }
    let mut d2 = Doc::with_client_id(2);

    exchange_updates(&mut [&mut d1, &mut d2]);

    {
        let a2 = d2.get_or_insert_array("array");
        let mut t2 = d2.transact_mut();

        a2.remove_range(&mut t2, 0, 3);
    }

    exchange_updates(&mut [&mut d1, &mut d2]);

    let a1 = to_array(&mut d1);
    let a2 = to_array(&mut d2);

    assert_eq!(a1, a2, "Peer 1 and peer 2 states are different");
}

#[test]
fn iter_array_containing_types() {
    let mut d = Doc::with_client_id(1);
    let a = d.get_or_insert_array("arr");
    let mut txn = d.transact_mut();
    for i in 0..10 {
        let mut m = HashMap::new();
        m.insert("value".to_owned(), i);
        a.push_back(&mut txn, MapPrelim::from_iter(m));
    }

    for (i, value) in a.iter(&txn).enumerate() {
        match value {
            Out::YMap(_) => {
                assert_eq!(value.to_json(&txn), any!({"value": (i as f64) }))
            }
            _ => panic!("Value of array at index {} was no YMap", i),
        }
    }
}

#[test]
fn insert_and_remove_events() {
    let mut d = Doc::with_client_id(1);
    let array = d.get_or_insert_array("array");
    let happened = Arc::new(AtomicBool::new(false));
    let happened_clone = happened.clone();
    let _sub = array.observe(move |_, _| {
        happened_clone.store(true, Ordering::Relaxed);
    });

    {
        let mut txn = d.transact_mut();
        array.insert_range(&mut txn, 0, [0, 1, 2]);
        // txn is committed at the end of this scope
    }
    assert!(
        happened.swap(false, Ordering::Relaxed),
        "insert of [0,1,2] should trigger event"
    );

    {
        let mut txn = d.transact_mut();
        array.remove_range(&mut txn, 0, 1);
        // txn is committed at the end of this scope
    }
    assert!(
        happened.swap(false, Ordering::Relaxed),
        "removal of [0] should trigger event"
    );

    {
        let mut txn = d.transact_mut();
        array.remove_range(&mut txn, 0, 2);
        // txn is committed at the end of this scope
    }
    assert!(
        happened.swap(false, Ordering::Relaxed),
        "removal of [1,2] should trigger event"
    );
}

#[test]
fn insert_and_remove_event_changes() {
    let mut d1 = Doc::with_client_id(1);
    let array = d1.get_or_insert_array("array");
    let added = Arc::new(ArcSwapOption::default());
    let removed = Arc::new(ArcSwapOption::default());
    let delta = Arc::new(ArcSwapOption::default());

    let (added_c, removed_c, delta_c) = (added.clone(), removed.clone(), delta.clone());
    let _sub = array.observe(move |txn, e| {
        added_c.store(Some(Arc::new(e.inserts(txn).clone())));
        removed_c.store(Some(Arc::new(e.removes(txn).clone())));
        delta_c.store(Some(Arc::new(e.delta(txn).to_vec())));
    });

    {
        let mut txn = d1.transact_mut();
        array.push_back(&mut txn, 4);
        array.push_back(&mut txn, "dtrn");
        // txn is committed at the end of this scope
    }
    assert_eq!(
        added.swap(None),
        Some(HashSet::from([ID::new(ClientID::new(1), 0), ID::new(ClientID::new(1), 1)]).into())
    );
    assert_eq!(removed.swap(None), Some(HashSet::new().into()));
    assert_eq!(
        delta.swap(None),
        Some(
            vec![Change::Added(vec![
                Any::Number(4.0).into(),
                Any::String("dtrn".into()).into()
            ])]
            .into()
        )
    );

    {
        let mut txn = d1.transact_mut();
        array.remove_range(&mut txn, 0, 1);
    }
    assert_eq!(added.swap(None), Some(HashSet::new().into()));
    assert_eq!(
        removed.swap(None),
        Some(HashSet::from([ID::new(ClientID::new(1), 0)]).into())
    );
    assert_eq!(delta.swap(None), Some(vec![Change::Removed(1)].into()));

    {
        let mut txn = d1.transact_mut();
        array.insert(&mut txn, 1, 0.5);
    }
    assert_eq!(
        added.swap(None),
        Some(HashSet::from([ID::new(ClientID::new(1), 2)]).into())
    );
    assert_eq!(removed.swap(None), Some(HashSet::new().into()));
    assert_eq!(
        delta.swap(None),
        Some(
            vec![
                Change::Retain(1),
                Change::Added(vec![Any::Number(0.5).into()])
            ]
            .into()
        )
    );

    let mut d2 = Doc::with_client_id(2);
    let array2 = d2.get_or_insert_array("array");
    let (added_c, removed_c, delta_c) = (added.clone(), removed.clone(), delta.clone());
    let _sub = array2.observe(move |txn, e| {
        added_c.store(Some(e.inserts(txn).clone().into()));
        removed_c.store(Some(e.removes(txn).clone().into()));
        delta_c.store(Some(e.delta(txn).to_vec().into()));
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
        added.swap(None),
        Some(HashSet::from([ID::new(ClientID::new(1), 1)]).into())
    );
    assert_eq!(removed.swap(None), Some(HashSet::new().into()));
    assert_eq!(
        delta.swap(None),
        Some(
            vec![Change::Added(vec![
                Any::String("dtrn".into()).into(),
                Any::Number(0.5).into(),
            ])]
            .into()
        )
    );
}

#[test]
fn target_on_local_and_remote() {
    let mut d1 = Doc::with_client_id(1);
    let mut d2 = Doc::with_client_id(2);
    let a1 = d1.get_or_insert_array("array");
    let a2 = d2.get_or_insert_array("array");

    let c1 = Arc::new(ArcSwapOption::default());
    let c1c = c1.clone();
    let _s1 = a1.observe(move |_, e| {
        c1c.store(Some(e.target().hook().into()));
    });
    let c2 = Arc::new(ArcSwapOption::default());
    let c2c = c2.clone();
    let _s2 = a2.observe(move |_, e| {
        c2c.store(Some(e.target().hook().into()));
    });

    {
        let mut t1 = d1.transact_mut();
        a1.insert_range(&mut t1, 0, [1, 2]);
    }
    exchange_updates(&mut [&mut d1, &mut d2]);

    assert_eq!(c1.swap(None), Some(Arc::new(a1.hook())));
    assert_eq!(c2.swap(None), Some(Arc::new(a2.hook())));
}

use crate::updates::decoder::Decode;
use crate::updates::encoder::{Encoder, EncoderV1};
use arc_swap::ArcSwapOption;
use fastrand::Rng;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Duration;
use yrs::updates::decoder::Decode;
use yrs::{Doc, Out, StateVector, Update};

static UNIQUE_NUMBER: AtomicI64 = AtomicI64::new(0);

fn get_unique_number() -> i64 {
    UNIQUE_NUMBER.fetch_add(1, Ordering::SeqCst)
}

fn array_transactions() -> [Box<dyn Fn(&mut Doc, &mut Rng)>; 4] {
    fn insert(doc: &mut Doc, rng: &mut Rng) {
        let yarray = doc.get_or_insert_array("array");
        let mut txn = doc.transact_mut();
        let unique_number = get_unique_number();
        let len = rng.between(1, 4);
        let content: Vec<_> = (0..len)
            .into_iter()
            .map(|_| Any::BigInt(unique_number))
            .collect();
        let mut pos = rng.between(0, yarray.len(&txn)) as usize;
        if let Any::Array(expected) = yarray.to_json(&txn) {
            let mut expected = Vec::from(expected.as_ref());
            yarray.insert_range(&mut txn, pos as u32, content.clone());

            for any in content {
                expected.insert(pos, any);
                pos += 1;
            }
            let actual = yarray.to_json(&txn);
            assert_eq!(actual, Any::from(expected))
        } else {
            panic!("should not happen")
        }
    }

    fn insert_type_array(doc: &mut Doc, rng: &mut Rng) {
        let yarray = doc.get_or_insert_array("array");
        let mut txn = doc.transact_mut();
        let pos = rng.between(0, yarray.len(&txn));
        let array2 = yarray.insert(&mut txn, pos, ArrayPrelim::from([1, 2, 3, 4]));
        let expected: Arc<[Any]> = (1..=4).map(|i| Any::Number(i as f64)).collect();
        assert_eq!(array2.to_json(&txn), Any::Array(expected));
    }

    fn insert_type_map(doc: &mut Doc, rng: &mut Rng) {
        let yarray = doc.get_or_insert_array("array");
        let mut txn = doc.transact_mut();
        let pos = rng.between(0, yarray.len(&txn));
        let map = yarray.insert(&mut txn, pos, MapPrelim::default());
        map.insert(&mut txn, "someprop".to_string(), 42);
        map.insert(&mut txn, "someprop".to_string(), 43);
        map.insert(&mut txn, "someprop".to_string(), 44);
    }

    fn delete(doc: &mut Doc, rng: &mut Rng) {
        let yarray = doc.get_or_insert_array("array");
        let mut txn = doc.transact_mut();
        let len = yarray.len(&txn);
        if len > 0 {
            let pos = rng.between(0, len - 1);
            let del_len = rng.between(1, 2.min(len - pos));
            if rng.bool() {
                if let Out::YArray(array2) = yarray.get(&txn, pos).unwrap() {
                    let pos = rng.between(0, array2.len(&txn) - 1);
                    let del_len = rng.between(0, 2.min(array2.len(&txn) - pos));
                    array2.remove_range(&mut txn, pos, del_len);
                }
            } else {
                if let Any::Array(old_content) = yarray.to_json(&txn) {
                    let mut old_content = Vec::from(old_content.as_ref());
                    yarray.remove_range(&mut txn, pos, del_len);
                    old_content.drain(pos as usize..(pos + del_len) as usize);
                    assert_eq!(yarray.to_json(&txn), Any::from(old_content));
                } else {
                    panic!("should not happen")
                }
            }
        }
    }

    [
        Box::new(insert),
        Box::new(insert_type_array),
        Box::new(insert_type_map),
        Box::new(delete),
    ]
}

fn fuzzy(iterations: usize) {
    run_scenario(0, &array_transactions(), 5, iterations)
}

#[test]
fn fuzzy_test_6() {
    fuzzy(6)
}

#[test]
fn fuzzy_test_300() {
    fuzzy(300)
}

#[test]
fn get_at_removed_index() {
    let mut d1 = Doc::with_client_id(1);
    let a1 = d1.get_or_insert_array("array");
    let mut t1 = d1.transact_mut();

    a1.insert_range(&mut t1, 0, ["A"]);
    a1.remove(&mut t1, 0);

    let actual = a1.get(&t1, 0);
    assert_eq!(actual, None);
}

#[test]
fn observe_deep_event_order() {
    let mut doc = Doc::with_client_id(1);
    let array = doc.get_or_insert_array("array");

    let paths = Arc::new(Mutex::new(vec![]));
    let paths_copy = paths.clone();

    let _sub = array.observe_deep(move |_txn, e| {
        let path: Vec<Path> = e.iter().map(Event::path).collect();
        paths_copy.lock().unwrap().push(path);
    });

    array.insert(&mut doc.transact_mut(), 0, MapPrelim::default());

    {
        let mut txn = doc.transact_mut();
        let map = array.get(&txn, 0).unwrap().cast::<MapRef>().unwrap();
        map.insert(&mut txn, "a", "a");
        array.insert(&mut txn, 0, 0);
    }

    let expected = &[
        vec![Path::default()],
        vec![Path::default(), Path::from([PathSegment::Index(1)])],
    ];
    let actual = paths.lock().unwrap();
    assert_eq!(actual.as_slice(), expected);
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
            let array = doc.get_or_insert_array("test");
            let mut txn = doc.transact_mut();
            array.push_back(&mut txn, "a");
        }
    });

    let d3 = doc.clone();
    let h3 = spawn(move || {
        for _ in 0..10 {
            let millis = fastrand::u64(1..20);
            sleep(Duration::from_millis(millis));

            let mut doc = d3.write().unwrap();
            let array = doc.get_or_insert_array("test");
            let mut txn = doc.transact_mut();
            array.push_back(&mut txn, "b");
        }
    });

    h3.join().unwrap();
    h2.join().unwrap();

    let mut doc = doc.write().unwrap();
    let array = doc.get_or_insert_array("test");
    let len = array.len(&doc.transact());
    assert_eq!(len, 20);
}

#[test]
fn insert_empty_range() {
    let mut doc = Doc::with_client_id(1);
    let mut txn = doc.transact_mut();
    let array = txn.get_or_insert_array("array");

    array.insert(&mut txn, 0, 1);
    array.insert_range::<_, Any>(&mut txn, 1, []);
    array.push_back(&mut txn, 2);

    assert_eq!(
        array.iter(&txn).collect::<Vec<_>>(),
        vec![1.into(), 2.into()]
    );

    let data = txn.encode_state_as_update_v1(&StateVector::default());

    let mut doc2 = Doc::with_client_id(2);
    let mut txn = doc2.transact_mut();
    let array = txn.get_or_insert_array("array");
    txn.apply_update(Update::decode_v1(&data).unwrap()).unwrap();

    assert_eq!(
        array.iter(&txn).collect::<Vec<_>>(),
        vec![1.into(), 2.into()]
    );
}
