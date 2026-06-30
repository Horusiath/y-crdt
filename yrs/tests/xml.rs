use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use arc_swap::ArcSwapOption;
use yrs::updates::decoder::Decode;
use yrs::{Any, Doc, Out, StateVector, Update};

#[test]
fn insert_attribute() {
    let mut d1 = Doc::with_client_id(1);
    let f = d1.get_or_insert_xml_fragment("xml");
    let mut t1 = d1.transact_mut();
    let xml1 = f.push_back(&mut t1, XmlElementPrelim::empty("div"));
    xml1.insert_attribute(&mut t1, "height", 10.to_string());
    assert_eq!(xml1.get_attribute(&t1, "height"), Some(Out::from("10")));

    let mut d2 = Doc::with_client_id(1);
    let f = d2.get_or_insert_xml_fragment("xml");
    let mut t2 = d2.transact_mut();
    let xml2 = f.push_back(&mut t2, XmlElementPrelim::empty("div"));
    let u = t1.encode_state_as_update_v1(&StateVector::default());
    let u = Update::decode_v1(u.as_slice()).unwrap();
    t2.apply_update(u).unwrap();
    assert_eq!(xml2.get_attribute(&t2, "height"), Some(Out::from("10")));
}

#[test]
fn tree_walker() {
    let mut doc = Doc::with_client_id(1);
    let root = doc.get_or_insert_xml_fragment("xml");
    let mut txn = doc.transact_mut();
    /*
        <UNDEFINED>
            <p>{txt1}{txt2}</p>
            <p></p>
            <img/>
        </UNDEFINED>
    */
    let p1 = root.push_back(&mut txn, XmlElementPrelim::empty("p"));
    p1.push_back(&mut txn, XmlTextPrelim::new(""));
    p1.push_back(&mut txn, XmlTextPrelim::new(""));
    let p2 = root.push_back(&mut txn, XmlElementPrelim::empty("p"));
    root.push_back(&mut txn, XmlElementPrelim::empty("img"));

    let all_paragraphs = root.successors(&txn).filter_map(|n| match n {
        XmlOut::Element(e) if e.tag() == &"p".into() => Some(e),
        _ => None,
    });
    let actual: Vec<_> = all_paragraphs.collect();

    assert_eq!(
        actual.len(),
        2,
        "query selector should found two paragraphs"
    );
    assert_eq!(
        actual[0].hook(),
        p1.hook(),
        "query selector found 1st paragraph"
    );
    assert_eq!(
        actual[1].hook(),
        p2.hook(),
        "query selector found 2nd paragraph"
    );
}

#[test]
fn text_attributes() {
    let mut doc = Doc::with_client_id(1);
    let f = doc.get_or_insert_xml_fragment("test");
    let mut txn = doc.transact_mut();
    let txt = f.push_back(&mut txn, XmlTextPrelim::new(""));
    txt.insert_attribute(&mut txn, "test", 42.to_string());

    assert_eq!(txt.get_attribute(&txn, "test"), Some(Out::from("42")));
    let actual: Vec<_> = txt.attributes(&txn).collect();
    let expected: Vec<_> = vec![("test", Out::from("42"))].into_iter().collect();
    assert_eq!(actual, expected);
}

#[test]
fn text_attributes_any() {
    let mut doc = Doc::with_client_id(1);
    let f = doc.get_or_insert_xml_fragment("test");
    let mut txn = doc.transact_mut();
    let txt = f.push_back(&mut txn, XmlTextPrelim::new(""));
    txt.insert_attribute(&mut txn, "test", Any::BigInt(42));
    txt.insert_attribute(&mut txn, "test_true", true);
    txt.insert_attribute(&mut txn, "test_null", Any::Null);

    assert_eq!(
        txt.get_attribute(&txn, "test"),
        Some(Out::Any(Any::BigInt(42)))
    );
    assert_eq!(
        txt.get_attribute(&txn, "test_true"),
        Some(Out::Any(Any::Bool(true)))
    );
    assert_eq!(
        txt.get_attribute(&txn, "test_null"),
        Some(Out::Any(Any::Null))
    );

    // Collect attributes into a HashSet of keys to verify all expected keys are present
    let actual_keys: HashSet<&str> = txt.attributes(&txn).map(|(k, _)| k).collect();
    let expected_keys: HashSet<&str> = vec!["test", "test_true", "test_null"].into_iter().collect();
    assert_eq!(actual_keys, expected_keys);
}

