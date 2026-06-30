use std::collections::{Bound, HashMap};
use std::ops::RangeBounds;
use std::sync::{Arc, Mutex};

use arc_swap::ArcSwapOption;
use yrs::Doc;

#[test]
fn basic_map_link() {
    let mut doc = Doc::new();
    let map = doc.get_or_insert_map("map");
    let mut txn = doc.transact_mut();
    let nested = MapPrelim::from([("a1".to_owned(), "hello".to_owned())]);
    let nested = map.insert(&mut txn, "a", nested);
    let link = map.link(&txn, "a").unwrap();
    map.insert(&mut txn, "b", link);

    let link = map
        .get(&txn, "b")
        .unwrap()
        .cast::<WeakRef<MapRef>>()
        .unwrap();

    let expected = nested.to_json(&txn);
    let deref: MapRef = link.try_deref(&txn).unwrap();
    let actual = deref.to_json(&txn);

    assert_eq!(actual, expected);
}

#[test]
fn basic_array_link() {
    let mut d1 = Doc::with_client_id(1);
    let a1 = d1.get_or_insert_array("array");
    {
        let mut txn = d1.transact_mut();

        a1.insert_range(&mut txn, 0, [1, 2, 3]);
        let link = a1.quote(&txn, 1..2).unwrap();
        a1.insert(&mut txn, 3, link);

        assert_eq!(a1.get(&txn, 0), Some(1.into()));
        assert_eq!(a1.get(&txn, 1), Some(2.into()));
        assert_eq!(a1.get(&txn, 2), Some(3.into()));
        let mut u = a1
            .get(&txn, 3)
            .unwrap()
            .cast::<WeakRef<ArrayRef>>()
            .unwrap()
            .unquote(&txn);
        assert_eq!(u.next(), Some(2.into()));
        assert_eq!(u.next(), None);
    }

    let mut d2 = Doc::new();
    let a2 = d2.get_or_insert_array("array");

    exchange_updates(&mut [&mut d1, &mut d2]);
    let txn = d2.transact_mut();

    assert_eq!(a2.get(&txn, 0), Some(1.into()));
    assert_eq!(a2.get(&txn, 1), Some(2.into()));
    assert_eq!(a2.get(&txn, 2), Some(3.into()));
    let actual: Vec<_> = a2
        .get(&txn, 3)
        .unwrap()
        .cast::<WeakRef<ArrayRef>>()
        .unwrap()
        .unquote(&txn)
        .collect();
    assert_eq!(actual, vec![2.into()]);
}

