use yrs::lib0::Decode;
use yrs::node::Observable;
use yrs::test_utils::exchange_updates;
use yrs::{Acquire, AcquireMut, Attribute, Cell, Delta, Doc, In, Out, StateVector, Update};

#[test]
fn insert_attribute() {
    let mut d1 = Doc::with_client_id(1);
    let mut t1 = d1.transact_mut();
    let Out::Node(xml1) = t1
        .node_mut("xml")
        .unwrap()
        .push_back(In::Node(Box::new(Delta::with_name("div"))))
    else {
        panic!("expected a nested node")
    };
    t1.node_mut(xml1.id.clone())
        .unwrap()
        .insert_attr("height", 10.to_string());
    assert_eq!(
        t1.node(xml1.id).unwrap().attr("height"),
        Some(Out::from("10"))
    );

    let mut d2 = Doc::with_client_id(1);
    let mut t2 = d2.transact_mut();
    let Out::Node(xml2) = t2
        .node_mut("xml")
        .unwrap()
        .push_back(In::Node(Box::new(Delta::with_name("div"))))
    else {
        panic!("expected a nested node")
    };
    let u = t1.encode_state_as_update_v1(&StateVector::default());
    let u = Update::decode_v1(u.as_slice()).unwrap();
    t2.apply_update(u).unwrap();
    assert_eq!(
        t2.node(xml2.id).unwrap().attr("height"),
        Some(Out::from("10"))
    );
}

