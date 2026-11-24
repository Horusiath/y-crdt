use serde::Deserialize;
use std::collections::Bound;
use std::ops::RangeBounds;
use yrs::lib0::Decode;
use yrs::node::Attrs;
use yrs::test_utils::{exchange_updates, on_commit};
use yrs::{Delta, DeltaOptions, Doc, NodeID, NodeRef, Out, Reporter, Transaction, Update};

/// Dereferences a weak `link` living in `doc`.
fn deref(doc: &Doc, link: &NodeID) -> Option<Out> {
    let txn = doc.transact();
    txn.node(link.clone())?.try_deref()
}

/// Dereferences a weak `link` and returns the `key` attribute of its target.
fn deref_attr(doc: &Doc, link: &NodeID, key: &str) -> Option<Out> {
    let txn = doc.transact();
    let target = txn.node(link.clone())?.try_deref()?.node_id()?;
    txn.node(target)?.attr(key)
}

/// Collects all values within the quoted range of a weak `link`.
fn unquote(doc: &Doc, link: &NodeID) -> Vec<Out> {
    let txn = doc.transact();
    match txn.node(link.clone()) {
        Some(link) => link.unquote().collect(),
        None => vec![],
    }
}

fn sync_into(from: &Doc, to: &mut Doc, r: &mut impl Reporter) {
    let update = from
        .transact()
        .encode_state_as_update_v1(&to.transact().state_vector());
    let mut txn = to.transact_mut();
    txn.apply_update(Update::decode_v1(&update).unwrap())
        .unwrap();
    txn.commit(r);
}

fn deep_events<'a>(root: &NodeID, events: &'a mut Vec<(NodeID, Delta)>) -> impl Reporter + 'a {
    let root = root.clone();
    on_commit(move |t: &Transaction<&Doc>| {
        let mut nodes: Vec<_> = t.modified_within(root.clone()).collect();
        nodes.sort_by_key(|n| n.path().len());
        events.extend(nodes.into_iter().map(|n| (n.id(), t.delta(n.id(), false))));
    })
}

fn target<'a>(link: &NodeID, target: &'a mut Option<NodeID>) -> impl Reporter + 'a {
    let link = link.clone();
    on_commit(move |t: &Transaction<&Doc>| {
        if let Some(id) = t.modified().map(|(n, _)| n.id()).find(|id| *id == link) {
            *target = Some(id);
        }
    })
}

/// Renders a weak `link` as a string.
fn quoted_string(doc: &Doc, link: &NodeID) -> String {
    doc.transact().node(link.clone()).unwrap().to_string()
}

fn quoted_delta(doc: &Doc, link: &NodeID) -> Delta {
    let opts = DeltaOptions {
        link_depth: 1,
        ..DeltaOptions::deep()
    };
    doc.transact().node(link.clone()).unwrap().delta(&opts)
}

#[test]
fn basic_map_link() {
    let mut doc = Doc::new();
    let mut txn = doc.transact_mut();
    let mut map = txn.node_mut("map").unwrap();
    let nested = map
        .insert_attr("a", Delta::new().insert_attr("a1", "hello"))
        .node_id()
        .unwrap();
    let link = map.link("a").unwrap();
    map.insert_attr("b", Delta::link(link));

    let link = map.attr("b").unwrap().node_id().unwrap();
    drop(map);

    let expected = txn.node(nested).unwrap().to_json();
    let target = txn
        .node(link)
        .unwrap()
        .try_deref()
        .unwrap()
        .node_id()
        .unwrap();
    let actual = txn.node(target).unwrap().to_json();

    assert_eq!(actual, expected);
}

#[test]
fn basic_array_link() {
    let mut d1 = Doc::with_client_id(1);
    {
        let mut txn = d1.transact_mut();
        let mut a1 = txn.node_mut("array").unwrap();

        a1.insert_range(0, [1, 2, 3]);
        let link = a1.quote(1..2).unwrap();
        a1.insert(3, Delta::link(link));

        assert_eq!(a1.get(0), Some(1.into()));
        assert_eq!(a1.get(1), Some(2.into()));
        assert_eq!(a1.get(2), Some(3.into()));
        let link = a1.get(3).unwrap().node_id().unwrap();
        drop(a1);

        let link = txn.node(link).unwrap();
        let mut u = link.unquote();
        assert_eq!(u.next(), Some(2.into()));
        assert_eq!(u.next(), None);
    }

    let mut d2 = Doc::new();

    exchange_updates(&mut [&mut d1, &mut d2]);
    let txn = d2.transact_mut();
    let a2 = txn.node("array").unwrap();

    assert_eq!(a2.get(0), Some(1.into()));
    assert_eq!(a2.get(1), Some(2.into()));
    assert_eq!(a2.get(2), Some(3.into()));
    let link = txn.node(a2.get(3).unwrap().node_id().unwrap()).unwrap();
    let actual: Vec<_> = link.unquote().collect();
    assert_eq!(actual, vec![2.into()]);
}

/// Attributes of a nested map node quoted by a link.
#[derive(Debug, PartialEq, Deserialize)]
struct Entry {
    key: String,
}