#[test]
fn array_quote_multi_elements() {
    let mut d1 = Doc::with_client_id(1);
    let a1 = d1.get_or_insert_array("array");
    let mut d2 = Doc::with_client_id(2);
    let a2 = d2.get_or_insert_array("array");

    let nested = {
        let mut txn = d1.transact_mut();
        a1.insert_range(&mut txn, 0, [1, 2]);
        let nested = a1.push_back(&mut txn, MapPrelim::from([("key", "value")]));
        a1.push_back(&mut txn, 3);
        nested
    };
    let l1 = {
        let mut t1 = d1.transact_mut();
        let prelim = a1.quote(&t1, 1..=3).unwrap();
        a1.insert(&mut t1, 0, prelim)
    };

    let t1 = d1.transact();
    assert_eq!(
        l1.unquote(&t1).collect::<Vec<Out>>(),
        vec![2.into(), Out::YMap(nested.clone()), 3.into()]
    );
    assert_eq!(a1.get(&t1, 1), Some(1.into()));
    assert_eq!(a1.get(&t1, 2), Some(2.into()));
    assert_eq!(a1.get(&t1, 3), Some(Out::YMap(nested.clone())));
    assert_eq!(a1.get(&t1, 4), Some(3.into()));
    drop(t1);

    exchange_updates(&mut [&mut d1, &mut d2]);

    let t2 = d2.transact();
    let l2 = a2.get(&t2, 0).unwrap().cast::<WeakRef<ArrayRef>>().unwrap();
    let unquoted: Vec<_> = l2.unquote(&t2).map(|v| v.to_string(&t2)).collect();
    assert_eq!(
        unquoted,
        vec![
            "2".to_string(),
            r#"{key: value}"#.to_string(),
            "3".to_string()
        ]
    );
    assert_eq!(a2.get(&t2, 1), Some(1.into()));
    assert_eq!(a2.get(&t2, 2), Some(2.into()));
    assert_eq!(
        a2.get(&t2, 3).map(|v| v.to_string(&t2)),
        Some(r#"{key: value}"#.to_string())
    );
    assert_eq!(a2.get(&t2, 4), Some(3.into()));
    drop(t2);

    a2.insert_range(&mut d2.transact_mut(), 3, ["A", "B"]);

    let t2 = d2.transact();
    let unquoted: Vec<_> = l2.unquote(&t2).map(|v| v.to_string(&t2)).collect();
    assert_eq!(
        unquoted,
        vec![
            "2".to_string(),
            "A".to_string(),
            "B".to_string(),
            r#"{key: value}"#.to_string(),
            "3".to_string()
        ]
    );
    drop(t2);

    exchange_updates(&mut [&mut d1, &mut d2]);

    assert_eq!(
        l1.unquote(&d1.transact()).collect::<Vec<Out>>(),
        vec![
            2.into(),
            "A".into(),
            "B".into(),
            Out::YMap(nested.clone()),
            3.into()
        ]
    );
}

#[test]
fn self_quotation() {
    let mut d1 = Doc::with_client_id(1);
    let a1 = d1.get_or_insert_array("array");
    let mut d2 = Doc::with_client_id(2);
    let a2 = d2.get_or_insert_array("array");

    a1.insert_range(&mut d1.transact_mut(), 0, [1, 2, 3, 4]);
    let l1 = a1.quote(&d1.transact(), 0..3).unwrap();
    // link is inserted into its own range
    let l1 = a1.insert(&mut d1.transact_mut(), 1, l1);
    let t1 = d1.transact();
    let mut u = l1.unquote(&t1);
    assert_eq!(u.next(), Some(1.into()));
    assert_eq!(u.next(), Some(Out::YWeakLink(l1.clone().into_inner())));
    assert_eq!(u.next(), Some(2.into()));
    assert_eq!(u.next(), Some(3.into()));

    assert_eq!(a1.get(&t1, 0), Some(1.into()));
    assert_eq!(
        a1.get(&t1, 1),
        Some(Out::YWeakLink(l1.clone().into_inner()))
    );
    assert_eq!(a1.get(&t1, 2), Some(2.into()));
    assert_eq!(a1.get(&t1, 3), Some(3.into()));
    assert_eq!(a1.get(&t1, 4), Some(4.into()));
    drop(t1);

    exchange_updates(&mut [&mut d1, &mut d2]);

    let t2 = d2.transact();
    let l2 = a2.get(&t2, 1).unwrap().cast::<WeakRef<ArrayRef>>().unwrap();
    let unquote: Vec<_> = l2.unquote(&t2).collect();
    assert_eq!(
        unquote,
        vec![
            1.into(),
            Out::YWeakLink(l2.clone().into_inner()),
            2.into(),
            3.into()
        ]
    );
    assert_eq!(a2.get(&t2, 0), Some(1.into()));
    assert_eq!(a2.get(&t2, 1), Some(Out::YWeakLink(l2.into_inner())));
    assert_eq!(a2.get(&t2, 2), Some(2.into()));
    assert_eq!(a2.get(&t2, 3), Some(3.into()));
    assert_eq!(a2.get(&t2, 4), Some(4.into()));
}

#[test]
fn update() {
    let mut d1 = Doc::new();
    let m1 = d1.get_or_insert_map("map");

    let mut d2 = Doc::new();
    let m2 = d2.get_or_insert_map("map");

    let link1 = {
        let mut txn = d1.transact_mut();
        let nested = MapPrelim::from([("a1".to_owned(), "hello".to_owned())]);
        m1.insert(&mut txn, "a", nested);
        let link = m1.link(&txn, "a").unwrap();
        m1.insert(&mut txn, "b", link)
    };

    exchange_updates(&mut [&mut d1, &mut d2]);

    let link2 = m2
        .get(&d2.transact(), "b")
        .unwrap()
        .cast::<WeakRef<MapRef>>()
        .unwrap();
    let l1: MapRef = link1.try_deref(&d1.transact()).unwrap();
    let l2: MapRef = link2.try_deref(&d2.transact()).unwrap();
    assert_eq!(l1.get(&d1.transact(), "a1"), l2.get(&d2.transact(), "a1"));

    m2.insert(&mut d2.transact_mut(), "a2", "world");

    exchange_updates(&mut [&mut d1, &mut d2]);

    let l1: MapRef = link1.try_deref(&d1.transact()).unwrap();
    let l2: MapRef = link2.try_deref(&d2.transact()).unwrap();
    assert_eq!(l1.get(&d1.transact(), "a2"), l2.get(&d2.transact(), "a2"));
}

#[test]
#[cfg_attr(target_os = "windows", ignore)]
fn delete_weak_link() {
    let mut d1 = Doc::new();
    let m1 = d1.get_or_insert_map("map");

    let mut d2 = Doc::new();
    let m2 = d2.get_or_insert_map("map");

    let link1 = {
        let mut txn = d1.transact_mut();
        let nested = MapPrelim::from([("a1".to_owned(), "hello".to_owned())]);
        m1.insert(&mut txn, "a", nested);
        let link = m1.link(&txn, "a").unwrap();
        m1.insert(&mut txn, "b", link)
    };

    exchange_updates(&mut [&mut d1, &mut d2]);

    let link2 = m2
        .get(&d2.transact(), "b")
        .unwrap()
        .cast::<WeakRef<MapRef>>()
        .unwrap();
    let l1: MapRef = link1.try_deref(&d1.transact()).unwrap();
    let l2: MapRef = link2.try_deref(&d2.transact()).unwrap();
    assert_eq!(l1.get(&d1.transact(), "a1"), l2.get(&d2.transact(), "a1"));

    m2.remove(&mut d2.transact_mut(), "b"); // delete links

    exchange_updates(&mut [&mut d1, &mut d2]);

    // since links have been deleted, they no longer refer to any content
    assert_eq!(link1.try_deref_value(&d1.transact()), None);
    assert_eq!(link2.try_deref_value(&d2.transact()), None);
}

#[test]
fn delete_source() {
    let mut d1 = Doc::new();
    let m1 = d1.get_or_insert_map("map");

    let mut d2 = Doc::new();
    let m2 = d2.get_or_insert_map("map");

    let link1 = {
        let mut txn = d1.transact_mut();
        let nested = MapPrelim::from([("a1".to_owned(), "hello".to_owned())]);
        m1.insert(&mut txn, "a", nested);
        let link = m1.link(&txn, "a").unwrap();
        m1.insert(&mut txn, "b", link)
    };

    exchange_updates(&mut [&mut d1, &mut d2]);

    let link2 = m2
        .get(&d2.transact(), "b")
        .unwrap()
        .cast::<WeakRef<MapRef>>()
        .unwrap();
    let l1: MapRef = link1.try_deref(&d1.transact()).unwrap();
    let l2: MapRef = link2.try_deref(&d2.transact()).unwrap();
    assert_eq!(l1.get(&d1.transact(), "a1"), l2.get(&d2.transact(), "a1"));

    m2.remove(&mut d2.transact_mut(), "a"); // delete source of the link

    exchange_updates(&mut [&mut d1, &mut d2]);

    // since links have been deleted, they no longer refer to any content
    assert_eq!(link1.try_deref_value(&d1.transact()), None);
    assert_eq!(link2.try_deref_value(&d2.transact()), None);
}

#[test]
fn observe_map_update() {
    let mut d1 = Doc::new();
    let m1 = d1.get_or_insert_map("map");
    let mut d2 = Doc::new();
    let m2 = d2.get_or_insert_map("map");

    let link1 = {
        let mut txn = d1.transact_mut();
        m1.insert(&mut txn, "a", "value");
        let link1 = m1.link(&txn, "a").unwrap();
        m1.insert(&mut txn, "b", link1)
    };

    let target1 = Arc::new(ArcSwapOption::default());
    let _sub1 = {
        let target = target1.clone();
        link1.observe(move |_, e| target.store(Some(Arc::new(e.target.clone()))))
    };

    exchange_updates(&mut [&mut d1, &mut d2]);

    let link2 = m2
        .get(&d2.transact(), "b")
        .unwrap()
        .cast::<WeakRef<MapRef>>()
        .unwrap();
    assert_eq!(link2.try_deref_value(&d2.transact()), Some("value".into()));

    let target2 = Arc::new(ArcSwapOption::default());
    let _sub2 = {
        let target = target2.clone();
        link2.observe(move |_, e| target.store(Some(Arc::new(e.target.clone()))))
    };

    m1.insert(&mut d1.transact_mut(), "a", "value2");
    assert_eq!(link1.try_deref_value(&d1.transact()), Some("value2".into()));

    exchange_updates(&mut [&mut d1, &mut d2]);
    assert_eq!(link2.try_deref_value(&d2.transact()), Some("value2".into()));
}

#[test]
fn observe_map_delete() {
    let mut d1 = Doc::new();
    let m1 = d1.get_or_insert_map("map");
    let mut d2 = Doc::new();
    let m2 = d2.get_or_insert_map("map");

    let link1 = {
        let mut txn = d1.transact_mut();
        m1.insert(&mut txn, "a", "value");
        let link1 = m1.link(&txn, "a").unwrap();
        m1.insert(&mut txn, "b", link1)
    };

    let target1 = Arc::new(ArcSwapOption::default());
    let _sub1 = {
        let target = target1.clone();
        link1.observe(move |_, e| target.store(Some(Arc::new(e.as_target::<MapRef>()))))
    };

    exchange_updates(&mut [&mut d1, &mut d2]);

    let link2 = m2
        .get(&d2.transact(), "b")
        .unwrap()
        .cast::<WeakRef<MapRef>>()
        .unwrap();
    assert_eq!(link2.try_deref_value(&d2.transact()), Some("value".into()));

    let target2 = Arc::new(ArcSwapOption::default());
    let _sub2 = {
        let target = target2.clone();
        link2.observe(move |_, e| target.store(Some(Arc::new(e.as_target::<MapRef>()))))
    };

    m1.remove(&mut d1.transact_mut(), "a");
    let l1 = target1.swap(None).unwrap();
    assert_eq!(l1.try_deref_value(&d1.transact()), None);

    exchange_updates(&mut [&mut d1, &mut d2]);
    let l2 = target2.swap(None).unwrap();
    assert_eq!(l2.try_deref_value(&d2.transact()), None);
}

#[test]
fn observe_array() {
    let mut d1 = Doc::with_client_id(1);
    let a1 = d1.get_or_insert_array("array");
    let mut d2 = Doc::with_client_id(2);
    let a2 = d2.get_or_insert_array("array");

    let link1 = {
        let mut txn = d1.transact_mut();
        a1.insert_range(&mut txn, 0, ["A", "B", "C"]);
        let link1 = a1.quote(&txn, 1..=2).unwrap();
        a1.insert(&mut txn, 0, link1)
    };

    let target1 = Arc::new(ArcSwapOption::default());
    let _sub1 = {
        let target = target1.clone();
        link1.observe(move |_, e| target.store(Some(Arc::new(e.as_target::<ArrayRef>()))))
    };

    exchange_updates(&mut [&mut d1, &mut d2]);

    let link2 = a2
        .get(&d2.transact(), 0)
        .unwrap()
        .cast::<WeakRef<ArrayRef>>()
        .unwrap();
    let actual: Vec<_> = link2.unquote(&d2.transact()).collect();
    assert_eq!(actual, vec!["B".into(), "C".into()]);

    let target2 = Arc::new(ArcSwapOption::default());
    let _sub2 = {
        let target = target2.clone();
        link2.observe(move |_, e| target.store(Some(Arc::new(e.as_target::<ArrayRef>()))))
    };

    a1.remove(&mut d1.transact_mut(), 2);
    let actual: Vec<_> = link1.unquote(&d1.transact()).collect();
    assert_eq!(actual, vec!["C".into()]);

    exchange_updates(&mut [&mut d1, &mut d2]);
    let l2 = target2.swap(None).unwrap();
    let actual: Vec<_> = l2.unquote(&d2.transact()).collect();
    assert_eq!(actual, vec!["C".into()]);

    a2.remove(&mut d2.transact_mut(), 2);
    let l2 = target2.swap(None).unwrap();
    let actual: Vec<_> = l2.unquote(&d2.transact()).collect();
    assert_eq!(actual, vec![]);

    exchange_updates(&mut [&mut d1, &mut d2]);
    let l1 = target1.swap(None).unwrap();
    let actual: Vec<_> = l1.unquote(&d1.transact()).collect();
    assert_eq!(actual, vec![]);

    a1.remove(&mut d1.transact_mut(), 1);
    assert_eq!(target1.swap(None), None);
}

#[test]
fn deep_observe_transitive() {
    /*
      Structure:
        - map1
          - link-key: <=+-+
        - map2:         | |
          - key: value1-+ |
          - link-link: <--+
    */
    let mut doc = Doc::new();
    let m1 = doc.get_or_insert_map("map1");
    let m2 = doc.get_or_insert_map("map2");
    let mut txn = doc.transact_mut();

    // test observers in a face of linked chains of values
    m2.insert(&mut txn, "key", "value1");
    let link1 = m2.link(&txn, "key").unwrap();
    m1.insert(&mut txn, "link-key", link1);
    let link2 = m1.link(&txn, "link-key").unwrap();
    let link2 = m2.insert(&mut txn, "link-link", link2);
    drop(txn);

    let events = Arc::new(Mutex::new(vec![]));
    let _sub1 = {
        let events = events.clone();
        link2.observe_deep(move |_, evts| {
            let mut er = events.lock().unwrap();
            for e in evts.iter() {
                er.push(e.target());
            }
        })
    };
    m2.insert(&mut doc.transact_mut(), "key", "value2");
    let actual: Vec<_> = events
        .lock()
        .unwrap()
        .iter()
        .flat_map(|v| {
            v.clone()
                .cast::<WeakRef<MapRef>>()
                .unwrap()
                .try_deref_value(&doc.transact())
        })
        .collect();
    assert_eq!(actual, vec!["value2".into()])
}

#[test]
fn deep_observe_transitive2() {
    /*
      Structure:
        - map1
          - link-key: <=+-+
        - map2:         | |
          - key: value1-+ |
          - link-link: <==+--+
        - map3:              |
          - link-link-link:<-+
    */
    let mut doc = Doc::new();
    let m1 = doc.get_or_insert_map("map1");
    let m2 = doc.get_or_insert_map("map2");
    let m3 = doc.get_or_insert_map("map3");
    let mut txn = doc.transact_mut();

    // test observers in a face of multi-layer linked chains of values
    m2.insert(&mut txn, "key", "value1");
    let link1 = m2.link(&txn, "key").unwrap();
    m1.insert(&mut txn, "link-key", link1);
    let link2 = m1.link(&txn, "link-key").unwrap();
    m2.insert(&mut txn, "link-link", link2);
    let link3 = m2.link(&txn, "link-link").unwrap();
    let link3 = m3.insert(&mut txn, "link-link-link", link3);
    drop(txn);

    let events = Arc::new(Mutex::new(vec![]));
    let _sub1 = {
        let events = events.clone();
        link3.observe_deep(move |_, evts| {
            let mut er = events.lock().unwrap();
            for e in evts.iter() {
                er.push(e.target());
            }
        })
    };
    m2.insert(&mut doc.transact_mut(), "key", "value2");
    let mut guard = events.lock().unwrap();
    let actual = std::mem::take(&mut *guard);
    let actual: Vec<_> = actual
        .into_iter()
        .flat_map(|v| {
            v.cast::<WeakRef<MapRef>>()
                .unwrap()
                .try_deref_value(&doc.transact())
        })
        .collect();
    assert_eq!(actual, vec!["value2".into()])
}

#[test]
fn deep_observe_map() {
    /*
      Structure:
        - map (observed):
          - link:<----+
        - array:      |
           0: nested:-+
             - key: value
    */
    let mut doc = Doc::with_client_id(1);
    let map = doc.get_or_insert_map("map");
    let array = doc.get_or_insert_array("array");

    let events = Arc::new(Mutex::new(vec![]));
    let _sub = {
        let events = events.clone();
        map.observe_deep(move |txn, e| {
            let mut rs = events.lock().unwrap();
            for e in e.iter() {
                match e {
                    Event::Map(e) => {
                        let value = Out::YMap(e.target().clone());
                        rs.push((value, Some(e.keys(txn).clone())));
                    }
                    Event::Weak(e) => {
                        let value = Out::YWeakLink(e.as_target());
                        rs.push((value, None));
                    }
                    _ => {}
                }
            }
        })
    };

    let mut txn = doc.transact_mut();
    let nested = array.insert(&mut txn, 0, MapPrelim::default());
    let link = array.quote(&txn, 0..=0).unwrap();
    let link = map.insert(&mut txn, "link", link);
    drop(txn);

    // update entry in linked map
    events.lock().unwrap().clear();
    nested.insert(&mut doc.transact_mut(), "key", "value");
    let actual = {
        let mut guard = events.lock().unwrap();
        std::mem::take(&mut *guard)
    };
    assert_eq!(
        actual,
        vec![(
            Out::YMap(nested.clone()),
            Some(HashMap::from([(
                Arc::from("key"),
                EntryChange::Inserted("value".into())
            )]))
        )]
    );

    // delete entry in linked map
    nested.remove(&mut doc.transact_mut(), "key");
    let actual = {
        let mut guard = events.lock().unwrap();
        std::mem::take(&mut *guard)
    };
    assert_eq!(
        actual,
        vec![(
            Out::YMap(nested.clone()),
            Some(HashMap::from([(
                Arc::from("key"),
                EntryChange::Removed("value".into())
            )]))
        )]
    );

    // delete linked map
    array.remove(&mut doc.transact_mut(), 0);
    let actual = {
        let mut guard = events.lock().unwrap();
        std::mem::take(&mut *guard)
    };
    assert_eq!(actual, vec![(Out::YWeakLink(link.into_inner()), None)]);
}

#[test]
fn deep_observe_array() {
    // test observers in a face of linked chains of values
    /*
      Structure:
        - map:
          - nested: --------+
            - key: value    |
        - array (observed): |
          0: <--------------+
    */
    let mut doc = Doc::with_client_id(1);
    let map = doc.get_or_insert_map("map");
    let array = doc.get_or_insert_array("array");

    let nested = map.insert(&mut doc.transact_mut(), "nested", MapPrelim::default());
    let link = map.link(&doc.transact(), "nested").unwrap();
    let link = array.insert(&mut doc.transact_mut(), 0, link);

    let events = Arc::new(Mutex::new(vec![]));
    let _sub = {
        let events = events.clone();
        array.observe_deep(move |txn, e| {
            let mut events = events.lock().unwrap();
            for e in e.iter() {
                match e {
                    Event::Map(e) => {
                        events.push((Out::YMap(e.target().clone()), Some(e.keys(&txn).clone())))
                    }
                    Event::Weak(e) => events.push((Out::YWeakLink(e.as_target()), None)),
                    _ => {}
                }
            }
        })
    };
    nested.insert(&mut doc.transact_mut(), "key", "value");
    let actual = {
        let mut guard = events.lock().unwrap();
        std::mem::take(&mut *guard)
    };
    assert_eq!(
        actual,
        vec![(
            Out::YMap(nested.clone()),
            Some(HashMap::from([(
                Arc::from("key"),
                EntryChange::Inserted("value".into())
            )]))
        )]
    );
    // update existing entry
    nested.insert(&mut doc.transact_mut(), "key", "value2");
    let actual = {
        let mut guard = events.lock().unwrap();
        std::mem::take(&mut *guard)
    };
    assert_eq!(
        actual,
        vec![(
            Out::YMap(nested.clone()),
            Some(HashMap::from([(
                Arc::from("key"),
                EntryChange::Updated("value".into(), "value2".into())
            )]))
        )]
    );

    // delete entry in linked map
    nested.remove(&mut doc.transact_mut(), "key");
    let actual = {
        let mut guard = events.lock().unwrap();
        std::mem::take(&mut *guard)
    };
    assert_eq!(
        actual,
        vec![(
            Out::YMap(nested.clone()),
            Some(HashMap::from([(
                Arc::from("key"),
                EntryChange::Removed("value2".into())
            )]))
        )]
    );

    // delete linked map
    map.remove(&mut doc.transact_mut(), "nested");
    let actual = {
        let mut guard = events.lock().unwrap();
        std::mem::take(&mut *guard)
    };
    assert_eq!(actual, vec![(Out::YWeakLink(link.into_inner()), None)]);
}

#[test]
fn deep_observe_new_element_within_quoted_range() {
    let mut d1 = Doc::with_client_id(1);
    let a1 = d1.get_or_insert_array("array");
    let mut d2 = Doc::with_client_id(2);
    let a2 = d2.get_or_insert_array("array");

    {
        let mut t1 = d1.transact_mut();
        a1.push_back(&mut t1, 1);
        a1.push_back(&mut t1, MapPrelim::default());
        a1.push_back(&mut t1, MapPrelim::default());
        a1.push_back(&mut t1, 2);
    }
    let l1 = {
        let mut t1 = d1.transact_mut();
        let link = a1.quote(&t1, 1..=2).unwrap();
        a1.insert(&mut t1, 0, link)
    };

    exchange_updates(&mut [&mut d1, &mut d2]);

    let e1 = Arc::new(Mutex::new(vec![]));
    let _s1 = {
        let events = e1.clone();
        l1.observe_deep(move |txn, e| {
            let mut events = events.lock().unwrap();
            events.clear();
            for e in e.iter() {
                match e {
                    Event::Map(e) => {
                        events.push((Out::YMap(e.target().clone()), Some(e.keys(txn).clone())))
                    }
                    Event::Weak(e) => events.push((Out::YWeakLink(e.as_target()), None)),
                    _ => {}
                }
            }
        })
    };

    let l2 = a2
        .get(&d2.transact(), 0)
        .unwrap()
        .cast::<WeakRef<ArrayRef>>()
        .unwrap();
    let e2 = Arc::new(Mutex::new(vec![]));
    let _s2 = {
        let events = e2.clone();
        l2.observe_deep(move |txn, e| {
            let mut events = events.lock().unwrap();
            events.clear();
            for e in e.iter() {
                match e {
                    Event::Map(e) => {
                        events.push((Out::YMap(e.target().clone()), Some(e.keys(txn).clone())))
                    }
                    Event::Weak(e) => events.push((Out::YWeakLink(e.as_target()), None)),
                    _ => {}
                }
            }
        })
    };

    let m20 = a1.insert(&mut d1.transact_mut(), 3, MapPrelim::default());
    exchange_updates(&mut [&mut d1, &mut d2]);
    m20.insert(&mut d1.transact_mut(), "key", "value");
    assert_eq!(
        &*e1.lock().unwrap(),
        &vec![(
            Out::YMap(m20.clone()),
            Some(HashMap::from([(
                Arc::from("key"),
                EntryChange::Inserted("value".into())
            )]))
        )]
    );

    exchange_updates(&mut [&mut d1, &mut d2]);

    let m21 = a2.get(&d2.transact(), 3).unwrap().cast::<MapRef>().unwrap();
    assert_eq!(
        &*e2.lock().unwrap(),
        &vec![(
            Out::YMap(m21.clone()),
            Some(HashMap::from([(
                Arc::from("key"),
                EntryChange::Inserted("value".into())
            )]))
        )]
    );
}

#[test]
fn deep_observe_recursive() {
    // test observers in a face of cycled chains of values
    /*
      Structure:
       array (observed):
         m0:--------+
          - k1:<-+  |
                 |  |
         m1------+  |
          - k2:<-+  |
                 |  |
         m2------+  |
          - k0:<----+
    */
    let mut doc = Doc::new();
    let root = doc.get_or_insert_array("array");
    let mut txn = doc.transact_mut();

    let m0 = root.insert(&mut txn, 0, MapPrelim::default());
    let m1 = root.insert(&mut txn, 1, MapPrelim::default());
    let m2 = root.insert(&mut txn, 2, MapPrelim::default());

    let l0 = root.quote(&txn, 0..=0).unwrap();
    let l1 = root.quote(&txn, 1..=1).unwrap();
    let l2 = root.quote(&txn, 2..=2).unwrap();

    // create cyclic reference between links
    m0.insert(&mut txn, "k1", l1);
    m1.insert(&mut txn, "k2", l2);
    m2.insert(&mut txn, "k0", l0);
    drop(txn);

    let events = Arc::new(Mutex::new(vec![]));
    let _sub = {
        let events = events.clone();
        m0.observe_deep(move |txn, e| {
            let mut rs = events.lock().unwrap();
            for e in e.iter() {
                if let Event::Map(e) = e {
                    let value = e.target().clone();
                    rs.push((value, e.keys(txn).clone()));
                }
            }
        })
    };

    m1.insert(&mut doc.transact_mut(), "test-key1", "value1");
    let actual = {
        let mut guard = events.lock().unwrap();
        std::mem::take(&mut *guard)
    };
    assert_eq!(
        actual,
        vec![(
            m1.clone(),
            HashMap::from([(
                Arc::from("test-key1"),
                EntryChange::Inserted("value1".into())
            )])
        )]
    );

    m2.insert(&mut doc.transact_mut(), "test-key2", "value2");
    let actual = {
        let mut guard = events.lock().unwrap();
        std::mem::take(&mut *guard)
    };
    assert_eq!(
        actual,
        vec![(
            m2.clone(),
            HashMap::from([(
                Arc::from("test-key2"),
                EntryChange::Inserted("value2".into())
            )])
        )]
    );

    m1.remove(&mut doc.transact_mut(), "test-key1");
    let actual = {
        let mut guard = events.lock().unwrap();
        std::mem::take(&mut *guard)
    };
    assert_eq!(
        actual,
        vec![(
            m1.clone(),
            HashMap::from([(
                Arc::from("test-key1"),
                EntryChange::Removed("value1".into())
            )])
        )]
    );
}

#[test]
fn remote_map_update() {
    let mut d1 = Doc::with_client_id(1);
    let m1 = d1.get_or_insert_map("map");
    let mut d2 = Doc::with_client_id(2);
    let m2 = d2.get_or_insert_map("map");
    let mut d3 = Doc::with_client_id(3);
    let m3 = d3.get_or_insert_map("map");

    m1.insert(&mut d1.transact_mut(), "key", 1);

    exchange_updates(&mut [&mut d1, &mut d2, &mut d3]);

    let l2 = m2.link(&d2.transact(), "key").unwrap();
    m2.insert(&mut d2.transact_mut(), "link", l2);
    m1.insert(&mut d1.transact_mut(), "key", 2);
    m1.insert(&mut d1.transact_mut(), "key", 3);

    // apply updated content first, link second
    exchange_updates(&mut [&mut d3, &mut d1]);
    exchange_updates(&mut [&mut d3, &mut d2]);

    // make sure that link can find the most recent block
    let l3 = m3
        .get(&d3.transact(), "link")
        .unwrap()
        .cast::<WeakRef<MapRef>>()
        .unwrap();
    assert_eq!(l3.try_deref_value(&d3.transact()), Some(3.into()));

    exchange_updates(&mut [&mut d1, &mut d2, &mut d3]);

    let l1 = m1
        .get(&d1.transact(), "link")
        .unwrap()
        .cast::<WeakRef<MapRef>>()
        .unwrap();
    let l2 = m2
        .get(&d2.transact(), "link")
        .unwrap()
        .cast::<WeakRef<MapRef>>()
        .unwrap();

    assert_eq!(l1.try_deref_value(&d1.transact()), Some(3.into()));
    assert_eq!(l2.try_deref_value(&d2.transact()), Some(3.into()));
    assert_eq!(l3.try_deref_value(&d3.transact()), Some(3.into()));
}

#[test]
fn basic_text() {
    let mut d1 = Doc::with_client_id(1);
    let txt1 = d1.get_or_insert_text("text");
    let a1 = d1.get_or_insert_array("array");
    let mut d2 = Doc::with_client_id(2);
    let txt2 = d2.get_or_insert_text("text");

    txt1.insert(&mut d1.transact_mut(), 0, "abcd"); // 'abcd'
    let l1 = {
        let mut txn = d1.transact_mut();
        let q = txt1.quote(&mut txn, 1..=2); // quote: [bc]
        a1.insert(&mut txn, 0, q.unwrap())
    };
    assert_eq!(l1.get_string(&d1.transact()), "bc".to_string());

    txt1.insert(&mut d1.transact_mut(), 2, "ef"); // 'abefcd', quote: [befc]
    assert_eq!(l1.get_string(&d1.transact()), "befc".to_string());

    txt1.remove_range(&mut d1.transact_mut(), 3, 3); // 'abe', quote: [be]
    assert_eq!(l1.get_string(&d1.transact()), "be".to_string());

    txt1.insert_embed(&mut d1.transact_mut(), 3, WeakPrelim::from(l1.clone())); // 'abe[be]'

    exchange_updates(&mut [&mut d1, &mut d2]);

    let diff = txt2.diff(&d2.transact(), YChange::identity);
    let l2 = diff[1].insert.clone().cast::<WeakRef<TextRef>>().unwrap();
    assert_eq!(l2.get_string(&d2.transact()), "be".to_string());
}

#[test]
fn basic_xml_text() {
    let mut d1 = Doc::with_client_id(1);
    let txt1 = d1.get_or_insert_text("text");
    let txt1: &XmlTextRef = txt1.as_ref();
    let a1 = d1.get_or_insert_array("array");
    let mut d2 = Doc::with_client_id(2);
    let txt2 = d2.get_or_insert_text("text");
    let txt2: &XmlTextRef = txt2.as_ref();

    txt1.insert(&mut d1.transact_mut(), 0, "abcd"); // 'abcd'
    let l1 = {
        let mut txn = d1.transact_mut();
        let q = txt1.quote(&mut txn, 1..=2); // quote: [bc]
        a1.insert(&mut txn, 0, q.unwrap())
    };
    assert_eq!(l1.get_string(&d1.transact()), "bc".to_string());

    txt1.insert(&mut d1.transact_mut(), 2, "ef"); // 'abefcd', quote: [befc]
    assert_eq!(l1.get_string(&d1.transact()), "befc".to_string());

    txt1.remove_range(&mut d1.transact_mut(), 3, 3); // 'abe', quote: [be]
    assert_eq!(l1.get_string(&d1.transact()), "be".to_string());

    txt1.insert_embed(&mut d1.transact_mut(), 3, WeakPrelim::from(l1.clone())); // 'abe[be]'

    exchange_updates(&mut [&mut d1, &mut d2]);

    let diff = txt2.diff(&d2.transact(), YChange::identity);
    let l2 = diff[1].insert.clone().cast::<WeakRef<TextRef>>().unwrap();
    assert_eq!(l2.get_string(&d2.transact()), "be".to_string());
}

#[test]
fn quote_formatted_text() {
    let mut doc = Doc::with_client_id(1);
    let txt1 = doc.get_or_insert_text("text1");
    let txt1: &XmlTextRef = txt1.as_ref();
    let txt2 = doc.get_or_insert_text("text2");
    let txt2: &XmlTextRef = txt2.as_ref();
    let array = doc.get_or_insert_array("array");
    txt1.insert(&mut doc.transact_mut(), 0, "abcde");
    let b = Attrs::from([("b".into(), true.into())]);
    let i = Attrs::from([("i".into(), true.into())]);
    txt1.format(&mut doc.transact_mut(), 0, 1, b.clone()); // '<b>a</b>bcde'
    txt1.format(&mut doc.transact_mut(), 1, 3, i.clone()); // '<b>a</b><i>bcd</i>e'
    let l1 = {
        let mut txn = doc.transact_mut();
        let l = txt1.quote(&mut txn, 0..=1).unwrap();
        array.insert(&mut txn, 0, l) // <b>a</b><i>b</i>
    };
    let l2 = {
        let mut txn = doc.transact_mut();
        let l = txt1.quote(&mut txn, 2..=2).unwrap();
        array.insert(&mut txn, 0, l) // <i>c</i>
    };
    let l3 = {
        let mut txn = doc.transact_mut();
        let l = txt1.quote(&mut txn, 3..=4).unwrap();
        array.insert(&mut txn, 0, l) // <i>d</i>e
    };
    assert_eq!(l1.get_string(&doc.transact()), "<b>a</b><i>b</i>");
    assert_eq!(l2.get_string(&doc.transact()), "<i>c</i>");
    assert_eq!(l3.get_string(&doc.transact()), "<i>d</i>e");

    txt2.insert_embed(&mut doc.transact_mut(), 0, WeakPrelim::from(l1.clone()));
    txt2.insert_embed(&mut doc.transact_mut(), 1, WeakPrelim::from(l2.clone()));
    txt2.insert_embed(&mut doc.transact_mut(), 2, WeakPrelim::from(l3.clone()));

    let txn = doc.transact();
    let diff: Vec<_> = txt2
        .diff(&txn, YChange::identity)
        .into_iter()
        .map(|d| {
            d.insert
                .cast::<WeakRef<XmlTextRef>>()
                .unwrap()
                .get_string(&txn)
        })
        .collect();
    assert_eq!(
        diff,
        vec![
            "<b>a</b><i>b</i>".to_string(),
            "<i>c</i>".to_string(),
            "<i>d</i>e".to_string()
        ]
    );
}

fn to_weak_xml_text(weak: &WeakRef<TextRef>) -> WeakRef<XmlTextRef> {
    WeakRef::from(weak.clone().into_inner())
}

#[test]
fn quoted_text_start_boundary_inserts() {
    let mut d1 = Doc::with_client_id(1);
    let arr1 = d1.get_or_insert_array("array");
    let txt1 = d1.get_or_insert_text("text");
    {
        let mut txn = d1.transact_mut();
        txt1.insert(&mut txn, 0, "abcdef"); // t1: 'abcdef'
    }

    let mut d2 = Doc::with_client_id(2);
    let _arr2 = d2.get_or_insert_array("array");
    let txt2 = d2.get_or_insert_text("text");

    exchange_updates(&mut [&mut d1, &mut d2]); // t2: 'abcdef'

    txt2.insert(&mut d2.transact_mut(), 1, "xyz"); // t2: 'axyzbcdef'

    let link_excl = {
        struct RangeLeftExclusive(u32, u32);
        impl RangeBounds<u32> for RangeLeftExclusive {
            fn start_bound(&self) -> Bound<&u32> {
                Bound::Excluded(&self.0)
            }

            fn end_bound(&self) -> Bound<&u32> {
                Bound::Excluded(&self.1)
            }
        }

        let mut txn = d1.transact_mut();
        let q = txt1.quote(&txn, RangeLeftExclusive(0, 5)).unwrap(); // [bcde]
        arr1.insert(&mut txn, 0, q)
    };
    let link_incl = {
        let mut txn = d1.transact_mut();
        let q = txt1.quote(&txn, 1..5).unwrap(); // [bcde]
        arr1.insert(&mut txn, 0, q)
    };
    {
        let txn = d1.transact();
        let str = link_excl.get_string(&txn);
        assert_eq!(&str, "bcde");
        let str = to_weak_xml_text(&link_excl).get_string(&txn);
        assert_eq!(&str, "bcde");
        let str = link_incl.get_string(&txn);
        assert_eq!(&str, "bcde");
        let str = to_weak_xml_text(&link_incl).get_string(&txn);
        assert_eq!(&str, "bcde");
    }

    exchange_updates(&mut [&mut d1, &mut d2]);

    {
        let txn = d1.transact();
        let str = link_excl.get_string(&txn);
        assert_eq!(&str, "xyzbcde");
        let str = to_weak_xml_text(&link_excl).get_string(&txn);
        assert_eq!(&str, "xyzbcde");
        let str = link_incl.get_string(&txn);
        assert_eq!(&str, "bcde");
        let str = to_weak_xml_text(&link_incl).get_string(&txn);
        assert_eq!(&str, "bcde");
    }
}

#[test]
fn quoted_text_end_boundary_inserts() {
    let mut d1 = Doc::with_client_id(1);
    let arr1 = d1.get_or_insert_array("array");
    let txt1 = d1.get_or_insert_text("text");
    {
        let mut txn = d1.transact_mut();
        txt1.insert(&mut txn, 0, "abcdef");
    }

    let mut d2 = Doc::with_client_id(2);
    let _arr2 = d2.get_or_insert_array("array");
    let txt2 = d2.get_or_insert_text("text");

    exchange_updates(&mut [&mut d1, &mut d2]);

    txt2.insert(&mut d2.transact_mut(), 5, "xyz");

    let link_excl = {
        let mut txn = d1.transact_mut();
        let q = txt1.quote(&txn, 1..5).unwrap();
        arr1.insert(&mut txn, 0, q)
    };
    let link_incl = {
        let mut txn = d1.transact_mut();
        let q = txt1.quote(&txn, 1..=4).unwrap();
        arr1.insert(&mut txn, 0, q)
    };

    {
        let txn = d1.transact();
        let str = link_excl.get_string(&txn);
        assert_eq!(&str, "bcde");
        let str = to_weak_xml_text(&link_excl).get_string(&txn);
        assert_eq!(&str, "bcde");
        let str = link_incl.get_string(&txn);
        assert_eq!(&str, "bcde");
        let str = to_weak_xml_text(&link_incl).get_string(&txn);
        assert_eq!(&str, "bcde");
    }

    exchange_updates(&mut [&mut d1, &mut d2]);

    {
        let txn = d1.transact();
        let str = link_excl.get_string(&txn);
        assert_eq!(&str, "bcdexyz");
        let str = to_weak_xml_text(&link_excl).get_string(&txn);
        assert_eq!(&str, "bcdexyz");
        let str = link_incl.get_string(&txn);
        assert_eq!(&str, "bcde");
        let str = to_weak_xml_text(&link_incl).get_string(&txn);
        assert_eq!(&str, "bcde");
    }
}

#[test]
fn quote_end_unbounded_text() {
    let mut d1 = Doc::with_client_id(1);
    let mut txn = d1.transact_mut();
    let txt1 = txn.get_or_insert_text("text");
    let arr1 = txn.get_or_insert_array("array");
    txt1.insert(&mut txn, 0, "abc");
    let link1 = txt1.quote(&txn, 1..).unwrap();
    let link1 = arr1.insert(&mut txn, 0, link1);
    let str = link1.get_string(&txn);
    assert_eq!(str, "bc");

    txt1.push(&mut txn, "def");
    let str = link1.get_string(&txn);
    assert_eq!(str, "bcdef");
    drop(txn);

    let mut d2 = Doc::with_client_id(2);

    exchange_updates(&mut [&mut d1, &mut d2]);

    let mut txn = d2.transact_mut();
    let txt2 = txn.get_or_insert_text("text");
    let arr2 = txn.get_or_insert_array("array");

    let link2 = arr2
        .get(&txn, 0)
        .unwrap()
        .cast::<WeakRef<TextRef>>()
        .unwrap();
    let str = link2.get_string(&txn);
    assert_eq!(str, "bcdef");
}

#[test]
fn quote_start_unbounded_text() {
    let mut d1 = Doc::with_client_id(1);
    let mut txn = d1.transact_mut();
    let txt1 = txn.get_or_insert_text("text");
    let arr1 = txn.get_or_insert_array("array");
    txt1.insert(&mut txn, 0, "xyz");
    let link1 = txt1.quote(&txn, ..=1).unwrap();
    let link1 = arr1.insert(&mut txn, 0, link1);
    let str = link1.get_string(&txn);
    assert_eq!(str, "xy");

    txt1.insert(&mut txn, 0, "uwv"); // 'uwvxyz'
    let str = link1.get_string(&txn);
    assert_eq!(str, "uwvxy");
    drop(txn);

    let mut d2 = Doc::with_client_id(2);

    exchange_updates(&mut [&mut d1, &mut d2]);

    let mut txn = d2.transact_mut();
    let _txt2 = txn.get_or_insert_text("text");
    let arr2 = txn.get_or_insert_array("array");

    let link2 = arr2
        .get(&txn, 0)
        .unwrap()
        .cast::<WeakRef<TextRef>>()
        .unwrap();
    let str = link2.get_string(&txn);
    assert_eq!(str, "uwvxy");
}

#[test]
fn quote_both_sides_unbounded_text() {
    let mut d1 = Doc::with_client_id(1);
    let mut txn = d1.transact_mut();
    let txt1 = txn.get_or_insert_text("text");
    let arr1 = txn.get_or_insert_array("array");
    txt1.insert(&mut txn, 0, "xyz");
    let link1 = txt1.quote(&txn, ..).unwrap();
    let link1 = arr1.insert(&mut txn, 0, link1);
    let str = link1.get_string(&txn);
    assert_eq!(str, "xyz");

    txt1.insert(&mut txn, 0, "uwv"); // 'uwvxyz'
    txt1.push(&mut txn, "abc"); // 'uwvxyzabc'
    let str = link1.get_string(&txn);
    assert_eq!(str, "uwvxyzabc");
    drop(txn);

    let mut d2 = Doc::with_client_id(2);

    exchange_updates(&mut [&mut d1, &mut d2]);

    let mut txn = d2.transact_mut();
    let txt2 = txn.get_or_insert_text("text");
    let arr2 = txn.get_or_insert_array("array");

    let link2 = arr2
        .get(&txn, 0)
        .unwrap()
        .cast::<WeakRef<TextRef>>()
        .unwrap();
    let str = link2.get_string(&txn);
    assert_eq!(str, "uwvxyzabc");
}
