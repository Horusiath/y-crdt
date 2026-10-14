use yrs::lib0::Decode;
use yrs::node::{Attrs, DeepObservable, Observable};
use yrs::test_utils::{clone_doc, exchange_updates};
use yrs::{
    Acquire, AcquireMut, Any, Assoc, AttrValue, Attribute, Attribution, AttributionsFormatter,
    Cell, ClientID, ContentAttribute, ContentIds, ContentMap, Delta, DeltaFormatter, DeltaOptions,
    DiffFormatter, Doc, IdSet, In, Options, Out, Update,
};

fn bold() -> Attrs {
    Attrs::from([("bold".into(), Any::Bool(true))])
}

fn unbold() -> Attrs {
    Attrs::from([("bold".into(), Any::Null)])
}

fn sync(from: &Doc, to: &mut Doc) -> ContentIds {
    let update = from
        .transact()
        .encode_state_as_update_v1(&Default::default());
    let update = Update::decode_v1(&update).unwrap();
    let mut txn = to.transact_mut();
    txn.apply_update(update).unwrap();
    ContentIds::from_transaction(&txn)
}

fn diff_formatter(base: &ContentIds, doc: &Doc) -> DiffFormatter {
    DiffFormatter::new(ContentIds::from_doc(doc).diff(base), None)
}

fn render(doc: &Doc, base: &ContentIds) -> Delta {
    let fmt = diff_formatter(base, doc);
    let txn = doc.transact();
    let root = txn.node("root").unwrap();
    let opts = DeltaOptions {
        deep: true,
        ..Default::default()
    };
    root.formatted(&fmt).delta(&opts)
}

#[test]
fn relative_positions() {
    let mut doc = Doc::with_options(Options {
        skip_gc: true,
        ..Options::with_client_id(ClientID::new(1))
    });
    doc.transact_mut()
        .node_mut("text")
        .unwrap()
        .insert_text(0, "hello world");
    let v1 = ContentIds::from_doc(&doc);
    {
        let mut txn = doc.transact_mut();
        let mut text = txn.node_mut("text").unwrap();
        text.remove(1, 6); // "horld"
        text.insert_text(1, "x"); // "hxorld"
    }
    let fmt = diff_formatter(&v1, &doc);
    let txn = doc.transact();
    let text = txn.node("text").unwrap();
    let pos = text.formatted(&fmt).sticky_index(9, Assoc::After).unwrap(); // after "hello wxo"
    assert_eq!(pos.get_offset_fmt(&txn, true, &fmt).unwrap().index, 9); // "hello wxo.rld"
    assert_eq!(pos.get_offset(&txn).unwrap().index, 3); // "hxo.rld"
}

#[test]
fn attributed_events() {
    let mut doc = Doc::with_options(Options {
        skip_gc: true,
        ..Options::with_client_id(ClientID::new(1))
    });
    doc.transact_mut()
        .node_mut("text")
        .unwrap()
        .insert_text(0, "hello world");
    let v1 = ContentIds::from_doc(&clone_doc(&doc, None));
    doc.transact_mut().node_mut("text").unwrap().remove(6, 5); // "hello "
    let fmt = diff_formatter(&v1, &doc);
    let c1 = doc
        .transact()
        .node("text")
        .unwrap()
        .formatted(&fmt)
        .delta(&DeltaOptions::default());
    let expected = Delta::out().insert_text("hello ").insert_text_attributed(
        "world",
        None,
        Some(Attribution::delete::<Attribute>([])),
    );
    assert_eq!(c1, expected); // "hello ~world~"

    let deltas = Cell::new(vec![]);
    let deltas_c = deltas.clone();
    let _sub = doc
        .transact_mut()
        .node_mut("text")
        .unwrap()
        .observe(move |e| {
            // the formatter of a live diff knows about the content of this transaction
            let fmt = diff_formatter(&v1, e.transaction().doc());
            deltas_c.acquire_mut().push(e.delta_fmt(false, &fmt));
        });
    {
        let mut txn = doc.transact_mut();
        let mut text = txn.node_mut("text").unwrap();
        text.formatted_mut(&fmt)
            .apply_delta(Delta::new().retain(11).insert_text("!")); // "hello ~world~!"
    }
    let expected = Delta::out().retain(11).insert_text_attributed(
        "!",
        None,
        Some(Attribution::insert::<Attribute>([])),
    );
    assert_eq!(*deltas.acquire(), vec![expected]);
}