#[test]
fn array_quote_multi_elements() {
    let mut d1 = Doc::with_client_id(1);
    let mut d2 = Doc::with_client_id(2);

    let nested = {
        let mut txn = d1.transact_mut();
        let mut a1 = txn.node_mut("array").unwrap();
        a1.insert_range(0, [1, 2]);
        let nested = a1.push_back(Delta::new().insert_attr("key", "value"));
        a1.push_back(3);
        nested
    };
    let l1 = {
        let mut t1 = d1.transact_mut();
        let mut a1 = t1.node_mut("array").unwrap();
        let prelim = a1.quote(1..=3).unwrap();
        a1.insert(0, Delta::link(prelim)).node_id().unwrap()
    };

    {
        let t1 = d1.transact();
        let link = t1.node(l1.clone()).unwrap();
        assert_eq!(
            link.unquote().collect::<Vec<Out>>(),
            vec![2.into(), nested.clone(), 3.into()]
        );
        let a1 = t1.node("array").unwrap();
        assert_eq!(a1.get(1), Some(1.into()));
        assert_eq!(a1.get(2), Some(2.into()));
        assert_eq!(a1.get(3), Some(nested.clone()));
        assert_eq!(a1.get(4), Some(3.into()));
    }

    exchange_updates(&mut [&mut d1, &mut d2]);

    let l2 = {
        let t2 = d2.transact();
        let a2 = t2.node("array").unwrap();
        let l2 = a2.get(0).unwrap().node_id().unwrap();
        let link = t2.node(l2.clone()).unwrap();
        assert_eq!(
            link.unquote().collect::<Vec<Out>>(),
            vec![2.into(), nested.clone(), 3.into()]
        );
        assert_eq!(a2.get(1), Some(1.into()));
        assert_eq!(a2.get(2), Some(2.into()));
        assert_eq!(a2.get(3), Some(nested.clone()));
        assert_eq!(a2.get(4), Some(3.into()));
        let entry: Entry = t2
            .node(nested.clone().node_id().unwrap())
            .unwrap()
            .deserialize()
            .unwrap();
        assert_eq!(
            entry,
            Entry {
                key: "value".into()
            }
        );
        l2
    };

    d2.transact_mut()
        .node_mut("array")
        .unwrap()
        .insert_range(3, ["A", "B"]);

    {
        let t2 = d2.transact();
        let link = t2.node(l2).unwrap();
        assert_eq!(
            link.unquote().collect::<Vec<Out>>(),
            vec![2.into(), "A".into(), "B".into(), nested.clone(), 3.into()]
        );
    }

    exchange_updates(&mut [&mut d1, &mut d2]);

    let t1 = d1.transact();
    assert_eq!(
        t1.node(l1).unwrap().unquote().collect::<Vec<Out>>(),
        vec![2.into(), "A".into(), "B".into(), nested.clone(), 3.into()]
    );
}

#[test]
fn self_quotation() {
    let mut d1 = Doc::with_client_id(1);
    let mut d2 = Doc::with_client_id(2);

    d1.transact_mut()
        .node_mut("array")
        .unwrap()
        .insert_range(0, [1, 2, 3, 4]);
    let l1 = {
        let mut txn = d1.transact_mut();
        let mut a1 = txn.node_mut("array").unwrap();
        let q = a1.quote(0..3).unwrap();
        // link is inserted into its own range
        a1.insert(1, Delta::link(q))
    };
    {
        let t1 = d1.transact();
        let link = t1.node(l1.clone().node_id().unwrap()).unwrap();
        let mut u = link.unquote();
        assert_eq!(u.next(), Some(1.into()));
        assert_eq!(u.next(), Some(l1.clone()));
        assert_eq!(u.next(), Some(2.into()));
        assert_eq!(u.next(), Some(3.into()));

        let a1 = t1.node("array").unwrap();
        assert_eq!(a1.get(0), Some(1.into()));
        assert_eq!(a1.get(1), Some(l1.clone()));
        assert_eq!(a1.get(2), Some(2.into()));
        assert_eq!(a1.get(3), Some(3.into()));
        assert_eq!(a1.get(4), Some(4.into()));
    }

    exchange_updates(&mut [&mut d1, &mut d2]);

    let t2 = d2.transact();
    let a2 = t2.node("array").unwrap();
    let l2 = a2.get(1).unwrap();
    let link = t2.node(l2.clone().node_id().unwrap()).unwrap();
    let unquote: Vec<_> = link.unquote().collect();
    assert_eq!(unquote, vec![1.into(), l2.clone(), 2.into(), 3.into()]);
    assert_eq!(a2.get(0), Some(1.into()));
    assert_eq!(a2.get(1), Some(l2));
    assert_eq!(a2.get(2), Some(2.into()));
    assert_eq!(a2.get(3), Some(3.into()));
    assert_eq!(a2.get(4), Some(4.into()));
}

#[test]
fn update() {
    let mut d1 = Doc::new();
    let mut d2 = Doc::new();

    let link1 = {
        let mut txn = d1.transact_mut();
        let mut m1 = txn.node_mut("map").unwrap();
        m1.insert_attr("a", Delta::new().insert_attr("a1", "hello"));
        let link = m1.link("a").unwrap();
        m1.insert_attr("b", Delta::link(link)).node_id().unwrap()
    };

    exchange_updates(&mut [&mut d1, &mut d2]);

    let link2 = d2
        .transact()
        .node("map")
        .unwrap()
        .attr("b")
        .unwrap()
        .node_id()
        .unwrap();
    assert_eq!(deref_attr(&d1, &link1, "a1"), deref_attr(&d2, &link2, "a1"));

    d2.transact_mut()
        .node_mut("map")
        .unwrap()
        .insert_attr("a2", "world");

    exchange_updates(&mut [&mut d1, &mut d2]);

    assert_eq!(deref_attr(&d1, &link1, "a2"), deref_attr(&d2, &link2, "a2"));
}