#[test]
fn siblings() {
    let mut doc = Doc::with_client_id(1);
    let root = doc.get_or_insert_xml_fragment("root");
    let mut txn = doc.transact_mut();
    let first = root.push_back(&mut txn, XmlTextPrelim::new("hello"));
    let second = root.push_back(&mut txn, XmlElementPrelim::empty("p"));

    assert_eq!(
        &first.siblings(&txn).next().unwrap().id(),
        second.hook().id(),
        "first.next_sibling should point to second"
    );
    assert_eq!(
        &second.siblings(&txn).next_back().unwrap().id(),
        first.hook().id(),
        "second.prev_sibling should point to first"
    );
    assert_eq!(
        &first.parent().unwrap().id(),
        root.hook().id(),
        "first.parent should point to root"
    );
    assert!(root.parent().is_none(), "root parent should not exist");
    assert_eq!(
        &root.first_child().unwrap().id(),
        first.hook().id(),
        "root.first_child should point to first"
    );
}

#[test]
fn serialization() {
    let mut d1 = Doc::with_client_id(1);
    let r1 = d1.get_or_insert_xml_fragment("root");
    let mut t1 = d1.transact_mut();
    let _first = r1.push_back(&mut t1, XmlTextPrelim::new("hello"));
    r1.push_back(&mut t1, XmlElementPrelim::empty("p"));

    let expected = "hello<p></p>";
    assert_eq!(r1.get_string(&t1), expected);

    let u1 = t1.encode_state_as_update_v1(&StateVector::default());

    let mut d2 = Doc::with_client_id(2);
    let r2 = d2.get_or_insert_xml_fragment("root");
    let mut t2 = d2.transact_mut();

    let u1 = Update::decode_v1(u1.as_slice()).unwrap();
    t2.apply_update(u1).unwrap();
    assert_eq!(r2.get_string(&t2), expected);
}

