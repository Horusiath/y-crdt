use arc_swap::ArcSwapOption;
use std::collections::{HashMap, HashSet};
use std::iter::FromIterator;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use yrs::node::{DeepObservable, Observable, Path, PathSegment};

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
    let mut a = txn.node_mut("array").unwrap();

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

        a.remove(0, 1); // len: 3
        a.insert(0, 0); // len: 4

        assert_eq!(a.len(), 4);
    }
    {
        let mut txn = d.transact_mut();
        let mut a = txn.node_mut("array").unwrap();
        a.remove(1, 1); // len: 3
        assert_eq!(a.len(), 3);

        a.insert(1, 1); // len: 4
        assert_eq!(a.len(), 4);

        a.remove(2, 1); // len: 3
        assert_eq!(a.len(), 3);

        a.insert(2, 2); // len: 4
        assert_eq!(a.len(), 4);
    }

    let mut txn = d.transact_mut();
    let mut a = txn.node_mut("array").unwrap();
    assert_eq!(a.len(), 4);

    a.remove(1, 1);
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

    let t2 = d2.transact();
    let a2 = t2.node("array").unwrap();
    let actual: Vec<_> = a2.iter().collect();
    assert_eq!(
        actual,
        vec![Out::from(1.0), Out::from(true), Out::from(false)]
    );
}

#[test]
fn concurrent_insert_with_3_conflicts() {
    let mut d1 = Doc::with_client_id(1);
    d1.transact_mut().node_mut("array").unwrap().insert(0, 0);

    let mut d2 = Doc::with_client_id(2);
    d1.transact_mut().node_mut("array").unwrap().insert(0, 1);

    let mut d3 = Doc::with_client_id(3);
    d1.transact_mut().node_mut("array").unwrap().insert(0, 2);

    exchange_updates(&mut [&mut d1, &mut d2, &mut d3]);

    let a1 = to_array(&mut d1);
    let a2 = to_array(&mut d2);
    let a3 = to_array(&mut d3);

    assert_eq!(a1, a2, "Peer 1 and peer 2 states are different");
    assert_eq!(a2, a3, "Peer 2 and peer 3 states are different");
}

fn to_array(d: &mut Doc) -> Vec<Out> {
    d.transact().node("array").unwrap().iter().collect()
}

#[test]
fn concurrent_insert_remove_with_3_conflicts() {
    let mut d1 = Doc::with_client_id(1);
    {
        let mut txn = d1.transact_mut();
        let mut a = txn.node_mut("array").unwrap();
        a.insert(0, "x");
        a.insert(1, "y");
        a.insert(2, "z");
    }
    let mut d2 = Doc::with_client_id(2);
    let mut d3 = Doc::with_client_id(3);

    exchange_updates(&mut [&mut d1, &mut d2, &mut d3]);

    {
        // start state: [x,y,z]
        d1.transact_mut().node_mut("array").unwrap().insert(1, 0); // [x,0,y,z]
        {
            let mut t2 = d2.transact_mut();
            let mut a2 = t2.node_mut("array").unwrap();
            a2.remove(0, 1); // [y,z]
            a2.remove(1, 1); // [y]
        }
        d3.transact_mut().node_mut("array").unwrap().insert(1, 2); // [x,2,y,z]
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
        let mut txn = d1.transact_mut();
        let mut a = txn.node_mut("array").unwrap();
        a.push_back("x");
        a.push_back("y");
    }
    let mut d2 = Doc::with_client_id(2);
    let mut d3 = Doc::with_client_id(3);

    exchange_updates(&mut [&mut d1, &mut d2, &mut d3]);

    {
        d1.transact_mut()
            .node_mut("array")
            .unwrap()
            .insert(1, "user0");
        d2.transact_mut()
            .node_mut("array")
            .unwrap()
            .insert(1, "user1");
        d3.transact_mut()
            .node_mut("array")
            .unwrap()
            .insert(1, "user2");
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
        let mut txn = d1.transact_mut();
        let mut a = txn.node_mut("array").unwrap();
        a.push_back("x");
        a.push_back("y");
    }
    let mut d2 = Doc::with_client_id(2);

    exchange_updates(&mut [&mut d1, &mut d2]);

    {
        d2.transact_mut().node_mut("array").unwrap().remove(1, 1);
        d1.transact_mut().node_mut("array").unwrap().remove(0, 2);
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
        let mut txn = d1.transact_mut();
        let mut a = txn.node_mut("array").unwrap();
        a.push_back("x");
        a.push_back("y");
        a.push_back("z");
    }
    let mut d2 = Doc::with_client_id(2);

    exchange_updates(&mut [&mut d1, &mut d2]);

    d2.transact_mut().node_mut("array").unwrap().remove(0, 3);

    exchange_updates(&mut [&mut d1, &mut d2]);

    let a1 = to_array(&mut d1);
    let a2 = to_array(&mut d2);

    assert_eq!(a1, a2, "Peer 1 and peer 2 states are different");
}