#[test]
#[cfg_attr(target_os = "windows", ignore)]
fn delete_weak_link() {
    let mut d1 = Doc::new();
    let mut d2 = Doc::new();

    let link1 = {
        let mut txn = d1.transact_mut();
        let mut m1 = txn.node_mut("map").unwrap();
        m1.insert_attr("a", Delta::new().insert_attr("a1", "hello"));
        let link = m1.link("a").unwrap();
        m1.insert_attr("b", Delta::link(link)).node_id().unwrap()
    };

    exchange_updates(&mut [&mut d1, &mut d2]);

    let link2 = d2
        .transact()
        .node("map")
        .unwrap()
        .attr("b")
        .unwrap()
        .node_id()
        .unwrap();
    assert_eq!(deref_attr(&d1, &link1, "a1"), deref_attr(&d2, &link2, "a1"));

    d2.transact_mut().node_mut("map").unwrap().remove_attr("b"); // delete links

    exchange_updates(&mut [&mut d1, &mut d2]);

    // since links have been deleted, they no longer refer to any content
    assert_eq!(deref(&d1, &link1), None);
    assert_eq!(deref(&d2, &link2), None);
}

#[test]
fn delete_source() {
    let mut d1 = Doc::new();
    let mut d2 = Doc::new();

    let link1 = {
        let mut txn = d1.transact_mut();
        let mut m1 = txn.node_mut("map").unwrap();
        m1.insert_attr("a", Delta::new().insert_attr("a1", "hello"));
        let link = m1.link("a").unwrap();
        m1.insert_attr("b", Delta::link(link)).node_id().unwrap()
    };

    exchange_updates(&mut [&mut d1, &mut d2]);

    let link2 = d2
        .transact()
        .node("map")
        .unwrap()
        .attr("b")
        .unwrap()
        .node_id()
        .unwrap();
    assert_eq!(deref_attr(&d1, &link1, "a1"), deref_attr(&d2, &link2, "a1"));

    d2.transact_mut().node_mut("map").unwrap().remove_attr("a"); // delete source of the link

    exchange_updates(&mut [&mut d1, &mut d2]);

    // since links have been deleted, they no longer refer to any content
    assert_eq!(deref(&d1, &link1), None);
    assert_eq!(deref(&d2, &link2), None);
}

#[test]
fn observe_map_update() {
    let mut d1 = Doc::new();
    let mut d2 = Doc::new();

    let link1 = {
        let mut txn = d1.transact_mut();
        let mut m1 = txn.node_mut("map").unwrap();
        m1.insert_attr("a", "value");
        let link1 = m1.link("a").unwrap();
        m1.insert_attr("b", Delta::link(link1)).node_id().unwrap()
    };

    exchange_updates(&mut [&mut d1, &mut d2]);

    let link2 = d2
        .transact()
        .node("map")
        .unwrap()
        .attr("b")
        .unwrap()
        .node_id()
        .unwrap();
    assert_eq!(deref(&d2, &link2), Some("value".into()));

    d1.transact_mut()
        .node_mut("map")
        .unwrap()
        .insert_attr("a", "value2");
    assert_eq!(deref(&d1, &link1), Some("value2".into()));

    exchange_updates(&mut [&mut d1, &mut d2]);
    assert_eq!(deref(&d2, &link2), Some("value2".into()));
}

#[test]
fn observe_map_delete() {
    let mut d1 = Doc::new();
    let mut d2 = Doc::new();

    let link1 = {
        let mut txn = d1.transact_mut();
        let mut m1 = txn.node_mut("map").unwrap();
        m1.insert_attr("a", "value");
        let link1 = m1.link("a").unwrap();
        m1.insert_attr("b", Delta::link(link1)).node_id().unwrap()
    };

    exchange_updates(&mut [&mut d1, &mut d2]);

    let link2 = d2
        .transact()
        .node("map")
        .unwrap()
        .attr("b")
        .unwrap()
        .node_id()
        .unwrap();
    assert_eq!(deref(&d2, &link2), Some("value".into()));

    let (mut target1, mut target2) = (None, None);
    let mut txn = d1.transact_mut();
    txn.node_mut("map").unwrap().remove_attr("a");
    txn.commit(&mut on_commit(|t| {
        target1 = t.modified().map(|(n, _)| n.id()).find(|id| *id == link1)
    }));
    let l1 = target1.unwrap();
    assert_eq!(deref(&d1, &l1), None);

    sync_into(
        &d1,
        &mut d2,
        &mut on_commit(|t| target2 = t.modified().map(|(n, _)| n.id()).find(|id| *id == link2)),
    );
    let l2 = target2.unwrap();
    assert_eq!(deref(&d2, &l2), None);
}