#[test]
fn attributions() {
    let mut v1 = Doc::with_client_id(1);
    v1.transact_mut()
        .node_mut("txt")
        .unwrap()
        .apply_delta(Delta::new().insert_text("hello world"));
    let mut doc = Doc::with_options(Options {
        skip_gc: true,
        ..Options::with_client_id(ClientID::new(2))
    });
    sync(&v1, &mut doc);
    doc.transact_mut()
        .node_mut("txt")
        .unwrap()
        .apply_delta(Delta::new().retain(5).remove(6).insert_text("!")); // "hello~ world~!"
    let base = ContentIds::from_doc(&v1);
    let fmt = diff_formatter(&base, &doc);
    let actual = doc
        .transact()
        .node("txt")
        .unwrap()
        .formatted(&fmt)
        .delta(&DeltaOptions::default()); // "hello~ world~!"
    let expected = Delta::out()
        .insert_text("hello")
        .insert_text_attributed(" world", None, Some(Attribution::delete::<Attribute>([])))
        .insert_text_attributed("!", None, Some(Attribution::insert::<Attribute>([])));
    assert_eq!(actual, expected);

    let deltas = Cell::new(vec![]);
    let deltas_c = deltas.clone();
    let _sub = doc
        .transact_mut()
        .node_mut("txt")
        .unwrap()
        .observe(move |e| {
            let fmt = diff_formatter(&base, e.transaction().doc());
            let attributed = e.delta_fmt(false, &fmt);
            deltas_c.acquire_mut().push((attributed, e.delta(false)));
        });
    doc.transact_mut()
        .node_mut("txt")
        .unwrap()
        .formatted_mut(&fmt)
        .apply_delta(Delta::new().retain(11).insert_text("!"));
    let attributed = Delta::out().retain(11).insert_text_attributed(
        "!",
        None,
        Some(Attribution::insert::<Attribute>([])),
    );
    let unattributed = Delta::out().retain(5).insert_text("!");
    assert_eq!(*deltas.acquire(), vec![(attributed, unattributed)]);
}

#[test]
fn format_rebold() {
    let mut doc = Doc::with_options(Options {
        skip_gc: true,
        ..Options::with_client_id(ClientID::new(1))
    });
    doc.transact_mut()
        .node_mut("root")
        .unwrap()
        .insert_text(0, "abcdef");
    // diff against a baseline taken BEFORE formatting => every format change is an attributed suggestion
    let base = ContentIds::from_doc(&doc);
    // first access starts maintaining the incremental cache (baseline == current, so no suggestions yet)
    assert_eq!(render(&doc, &base), Delta::out().insert_text("abcdef"));
    let mut txn = doc.transact_mut();
    let mut root = txn.node_mut("root").unwrap();
    root.format(0, 6, &bold()); // all bold
    root.format(2, 2, &unbold()); // un-bold "cd" (inserts a transient bold:null marker)
    root.format(2, 2, &bold()); // re-bold "cd" => DELETES that transient marker
    drop(txn);
    let attribution = Attribution::format::<Attribute>("bold", []);
    let expected = Delta::out().insert_text_attributed("abcdef", Some(&bold()), Some(attribution));
    assert_eq!(render(&doc, &base), expected);
}

#[test]
fn format_embed_in_bold() {
    let mut doc = Doc::with_options(Options {
        skip_gc: true,
        ..Options::with_client_id(ClientID::new(1))
    });
    doc.transact_mut()
        .node_mut("root")
        .unwrap()
        .insert_text(0, "ab");
    let base = ContentIds::from_doc(&doc);
    assert_eq!(render(&doc, &base), Delta::out().insert_text("ab"));
    let mut txn = doc.transact_mut();
    let mut root = txn.node_mut("root").unwrap();
    root.format(0, 2, &bold());
    let embed = root.insert(1, In::Node(Box::new(Delta::new())));
    drop(txn);
    let format = Some(Attribution::format::<Attribute>("bold", []));
    let expected = Delta::out()
        .insert_text_attributed("a", Some(&bold()), format.clone())
        .insert_range_attributed([embed], None, Some(Attribution::insert::<Attribute>([])))
        .insert_text_attributed("b", Some(&bold()), format);
    assert_eq!(render(&doc, &base), expected);
}