#[test]
fn iter_array_containing_types() {
    let mut d = Doc::with_client_id(1);
    let mut txn = d.transact_mut();
    {
        let mut a = txn.node_mut("arr").unwrap();
        for i in 0..10 {
            a.push_back(Delta::new().insert_attr("value", i));
        }
    }

    let a = txn.node("arr").unwrap();
    for (i, value) in a.iter().enumerate() {
        match value {
            Out::Node(id) => {
                let actual = txn.node(id).unwrap().to_json();
                assert_eq!(actual, any!({"value": (i as f64) }))
            }
            _ => panic!("Value of array at index {} was no YMap", i),
        }
    }
}

#[test]
fn insert_and_remove_events() {
    let mut d = Doc::with_client_id(1);
    let happened = Arc::new(AtomicBool::new(false));
    let happened_clone = happened.clone();
    let _sub = {
        let mut txn = d.transact_mut();
        txn.node_mut("array").unwrap().observe(move |_, _| {
            happened_clone.store(true, Ordering::Relaxed);
        })
    };

    {
        let mut txn = d.transact_mut();
        txn.node_mut("array").unwrap().insert_range(0, [0, 1, 2]);
    }
    assert!(
        happened.swap(false, Ordering::Relaxed),
        "insert of [0,1,2] should trigger event"
    );

    {
        let mut txn = d.transact_mut();
        txn.node_mut("array").unwrap().remove(0, 1);
    }
    assert!(
        happened.swap(false, Ordering::Relaxed),
        "removal of [0] should trigger event"
    );

    {
        let mut txn = d.transact_mut();
        txn.node_mut("array").unwrap().remove(0, 2);
    }
    assert!(
        happened.swap(false, Ordering::Relaxed),
        "removal of [1,2] should trigger event"
    );
}