#[test]
fn observe_array() {
    let mut d1 = Doc::with_client_id(1);
    let mut d2 = Doc::with_client_id(2);

    let link1 = {
        let mut txn = d1.transact_mut();
        let mut a1 = txn.node_mut("array").unwrap();
        a1.insert_range(0, ["A", "B", "C"]);
        let link1 = a1.quote(1..=2).unwrap();
        a1.insert(0, Delta::link(link1)).node_id().unwrap()
    };

    exchange_updates(&mut [&mut d1, &mut d2]);

    let link2 = d2
        .transact()
        .node("array")
        .unwrap()
        .get(0)
        .unwrap()
        .node_id()
        .unwrap();
    assert_eq!(unquote(&d2, &link2), vec!["B".into(), "C".into()]);

    let (mut target1, mut target2) = (None, None);

    let mut txn = d1.transact_mut();
    txn.node_mut("array").unwrap().remove(2, 1);
    txn.commit(&mut target(&link1, &mut target1));
    assert_eq!(unquote(&d1, &link1), vec!["C".into()]);

    sync_into(&d1, &mut d2, &mut target(&link2, &mut target2));
    let l2 = target2.take().unwrap();
    assert_eq!(unquote(&d2, &l2), vec!["C".into()]);

    let mut txn = d2.transact_mut();
    txn.node_mut("array").unwrap().remove(2, 1);
    txn.commit(&mut target(&link2, &mut target2));
    let l2 = target2.take().unwrap();
    assert_eq!(unquote(&d2, &l2), vec![]);

    // link1 isn't modified by this sync, `target1` is still set by the local remove above
    sync_into(&d2, &mut d1, &mut target(&link1, &mut target1));
    let l1 = target1.take().unwrap();
    assert_eq!(unquote(&d1, &l1), vec![]);

    let mut txn = d1.transact_mut();
    txn.node_mut("array").unwrap().remove(1, 1);
    txn.commit(&mut target(&link1, &mut target1));
    assert_eq!(target1.take(), None);
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
    let link2 = {
        // test observers in a face of linked chains of values
        let mut txn = doc.transact_mut();
        let mut m2 = txn.node_mut("map2").unwrap();
        m2.insert_attr("key", "value1");
        let link1 = m2.link("key").unwrap();
        drop(m2);

        let mut m1 = txn.node_mut("map1").unwrap();
        m1.insert_attr("link-key", Delta::link(link1));
        let link2 = m1.link("link-key").unwrap();
        drop(m1);

        txn.node_mut("map2")
            .unwrap()
            .insert_attr("link-link", Delta::link(link2))
            .node_id()
            .unwrap()
    };

    let mut events = vec![];
    let mut txn = doc.transact_mut();
    txn.node_mut("map2").unwrap().insert_attr("key", "value2");
    txn.commit(&mut on_commit(|t| {
        events.extend(t.modified_within(link2.clone()).map(|n| n.id()))
    }));
    let actual: Vec<_> = events.iter().flat_map(|id| deref(&doc, id)).collect();
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
    let link3 = {
        // test observers in a face of multi-layer linked chains of values
        let mut txn = doc.transact_mut();
        let mut m2 = txn.node_mut("map2").unwrap();
        m2.insert_attr("key", "value1");
        let link1 = m2.link("key").unwrap();
        drop(m2);

        let mut m1 = txn.node_mut("map1").unwrap();
        m1.insert_attr("link-key", Delta::link(link1));
        let link2 = m1.link("link-key").unwrap();
        drop(m1);

        let mut m2 = txn.node_mut("map2").unwrap();
        m2.insert_attr("link-link", Delta::link(link2));
        let link3 = m2.link("link-link").unwrap();
        drop(m2);

        txn.node_mut("map3")
            .unwrap()
            .insert_attr("link-link-link", Delta::link(link3))
            .node_id()
            .unwrap()
    };

    let mut events = vec![];
    let mut txn = doc.transact_mut();
    txn.node_mut("map2").unwrap().insert_attr("key", "value2");
    txn.commit(&mut on_commit(|t| {
        events.extend(t.modified_within(link3.clone()).map(|n| n.id()))
    }));
    let actual = std::mem::take(&mut events);
    let actual: Vec<_> = actual.into_iter().flat_map(|id| deref(&doc, &id)).collect();
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

    let mut events = vec![];

    let (nested, link) = {
        let mut txn = doc.transact_mut();
        let mut array = txn.node_mut("array").unwrap();
        let nested = array.insert(0, Delta::new()).node_id().unwrap();
        let link = array.quote(0..=0).unwrap();
        drop(array);
        let link = txn
            .node_mut("map")
            .unwrap()
            .insert_attr("link", Delta::link(link))
            .node_id()
            .unwrap();
        (nested, link)
    };

    // update entry in linked map
    events.clear();
    let mut txn = doc.transact_mut();
    txn.node_mut(nested.clone())
        .unwrap()
        .insert_attr("key", "value");
    txn.commit(&mut deep_events(&NodeID::from("map"), &mut events));
    let actual = std::mem::take(&mut events);
    assert_eq!(
        actual,
        vec![(nested.clone(), Delta::out().insert_attr("key", "value"))]
    );

    // delete entry in linked map
    let mut txn = doc.transact_mut();
    txn.node_mut(nested.clone()).unwrap().remove_attr("key");
    txn.commit(&mut deep_events(&NodeID::from("map"), &mut events));
    let actual = std::mem::take(&mut events);
    assert_eq!(
        actual,
        vec![(nested.clone(), Delta::out().remove_attr("key"),)]
    );

    // delete linked map
    let mut txn = doc.transact_mut();
    txn.node_mut("array").unwrap().remove(0, 1);
    txn.commit(&mut deep_events(&NodeID::from("map"), &mut events));
    let actual = std::mem::take(&mut events);
    assert_eq!(actual, vec![(link.clone(), Delta::out())]);
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

    let (nested, link) = {
        let mut txn = doc.transact_mut();
        let mut map = txn.node_mut("map").unwrap();
        let nested = map.insert_attr("nested", Delta::new()).node_id().unwrap();
        let link = map.link("nested").unwrap();
        drop(map);
        let link = txn
            .node_mut("array")
            .unwrap()
            .insert(0, Delta::link(link))
            .node_id()
            .unwrap();
        (nested, link)
    };

    let mut events = vec![];
    let mut txn = doc.transact_mut();
    txn.node_mut(nested.clone())
        .unwrap()
        .insert_attr("key", "value");
    txn.commit(&mut deep_events(&NodeID::from("array"), &mut events));
    let actual = std::mem::take(&mut events);
    assert_eq!(
        actual,
        vec![(nested.clone(), Delta::out().insert_attr("key", "value"))]
    );

    // update existing entry
    let mut txn = doc.transact_mut();
    txn.node_mut(nested.clone())
        .unwrap()
        .insert_attr("key", "value2");
    txn.commit(&mut deep_events(&NodeID::from("array"), &mut events));
    let actual = std::mem::take(&mut events);
    assert_eq!(
        actual,
        vec![(nested.clone(), Delta::out().insert_attr("key", "value2"),)]
    );

    // delete entry in linked map
    let mut txn = doc.transact_mut();
    txn.node_mut(nested.clone()).unwrap().remove_attr("key");
    txn.commit(&mut deep_events(&NodeID::from("array"), &mut events));
    let actual = std::mem::take(&mut events);
    assert_eq!(
        actual,
        vec![(nested.clone(), Delta::out().remove_attr("key"))]
    );

    // delete linked map
    let mut txn = doc.transact_mut();
    txn.node_mut("map").unwrap().remove_attr("nested");
    txn.commit(&mut deep_events(&NodeID::from("array"), &mut events));
    let actual = std::mem::take(&mut events);
    assert_eq!(actual, vec![(link.clone(), Delta::out())]);
}