#[test]
fn format_delete_formatted() {
    let mut doc = Doc::with_options(Options {
        skip_gc: true,
        ..Options::with_client_id(ClientID::new(1))
    });
    doc.transact_mut()
        .node_mut("root")
        .unwrap()
        .insert_text(0, "a");
    let base = ContentIds::from_doc(&doc);
    assert_eq!(render(&doc, &base), Delta::out().insert_text("a"));
    let mut txn = doc.transact_mut();
    let mut root = txn.node_mut("root").unwrap();
    root.format(0, 1, &bold());
    root.remove(0, 1);
    drop(txn);
    let expected =
        Delta::out().insert_text_attributed("a", None, Some(Attribution::delete::<Attribute>([])));
    assert_eq!(render(&doc, &base), expected);
}

#[test]
fn delta_fresh_range_after_item_merge() {
    let mut doc = Doc::with_options(Options {
        skip_gc: true,
        ..Options::with_client_id(ClientID::new(1))
    });
    let mut sdoc = Doc::with_options(Options {
        skip_gc: true,
        cleanup_formatting: false,
        ..Options::with_client_id(ClientID::new(2))
    });
    doc.transact_mut()
        .node_mut("t")
        .unwrap()
        .apply_delta(Delta::new().insert_text("XY"));
    sync(&doc, &mut sdoc);
    let base_now = ContentIds::from_doc(&doc);
    let deltas = Cell::new(vec![]);
    let deltas_c = deltas.clone();
    let _sub = sdoc
        .transact_mut()
        .node_mut("t")
        .unwrap()
        .observe(move |e| {
            let fmt = diff_formatter(&base_now, e.transaction().doc());
            deltas_c.acquire_mut().push(e.delta_fmt(true, &fmt));
        });
    // suggested insert 'a' at position 0
    sdoc.transact_mut()
        .node_mut("t")
        .unwrap()
        .apply_delta(Delta::new().insert_text("a"));
    // insert 'b' right after 'a', then delete both
    {
        let mut txn = sdoc.transact_mut();
        let mut t = txn.node_mut("t").unwrap();
        t.apply_delta(Delta::new().retain(1).insert_text("b"));
        t.apply_delta(Delta::new().remove(2));
    }
    let inserted =
        Delta::out().insert_text_attributed("a", None, Some(Attribution::insert::<Attribute>([])));
    let expected = vec![inserted, Delta::out().remove(1)];
    assert_eq!(*deltas.acquire(), expected);
    let base = ContentIds::from_doc(&doc);
    let fmt = diff_formatter(&base, &sdoc);
    let fresh = sdoc
        .transact()
        .node("t")
        .unwrap()
        .formatted(&fmt)
        .delta(&DeltaOptions::default());
    assert_eq!(fresh, Delta::out().insert_text("XY"));
}

/// Creates a base doc with `abc` followed by two nodes and a suggestion doc striking `bc` and
/// both nodes (the struck content is still rendered).
fn struck_chunk_pair(base_client: u64, sdoc_client: u64) -> (Doc, Doc, ContentIds) {
    let mut doc = Doc::with_options(Options {
        skip_gc: true,
        ..Options::with_client_id(ClientID::new(base_client))
    });
    let mut sdoc = Doc::with_options(Options {
        skip_gc: true,
        cleanup_formatting: false,
        ..Options::with_client_id(ClientID::new(sdoc_client))
    });
    {
        let mut txn = doc.transact_mut();
        let mut t = txn.node_mut("t").unwrap();
        t.apply_delta(Delta::new().insert_text("abc"));
        let a = Delta::with_name("nA").insert_text("kk");
        let b = Delta::with_name("nB").insert_text("qqq");
        t.apply_delta(
            Delta::new()
                .retain(3)
                .insert_range([In::from(a), In::from(b)]),
        );
    }
    sync(&doc, &mut sdoc);
    let base = ContentIds::from_doc(&doc);
    {
        let mut txn = sdoc.transact_mut();
        let mut t = txn.node_mut("t").unwrap();
        t.apply_delta(Delta::new().retain(1).remove(2));
        t.apply_delta(Delta::new().retain(1).remove(2));
    }
    (doc, sdoc, base)
}