#[test]
fn insert_and_remove_event_changes() {
    let mut d1 = Doc::with_client_id(1);
    let delta = Arc::new(ArcSwapOption::default());

    let delta_c = delta.clone();
    let _sub = {
        let mut txn = d1.transact_mut();
        txn.node_mut("array").unwrap().observe(move |_txn, e| {
            delta_c.store(Some(Arc::new(e.delta(()).collect::<Vec<_>>())));
        })
    };

    {
        let mut txn = d1.transact_mut();
        let mut array = txn.node_mut("array").unwrap();
        array.push_back(4);
        array.push_back("dtrn");
    }
    // TODO(unified-api): Event::inserts/removes and the `Change` enum not available yet
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
        txn.node_mut("array").unwrap().remove(0, 1);
    }
    // TODO(unified-api): Event::inserts/removes and the `Change` enum not available yet
    assert_eq!(added.swap(None), Some(HashSet::new().into()));
    assert_eq!(
        removed.swap(None),
        Some(HashSet::from([ID::new(ClientID::new(1), 0)]).into())
    );
    assert_eq!(delta.swap(None), Some(vec![Change::Removed(1)].into()));

    {
        let mut txn = d1.transact_mut();
        txn.node_mut("array").unwrap().insert(1, 0.5);
    }
    // TODO(unified-api): Event::inserts/removes and the `Change` enum not available yet
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
    let delta_c = delta.clone();
    let _sub = {
        let mut txn = d2.transact_mut();
        txn.node_mut("array").unwrap().observe(move |_txn, e| {
            delta_c.store(Some(Arc::new(e.delta(()).collect::<Vec<_>>())));
        })
    };

    {
        let t1 = d1.transact();
        let mut t2 = d2.transact_mut();

        let sv = t2.state_vector();
        let update = t1.encode_diff_v1(&sv);
        t2.apply_update(Update::decode_v1(update.as_slice()).unwrap())
            .unwrap();
    }
    // TODO(unified-api): Event::inserts/removes and the `Change` enum not available yet
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

    let c1 = Arc::new(ArcSwapOption::default());
    let c1c = c1.clone();
    let _s1 = {
        let mut txn = d1.transact_mut();
        txn.node_mut("array").unwrap().observe(move |_, e| {
            c1c.store(Some(Arc::new(e.target().id())));
        })
    };
    let c2 = Arc::new(ArcSwapOption::default());
    let c2c = c2.clone();
    let _s2 = {
        let mut txn = d2.transact_mut();
        txn.node_mut("array").unwrap().observe(move |_, e| {
            c2c.store(Some(Arc::new(e.target().id())));
        })
    };

    {
        let mut t1 = d1.transact_mut();
        t1.node_mut("array").unwrap().insert_range(0, [1, 2]);
    }
    exchange_updates(&mut [&mut d1, &mut d2]);

    assert_eq!(c1.swap(None), Some(Arc::new(NodeID::root("array"))));
    assert_eq!(c2.swap(None), Some(Arc::new(NodeID::root("array"))));
}

use fastrand::Rng;
use std::sync::atomic::{AtomicI64, Ordering};
use yrs::test_utils::{RngExt, exchange_updates, run_scenario};
use yrs::updates::decoder::Decode;
use yrs::{Any, Delta, Doc, In, NodeID, Out, StateVector, Update, any};

static UNIQUE_NUMBER: AtomicI64 = AtomicI64::new(0);

fn get_unique_number() -> i64 {
    UNIQUE_NUMBER.fetch_add(1, Ordering::SeqCst)
}