#[test]
fn deep_observe_new_element_within_quoted_range() {
    let mut d1 = Doc::with_client_id(1);
    let mut d2 = Doc::with_client_id(2);

    {
        let mut t1 = d1.transact_mut();
        let mut a1 = t1.node_mut("array").unwrap();
        a1.push_back(1);
        a1.push_back(Delta::new());
        a1.push_back(Delta::new());
        a1.push_back(2);
    }
    let l1 = {
        let mut t1 = d1.transact_mut();
        let mut a1 = t1.node_mut("array").unwrap();
        let link = a1.quote(1..=2).unwrap();
        a1.insert(0, Delta::link(link)).node_id().unwrap()
    };

    exchange_updates(&mut [&mut d1, &mut d2]);

    let mut e1 = vec![];

    let l2 = d2
        .transact()
        .node("array")
        .unwrap()
        .get(0)
        .unwrap()
        .node_id()
        .unwrap();
    let mut e2 = vec![];

    let m20 = d1
        .transact_mut()
        .node_mut("array")
        .unwrap()
        .insert(3, Delta::new())
        .node_id()
        .unwrap();
    exchange_updates(&mut [&mut d1, &mut d2]);
    let mut txn = d1.transact_mut();
    txn.node_mut(m20.clone())
        .unwrap()
        .insert_attr("key", "value");
    txn.commit(&mut deep_events(&l1, &mut e1));
    assert_eq!(
        &e1,
        &vec![(m20.clone(), Delta::out().insert_attr("key", "value"))]
    );

    sync_into(&d1, &mut d2, &mut deep_events(&l2, &mut e2));

    let m21 = d2
        .transact()
        .node("array")
        .unwrap()
        .get(3)
        .unwrap()
        .node_id()
        .unwrap();
    assert_eq!(
        &e2,
        &vec![(m21.clone(), Delta::out().insert_attr("key", "value"))]
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
    let (m0, m1, m2) = {
        let mut txn = doc.transact_mut();
        let mut root = txn.node_mut("array").unwrap();

        let m0 = root.insert(0, Delta::new()).node_id().unwrap();
        let m1 = root.insert(1, Delta::new()).node_id().unwrap();
        let m2 = root.insert(2, Delta::new()).node_id().unwrap();

        let l0 = root.quote(0..=0).unwrap();
        let l1 = root.quote(1..=1).unwrap();
        let l2 = root.quote(2..=2).unwrap();
        drop(root);

        // create cyclic reference between links
        txn.node_mut(m0.clone())
            .unwrap()
            .insert_attr("k1", Delta::link(l1));
        txn.node_mut(m1.clone())
            .unwrap()
            .insert_attr("k2", Delta::link(l2));
        txn.node_mut(m2.clone())
            .unwrap()
            .insert_attr("k0", Delta::link(l0));
        (m0, m1, m2)
    };

    let mut events = vec![];

    let mut txn = doc.transact_mut();
    txn.node_mut(m1.clone())
        .unwrap()
        .insert_attr("test-key1", "value1");
    txn.commit(&mut deep_events(&m0, &mut events));
    let actual = std::mem::take(&mut events);
    assert_eq!(
        actual,
        vec![(m1.clone(), Delta::out().insert_attr("test-key1", "value1"))]
    );

    let mut txn = doc.transact_mut();
    txn.node_mut(m2.clone())
        .unwrap()
        .insert_attr("test-key2", "value2");
    txn.commit(&mut deep_events(&m0, &mut events));
    let actual = std::mem::take(&mut events);
    assert_eq!(
        actual,
        vec![(m2.clone(), Delta::out().insert_attr("test-key2", "value2"))]
    );

    let mut txn = doc.transact_mut();
    txn.node_mut(m1.clone()).unwrap().remove_attr("test-key1");
    txn.commit(&mut deep_events(&m0, &mut events));
    let actual = std::mem::take(&mut events);
    assert_eq!(
        actual,
        vec![(m1.clone(), Delta::out().remove_attr("test-key1"))]
    );
}

#[test]
fn remote_map_update() {
    let mut d1 = Doc::with_client_id(1);
    let mut d2 = Doc::with_client_id(2);
    let mut d3 = Doc::with_client_id(3);

    d1.transact_mut()
        .node_mut("map")
        .unwrap()
        .insert_attr("key", 1);

    exchange_updates(&mut [&mut d1, &mut d2, &mut d3]);

    {
        let mut txn = d2.transact_mut();
        let mut m2 = txn.node_mut("map").unwrap();
        let l2 = m2.link("key").unwrap();
        m2.insert_attr("link", Delta::link(l2));
    }
    d1.transact_mut()
        .node_mut("map")
        .unwrap()
        .insert_attr("key", 2);
    d1.transact_mut()
        .node_mut("map")
        .unwrap()
        .insert_attr("key", 3);

    // apply updated content first, link second
    exchange_updates(&mut [&mut d3, &mut d1]);
    exchange_updates(&mut [&mut d3, &mut d2]);

    // make sure that link can find the most recent block
    let l3 = d3
        .transact()
        .node("map")
        .unwrap()
        .attr("link")
        .unwrap()
        .node_id()
        .unwrap();
    assert_eq!(deref(&d3, &l3), Some(3.into()));

    exchange_updates(&mut [&mut d1, &mut d2, &mut d3]);

    let l1 = d1
        .transact()
        .node("map")
        .unwrap()
        .attr("link")
        .unwrap()
        .node_id()
        .unwrap();
    let l2 = d2
        .transact()
        .node("map")
        .unwrap()
        .attr("link")
        .unwrap()
        .node_id()
        .unwrap();

    assert_eq!(deref(&d1, &l1), Some(3.into()));
    assert_eq!(deref(&d2, &l2), Some(3.into()));
    assert_eq!(deref(&d3, &l3), Some(3.into()));
}

#[test]
fn basic_text() {
    let mut d1 = Doc::with_client_id(1);
    let mut d2 = Doc::with_client_id(2);

    d1.transact_mut()
        .node_mut("text")
        .unwrap()
        .insert_text(0, "abcd"); // 'abcd'
    let l1 = {
        let mut txn = d1.transact_mut();
        let q = txn.node("text").unwrap().quote(1..=2).unwrap(); // quote: [bc]
        txn.node_mut("array")
            .unwrap()
            .insert(0, Delta::link(q))
            .node_id()
            .unwrap()
    };
    assert_eq!(quoted_string(&d1, &l1), "bc".to_string());

    d1.transact_mut()
        .node_mut("text")
        .unwrap()
        .insert_text(2, "ef"); // 'abefcd', quote: [befc]
    assert_eq!(quoted_string(&d1, &l1), "befc".to_string());

    d1.transact_mut().node_mut("text").unwrap().remove(3, 3); // 'abe', quote: [be]
    assert_eq!(quoted_string(&d1, &l1), "be".to_string());

    {
        let mut txn = d1.transact_mut();
        let source = txn.node(l1.clone()).unwrap().source().clone();
        txn.node_mut("text").unwrap().insert(3, Delta::link(source)); // 'abe[be]'
    }

    exchange_updates(&mut [&mut d1, &mut d2]);

    let l2 = d2
        .transact()
        .node("text")
        .unwrap()
        .get(3)
        .unwrap()
        .node_id()
        .unwrap();
    assert_eq!(quoted_string(&d2, &l2), "be".to_string());
}

#[test]
fn basic_xml_text() {
    // TODO(unified-api): XmlTextRef and TextRef have been unified into a single node type
    let mut d1 = Doc::with_client_id(1);
    let mut d2 = Doc::with_client_id(2);

    d1.transact_mut()
        .node_mut("text")
        .unwrap()
        .insert_text(0, "abcd"); // 'abcd'
    let l1 = {
        let mut txn = d1.transact_mut();
        let q = txn.node("text").unwrap().quote(1..=2).unwrap(); // quote: [bc]
        txn.node_mut("array")
            .unwrap()
            .insert(0, Delta::link(q))
            .node_id()
            .unwrap()
    };
    assert_eq!(quoted_string(&d1, &l1), "bc".to_string());

    d1.transact_mut()
        .node_mut("text")
        .unwrap()
        .insert_text(2, "ef"); // 'abefcd', quote: [befc]
    assert_eq!(quoted_string(&d1, &l1), "befc".to_string());

    d1.transact_mut().node_mut("text").unwrap().remove(3, 3); // 'abe', quote: [be]
    assert_eq!(quoted_string(&d1, &l1), "be".to_string());

    {
        let mut txn = d1.transact_mut();
        let source = txn.node(l1.clone()).unwrap().source().clone();
        txn.node_mut("text").unwrap().insert(3, Delta::link(source)); // 'abe[be]'
    }

    exchange_updates(&mut [&mut d1, &mut d2]);

    let l2 = d2
        .transact()
        .node("text")
        .unwrap()
        .get(3)
        .unwrap()
        .node_id()
        .unwrap();
    assert_eq!(quoted_string(&d2, &l2), "be".to_string());
}

#[test]
fn quote_formatted_text() {
    let mut doc = Doc::with_client_id(1);
    let b = Attrs::from([("b".into(), true.into())]);
    let i = Attrs::from([("i".into(), true.into())]);
    {
        let mut txn = doc.transact_mut();
        let mut txt1 = txn.node_mut("text1").unwrap();
        txt1.insert_text(0, "abcde");
        txt1.format(0, 1, &b); // '<b>a</b>bcde'
        txt1.format(1, 3, &i); // '<b>a</b><i>bcd</i>e'
    }
    let (l1, l2, l3) = {
        let mut txn = doc.transact_mut();
        let q1 = txn.node("text1").unwrap().quote(0..=1).unwrap();
        let l1 = txn
            .node_mut("array")
            .unwrap()
            .insert(0, Delta::link(q1)) // <b>a</b><i>b</i>
            .node_id()
            .unwrap();
        let q2 = txn.node("text1").unwrap().quote(2..=2).unwrap();
        let l2 = txn
            .node_mut("array")
            .unwrap()
            .insert(0, Delta::link(q2)) // <i>c</i>
            .node_id()
            .unwrap();
        let q3 = txn.node("text1").unwrap().quote(3..=4).unwrap();
        let l3 = txn
            .node_mut("array")
            .unwrap()
            .insert(0, Delta::link(q3)) // <i>d</i>e
            .node_id()
            .unwrap();
        (l1, l2, l3)
    };
    let expected = vec![
        Delta::out()
            .insert_text_fmt("a", &b)
            .insert_text_fmt("b", &i)
            .children,
        Delta::out().insert_text_fmt("c", &i).children,
        Delta::out()
            .insert_text_fmt("d", &i)
            .insert_text("e")
            .children,
    ];
    let quotes: Vec<_> = [&l1, &l2, &l3]
        .into_iter()
        .map(|link| quoted_delta(&doc, link).children)
        .collect();
    assert_eq!(quotes, expected);

    {
        let mut txn = doc.transact_mut();
        for (index, link) in [&l1, &l2, &l3].into_iter().enumerate() {
            let source = txn.node(link.clone()).unwrap().source().clone();
            txn.node_mut("text2")
                .unwrap()
                .insert(index as u32, Delta::link(source));
        }
    }

    let links: Vec<_> = doc.transact().node("text2").unwrap().iter().collect();
    let quotes: Vec<_> = links
        .into_iter()
        .map(|v| quoted_delta(&doc, &v.node_id().unwrap()).children)
        .collect();
    assert_eq!(quotes, expected);
}

#[test]
fn map_link_delta() {
    let mut doc = Doc::new();
    let mut txn = doc.transact_mut();
    let mut map = txn.node_mut("map").unwrap();
    map.insert_attr("a", "hello");
    let link = map.link("a").unwrap();
    map.insert_attr("b", Delta::link(link));
    let nested = map
        .insert_attr("c", Delta::new().insert_attr("x", "nested"))
        .node_id()
        .unwrap();
    let link = map.link("c").unwrap();
    map.insert_attr("d", Delta::link(link));
    drop(map);
    drop(txn);

    // {a:"hello", b:"hello", c:{x:"nested"}, d:{x:"nested"}}
    let expected = |a: &str| {
        let c = Out::node_with_delta(nested.clone(), Delta::out().insert_attr("x", "nested"));
        Delta::out()
            .insert_attr("a", a)
            .insert_attr("b", a)
            .insert_attr("c", c.clone())
            .insert_attr("d", c)
            .attrs
    };
    assert_eq!(quoted_delta(&doc, &"map".into()).attrs, expected("hello"));

    doc.transact_mut()
        .node_mut("map")
        .unwrap()
        .insert_attr("a", "world");
    assert_eq!(quoted_delta(&doc, &"map".into()).attrs, expected("world"));
}

#[test]
fn quoted_text_start_boundary_inserts() {
    let mut d1 = Doc::with_client_id(1);
    d1.transact_mut()
        .node_mut("text")
        .unwrap()
        .insert_text(0, "abcdef"); // t1: 'abcdef'

    let mut d2 = Doc::with_client_id(2);

    exchange_updates(&mut [&mut d1, &mut d2]); // t2: 'abcdef'

    d2.transact_mut()
        .node_mut("text")
        .unwrap()
        .insert_text(1, "xyz"); // t2: 'axyzbcdef'

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
        let q = txn
            .node("text")
            .unwrap()
            .quote(RangeLeftExclusive(0, 5))
            .unwrap(); // [bcde]
        txn.node_mut("array")
            .unwrap()
            .insert(0, Delta::link(q))
            .node_id()
            .unwrap()
    };
    let link_incl = {
        let mut txn = d1.transact_mut();
        let q = txn.node("text").unwrap().quote(1..5).unwrap(); // [bcde]
        txn.node_mut("array")
            .unwrap()
            .insert(0, Delta::link(q))
            .node_id()
            .unwrap()
    };
    {
        let txn = d1.transact();
        let excl = txn.node(link_excl.clone()).unwrap();
        let incl = txn.node(link_incl.clone()).unwrap();
        let str = excl.to_string();
        assert_eq!(&str, "bcde");
        let str = incl.to_string();
        assert_eq!(&str, "bcde");
    }

    exchange_updates(&mut [&mut d1, &mut d2]);

    {
        let txn = d1.transact();
        let excl = txn.node(link_excl).unwrap();
        let incl = txn.node(link_incl).unwrap();
        let str = excl.to_string();
        assert_eq!(&str, "xyzbcde");
        let str = incl.to_string();
        assert_eq!(&str, "bcde");
    }
}