/// Port of Yjs `testRdtApplyDeltaDeleteMidStruckChunkKeepsCursorSync`, without the `modify` op:
/// a delete ending mid-way through a struck chunk only advances the cursor past the consumed part.
#[test]
fn apply_delta_delete_mid_struck_chunk_keeps_cursor_sync() {
    for (base_client, sdoc_client) in [(1, 2), (2, 1)] {
        let (_doc, mut sdoc, base) = struck_chunk_pair(base_client, sdoc_client);
        let fmt = diff_formatter(&base, &sdoc);
        let mut txn = sdoc.transact_mut();
        let mut t = txn.node_mut("t").unwrap();
        // delete struck 'b' (ends mid-chunk), retain struck 'c', insert after it
        let d = Delta::new().retain(1).remove(1).retain(1).insert_text("!");
        t.formatted_mut(&fmt).apply_delta(d);
        let struck_b = IdSet::from_iter([(ClientID::new(base_client), [1..2])]);
        assert_eq!(txn.attributed_deletes(), Some(&struck_b));
        drop(txn);
        let fmt = diff_formatter(&base, &sdoc);
        let txn = sdoc.transact();
        let delta = txn
            .node("t")
            .unwrap()
            .formatted(&fmt)
            .delta(&DeltaOptions::default());
        let deleted = Some(Attribution::delete::<Attribute>([]));
        assert_eq!(delta.children[0], Delta::out().insert_text("a").children[0]);
        let bc = Delta::out().insert_text_attributed("bc", None, deleted.clone());
        assert_eq!(delta.children[1], bc.children[0]);
        let inserted = Delta::out().insert_text_attributed(
            "!",
            None,
            Some(Attribution::insert::<Attribute>([])),
        );
        assert_eq!(delta.children[2], inserted.children[0]);
        assert_eq!(delta.children[3].attribution(), deleted.as_ref());
    }
}

#[test]
fn apply_delta_delete_mid_struck_chunk_modify_returns_fix() {
    for (base_client, sdoc_client) in [(1, 2), (2, 1)] {
        let (_doc, mut sdoc, base) = struck_chunk_pair(base_client, sdoc_client);
        let fmt = diff_formatter(&base, &sdoc);
        let opts = DeltaOptions {
            deep: true,
            ..Default::default()
        };
        let before = sdoc
            .transact()
            .node("t")
            .unwrap()
            .formatted(&fmt)
            .delta(&opts);
        let mut txn = sdoc.transact_mut();
        let mut t = txn.node_mut("t").unwrap();
        // delete struck 'b' (ends mid-chunk), retain struck 'c', revert-modify tombstone nA
        let d = Delta::new()
            .retain(1)
            .remove(1)
            .retain(1)
            .modify(Delta::new().remove(2));
        t.formatted_mut(&fmt).apply_delta(d);
        // step 8: `apply_delta` returns the inverse of the part addressing the rendered tombstone
        // let fix = format!("{:?}", fix.expect("fix"));
        // assert!(fix.contains("kk") && !fix.contains("qq"), "fix restores nA's content");
        drop(txn);
        let fresh = sdoc
            .transact()
            .node("t")
            .unwrap()
            .formatted(&fmt)
            .delta(&opts);
        assert_eq!(before, fresh, "nothing was applied");
    }
}