fn array_transactions() -> [Box<dyn Fn(&mut Doc, &mut Rng)>; 4] {
    fn insert(doc: &mut Doc, rng: &mut Rng) {
        let unique_number = get_unique_number();
        let len = rng.between(1, 4);
        let content: Vec<_> = (0..len)
            .into_iter()
            .map(|_| Any::BigInt(unique_number))
            .collect();
        let mut txn = doc.transact_mut();
        let mut yarray = txn.node_mut("array").unwrap();
        let mut pos = rng.between(0, yarray.len()) as usize;
        if let Any::Array(expected) = yarray.to_json() {
            let mut expected = Vec::from(expected.as_ref());
            yarray.insert_range(pos as u32, content.clone());

            for any in content {
                expected.insert(pos, any);
                pos += 1;
            }
            let actual = yarray.to_json();
            assert_eq!(actual, Any::from(expected))
        } else {
            panic!("should not happen")
        }
    }

    fn insert_type_array(doc: &mut Doc, rng: &mut Rng) {
        let mut txn = doc.transact_mut();
        let mut yarray = txn.node_mut("array").unwrap();
        let pos = rng.between(0, yarray.len());
        let nested = Delta::new().insert(1).insert(2).insert(3).insert(4);
        let Out::Node(array2) = yarray.insert(pos, In::Node(nested)) else {
            panic!("expected a nested node")
        };
        let expected: Arc<[Any]> = (1..=4).map(|i| Any::Number(i as f64)).collect();
        assert_eq!(txn.node(array2).unwrap().to_json(), Any::Array(expected));
    }

    fn insert_type_map(doc: &mut Doc, rng: &mut Rng) {
        let mut txn = doc.transact_mut();
        let mut yarray = txn.node_mut("array").unwrap();
        let pos = rng.between(0, yarray.len());
        let Out::Node(map) = yarray.insert(pos, In::Node(Delta::new())) else {
            panic!("expected a nested node")
        };
        let mut map = txn.node_mut(map).unwrap();
        map.insert_attr("someprop", 42);
        map.insert_attr("someprop", 43);
        map.insert_attr("someprop", 44);
    }

    fn delete(doc: &mut Doc, rng: &mut Rng) {
        let mut txn = doc.transact_mut();
        let mut yarray = txn.node_mut("array").unwrap();
        let len = yarray.len();
        if len > 0 {
            let pos = rng.between(0, len - 1);
            let del_len = rng.between(1, 2.min(len - pos));
            if rng.bool() {
                if let Some(Out::Node(array2)) = yarray.get(pos) {
                    let mut array2 = txn.node_mut(array2).unwrap();
                    if array2.len() > 0 {
                        let pos = rng.between(0, array2.len() - 1);
                        let del_len = rng.between(0, 2.min(array2.len() - pos));
                        array2.remove(pos, del_len);
                    }
                }
            } else {
                if let Any::Array(old_content) = yarray.to_json() {
                    let mut old_content = Vec::from(old_content.as_ref());
                    yarray.remove(pos, del_len);
                    old_content.drain(pos as usize..(pos + del_len) as usize);
                    assert_eq!(yarray.to_json(), Any::from(old_content));
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
    let mut t1 = d1.transact_mut();
    let mut a1 = t1.node_mut("array").unwrap();

    a1.insert(0, "A");
    a1.remove(0, 1);

    let actual = a1.get(0);
    assert_eq!(actual, None);
}

#[test]
fn observe_deep_event_order() {
    let mut doc = Doc::with_client_id(1);
    let paths = Arc::new(Mutex::new(vec![]));
    let paths_copy = paths.clone();

    let mut txn = doc.transact_mut();
    let mut array = txn.node_mut("array").unwrap();
    let _sub = array.observe_deep(move |_txn, e| {
        let path: Vec<Path> = e.iter().map(|e| e.path()).collect();
        paths_copy.lock().unwrap().push(path);
    });

    array.insert(0, In::Node(Delta::new()));
    drop(txn);

    {
        let mut txn = doc.transact_mut();
        let Some(Out::Node(map)) = txn.node("array").unwrap().get(0) else {
            panic!("expected a nested node")
        };
        txn.node_mut(map).unwrap().insert_attr("a", "a");
        txn.node_mut("array").unwrap().insert(0, 0);
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
            doc.transact_mut().node_mut("test").unwrap().push_back("a");
        }
    });

    let d3 = doc.clone();
    let h3 = spawn(move || {
        for _ in 0..10 {
            let millis = fastrand::u64(1..20);
            sleep(Duration::from_millis(millis));

            let mut doc = d3.write().unwrap();
            doc.transact_mut().node_mut("test").unwrap().push_back("b");
        }
    });

    h3.join().unwrap();
    h2.join().unwrap();

    let doc = doc.write().unwrap();
    let len = doc.transact().node("test").unwrap().len();
    assert_eq!(len, 20);
}

#[test]
fn insert_empty_range() {
    let mut doc = Doc::with_client_id(1);
    let mut txn = doc.transact_mut();
    {
        let mut array = txn.node_mut("array").unwrap();
        array.insert(0, 1);
        array.push_back(2);

        assert_eq!(array.iter().collect::<Vec<_>>(), vec![1.into(), 2.into()]);
    }

    let data = txn.encode_state_as_update_v1(&StateVector::default());

    let mut doc2 = Doc::with_client_id(2);
    let mut txn = doc2.transact_mut();
    txn.apply_update(Update::decode_v1(&data).unwrap()).unwrap();
    let array = txn.node("array").unwrap();

    assert_eq!(array.iter().collect::<Vec<_>>(), vec![1.into(), 2.into()]);
}