#[test]
fn quoted_text_end_boundary_inserts() {
    let mut d1 = Doc::with_client_id(1);
    d1.transact_mut()
        .node_mut("text")
        .unwrap()
        .insert_text(0, "abcdef");

    let mut d2 = Doc::with_client_id(2);

    exchange_updates(&mut [&mut d1, &mut d2]);

    d2.transact_mut()
        .node_mut("text")
        .unwrap()
        .insert_text(5, "xyz");

    let link_excl = {
        let mut txn = d1.transact_mut();
        let q = txn.node("text").unwrap().quote(1..5).unwrap();
        txn.node_mut("array")
            .unwrap()
            .insert(0, Delta::link(q))
            .node_id()
            .unwrap()
    };
    let link_incl = {
        let mut txn = d1.transact_mut();
        let q = txn.node("text").unwrap().quote(1..=4).unwrap();
        txn.node_mut("array")
            .unwrap()
            .insert(0, Delta::link(q))
            .node_id()
            .unwrap()
    };

    {
        let txn = d1.transact();
        let excl = txn.node(link_excl.clone()).unwrap();
        let incl = txn.node(link_incl.clone()).unwrap();
        let str = excl.to_string();
        assert_eq!(&str, "bcde");
        let str = incl.to_string();
        assert_eq!(&str, "bcde");
    }

    exchange_updates(&mut [&mut d1, &mut d2]);

    {
        let txn = d1.transact();
        let excl = txn.node(link_excl).unwrap();
        let incl = txn.node(link_incl).unwrap();
        let str = excl.to_string();
        assert_eq!(&str, "bcdexyz");
        let str = incl.to_string();
        assert_eq!(&str, "bcde");
    }
}

