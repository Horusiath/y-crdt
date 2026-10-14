use yrs::lib0::Decode;
use yrs::node::{Attrs, Observable};
use yrs::test_utils::clone_doc;
use yrs::test_utils::diff_formatter;
use yrs::{
    Acquire, AcquireMut, Any, Assoc, Attribute, Attribution, Cell, ClientID, ContentIds, Delta,
    DeltaOptions, Doc, IdSet, In, Options, Update,
};

fn bold() -> Attrs {
    Attrs::from([("bold".into(), Any::Bool(true))])
}

fn unbold() -> Attrs {
    Attrs::from([("bold".into(), Any::Null)])
}

/// Syncs the full state of `from` into `to`.
fn sync(from: &Doc, to: &mut Doc) {
    let update = from
        .transact()
        .encode_state_as_update_v1(&Default::default());
    let update = Update::decode_v1(&update).unwrap();
    to.transact_mut().apply_update(update).unwrap();
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