#[test]
fn apply_delta_format_struck_content() {
    let (_doc, mut sdoc, base) = struck_chunk_pair(1, 2);
    let fmt = diff_formatter(&base, &sdoc);
    sdoc.transact_mut()
        .node_mut("t")
        .unwrap()
        .formatted_mut(&fmt)
        .apply_delta(Delta::new().retain(1).retain_fmt(1, &bold()));
    let fmt = diff_formatter(&base, &sdoc);
    let txn = sdoc.transact();
    let delta = txn
        .node("t")
        .unwrap()
        .formatted(&fmt)
        .delta(&DeltaOptions::default());
    let deleted = Attribution::delete::<Attribute>([]);
    let bold_b = Attribution {
        delete: Some(vec![]),
        ..Attribution::format::<Attribute>("bold", [])
    };
    let expected = Delta::out()
        .insert_text("a")
        .insert_text_attributed("b", Some(&bold()), Some(bold_b))
        .insert_text_attributed("c", None, Some(deleted.clone()));
    assert_eq!(delta.children[..3], expected.children[..]);
}

fn render_text<F: DeltaFormatter>(doc: &Doc, fmt: &F) -> Delta {
    let txn = doc.transact();
    let text = txn.node("text").unwrap();
    text.formatted(fmt).delta(&DeltaOptions::default())
}

fn init_doc() -> (Doc, Doc) {
    let mut base = Doc::new();
    base.transact_mut()
        .node_mut("text")
        .unwrap()
        .insert_text(0, "hello world");
    let mut doc = clone_doc(&base, None);
    doc.transact_mut().node_mut("text").unwrap().remove(6, 5);
    (base, doc)
}

#[test]
fn formatted() {
    let (base, doc) = init_doc();
    let mut fmt: DiffFormatter = DiffFormatter::new(ContentIds::doc_diff(&base, &doc), None);
    fmt.restore_content(&base.transact());
    let txn = doc.transact();
    let text = txn.node("text").unwrap();
    let delta = text.formatted(&fmt).delta(&DeltaOptions::default());
    let expected = Delta::out().insert_text("hello ").insert_text_attributed(
        "world",
        None,
        Some(Attribution::delete::<Attribute>([])),
    );
    assert_eq!(delta, expected);
}

#[test]
fn insertions_minding_attributed_content() {
    let (base, mut doc) = init_doc();
    let mut fmt: DiffFormatter = DiffFormatter::new(ContentIds::doc_diff(&base, &doc), None);
    fmt.restore_content(&base.transact());
    let c1 = render_text(&doc, &fmt);
    let expected = Delta::out().insert_text("hello ").insert_text_attributed(
        "world",
        None,
        Some(Attribution::delete::<Attribute>([])),
    );
    assert_eq!(c1, expected);
    let mut txn = doc.transact_mut();
    let mut text = txn.node_mut("text").unwrap();
    text.formatted_mut(&fmt)
        .apply_delta(Delta::new().retain(11).insert_text("content"));
    assert_eq!(text.to_string(), "hello content");
}

#[test]
fn insertions_into_attributed_content() {
    let mut base = Doc::new();
    base.transact_mut()
        .node_mut("text")
        .unwrap()
        .insert_text(0, "hello ");
    let mut doc = clone_doc(&base, None);
    doc.transact_mut()
        .node_mut("text")
        .unwrap()
        .insert_text(6, "word"); // "hello word"
    let fmt: DiffFormatter = DiffFormatter::new(ContentIds::doc_diff(&base, &doc), None);
    let c1 = render_text(&doc, &fmt);
    let expected = Delta::out().insert_text("hello ").insert_text_attributed(
        "word",
        None,
        Some(Attribution::insert::<Attribute>([])),
    );
    assert_eq!(c1, expected);
    let mut txn = doc.transact_mut();
    let mut text = txn.node_mut("text").unwrap();
    text.formatted_mut(&fmt)
        .apply_delta(Delta::new().retain(9).insert_text("l"));
    assert_eq!(text.to_string(), "hello world");
}