#[test]
fn quote_end_unbounded_text() {
    let mut d1 = Doc::with_client_id(1);
    let mut txn = d1.transact_mut();
    txn.node_mut("text").unwrap().insert_text(0, "abc");
    let link1 = {
        let q = txn.node("text").unwrap().quote(1..).unwrap();
        txn.node_mut("array")
            .unwrap()
            .insert(0, Delta::link(q))
            .node_id()
            .unwrap()
    };
    let str = txn.node(link1.clone()).unwrap().to_string();
    assert_eq!(str, "bc");

    txn.node_mut("text").unwrap().push_text("def");
    let str = txn.node(link1).unwrap().to_string();
    assert_eq!(str, "bcdef");
    drop(txn);

    let mut d2 = Doc::with_client_id(2);

    exchange_updates(&mut [&mut d1, &mut d2]);

    let txn = d2.transact_mut();
    let link2 = txn
        .node("array")
        .unwrap()
        .get(0)
        .unwrap()
        .node_id()
        .unwrap();
    let str = txn.node(link2).unwrap().to_string();
    assert_eq!(str, "bcdef");
}

#[test]
fn quote_start_unbounded_text() {
    let mut d1 = Doc::with_client_id(1);
    let mut txn = d1.transact_mut();
    txn.node_mut("text").unwrap().insert_text(0, "xyz");
    let link1 = {
        let q = txn.node("text").unwrap().quote(..=1).unwrap();
        txn.node_mut("array")
            .unwrap()
            .insert(0, Delta::link(q))
            .node_id()
            .unwrap()
    };
    let str = txn.node(link1.clone()).unwrap().to_string();
    assert_eq!(str, "xy");

    txn.node_mut("text").unwrap().insert_text(0, "uwv"); // 'uwvxyz'
    let str = txn.node(link1).unwrap().to_string();
    assert_eq!(str, "uwvxy");
    drop(txn);

    let mut d2 = Doc::with_client_id(2);

    exchange_updates(&mut [&mut d1, &mut d2]);

    let txn = d2.transact_mut();
    let link2 = txn
        .node("array")
        .unwrap()
        .get(0)
        .unwrap()
        .node_id()
        .unwrap();
    let str = txn.node(link2).unwrap().to_string();
    assert_eq!(str, "uwvxy");
}

#[test]
fn quote_both_sides_unbounded_text() {
    let mut d1 = Doc::with_client_id(1);
    let mut txn = d1.transact_mut();
    txn.node_mut("text").unwrap().insert_text(0, "xyz");
    let link1 = {
        let q = txn.node("text").unwrap().quote(..).unwrap();
        txn.node_mut("array")
            .unwrap()
            .insert(0, Delta::link(q))
            .node_id()
            .unwrap()
    };
    let str = txn.node(link1.clone()).unwrap().to_string();
    assert_eq!(str, "xyz");

    txn.node_mut("text").unwrap().insert_text(0, "uwv"); // 'uwvxyz'
    txn.node_mut("text").unwrap().push_text("abc"); // 'uwvxyzabc'
    let str = txn.node(link1).unwrap().to_string();
    assert_eq!(str, "uwvxyzabc");
    drop(txn);

    let mut d2 = Doc::with_client_id(2);

    exchange_updates(&mut [&mut d1, &mut d2]);

    let txn = d2.transact_mut();
    let link2 = txn
        .node("array")
        .unwrap()
        .get(0)
        .unwrap()
        .node_id()
        .unwrap();
    let str = txn.node(link2).unwrap().to_string();
    assert_eq!(str, "uwvxyzabc");
}
