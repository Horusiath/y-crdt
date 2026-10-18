use yrs::lib0::Decode;
use yrs::node::{Attrs, DeepObservable, Observable};
use yrs::test_utils::{clone_doc, exchange_updates};
use yrs::{
    Acquire, AcquireMut, Any, Assoc, AttrValue, Attribute, Attribution, AttributionsFormatter,
    Cell, ClientID, ContentAttribute, ContentIds, ContentMap, DefaultFormatter, Delta,
    DeltaFormatter, DeltaOptions, DiffFormatter, Doc, IdSet, In, Options, Out, Update, any,
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
    DiffFormatter::unattributed(&ContentIds::from_doc(doc).diff(base))
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
        let fix = t.formatted_mut(&fmt).apply_delta(d);
        // the fix restores nA's content, not nB's
        let kk = Delta::new().insert_text_attributed("kk", None, Some(deleted()));
        assert_eq!(fix, Delta::new().retain(2).modify(kk));
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
    let mut fmt = DiffFormatter::unattributed(&ContentIds::doc_diff(&base, &doc));
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
    let mut fmt = DiffFormatter::unattributed(&ContentIds::doc_diff(&base, &doc));
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
    let fmt = DiffFormatter::unattributed(&ContentIds::doc_diff(&base, &doc));
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
    let fmt = DiffFormatter::new(&ContentIds::doc_diff(&v1, &u0), Some(global.clone()));
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
    let fmt = DiffFormatter::new(&ContentIds::doc_diff(&v2, &u0), Some(global.clone()));
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
    let mut fmt = DiffFormatter::unattributed(&ContentIds::doc_diff(&base, &doc));
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
    let mut fmt = DiffFormatter::unattributed(&ContentIds::doc_diff(&base, &doc));
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

fn deleted() -> Attribution {
    Attribution::delete::<Attribute>([])
}

fn align(value: &str) -> Attrs {
    Attrs::from([("align".into(), Any::from(value))])
}

fn paragraph(text: &str) -> In {
    In::from(Delta::with_name("paragraph").insert_text(text))
}

/// A base doc initialized with `init` deltas applied to a root `node`, and a suggestion doc synced
/// from it, which keeps the content it deletes.
fn suggestion_pair(clients: (u64, u64), node: &str, init: Vec<Delta<In>>) -> (Doc, Doc) {
    let mut doc = Doc::with_options(Options {
        skip_gc: true,
        ..Options::with_client_id(ClientID::new(clients.0))
    });
    let mut sdoc = Doc::with_options(Options {
        skip_gc: true,
        cleanup_formatting: false,
        ..Options::with_client_id(ClientID::new(clients.1))
    });
    {
        let mut txn = doc.transact_mut();
        let mut root = txn.node_mut(node).unwrap();
        for d in init {
            root.apply_delta(d);
        }
    }
    sync(&doc, &mut sdoc);
    (doc, sdoc)
}

/// 1. `{ prosemirror: [ <paragraph>hello world</paragraph>] }`
/// 2. `{ prosemirror: [~~<paragraph>hello world</paragraph>~~] }` (child deleted)
fn struck_paragraph_pair(clients: (u64, u64)) -> (Doc, Doc) {
    let init = vec![Delta::new().insert(paragraph("hello world"))];
    let (doc, mut sdoc) = suggestion_pair(clients, "prosemirror", init);
    sdoc.transact_mut()
        .node_mut("prosemirror")
        .unwrap()
        .remove(0, 1);
    (doc, sdoc)
}

fn suggestions(base: &Doc, sdoc: &Doc) -> DiffFormatter {
    DiffFormatter::unattributed(&ContentIds::doc_diff(base, sdoc))
}

fn render_deep<F: DeltaFormatter>(doc: &Doc, node: &str, fmt: &F) -> Delta {
    let txn = doc.transact();
    let opts = DeltaOptions {
        deep: true,
        ..Default::default()
    };
    txn.node(node).unwrap().formatted(fmt).delta(&opts)
}

fn apply_fmt(doc: &mut Doc, node: &str, fmt: &DiffFormatter, d: Delta<In>) -> Delta<In> {
    let mut txn = doc.transact_mut();
    txn.node_mut(node)
        .unwrap()
        .formatted_mut(fmt)
        .apply_delta(d)
}

/// Ensure that no suggestion landed on the doc.
fn assert_reverted(sdoc: &Doc, node: &str, fmt: &DiffFormatter, before: &Delta) {
    assert_eq!(&render_deep(sdoc, node, fmt), before, "nothing was applied");
    // yrs has no maintained `.delta` cache:
    // t.assert(ynode.delta.equals(fresh))
    // yrs deltas can't be applied onto deltas (lib0 `apply`), so the round trip can't be checked:
    // before.apply(d).apply(fix) == fresh
}

#[test]
fn apply_delta_modify_into_tombstone_returns_inverse() {
    for clients in [(1, 2), (2, 1)] {
        let (doc, mut sdoc) = struck_paragraph_pair(clients);
        let fmt = suggestions(&doc, &sdoc);
        let before = render_deep(&sdoc, "prosemirror", &fmt);
        let fired = Cell::new(0);
        let fired_c = fired.clone();
        let _sub = sdoc
            .transact_mut()
            .node_mut("prosemirror")
            .unwrap()
            .observe_deep(move |_| *fired_c.acquire_mut() += 1);
        let d = Delta::new().modify(Delta::new().retain(2).insert_text("XY"));
        let fix = apply_fmt(&mut sdoc, "prosemirror", &fmt, d);
        assert_eq!(fix, Delta::new().modify(Delta::new().retain(2).remove(2)));
        assert_eq!(*fired.acquire(), 0, "a fully reverted apply emits no event");
        assert_reverted(&sdoc, "prosemirror", &fmt, &before);
    }
}

#[test]
fn apply_delta_delete_inside_tombstone_inverse() {
    for clients in [(1, 2), (2, 1)] {
        let (doc, mut sdoc) = struck_paragraph_pair(clients);
        let fmt = suggestions(&doc, &sdoc);
        let before = render_deep(&sdoc, "prosemirror", &fmt);
        let d = Delta::new().modify(Delta::new().retain(2).remove(4));
        let fix = apply_fmt(&mut sdoc, "prosemirror", &fmt, d);
        let reinsert = Delta::new()
            .retain(2)
            .insert_text_attributed("llo ", None, Some(deleted()));
        assert_eq!(fix, Delta::new().modify(reinsert));
        assert_reverted(&sdoc, "prosemirror", &fmt, &before);
    }
}

#[test]
fn apply_delta_format_inside_tombstone_inverse() {
    for clients in [(1, 2), (2, 1)] {
        let (doc, mut sdoc) = struck_paragraph_pair(clients);
        let fmt = suggestions(&doc, &sdoc);
        let before = render_deep(&sdoc, "prosemirror", &fmt);
        let bold = Attrs::from([("bold".into(), any!({}))]); // `{ bold: {} }`
        let d = Delta::new().modify(Delta::new().retain(1).retain_fmt(4, &bold));
        let fix = apply_fmt(&mut sdoc, "prosemirror", &fmt, d);
        let unbold = Delta::new().retain(1).retain_fmt(4, &unbold());
        assert_eq!(fix, Delta::new().modify(unbold));
        assert_reverted(&sdoc, "prosemirror", &fmt, &before);
    }
}

#[test]
fn apply_delta_node_format_on_tombstone_inverse() {
    for clients in [(1, 2), (2, 1)] {
        let init = vec![
            Delta::new().insert_range([paragraph("aa"), paragraph("bb")]),
            // base-doc node format over both paragraphs: an alive `align` marker sits before the first one
            Delta::new().retain_fmt(2, &align("x")),
        ];
        let (doc, mut sdoc) = suggestion_pair(clients, "prosemirror", init);
        // suggestion-delete the second paragraph
        sdoc.transact_mut()
            .node_mut("prosemirror")
            .unwrap()
            .remove(1, 1);
        let fmt = suggestions(&doc, &sdoc);
        let before = render_deep(&sdoc, "prosemirror", &fmt);
        let d = Delta::new().retain(1).modify_fmt(Delta::new(), &align("y"));
        let fix = apply_fmt(&mut sdoc, "prosemirror", &fmt, d);
        let expected = Delta::new().retain(1).modify_fmt(Delta::new(), &align("x"));
        assert_eq!(fix, expected);
        assert_reverted(&sdoc, "prosemirror", &fmt, &before);
    }
}

/// Base `[aa, hello world, cc]` paragraphs with `hello world` struck by the suggestion doc.
fn deleted_mid_par(clients: (u64, u64)) -> (Doc, Doc) {
    let paragraphs = [paragraph("aa"), paragraph("hello world"), paragraph("cc")];
    let init = vec![Delta::new().insert_range(paragraphs)];
    let (doc, mut sdoc) = suggestion_pair(clients, "prosemirror", init);
    // suggestion-delete the middle paragraph
    sdoc.transact_mut()
        .node_mut("prosemirror")
        .unwrap()
        .remove(1, 1);
    (doc, sdoc)
    // { prosemirror: [ <paragraph>aa</paragraph>, ~<paragraph>hello world</paragraph>~, <paragraph>cc</paragraph> ] }
}

/// Applied and reverted ops coexist in one delta: ops on live content land (one 'delta' event),
/// only the tombstone-targeting modify reverts — and the fix's retain pad is measured in the
/// caller's *expected* space (including `d`'s own earlier insert).
#[test]
fn apply_delta_mixed_fix_coordinates() {
    for clients in [(1, 2), (2, 1)] {
        let (doc, mut sdoc) = deleted_mid_par(clients);
        let fmt = suggestions(&doc, &sdoc);
        let fired = Cell::new(0);
        let fired_c = fired.clone();
        let _sub = sdoc
            .transact_mut()
            .node_mut("prosemirror")
            .unwrap()
            .observe_deep(move |_| *fired_c.acquire_mut() += 1);
        let d = Delta::new()
            .insert(paragraph("nn"))
            .retain(1)
            .modify(Delta::new().retain(2).insert_text("XY"))
            .modify(Delta::new().retain(2).insert_text("ZZ"));
        let fix = apply_fmt(&mut sdoc, "prosemirror", &fmt, d);
        assert_eq!(
            *fired.acquire(),
            1,
            "the applied part of the change emits exactly one delta event"
        );
        let inverse = Delta::new().retain(2).remove(2);
        assert_eq!(fix, Delta::new().retain(2).modify(inverse));
        let fresh = format!(
            "{:?}",
            render_deep(&sdoc, "prosemirror", &suggestions(&doc, &sdoc))
        );
        assert!(
            fresh.contains("\"nn\"") && fresh.contains("\"ZZ\""),
            "ops on live content were applied"
        );
        assert!(
            !fresh.contains("XY"),
            "the tombstone-targeting modify was not applied"
        );
        // yrs has no maintained `.delta` cache:
        // t.assert(ynode.delta.equals(ynode.toDelta({ deep: true })))
    }
}

/// `modifyAttr` addressing a suggestion-deleted (still rendered) map value: the renderer-aware
/// lookup finds the tombstone, nothing is applied, and the fix wraps the inverse in a
/// `modifyAttr` — previously this hit `unexpectedCase` (`nodeMapGet` is blind to deleted items).
#[test]
fn apply_delta_modify_attr_on_deleted_map_value() {
    for clients in [(1, 2), (2, 1)] {
        let init = vec![Delta::new().insert_attr("title", Delta::new().insert_text("hi"))];
        let (doc, mut sdoc) = suggestion_pair(clients, "m", init);
        // suggestion-delete the attribute (stays rendered, delete-attributed)
        sdoc.transact_mut()
            .node_mut("m")
            .unwrap()
            .remove_attr("title");
        let fmt = suggestions(&doc, &sdoc);
        let before = render_deep(&sdoc, "m", &fmt);
        let d = Delta::new().modify_attr("title", Delta::new().insert_text("X"));
        let fix = apply_fmt(&mut sdoc, "m", &fmt, d);
        assert_eq!(
            fix,
            Delta::new().modify_attr("title", Delta::new().remove(1)),
            "no throw — the reverted operation is returned"
        );
        assert_reverted(&sdoc, "m", &fmt, &before);
    }
}

/// A plainly deleted (invisible — no renderer claims it) type keeps today's semantics: the apply
/// is silently dropped and `applyDelta` returns `null` (the caller's view shows nothing there, so
/// there is nothing to revert).
#[test]
fn apply_delta_invisible_deleted_silent_null() {
    let mut doc = Doc::with_options(Options {
        skip_gc: true,
        ..Options::with_client_id(ClientID::new(1))
    });
    let mut txn = doc.transact_mut();
    let par = txn
        .node_mut("prosemirror")
        .unwrap()
        .push_back(paragraph("hello"))
        .node_id()
        .unwrap();
    let mut par = txn.node_mut(par).unwrap();
    par.txn_mut().node_mut("prosemirror").unwrap().remove(0, 1);
    let d = Delta::new().retain(2).insert_text("XY");
    let fix = par.formatted_mut(&DefaultFormatter).apply_delta(d);
    assert!(
        fix.is_empty(),
        "invisible deleted type: silent drop, no fix"
    );
    let opts = DeltaOptions {
        deep: true,
        ..Default::default()
    };
    let root = txn.node("prosemirror").unwrap().delta(&opts);
    assert_eq!(root, Delta::out(), "nothing was applied");
}

/// `applyDelta` called directly on a deleted-but-rendered type (the top-level guard): nothing is
/// applied and the inverse against the rendered state is returned. Without a renderer the same
/// call stays a silent `null` drop.
#[test]
fn apply_delta_direct_guard_on_deleted_type() {
    for clients in [(1, 2), (2, 1)] {
        let init = vec![Delta::new().insert(paragraph("hello world"))];
        let (doc, mut sdoc) = suggestion_pair(clients, "prosemirror", init);
        let mut txn = sdoc.transact_mut();
        let par = txn.node("prosemirror").unwrap().get(0).unwrap();
        let mut par = txn.node_mut(par.node_id().unwrap()).unwrap();
        par.txn_mut().node_mut("prosemirror").unwrap().remove(0, 1);
        let fmt = suggestions(&doc, par.txn().doc());
        let opts = DeltaOptions {
            deep: true,
            ..Default::default()
        };
        let root_before = par
            .txn()
            .node("prosemirror")
            .unwrap()
            .formatted(&fmt)
            .delta(&opts);
        let d = || Delta::new().retain(2).insert_text("XY");
        // children do not inherit the root's renderer — without one the node is invisible
        let fix = par.formatted_mut(&DefaultFormatter).apply_delta(d());
        assert!(fix.is_empty(), "no renderer: silent drop");
        let fix = par.formatted_mut(&fmt).apply_delta(d());
        assert_eq!(fix, Delta::new().retain(2).remove(2));
        let root = par
            .txn()
            .node("prosemirror")
            .unwrap()
            .formatted(&fmt)
            .delta(&opts);
        assert_eq!(root, root_before, "nothing was applied to the doc");
        // yrs has no maintained `.delta` cache, nor deltas applicable onto deltas (lib0 `apply`):
        // t.assert(ynode.delta.equals(ynode.toDelta({ deep: true })))
        // parBefore.apply(d).apply(fix) == par.toDelta({ deep: true, renderer })
    }
}

/// A tombstone grandchild behind an alive child: the alive child's applyDelta bubbles the nested
/// fix, and the parent wraps it positionally — `modify(modify(inverse))`.
#[test]
fn apply_delta_nested_tombstone_fix_bubbles() {
    for clients in [(1, 2), (2, 1)] {
        let nested = In::from(Delta::with_name("nested").insert_text("ww"));
        let par = In::from(Delta::with_name("paragraph").insert(nested));
        let (doc, mut sdoc) =
            suggestion_pair(clients, "prosemirror", vec![Delta::new().insert(par)]);
        {
            let mut txn = sdoc.transact_mut();
            // suggestion-delete only the nested node inside the (alive) paragraph
            let par = txn.node("prosemirror").unwrap().get(0).unwrap();
            txn.node_mut(par.node_id().unwrap()).unwrap().remove(0, 1);
        }
        let fmt = suggestions(&doc, &sdoc);
        let before = render_deep(&sdoc, "prosemirror", &fmt);
        let d = Delta::new().modify(Delta::new().modify(Delta::new().insert_text("X")));
        let fix = apply_fmt(&mut sdoc, "prosemirror", &fmt, d);
        let inverse = Delta::new().modify(Delta::new().modify(Delta::new().remove(1)));
        assert_eq!(fix, inverse);
        assert_reverted(&sdoc, "prosemirror", &fmt, &before);
    }
}

/// A plain `delete` op spanning a tombstone keeps its existing semantics (suggestion-deletes the
/// alive content, records the attributed range) and returns no fix — the known, downstream-healed
/// gap; pinned here so a change of behavior is a conscious one.
#[test]
fn apply_delta_pure_delete_over_tombstone_no_fix() {
    for clients in [(1, 2), (2, 1)] {
        let (doc, mut sdoc) = deleted_mid_par(clients);
        let fmt = suggestions(&doc, &sdoc);
        let fix = apply_fmt(&mut sdoc, "prosemirror", &fmt, Delta::new().remove(3));
        assert!(
            fix.is_empty(),
            "a plain delete over a tombstone range returns no fix"
        );
        // yrs has no maintained `.delta` cache:
        // t.assert(ynode.delta.equals(fresh))
    }
}

/// The fix of a delta touching alive, struck and restored content, a struck nested node and a
/// struck attribute equals Yjs `inverse(d, toDeltaDeep())` of the struck node.
#[test]
fn apply_delta_fix_is_inverse_of_deep_render() {
    let p = In::from(Delta::with_name("p").insert_text("p"));
    let q = Delta::with_name("q").insert_text("abcdef").insert(p);
    let q = In::from(q.insert_attr("title", "t"));
    let init = vec![Delta::new().insert_text("x").insert(q).insert_text("z")];
    let (doc, _) = suggestion_pair((1, 2), "r", init);
    // `cd` is gc'd by a replica, `sdoc` only receives its tombstone
    let mut replica = clone_doc(&doc, None);
    let strike_cd = Delta::new().retain(2).remove(2);
    replica
        .transact_mut()
        .node_mut("r")
        .unwrap()
        .apply_delta(Delta::new().retain(1).modify(strike_cd));
    let mut sdoc = clone_doc(
        &replica,
        Some(Options {
            skip_gc: true,
            cleanup_formatting: false,
            ..Options::with_client_id(ClientID::new(2))
        }),
    );
    sdoc.transact_mut().node_mut("r").unwrap().remove(1, 1);
    let mut fmt = suggestions(&doc, &sdoc);
    fmt.restore_content(&doc.transact());
    let qd = Delta::new()
        .retain(1)
        .remove(2) // struck 'b', restored 'c'
        .retain_fmt(2, &bold()) // restored 'd', struck 'e'
        .insert_text("Y")
        .retain(1)
        .modify(Delta::new().remove(1))
        .insert_attr("title", "t2")
        .insert_attr("k", 1);
    let d = Delta::new().retain(1).modify(qd).retain_fmt(1, &bold());
    let fix = apply_fmt(&mut sdoc, "r", &fmt, d);
    let q_fix = Delta::new()
        .retain(1)
        .insert_text_attributed("bc", None, Some(deleted()))
        .retain_fmt(2, &unbold())
        .remove(1)
        .retain(1)
        .modify(Delta::new().insert_text_attributed("p", None, Some(deleted())))
        .insert_attr_attributed("title", "t", Some(deleted()))
        .remove_attr("k");
    assert_eq!(fix, Delta::new().retain(1).modify(q_fix));
}