#[test]
fn ydoc_diff() {
    let mut start = Doc::new();
    let nested = {
        let mut txn = start.transact_mut();
        txn.node_mut("text").unwrap().insert_text(0, "hello");
        txn.node_mut("array").unwrap().insert_range(0, [1, 2, 3]);
        let mut map = txn.node_mut("map").unwrap();
        map.insert_attr("k", 42);
        map.insert_attr("nested", Delta::new()).node_id().unwrap()
        // { text: "hello", array: [1,2,3], map: {"k":42,"nested": {}}}
    };
    let mut updated = clone_doc(&start, None);
    {
        let mut txn = updated.transact_mut();
        txn.node_mut("text").unwrap().insert_text(5, " world");
        txn.node_mut("array").unwrap().insert(1, "x");
        txn.node_mut("map").unwrap().insert_attr("newk", 42);
        txn.node_mut(nested).unwrap().insert(0, 1);
        // { text: "hello world", array: [1,'x',2,3], map: {"k":42, "newk": 42,"nested": [1]}}
    }
    let text = Delta::out().retain(5).insert_text_attributed(
        " world",
        None,
        Some(Attribution::insert::<Attribute>([])),
    );
    let array = Delta::out().retain(1).insert_range_attributed(
        [Out::from("x")],
        None,
        Some(Attribution::insert::<Attribute>([])),
    );
    let nested = Delta::out().insert_range_attributed(
        [Out::from(1)],
        None,
        Some(Attribution::insert::<Attribute>([])),
    );
    let map = Delta::out()
        .insert_attr_attributed("newk", 42, Some(Attribution::insert::<Attribute>([])))
        .modify_attr("nested", nested);
    let expected = Delta::out()
        .modify_attr("text", text)
        .modify_attr("array", array)
        .modify_attr("map", map);
    assert_eq!(updated.diff(&start), expected);
}

#[test]
fn child_list_content() {
    let start = Doc::new();
    let mut updated = clone_doc(&start, None);
    let deltas = Cell::new(vec![]);
    let deltas_c = deltas.clone();
    let _sub = updated
        .transact_mut()
        .node_mut("array")
        .unwrap()
        .observe_deep(move |events| deltas_c.acquire_mut().push(events[0].delta(true)));
    let yf = Delta::with_name("test")
        .insert_text("test content")
        .insert_attr("k", "v"); // <test k="v">test content</test>
    let id = updated
        .transact_mut()
        .node_mut("array")
        .unwrap()
        .insert(0, yf)
        .node_id()
        .unwrap(); // [<test k="v">test content</test>]
    let test = Delta::with_name("test")
        .insert_text("test content")
        .insert_attr("k", "v");
    let expected = Delta::out().insert(Out::node_with_delta(id.clone(), test));
    assert_eq!(*deltas.acquire(), vec![expected]);
    let test = Delta::with_name("test")
        .insert_text_attributed(
            "test content",
            None,
            Some(Attribution::insert::<Attribute>([])),
        )
        .insert_attr_attributed("k", "v", Some(Attribution::insert::<Attribute>([])));
    let array = Delta::out().insert_range_attributed(
        [Out::node_with_delta(id, test)],
        None,
        Some(Attribution::insert::<Attribute>([])),
    );
    let expected = Delta::out().modify_attr("array", array);
    assert_eq!(updated.diff(&start), expected);
}

/// Attributes content `ids` of a local transaction to user `by`.
fn attribute(global: &mut ContentMap, ids: ContentIds, by: &str) {
    let map = ContentMap::from_content_ids(
        &ids,
        vec![ContentAttribute::insert(by)],
        vec![ContentAttribute::delete(by)],
    );
    *global = ContentMap::merge(&[std::mem::take(global), map]);
}