#[test]
fn event_observers() {
    let mut d1 = Doc::with_client_id(1);
    let xml = {
        let mut txn = d1.transact_mut();
        let out = txn
            .node_mut("xml")
            .unwrap()
            .insert(0, In::Node(Box::new(Delta::with_name("test"))));
        let Out::Node(xml) = out else {
            panic!("expected a nested node")
        };
        xml.id
    };

    let mut d2 = Doc::with_client_id(2);
    {
        let mut txn = d2.transact_mut();
        txn.node_mut("xml").unwrap();
    }
    exchange_updates(&mut [&mut d1, &mut d2]);
    let xml2 = {
        let txn = d2.transact();
        let Some(Out::Node(xml2)) = txn.node("xml").unwrap().get(0) else {
            panic!("expected a nested node")
        };
        xml2.id
    };

    let delta = Cell::new(Delta::out());
    let delta1 = delta.clone();
    let delta2 = delta.clone();
    let delta = || delta.acquire().clone();
    let _sub = {
        let txn = d1.transact();
        txn.node(xml.clone())
            .unwrap()
            .observe(move |e| *delta1.acquire_mut() = e.delta(true))
    };

    // insert attribute
    {
        let mut txn = d1.transact_mut();
        let mut node = txn.node_mut(xml.clone()).unwrap();
        node.insert_attr("key1", "value1");
        node.insert_attr("key2", "value2");
    }
    assert_eq!(
        delta(),
        Delta::with_name("test")
            .insert_attr("key1", "value1")
            .insert_attr("key2", "value2")
    );

    // change and remove attribute
    {
        let mut txn = d1.transact_mut();
        let mut node = txn.node_mut(xml.clone()).unwrap();
        node.insert_attr("key1", "value11");
        node.remove_attr("key2");
    }
    assert_eq!(
        delta(),
        Delta::with_name("test")
            .insert_attr("key1", "value11")
            .remove_attr("key2")
    );

    // add xml elements
    let (n1, n2) = {
        let mut txn = d1.transact_mut();
        let mut node = txn.node_mut(xml.clone()).unwrap();
        let Out::Node(n1) = node.insert(0, In::Node(Box::new(Delta::new().insert_text("")))) else {
            unreachable!()
        };
        let Out::Node(n2) = node.insert(1, In::Node(Box::new(Delta::with_name("div")))) else {
            unreachable!()
        };
        (n1.id, n2.id)
    };
    assert_eq!(
        delta(),
        Delta::with_name("test")
            .insert(Out::node(n1.clone()))
            .insert(Out::node_with_delta(n2.clone(), Delta::with_name("div")))
    );

    // remove and add
    let n3 = {
        let mut txn = d1.transact_mut();
        let mut node = txn.node_mut(xml.clone()).unwrap();
        node.remove(1, 1);
        let Out::Node(n) = node.insert(1, In::Node(Box::new(Delta::with_name("p")))) else {
            unreachable!()
        };
        n.id
    };
    assert_eq!(
        delta(),
        Delta::with_name("test")
            .retain(1)
            .remove(1)
            .insert(Out::node_with_delta(n3.clone(), Delta::with_name("p")))
    );

    // copy updates over
    let _sub = {
        let txn = d2.transact();
        txn.node(xml2).unwrap().observe(move |e| {
            *delta2.acquire_mut() = e.delta(true);
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
    assert_eq!(
        delta(),
        Delta::with_name("test")
            .insert_attr("key1", "value11")
            .insert(Out::node(n1))
            .insert(Out::node_with_delta(n3, Delta::with_name("p")))
    );
}

#[test]
fn serialization_compatibility() {
    /* This binary is result of following Yjs code:
    ```js
        let d1 = new Y.Doc()
        d1.clientID = 1
        let root = d1.get('root', Y.XmlElement)
        let first = new Y.XmlText()
        first.insert(0, 'hello')
        let second = new Y.XmlElement('p')
        root.insert(0, [first,second])

        let expected = Y.encodeStateAsUpdate(d1)
    ``` */
    let expected = &[
        1, 3, 1, 0, 7, 1, 4, 114, 111, 111, 116, 6, 4, 0, 1, 0, 5, 104, 101, 108, 108, 111, 135, 1,
        0, 3, 1, 112, 0,
    ];
    let update = Update::decode_v1(expected).unwrap();
    let mut doc = Doc::with_client_id(1);
    let mut txn = doc.transact_mut();
    txn.apply_update(update).unwrap();

    let actual = txn.encode_state_as_update_v1(&StateVector::default());
    assert_eq!(actual.as_slice(), expected);
    assert_eq!(
        txn.node("root").unwrap().format_xml().to_string(),
        "hello<p></p>"
    );
}

#[test]
fn format_attributes_decode_compatibility_v1() {
    let data = &[
        1, 6, 1, 0, 6, 1, 4, 116, 101, 115, 116, 1, 105, 4, 116, 114, 117, 101, 132, 1, 0, 6, 104,
        101, 108, 108, 111, 32, 132, 1, 6, 5, 119, 111, 114, 108, 100, 134, 1, 11, 1, 105, 4, 110,
        117, 108, 108, 198, 1, 6, 1, 7, 1, 98, 4, 116, 114, 117, 101, 134, 1, 12, 1, 98, 4, 110,
        117, 108, 108, 0,
    ];
    let update = Update::decode_v1(data).unwrap();
    let mut doc = Doc::new();
    let mut txn = doc.transact_mut();

    txn.apply_update(update).unwrap();
    assert_eq!(
        txn.node("test").unwrap().format_xml().to_string(),
        "<i>hello </i><b><i>world</i></b>"
    );

    let actual = txn.encode_state_as_update_v1(&StateVector::default());
    assert_eq!(actual, data);
}

#[test]
fn format_attributes_decode_compatibility_v2() {
    let data = &[
        0, 3, 0, 3, 1, 2, 65, 5, 5, 0, 12, 10, 74, 12, 1, 14, 9, 6, 0, 132, 1, 134, 0, 198, 0, 134,
        26, 19, 116, 101, 115, 116, 105, 104, 101, 108, 108, 111, 32, 119, 111, 114, 108, 100, 105,
        98, 98, 4, 1, 6, 5, 65, 1, 1, 1, 0, 0, 1, 6, 0, 120, 126, 120, 126, 0,
    ];
    let update = Update::decode_v2(data).unwrap();
    let mut doc = Doc::new();
    let mut txn = doc.transact_mut();

    txn.apply_update(update).unwrap();
    assert_eq!(
        txn.node("test").unwrap().format_xml().to_string(),
        "<i>hello </i><b><i>world</i></b>"
    );

    let actual = txn.encode_state_as_update_v2(&StateVector::default());
    assert_eq!(actual, data);
}

#[test]
fn issue_607() {
    let mut doc = Doc::new();
    /* Update below created through yjs (v13.6)
        ```js
        // Create a short GC-prefixed history: client 2 writes an attribute onto a node
        // that client 1 then deletes.
        docA.getXmlFragment('doc').insert(0, [xml('t')]);
        docB.getXmlFragment('doc').get(0).setAttribute('x', 'x');
        docA.getXmlFragment('doc').delete(0, 1);

        // This is the visible tree stored in the "before" update.
        docA.getXmlFragment('doc').insert(0, [xml('p')]);
        docB.getXmlFragment('doc').get(0).insert(0, [xml('a')]);
        Y.encodeStateAsUpdate(left)
        ```
    */
    let u1 = Update::decode_v1(&[
        2, 2, 2, 0, 0, 1, 7, 0, 1, 1, 3, 1, 97, 2, 1, 0, 1, 1, 3, 100, 111, 99, 1, 71, 1, 0, 3, 1,
        112, 2, 2, 1, 0, 1, 1, 1, 0, 1,
    ])
    .unwrap();
    /* Second update made through on top of state from u1:
       ```js
       const parent = docC.getXmlFragment('doc').get(0);
       parent.delete(0, 1);
       parent.insert(0, [xml('b')]);
       ```
    */
    let u2 = Update::decode_v1(&[
        1, 1, 209, 229, 151, 135, 6, 0, 71, 2, 1, 3, 1, 98, 2, 2, 1, 0, 2, 1, 1, 0, 1,
    ])
    .unwrap();
    doc.transact_mut().apply_update(u1).unwrap();
    {
        let txn = doc.transact();
        let Out::Node(n) = txn.node("doc").unwrap().get(0).unwrap() else {
            panic!("expected xml element node");
        };
        let actual = txn.node(n.id).unwrap().format_xml().to_string();
        assert_eq!(actual, "<p><a></a></p>");
    }

    doc.transact_mut().apply_update(u2).unwrap();
    {
        let txn = doc.transact();
        let Out::Node(n) = txn.node("doc").unwrap().get(0).unwrap() else {
            panic!("expected xml element node");
        };
        let actual = txn.node(n.id).unwrap().format_xml().to_string();
        assert_eq!(actual, "<p><b></b></p>");
    }
}

#[test]
fn fragment_attributed_content() {
    use yrs::test_utils::attribute_all;
    use yrs::{Attribution, ContentIds, DeltaOptions};
    let mut doc = Doc::with_options(yrs::Options {
        skip_gc: true,
        ..yrs::Options::with_client_id(yrs::ClientID::new(1))
    });
    let (frag, e1, e2) = {
        let mut txn = doc.transact_mut();
        let fragment = Delta::new()
            .insert(Delta::new().insert_text("hello"))
            .insert(Delta::new());
        let frag = txn
            .node_mut("root")
            .unwrap()
            .insert(0, fragment)
            .node_id()
            .unwrap();
        let frag_ref = txn.node(frag.clone()).unwrap();
        let (e1, e2) = (frag_ref.get(0).unwrap(), frag_ref.get(1).unwrap());
        (frag, e1.node_id().unwrap(), e2.node_id().unwrap())
        // root: [frag:[e1:{"hello"}, e2:{}]]
    };
    let (e3, ids) = {
        let mut txn = doc.transact_mut();
        let mut frag = txn.node_mut(frag.clone()).unwrap();
        frag.remove(0, 1);
        let e3 = frag
            .insert(1, Delta::new().insert_text("world"))
            .node_id()
            .unwrap();
        (e3, ContentIds::from_transaction(&txn))
        // root: [frag:[e2:{}, e3:{"world"}]]
    };
    let fmt = attribute_all(&ids);
    let txn = doc.transact();
    let opts = DeltaOptions::default();
    let actual = txn.node(frag.clone()).unwrap().formatted(&fmt).delta(&opts);
    let deleted = Some(Attribution::delete::<Attribute>([]));
    // e1: <deleted>, e2: <empty>, e3: "world"
    let expected = Delta::out()
        .insert_range_attributed([Out::node(e1.clone())], None, deleted.clone())
        .insert(Out::node(e2))
        .insert_range_attributed(
            [Out::node(e3)],
            None,
            Some(Attribution::insert::<Attribute>([])),
        );
    assert_eq!(actual, expected);
    // deleted nodes can't be reached with `Transaction::node`: check elem1 through a deep render
    let deep = DeltaOptions {
        deep: true,
        ..Default::default()
    };
    let actual = txn.node(frag).unwrap().formatted(&fmt).delta(&deep);
    let hello = Delta::out().insert_text_attributed("hello", None, deleted.clone());
    let elem1 =
        Delta::out().insert_range_attributed([Out::node_with_delta(e1, hello)], None, deleted);
    assert_eq!(actual.children[0], elem1.children[0]);
}

#[test]
fn element_attributed_content() {
    use yrs::test_utils::attribute_all;
    use yrs::{Attribution, ContentIds, DeltaOptions};
    let mut doc = Doc::with_options(yrs::Options {
        skip_gc: true,
        ..yrs::Options::with_client_id(yrs::ClientID::new(1))
    });
    let (e1, e2) = {
        let mut txn = doc.transact_mut();
        let mut p = txn.node_mut("p").unwrap(); // <p></p>
        let e1 = p
            .insert(0, Delta::new().insert_text("hello"))
            .node_id()
            .unwrap();
        let e2 = p.insert(1, Delta::with_name("span")).node_id().unwrap();
        (e1, e2)
        // <p>{hello}<span/></p>
    };
    let (e3, ids) = {
        let mut txn = doc.transact_mut();
        let mut p = txn.node_mut("p").unwrap();
        p.remove(0, 1); // <p><span/></p>
        let e3 = p
            .insert(1, Delta::new().insert_text("world"))
            .node_id()
            .unwrap(); // <p><span/>{world}</p>
        p.insert_attr("key", "42"); // <p key="42"><span/>{world}</p>
        (e3, ContentIds::from_transaction(&txn))
    };
    let fmt = attribute_all(&ids);
    let txn = doc.transact();
    let opts = DeltaOptions {
        deep: true,
        ..Default::default()
    };
    let actual = txn.node("p").unwrap().formatted(&fmt).delta(&opts);
    let deleted = Some(Attribution::delete::<Attribute>([]));
    let inserted = Some(Attribution::insert::<Attribute>([]));
    let hello = Delta::out().insert_text_attributed("hello", None, deleted.clone());
    let world = Delta::out().insert_text_attributed("world", None, inserted.clone());
    let expected = Delta::out()
        .insert_range_attributed([Out::node_with_delta(e1, hello)], None, deleted)
        .insert(Out::node_with_delta(e2, Delta::<Out>::with_name("span")))
        .insert_range_attributed([Out::node_with_delta(e3, world)], None, inserted.clone())
        .insert_attr_attributed("key", "42", inserted);
    // <p key="42">~~{hello}~~<span/>{world}</p>
    assert_eq!(actual, expected);
}