#[test]
fn serialization_compatibility() {
    let mut d1 = Doc::with_client_id(1);
    let r1 = d1.get_or_insert_xml_fragment("root");
    let mut t1 = d1.transact_mut();
    let _first = r1.push_back(&mut t1, XmlTextPrelim::new("hello"));
    r1.push_back(&mut t1, XmlElementPrelim::empty("p"));

    /* This binary is result of following Yjs code (matching Rust code above):
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
    let u1 = t1.encode_state_as_update_v1(&StateVector::default());
    assert_eq!(u1.as_slice(), expected);
}

#[test]
fn event_observers() {
    let mut d1 = Doc::with_client_id(1);
    let f = d1.get_or_insert_xml_fragment("xml");
    let xml = f.insert(&mut d1.transact_mut(), 0, XmlElementPrelim::empty("test"));

    let mut d2 = Doc::with_client_id(2);
    let f = d2.get_or_insert_xml_fragment("xml");
    exchange_updates(&mut [&mut d1, &mut d2]);
    let xml2 = f
        .get(&d2.transact(), 0)
        .unwrap()
        .into_xml_element()
        .unwrap();

    let attributes = Arc::new(ArcSwapOption::default());
    let nodes = Arc::new(ArcSwapOption::default());
    let attributes_c = attributes.clone();
    let nodes_c = nodes.clone();
    let _sub = xml.observe(move |txn, e| {
        attributes_c.store(Some(Arc::new(e.keys(txn).clone())));
        nodes_c.store(Some(Arc::new(e.delta(txn).to_vec())));
    });

    // insert attribute
    {
        let mut txn = d1.transact_mut();
        xml.insert_attribute(&mut txn, "key1", "value1");
        xml.insert_attribute(&mut txn, "key2", "value2");
    }
    assert!(nodes.swap(None).unwrap().is_empty());
    assert_eq!(
        attributes.swap(None),
        Some(Arc::new(HashMap::from([
            (
                "key1".into(),
                EntryChange::Inserted(Any::String("value1".into()).into())
            ),
            (
                "key2".into(),
                EntryChange::Inserted(Any::String("value2".into()).into())
            )
        ])))
    );

    // change and remove attribute
    {
        let mut txn = d1.transact_mut();
        xml.insert_attribute(&mut txn, "key1", "value11");
        xml.remove_attribute(&mut txn, &"key2");
    }
    assert!(nodes.swap(None).unwrap().is_empty());
    assert_eq!(
        attributes.swap(None),
        Some(Arc::new(HashMap::from([
            (
                "key1".into(),
                EntryChange::Updated(
                    Any::String("value1".into()).into(),
                    Any::String("value11".into()).into()
                )
            ),
            (
                "key2".into(),
                EntryChange::Removed(Any::String("value2".into()).into())
            )
        ])))
    );

    // add xml elements
    let (nested_txt, nested_xml) = {
        let mut txn = d1.transact_mut();
        let txt = xml.insert(&mut txn, 0, XmlTextPrelim::new(""));
        let xml2 = xml.insert(&mut txn, 1, XmlElementPrelim::empty("div"));
        (txt, xml2)
    };
    assert_eq!(
        nodes.swap(None),
        Some(Arc::new(vec![Change::Added(vec![
            Out::YXmlText(nested_txt.clone()),
            Out::YXmlElement(nested_xml.clone())
        ])]))
    );
    assert_eq!(attributes.swap(None), Some(HashMap::new().into()));

    // remove and add
    let nested_xml2 = {
        let mut txn = d1.transact_mut();
        xml.remove_range(&mut txn, 1, 1);
        xml.insert(&mut txn, 1, XmlElementPrelim::empty("p"))
    };
    assert_eq!(
        nodes.swap(None),
        Some(Arc::new(vec![
            Change::Retain(1),
            Change::Added(vec![Out::YXmlElement(nested_xml2.clone())]),
            Change::Removed(1),
        ]))
    );
    assert_eq!(attributes.swap(None), Some(HashMap::new().into()));

    // copy updates over
    let attributes = Arc::new(ArcSwapOption::default());
    let nodes = Arc::new(ArcSwapOption::default());
    let attributes_c = attributes.clone();
    let nodes_c = nodes.clone();
    let _sub = xml2.observe(move |txn, e| {
        attributes_c.store(Some(Arc::new(e.keys(txn).clone())));
        nodes_c.store(Some(Arc::new(e.delta(txn).to_vec())));
    });

    {
        let t1 = d1.transact_mut();
        let mut t2 = d2.transact_mut();
        let sv = t2.state_vector();
        let mut encoder = EncoderV1::new();
        t1.encode_diff(&sv, &mut encoder);
        let update = Update::decode_v1(encoder.to_vec().as_slice()).unwrap();
        t2.apply_update(update).unwrap();
    }
    assert_eq!(
        nodes.swap(None),
        Some(Arc::new(vec![Change::Added(vec![
            Out::YXmlText(nested_txt),
            Out::YXmlElement(nested_xml2)
        ])]))
    );
    assert_eq!(
        attributes.swap(None),
        Some(Arc::new(HashMap::from([(
            "key1".into(),
            EntryChange::Inserted(Any::String("value11".into()).into())
        )])))
    );
}

#[test]
fn xml_to_string() {
    let mut doc = Doc::new();
    let f = doc.get_or_insert_xml_fragment("test");
    let mut txn = doc.transact_mut();
    let div = f.push_back(&mut txn, XmlElementPrelim::empty("div"));
    div.insert_attribute(&mut txn, "class", "t-button");
    let text = div.push_back(&mut txn, XmlTextPrelim::new("hello world"));
    text.format(
        &mut txn,
        6,
        5,
        Attrs::from([(
            "a".into(),
            HashMap::from([("href".into(), "http://domain.org")]).into(),
        )]),
    );
    drop(txn);

    let str = f.get_string(&doc.transact());
    assert_eq!(
        str.as_str(),
        "<div class=\"t-button\">hello <a href=\"http://domain.org\">world</a></div>"
    )
}

#[test]
fn xml_to_string_2() {
    let mut doc = Doc::new();
    let f = doc.get_or_insert_xml_fragment("article");
    let xml = f.insert(&mut doc.transact_mut(), 0, XmlTextPrelim::new(""));
    let mut txn = doc.transact_mut();

    let bold = Attrs::from([("b".into(), true.into())]);
    let italic = Attrs::from([("i".into(), true.into())]);

    xml.insert(&mut txn, 0, "hello ");
    xml.insert_with_attributes(&mut txn, 6, "world", italic);
    xml.format(&mut txn, 0, 5, bold);

    assert_eq!(xml.get_string(&txn), "<b>hello</b> <i>world</i>");

    let remove_italic = Attrs::from([("i".into(), Any::Null)]);
    xml.format(&mut txn, 6, 5, remove_italic);

    assert_eq!(xml.get_string(&txn), "<b>hello</b> world");
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
    let txt = doc.get_or_insert_text("test");
    let txt: &XmlTextRef = txt.as_ref();
    let mut txn = doc.transact_mut();

    txn.apply_update(update).unwrap();
    assert_eq!(txt.get_string(&txn), "<i>hello </i><b><i>world</i></b>");

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
    let txt = doc.get_or_insert_text("test");
    let txt: &XmlTextRef = txt.as_ref();
    let mut txn = doc.transact_mut();

    txn.apply_update(update).unwrap();
    assert_eq!(txt.get_string(&txn), "<i>hello </i><b><i>world</i></b>");

    let actual = txn.encode_state_as_update_v2(&StateVector::default());
    assert_eq!(actual, data);
}

#[test]
fn issue_607() {
    let mut doc = Doc::new();
    let xml = doc.get_or_insert_xml_fragment("doc");
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
        let xml = xml.get(&txn, 0).unwrap().into_xml_element().unwrap();
        let actual = xml.get_string(&txn);
        assert_eq!(actual, "<p><a></a></p>");
    }

    doc.transact_mut().apply_update(u2).unwrap();
    {
        let txn = doc.transact();
        let xml = xml.get(&txn, 0).unwrap().into_xml_element().unwrap();
        let actual = xml.get_string(&txn);
        assert_eq!(actual, "<p><b></b></p>");
    }
}