#[test]
fn attribution_session1() {
    let mut u0 = Doc::with_options(Options {
        skip_gc: true,
        ..Options::with_client_id(ClientID::new(1))
    });
    let mut u1 = Doc::with_client_id(2);
    let mut u2 = Doc::with_client_id(3);
    let mut global: ContentMap = ContentMap::default();
    let v1 = clone_doc(&u0, None);
    {
        let mut txn = u0.transact_mut();
        txn.node_mut("text").unwrap().insert_text(0, "a"); // u0: "a"
        attribute(&mut global, ContentIds::from_transaction(&txn), "0");
    }
    {
        let mut txn = u1.transact_mut();
        txn.node_mut("text").unwrap().insert_text(0, "b"); // u1: "b"
        attribute(&mut global, ContentIds::from_transaction(&txn), "1");
    }
    exchange_updates(&mut [&mut u0, &mut u1, &mut u2]);
    let by = |user: &str| [Attribute::from(user)];
    let fmt = DiffFormatter::new(ContentIds::doc_diff(&v1, &u0), Some(global.clone()));
    let d1 = render_text(&u0, &fmt);
    let expected = Delta::out()
        .insert_text_attributed("a", None, Some(Attribution::insert(by("0"))))
        .insert_text_attributed("b", None, Some(Attribution::insert(by("1"))));
    assert_eq!(d1, expected);
    let v2 = clone_doc(&u0, None);
    {
        let mut txn = u0.transact_mut();
        txn.node_mut("text").unwrap().remove(1, 1);
        attribute(&mut global, ContentIds::from_transaction(&txn), "0");
    }
    {
        let mut txn = u1.transact_mut();
        txn.node_mut("text").unwrap().insert_text(2, "c");
        attribute(&mut global, ContentIds::from_transaction(&txn), "1");
    }
    exchange_updates(&mut [&mut u0, &mut u1, &mut u2]);
    let fmt = DiffFormatter::new(ContentIds::doc_diff(&v2, &u0), Some(global.clone()));
    let d2 = render_text(&u0, &fmt);
    let expected = Delta::out()
        .insert_text("a")
        .insert_text_attributed("b", None, Some(Attribution::delete(by("0"))))
        .insert_text_attributed("c", None, Some(Attribution::insert(by("1"))));
    assert_eq!(d2, expected);

    let user0 = |attrs: &[ContentAttribute<AttrValue>]| {
        let user = AttrValue::Attributor("0".into());
        attrs.iter().any(|a| *a.value() == user)
    };
    let only_user0 = global.filter(user0, user0);
    let d3 = render_text(&u0, &AttributionsFormatter::new(&only_user0));
    let expected = Delta::out()
        .insert_text_attributed("a", None, Some(Attribution::insert(by("0"))))
        .insert_text_attributed("b", None, Some(Attribution::delete(by("0"))))
        .insert_text("c");
    assert_eq!(d3, expected);
    // step 9: `undo_content_ids` doesn't exist yet
    // undo_content_ids(&mut u0, &ContentIds::from(only_user0));
    // let d4 = u0.transact().node("text").unwrap().delta(&DeltaOptions::default());
    // assert_eq!(d4, Delta::out().insert_text("bc"));
}

#[test]
fn attribution_change() {
    let mut doc = Doc::new();
    doc.transact_mut()
        .node_mut("root")
        .unwrap()
        .apply_delta(Delta::new().insert_text("hi"));
    let mut base = clone_doc(&doc, None);
    doc.transact_mut()
        .node_mut("root")
        .unwrap()
        .apply_delta(Delta::new().retain(2).insert_text("!"));
    let mut fmt: DiffFormatter = DiffFormatter::new(ContentIds::doc_diff(&base, &doc), None);
    let changes = fmt.retract(&sync(&doc, &mut base));
    let opts = DeltaOptions {
        deep: true,
        items_to_render: Some(changes),
        retain_inserts: true,
        retain_deletes: true,
        ..Default::default()
    };
    let txn = doc.transact();
    let update = txn.node("root").unwrap().formatted(&fmt).delta(&opts);
    // the '!' lost its `{ insert: [] }` attribution: the change clears it
    let expected = Delta::out()
        .retain(2)
        .retain_attributed(1, None, Some(Attribution::default()));
    assert_eq!(update, expected);
}

#[test]
fn diff_gc_deleted_content() {
    let (base, doc) = init_doc();
    let mut fmt: DiffFormatter = DiffFormatter::new(ContentIds::doc_diff(&base, &doc), None);
    let opaque = Delta::out().insert_text("hello ").remove(5);
    assert_eq!(render_text(&doc, &fmt), opaque);
    fmt.restore_content(&base.transact());
    let restored = Delta::out().insert_text("hello ").insert_text_attributed(
        "world",
        None,
        Some(Attribution::delete::<Attribute>([])),
    );
    assert_eq!(render_text(&doc, &fmt), restored);
}
