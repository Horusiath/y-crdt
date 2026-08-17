use arc_swap::ArcSwapOption;
use fastrand::Rng;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use yrs::node::{Attrs, Observable};
use yrs::test_utils::{RngExt, exchange_updates, run_scenario};
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{
    Any, ClientID, Delta, DeltaOptions, Doc, In, OffsetKind, Options, Out, Snapshot, StateVector,
    Update, any,
};

#[test]
fn insert_empty_string() {
    let mut doc = Doc::new();
    let mut txn = doc.transact_mut();
    let mut txt = txn.node_mut("test").unwrap();

    assert_eq!(txt.to_string(), "");

    txt.push_text("");
    assert_eq!(txt.to_string(), "");

    txt.push_text("abc");
    txt.push_text("");
    assert_eq!(txt.to_string(), "abc");
}

#[test]
fn append_single_character_blocks() {
    let mut doc = Doc::new();
    let mut txn = doc.transact_mut();
    let mut txt = txn.node_mut("test").unwrap();

    txt.insert_text(0, "a");
    txt.insert_text(1, "b");
    txt.insert_text(2, "c");

    assert_eq!(txt.to_string(), "abc");
}

#[test]
fn append_mutli_character_blocks() {
    let mut doc = Doc::new();
    let mut txn = doc.transact_mut();
    let mut txt = txn.node_mut("test").unwrap();

    txt.insert_text(0, "hello");
    txt.insert_text(5, " ");
    txt.insert_text(6, "world");

    assert_eq!(txt.to_string(), "hello world");
}

#[test]
fn prepend_single_character_blocks() {
    let mut doc = Doc::new();
    let mut txn = doc.transact_mut();
    let mut txt = txn.node_mut("test").unwrap();

    txt.insert_text(0, "a");
    txt.insert_text(0, "b");
    txt.insert_text(0, "c");

    assert_eq!(txt.to_string(), "cba");
}

#[test]
fn prepend_mutli_character_blocks() {
    let mut doc = Doc::new();
    let mut txn = doc.transact_mut();
    let mut txt = txn.node_mut("test").unwrap();

    txt.insert_text(0, "hello");
    txt.insert_text(0, " ");
    txt.insert_text(0, "world");

    assert_eq!(txt.to_string(), "world hello");
}

#[test]
fn insert_after_block() {
    let mut doc = Doc::new();
    let mut txn = doc.transact_mut();
    let mut txt = txn.node_mut("test").unwrap();

    txt.insert_text(0, "hello");
    txt.insert_text(5, " ");
    txt.insert_text(6, "world");
    txt.insert_text(6, "beautiful ");

    assert_eq!(txt.to_string(), "hello beautiful world");
}

#[test]
fn insert_inside_of_block() {
    let mut doc = Doc::new();
    let mut txn = doc.transact_mut();
    let mut txt = txn.node_mut("test").unwrap();

    txt.insert_text(0, "it was expected");
    txt.insert_text(6, " not");

    assert_eq!(txt.to_string(), "it was not expected");
}

#[test]
fn insert_concurrent_root() {
    let mut d1 = Doc::with_client_id(1);
    let mut t1 = d1.transact_mut();

    t1.node_mut("test").unwrap().insert_text(0, "hello ");

    let mut d2 = Doc::with_client_id(2);
    let mut t2 = d2.transact_mut();

    t2.node_mut("test").unwrap().insert_text(0, "world");

    let d1_sv = t1.state_vector().encode_v1();
    let d2_sv = t2.state_vector().encode_v1();

    let u1 = t1.encode_diff_v1(&StateVector::decode_v1(&d2_sv).unwrap());
    let u2 = t2.encode_diff_v1(&StateVector::decode_v1(&d1_sv).unwrap());

    t1.apply_update(Update::decode_v1(u2.as_slice()).unwrap())
        .unwrap();
    t2.apply_update(Update::decode_v1(u1.as_slice()).unwrap())
        .unwrap();

    let a = t1.node("test").unwrap().to_string();
    let b = t2.node("test").unwrap().to_string();

    assert_eq!(a, b);
    assert_eq!(a.as_str(), "hello world");
}

#[test]
fn insert_concurrent_in_the_middle() {
    let mut d1 = Doc::with_client_id(1);
    let mut t1 = d1.transact_mut();

    t1.node_mut("test").unwrap().insert_text(0, "I expect that");
    assert_eq!(
        t1.node("test").unwrap().to_string().as_str(),
        "I expect that"
    );

    let mut d2 = Doc::with_client_id(2);
    let mut t2 = d2.transact_mut();

    let d2_sv = t2.state_vector().encode_v1();
    let u1 = t1.encode_diff_v1(&StateVector::decode_v1(&d2_sv).unwrap());
    t2.apply_update(Update::decode_v1(u1.as_slice()).unwrap())
        .unwrap();

    let actual = t2.node("test").unwrap().to_string();
    assert_eq!(actual, "I expect that");

    let mut txt2 = t2.node_mut("test").unwrap();
    txt2.insert_text(1, " have");
    txt2.insert_text(13, "ed");
    assert_eq!(txt2.to_string(), "I have expected that");

    let mut txt1 = t1.node_mut("test").unwrap();
    txt1.insert_text(1, " didn't");
    assert_eq!(txt1.to_string(), "I didn't expect that");

    let d2_sv = t2.state_vector().encode_v1();
    let d1_sv = t1.state_vector().encode_v1();
    let u1 = t1.encode_diff_v1(&StateVector::decode_v1(&d2_sv.as_slice()).unwrap());
    let u2 = t2.encode_diff_v1(&StateVector::decode_v1(&d1_sv.as_slice()).unwrap());
    t1.apply_update(Update::decode_v1(u2.as_slice()).unwrap())
        .unwrap();
    t2.apply_update(Update::decode_v1(u1.as_slice()).unwrap())
        .unwrap();

    let a = t1.node("test").unwrap().to_string();
    let b = t2.node("test").unwrap().to_string();

    assert_eq!(a, b);
    assert_eq!(a, "I didn't have expected that");
}

#[test]
fn append_concurrent() {
    let mut d1 = Doc::with_client_id(1);
    let mut t1 = d1.transact_mut();

    let mut txt1 = t1.node_mut("test").unwrap();
    txt1.insert_text(0, "aaa");
    assert_eq!(txt1.to_string(), "aaa");

    let mut d2 = Doc::with_client_id(2);
    let mut t2 = d2.transact_mut();

    let d2_sv = t2.state_vector().encode_v1();
    let u1 = t1.encode_diff_v1(&StateVector::decode_v1(&d2_sv.as_slice()).unwrap());
    t2.apply_update(Update::decode_v1(u1.as_slice()).unwrap())
        .unwrap();

    let mut txt2 = t2.node_mut("test").unwrap();
    assert_eq!(txt2.to_string().as_str(), "aaa");

    txt2.insert_text(3, "bbb");
    txt2.insert_text(6, "bbb");
    assert_eq!(txt2.to_string(), "aaabbbbbb");

    t1.node_mut("test").unwrap().insert_text(3, "aaa");
    assert_eq!(t1.node("test").unwrap().to_string(), "aaaaaa");

    let d2_sv = t2.state_vector().encode_v1();
    let d1_sv = t1.state_vector().encode_v1();
    let u1 = t1.encode_diff_v1(&StateVector::decode_v1(&d2_sv.as_slice()).unwrap());
    let u2 = t2.encode_diff_v1(&StateVector::decode_v1(&d1_sv.as_slice()).unwrap());

    t1.apply_update(Update::decode_v1(u2.as_slice()).unwrap())
        .unwrap();
    t2.apply_update(Update::decode_v1(u1.as_slice()).unwrap())
        .unwrap();

    let a = t1.node("test").unwrap().to_string();
    let b = t2.node("test").unwrap().to_string();

    assert_eq!(a.as_str(), "aaaaaabbbbbb");
    assert_eq!(a, b);
}

#[test]
fn delete_single_block_start() {
    let mut doc = Doc::new();
    let mut txn = doc.transact_mut();
    let mut txt = txn.node_mut("test").unwrap();

    txt.insert_text(0, "bbb");
    txt.insert_text(0, "aaa");
    txt.remove(0, 3);

    assert_eq!(txt.len(), 3);
    assert_eq!(txt.to_string(), "bbb");
}

#[test]
fn delete_single_block_end() {
    let mut doc = Doc::new();
    let mut txn = doc.transact_mut();
    let mut txt = txn.node_mut("test").unwrap();

    txt.insert_text(0, "bbb");
    txt.insert_text(0, "aaa");
    txt.remove(3, 3);

    assert_eq!(txt.to_string(), "aaa");
}

#[test]
fn delete_multiple_whole_blocks() {
    let mut doc = Doc::new();
    let mut txn = doc.transact_mut();
    let mut txt = txn.node_mut("test").unwrap();

    txt.insert_text(0, "a");
    txt.insert_text(1, "b");
    txt.insert_text(2, "c");

    txt.remove(1, 1);
    assert_eq!(txt.to_string(), "ac");

    txt.remove(1, 1);
    assert_eq!(txt.to_string(), "a");

    txt.remove(0, 1);
    assert_eq!(txt.to_string(), "");
}

#[test]
fn delete_slice_of_block() {
    let mut doc = Doc::new();
    let mut txn = doc.transact_mut();
    let mut txt = txn.node_mut("test").unwrap();

    txt.insert_text(0, "abc");
    txt.remove(1, 1);

    assert_eq!(txt.to_string(), "ac");
}

#[test]
fn delete_multiple_blocks_with_slicing() {
    let mut doc = Doc::new();
    let mut txn = doc.transact_mut();
    let mut txt = txn.node_mut("test").unwrap();

    txt.insert_text(0, "hello ");
    txt.insert_text(6, "beautiful");
    txt.insert_text(15, " world");

    txt.remove(5, 11);
    assert_eq!(txt.to_string(), "helloworld");
}

#[test]
fn insert_after_delete() {
    let mut doc = Doc::new();
    let mut txn = doc.transact_mut();
    let mut txt = txn.node_mut("test").unwrap();

    txt.insert_text(0, "hello ");
    txt.remove(0, 5);
    txt.insert_text(1, "world");

    assert_eq!(txt.to_string(), " world");
}

#[test]
fn concurrent_insert_delete() {
    let mut d1 = Doc::with_client_id(1);
    let mut t1 = d1.transact_mut();

    let mut txt1 = t1.node_mut("test").unwrap();
    txt1.insert_text(0, "hello world");
    assert_eq!(txt1.to_string(), "hello world");

    let u1 = t1.encode_state_as_update_v1(&StateVector::default());

    let mut d2 = Doc::with_client_id(2);
    let mut t2 = d2.transact_mut();
    t2.apply_update(Update::decode_v1(u1.as_slice()).unwrap())
        .unwrap();
    assert_eq!(t2.node("test").unwrap().to_string().as_str(), "hello world");

    let mut txt1 = t1.node_mut("test").unwrap();
    txt1.insert_text(5, " beautiful");
    txt1.insert_text(21, "!");
    txt1.remove(0, 5);
    assert_eq!(txt1.to_string(), " beautiful world!");

    let mut txt2 = t2.node_mut("test").unwrap();
    txt2.remove(5, 5);
    txt2.remove(0, 1);
    txt2.insert_text(0, "H");
    assert_eq!(txt2.to_string(), "Hellod");

    let sv1 = t1.state_vector().encode_v1();
    let sv2 = t2.state_vector().encode_v1();
    let u1 = t1.encode_diff_v1(&StateVector::decode_v1(&sv2.as_slice()).unwrap());
    let u2 = t2.encode_diff_v1(&StateVector::decode_v1(&sv1.as_slice()).unwrap());

    t1.apply_update(Update::decode_v1(u2.as_slice()).unwrap())
        .unwrap();
    t2.apply_update(Update::decode_v1(u1.as_slice()).unwrap())
        .unwrap();

    let a = t1.node("test").unwrap().to_string();
    let b = t2.node("test").unwrap().to_string();

    assert_eq!(a, b);
    assert_eq!(a, "H beautifuld!".to_owned());
}

#[test]
fn observer() {
    let mut doc = Doc::with_client_id(1);
    let delta = Arc::new(ArcSwapOption::default());
    let delta_c = delta.clone();
    let sub = {
        let mut txn = doc.transact_mut();
        txn.node_mut("text").unwrap().observe(move |_txn, e| {
            delta_c.store(Some(Arc::new(e.delta(()).collect::<Vec<_>>())));
        })
    };

    // insert initial data to an empty YText
    {
        let mut txn = doc.transact_mut();
        txn.node_mut("text").unwrap().insert_text(0, "abcd"); // => 'abcd'
    }
    // TODO(unified-api): the `Delta` event enum is not available yet
    assert_eq!(
        delta.load_full(),
        Some(Arc::new(vec![Delta::Inserted("abcd".into(), None)]))
    );

    // remove 2 chars from the middle
    {
        let mut txn = doc.transact_mut();
        txn.node_mut("text").unwrap().remove(1, 2); // => 'ad'
    }
    // TODO(unified-api): the `Delta` event enum is not available yet
    assert_eq!(
        delta.load_full(),
        Some(Arc::new(vec![Delta::Retain(1, None), Delta::Deleted(2)]))
    );

    // insert new item in the middle
    let attrs = Attrs::from([("bold".into(), true.into())]);
    {
        let mut txn = doc.transact_mut();
        txn.node_mut("text")
            .unwrap()
            .insert_text_with(1, "e", attrs.clone()); // => 'a<bold>e</bold>d'
    }
    // TODO(unified-api): the `Delta` event enum is not available yet
    assert_eq!(
        delta.load_full(),
        Some(Arc::new(vec![
            Delta::Retain(1, None),
            Delta::Inserted("e".into(), Some(Box::new(attrs)))
        ]))
    );

    // remove formatting
    let attrs = Attrs::from([("bold".into(), Any::Null)]);
    {
        let mut txn = doc.transact_mut();
        txn.node_mut("text").unwrap().format(1, 1, attrs.clone()); // => 'aed'
    }
    // TODO(unified-api): the `Delta` event enum is not available yet
    assert_eq!(
        delta.swap(None),
        Some(Arc::new(vec![
            Delta::Retain(1, None),
            Delta::Retain(2, Some(Box::new(attrs)))
        ]))
    );
    delta.swap(None);

    // free the observer and make sure that callback is no longer called
    drop(sub);
    {
        let mut txn = doc.transact_mut();
        txn.node_mut("text").unwrap().insert_text(1, "fgh"); // => 'afghed'
    }
    assert_eq!(delta.swap(None), None);
}

#[test]
fn insert_and_remove_event_changes() {
    let mut d1 = Doc::with_client_id(1);
    let delta = Arc::new(ArcSwapOption::default());
    let delta_c = delta.clone();
    let _sub = {
        let mut txn = d1.transact_mut();
        txn.node_mut("text")
            .unwrap()
            .observe(move |_txn, e| delta_c.store(Some(Arc::new(e.delta(()).collect::<Vec<_>>()))))
    };

    // insert initial string
    {
        let mut txn = d1.transact_mut();
        txn.node_mut("text").unwrap().insert_text(0, "abcd");
    }
    // TODO(unified-api): the `Delta` event enum is not available yet
    assert_eq!(
        delta.swap(None),
        Some(Arc::new(vec![Delta::Inserted("abcd".into(), None)]))
    );

    // remove middle
    {
        let mut txn = d1.transact_mut();
        txn.node_mut("text").unwrap().remove(1, 2);
    }
    // TODO(unified-api): the `Delta` event enum is not available yet
    assert_eq!(
        delta.swap(None),
        Some(Arc::new(vec![Delta::Retain(1, None), Delta::Deleted(2)]))
    );

    // insert again
    {
        let mut txn = d1.transact_mut();
        txn.node_mut("text").unwrap().insert_text(1, "ef");
    }
    // TODO(unified-api): the `Delta` event enum is not available yet
    assert_eq!(
        delta.swap(None),
        Some(Arc::new(vec![
            Delta::Retain(1, None),
            Delta::Inserted("ef".into(), None)
        ]))
    );

    // replicate data to another peer
    let mut d2 = Doc::with_client_id(2);
    let delta_c = delta.clone();
    let _sub = {
        let mut txn = d2.transact_mut();
        txn.node_mut("text")
            .unwrap()
            .observe(move |_txn, e| delta_c.store(Some(Arc::new(e.delta(()).collect::<Vec<_>>()))))
    };

    {
        let t1 = d1.transact();
        let mut t2 = d2.transact_mut();

        let sv = t2.state_vector();
        let update = t1.encode_diff_v1(&sv);
        t2.apply_update(Update::decode_v1(update.as_slice()).unwrap())
            .unwrap();
    }
    // TODO(unified-api): the `Delta` event enum is not available yet
    assert_eq!(
        delta.swap(None),
        Some(Arc::new(vec![Delta::Inserted("aefd".into(), None)]))
    );
}

fn text_transactions() -> [Box<dyn Fn(&mut Doc, &mut Rng)>; 2] {
    fn insert_text(doc: &mut Doc, rng: &mut Rng) {
        let mut txn = doc.transact_mut();
        let mut ytext = txn.node_mut("text").unwrap();
        let pos = rng.between(0, ytext.len());
        let word = rng.random_string();
        ytext.insert_text(pos, word.as_str());
    }

    fn delete_text(doc: &mut Doc, rng: &mut Rng) {
        let mut txn = doc.transact_mut();
        let mut ytext = txn.node_mut("text").unwrap();
        let len = ytext.len();
        if len > 0 {
            let pos = rng.between(0, len - 1);
            let to_delete = rng.between(2, len - pos);
            ytext.remove(pos, to_delete);
        }
    }

    [Box::new(insert_text), Box::new(delete_text)]
}

fn fuzzy(iterations: usize) {
    run_scenario(0, &text_transactions(), 5, iterations)
}

#[test]
fn fuzzy_test_3() {
    fuzzy(3)
}

#[test]
fn basic_format() {
    let mut d1 = Doc::with_client_id(1);
    let mut d2 = Doc::with_client_id(2);

    let delta1 = Arc::new(ArcSwapOption::default());
    let delta_clone = delta1.clone();
    let _sub1 = {
        let mut txn = d1.transact_mut();
        txn.node_mut("text").unwrap().observe(move |_txn, e| {
            delta_clone.store(Some(Arc::new(e.delta(()).collect::<Vec<_>>())))
        })
    };

    let delta2 = Arc::new(ArcSwapOption::default());
    let delta_clone = delta2.clone();
    let _sub2 = {
        let mut txn = d2.transact_mut();
        txn.node_mut("text").unwrap().observe(move |_txn, e| {
            delta_clone.store(Some(Arc::new(e.delta(()).collect::<Vec<_>>())))
        })
    };

    let a: Attrs = HashMap::from([("bold".into(), Any::Bool(true))]);

    // step 1
    {
        let mut txn = d1.transact_mut();
        txn.node_mut("text")
            .unwrap()
            .insert_text_with(0, "abc", a.clone());
        let update = txn.encode_update_v1();
        drop(txn);

        // TODO(unified-api): Text::diff/Diff and the `Delta` event enum are not available yet
        let expected = Some(Arc::new(vec![Delta::Inserted(
            "abc".into(),
            Some(Box::new(a.clone())),
        )]));
        assert_eq!(
            txt1.diff(&d1.transact(), YChange::identity),
            vec![Diff::new("abc".into(), Some(Box::new(a.clone())))]
        );
        assert_eq!(delta1.swap(None), expected);
        assert_eq!(delta2.swap(None), expected);
        assert_eq!(d1.transact().node("text").unwrap().to_string(), "abc");

        let mut txn = d2.transact_mut();
        txn.apply_update(Update::decode_v1(update.as_slice()).unwrap())
            .unwrap();
        drop(txn);

        assert_eq!(d2.transact().node("text").unwrap().to_string(), "abc");
    }

    // step 2
    {
        let mut txn = d1.transact_mut();
        txn.node_mut("text").unwrap().remove(0, 1);
        let update = txn.encode_update_v1();
        drop(txn);

        // TODO(unified-api): Text::diff/Diff and the `Delta` event enum are not available yet
        let expected = Some(Arc::new(vec![Delta::Deleted(1)]));
        assert_eq!(
            txt1.diff(&d1.transact(), YChange::identity),
            vec![Diff::new("bc".into(), Some(Box::new(a.clone())))]
        );
        assert_eq!(delta1.swap(None), expected);
        assert_eq!(delta2.swap(None), expected);
        assert_eq!(d1.transact().node("text").unwrap().to_string(), "bc");

        let mut txn = d2.transact_mut();
        txn.apply_update(Update::decode_v1(update.as_slice()).unwrap())
            .unwrap();
        drop(txn);

        assert_eq!(d2.transact().node("text").unwrap().to_string(), "bc");
    }

    // step 3
    {
        let mut txn = d1.transact_mut();
        txn.node_mut("text").unwrap().remove(1, 1);
        let update = txn.encode_update_v1();
        drop(txn);

        // TODO(unified-api): Text::diff/Diff and the `Delta` event enum are not available yet
        let expected = Some(Arc::new(vec![Delta::Retain(1, None), Delta::Deleted(1)]));
        assert_eq!(
            txt1.diff(&d1.transact(), YChange::identity),
            vec![Diff::new("b".into(), Some(Box::new(a.clone())))]
        );
        assert_eq!(delta1.swap(None), expected);
        assert_eq!(delta2.swap(None), expected);
        assert_eq!(d1.transact().node("text").unwrap().to_string(), "b");

        let mut txn = d2.transact_mut();
        txn.apply_update(Update::decode_v1(update.as_slice()).unwrap())
            .unwrap();
        drop(txn);

        assert_eq!(d2.transact().node("text").unwrap().to_string(), "b");
    }

    // step 4
    {
        let mut txn = d1.transact_mut();
        txn.node_mut("text")
            .unwrap()
            .insert_text_with(0, "z", a.clone());
        let update = txn.encode_update_v1();
        drop(txn);

        // TODO(unified-api): Text::diff/Diff and the `Delta` event enum are not available yet
        let expected = Some(Arc::new(vec![Delta::Inserted(
            "z".into(),
            Some(Box::new(a.clone())),
        )]));
        assert_eq!(
            txt1.diff(&mut d1.transact_mut(), YChange::identity),
            vec![Diff::new("zb".into(), Some(Box::new(a.clone())))]
        );
        assert_eq!(delta1.swap(None), expected);
        assert_eq!(delta2.swap(None), expected);
        assert_eq!(d1.transact().node("text").unwrap().to_string(), "zb");

        let mut txn = d2.transact_mut();
        txn.apply_update(Update::decode_v1(update.as_slice()).unwrap())
            .unwrap();
        drop(txn);

        assert_eq!(d2.transact().node("text").unwrap().to_string(), "zb");
    }

    // step 5
    {
        let mut txn = d1.transact_mut();
        txn.node_mut("text").unwrap().insert_text(0, "y");
        let update = txn.encode_update_v1();
        drop(txn);

        // TODO(unified-api): Text::diff/Diff and the `Delta` event enum are not available yet
        let expected = Some(Arc::new(vec![Delta::Inserted("y".into(), None)]));
        assert_eq!(
            txt1.diff(&mut d1.transact_mut(), YChange::identity),
            vec![
                Diff::new("y".into(), None),
                Diff::new("zb".into(), Some(Box::new(a.clone())))
            ]
        );
        assert_eq!(delta1.swap(None), expected);
        assert_eq!(delta2.swap(None), expected);
        assert_eq!(d1.transact().node("text").unwrap().to_string(), "yzb");

        let mut txn = d2.transact_mut();
        txn.apply_update(Update::decode_v1(update.as_slice()).unwrap())
            .unwrap();
        drop(txn);

        assert_eq!(d2.transact().node("text").unwrap().to_string(), "yzb");
    }

    // step 6
    {
        let mut txn = d1.transact_mut();
        let b: Attrs = HashMap::from([("bold".into(), Any::Null)]);
        txn.node_mut("text").unwrap().format(0, 2, b.clone());
        let update = txn.encode_update_v1();
        drop(txn);

        // TODO(unified-api): Text::diff/Diff and the `Delta` event enum are not available yet
        let expected = Some(Arc::new(vec![
            Delta::Retain(1, None),
            Delta::Retain(1, Some(Box::new(b))),
        ]));
        assert_eq!(
            txt1.diff(&mut d1.transact_mut(), YChange::identity),
            vec![
                Diff::new("yz".into(), None),
                Diff::new("b".into(), Some(Box::new(a.clone())))
            ]
        );
        assert_eq!(delta1.swap(None), expected);
        assert_eq!(delta2.swap(None), expected);
        assert_eq!(d1.transact().node("text").unwrap().to_string(), "yzb");

        let mut txn = d2.transact_mut();
        txn.apply_update(Update::decode_v1(update.as_slice()).unwrap())
            .unwrap();
        drop(txn);

        assert_eq!(d2.transact().node("text").unwrap().to_string(), "yzb");
    }
}

#[test]
fn embed_with_attributes() {
    let mut d1 = Doc::with_client_id(1);

    let delta1 = Arc::new(ArcSwapOption::default());
    let delta_clone = delta1.clone();
    let _sub1 = {
        let mut txn = d1.transact_mut();
        txn.node_mut("text").unwrap().observe(move |_txn, e| {
            delta_clone.store(Some(Arc::new(e.delta(()).collect::<Vec<_>>())));
        })
    };

    let a1: Attrs = HashMap::from([("bold".into(), true.into())]);
    let embed = any!({
        "image": "imageSrc.png"
    });

    let (update_v1, update_v2) = {
        let mut txn = d1.transact_mut();
        let a2: Attrs = HashMap::from([("width".into(), Any::Number(100.0))]);
        let mut txt1 = txn.node_mut("text").unwrap();
        txt1.insert_text_with(0, "ab", a1.clone());
        txt1.apply_delta([Delta::new()
            .retain(1)
            .insert_with(embed.clone(), a2.clone())]);

        // TODO(unified-api): Text::diff/Diff and the `Delta` event enum are not available yet
        let a1 = Some(Box::new(a1.clone()));
        let a2 = Some(Box::new(a2.clone()));
        let expected = Some(Arc::new(vec![
            Delta::Inserted("a".into(), a1.clone()),
            Delta::Inserted(embed.clone().into(), a2.clone()),
            Delta::Inserted("b".into(), a1.clone()),
        ]));
        assert_eq!(delta1.swap(None), expected);
        let expected = vec![
            Diff::new("a".into(), a1.clone()),
            Diff::new(embed.clone().into(), a2),
            Diff::new("b".into(), a1.clone()),
        ];
        assert_eq!(txt1.diff(&mut txn, YChange::identity), expected);

        let update_v1 = txn.encode_state_as_update_v1(&StateVector::default());
        let update_v2 = txn.encode_state_as_update_v2(&StateVector::default());
        (update_v1, update_v2)
    };

    let mut d2 = Doc::new();
    {
        let mut txn = d2.transact_mut();
        let update = Update::decode_v1(&update_v1).unwrap();
        txn.apply_update(update).unwrap();
        // TODO(unified-api): Text::diff/Diff are not available yet
        assert_eq!(txt2.diff(txn, YChange::identity), expected);
    }

    let mut d3 = Doc::new();
    {
        let mut txn = d3.transact_mut();
        let update = Update::decode_v2(&update_v2).unwrap();
        txn.apply_update(update).unwrap();
        // TODO(unified-api): Text::diff/Diff are not available yet
        let actual = txt3.diff(txn, YChange::identity);
        assert_eq!(actual, expected);
    }
}

#[test]
fn issue_101() {
    let mut d1 = Doc::with_client_id(1);
    let delta = Arc::new(ArcSwapOption::default());
    let delta_copy = delta.clone();

    let attrs: Attrs = HashMap::from([("bold".into(), true.into())]);

    {
        let mut txn = d1.transact_mut();
        txn.node_mut("text").unwrap().insert_text(0, "abcd");
    }

    let _sub = {
        let txn = d1.transact();
        txn.node("text").unwrap().observe(move |_txn, e| {
            delta_copy.store(Some(Arc::new(e.delta(()).collect::<Vec<_>>())));
        })
    };
    {
        let mut txn = d1.transact_mut();
        txn.node_mut("text").unwrap().format(1, 2, attrs.clone());
    }
    // TODO(unified-api): the `Delta` event enum is not available yet
    let expected = Arc::new(vec![
        Delta::Retain(1, None),
        Delta::Retain(2, Some(Box::new(attrs))),
    ]);
    let actual = delta.load_full();
    assert_eq!(actual, Some(expected));
}

#[test]
fn yrs_delete() {
    let mut doc = Doc::with_options(Options {
        offset_kind: OffsetKind::Utf16,
        ..Default::default()
    });

    let text1 = r#"
		Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Eleifend mi in nulla posuere sollicitudin. Lorem mollis aliquam ut porttitor. Enim ut sem viverra aliquet eget sit amet. Sed turpis tincidunt id aliquet risus feugiat in ante metus. Accumsan lacus vel facilisis volutpat. Non consectetur a erat nam at lectus urna. Enim diam vulputate ut pharetra sit amet. In dictum non consectetur a erat. Bibendum at varius vel pharetra vel turpis nunc eget lorem. Blandit cursus risus at ultrices. Sed lectus vestibulum mattis ullamcorper velit sed ullamcorper. Sagittis nisl rhoncus mattis rhoncus.

		Sed vulputate odio ut enim. Erat pellentesque adipiscing commodo elit at imperdiet dui. Ultricies tristique nulla aliquet enim tortor at auctor urna nunc. Tincidunt eget nullam non nisi est sit amet. Sed adipiscing diam donec adipiscing tristique risus nec. Risus commodo viverra maecenas accumsan lacus vel facilisis volutpat est. Donec enim diam vulputate ut pharetra sit amet aliquam id. Netus et malesuada fames ac turpis egestas sed tempus urna. Augue mauris augue neque gravida. Tellus orci ac auctor augue mauris augue. Ante metus dictum at tempor. Feugiat in ante metus dictum at. Vitae elementum curabitur vitae nunc sed velit dignissim. Non arcu risus quis varius quam quisque id diam vel. Fermentum leo vel orci porta non. Donec adipiscing tristique risus nec feugiat in fermentum posuere. Duis convallis convallis tellus id interdum velit laoreet id. Vel eros donec ac odio tempor orci dapibus ultrices in. At varius vel pharetra vel turpis nunc eget lorem. Blandit aliquam etiam erat velit scelerisque in.
		"#;

    let text2 = r#"test"#;

    {
        let mut txn = doc.transact_mut();
        txn.node_mut("content").unwrap().insert_text(0, text1);
        txn.commit();
    }

    {
        let mut txn = doc.transact_mut();
        txn.node_mut("content").unwrap().insert_text(100, text2);
        txn.commit();
    }

    {
        let c1 = text1.chars().count();
        let c2 = text2.chars().count();
        let count = c1 as u32 + c2 as u32;

        // TODO(unified-api): the `Delta` event enum is not available yet
        let _observer =
            text.observe(move |txn, edit| assert_eq!(edit.delta(txn)[0], Delta::Deleted(count)));
        let _observer = {
            let txn = doc.transact();
            txn.node("content").unwrap().observe(move |_txn, e| {
                e.delta(()).for_each(drop);
            })
        };

        let mut txn = doc.transact_mut();
        txn.node_mut("content").unwrap().remove(0, count);
        txn.commit();
    }

    assert_eq!(doc.transact().node("content").unwrap().to_string(), "");
}

#[test]
fn text_diff_adjacent() {
    let mut doc = Doc::with_client_id(1);
    let mut txn = doc.transact_mut();
    let mut txt = txn.node_mut("text").unwrap();
    let attrs1 = Attrs::from([("a".into(), "a".into())]);
    txt.insert_text_with(0, "abc", attrs1.clone());
    let attrs2 = Attrs::from([("a".into(), "a".into()), ("b".into(), "b".into())]);
    txt.insert_text_with(3, "def", attrs2.clone());

    // TODO(unified-api): Text::diff/Diff are not available yet
    let diff = txt.diff(&mut txn, YChange::identity);
    let expected = vec![
        Diff::new("abc".into(), Some(Box::new(attrs1))),
        Diff::new("def".into(), Some(Box::new(attrs2))),
    ];
    assert_eq!(diff, expected);
}

#[test]
fn text_remove_4_byte_range() {
    let mut d1 = Doc::new();

    d1.transact_mut()
        .node_mut("test")
        .unwrap()
        .insert_text(0, "😭😊");

    let mut d2 = Doc::new();
    exchange_updates(&mut [&mut d1, &mut d2]);

    d1.transact_mut()
        .node_mut("test")
        .unwrap()
        .remove(0, "😭".len() as u32);
    assert_eq!(d1.transact().node("test").unwrap().to_string(), "😊");

    exchange_updates(&mut [&mut d1, &mut d2]);
    assert_eq!(d2.transact().node("test").unwrap().to_string(), "😊");
}

#[test]
fn text_remove_3_byte_range() {
    let mut d1 = Doc::new();

    d1.transact_mut()
        .node_mut("test")
        .unwrap()
        .insert_text(0, "⏰⏳");

    let mut d2 = Doc::new();
    exchange_updates(&mut [&mut d1, &mut d2]);

    d1.transact_mut()
        .node_mut("test")
        .unwrap()
        .remove(0, "⏰".len() as u32);
    assert_eq!(d1.transact().node("test").unwrap().to_string(), "⏳");

    exchange_updates(&mut [&mut d1, &mut d2]);
    assert_eq!(d2.transact().node("test").unwrap().to_string(), "⏳");
}
#[test]
fn delete_4_byte_character_from_middle() {
    let mut doc = Doc::new();
    let mut txn = doc.transact_mut();
    let mut txt = txn.node_mut("test").unwrap();

    txt.insert_text(0, "😊😭");
    // uncomment the following line will pass the test
    // txt.format(0, "😊".len() as u32, HashMap::new());
    txt.remove("😊".len() as u32, "😭".len() as u32);

    assert_eq!(txt.to_string(), "😊");
}

#[test]
fn delete_3_byte_character_from_middle_1() {
    let mut doc = Doc::new();
    let mut txn = doc.transact_mut();
    let mut txt = txn.node_mut("test").unwrap();

    txt.insert_text(0, "⏰⏳");
    // uncomment the following line will pass the test
    // txt.format(0, "⏰".len() as u32, HashMap::new());
    txt.remove("⏰".len() as u32, "⏳".len() as u32);

    assert_eq!(txt.to_string(), "⏰");
}

#[test]
fn delete_3_byte_character_from_middle_2() {
    let mut doc = Doc::new();
    let mut txn = doc.transact_mut();
    let mut txt = txn.node_mut("test").unwrap();

    txt.insert_text(0, "👯🙇‍♀️🙇‍♀️⏰👩‍❤️‍💋‍👨");

    txt.format("👯".len() as u32, "🙇‍♀️🙇‍♀️".len() as u32, HashMap::new());
    txt.remove("👯🙇‍♀️🙇‍♀️".len() as u32, "⏰".len() as u32); // will delete ⏰ and 👩‍❤️‍💋‍👨

    assert_eq!(txt.to_string(), "👯🙇‍♀️🙇‍♀️👩‍❤️‍💋‍👨");
}

#[test]
fn delete_3_byte_character_from_middle_after_insert_and_format() {
    let mut doc = Doc::new();
    let mut txn = doc.transact_mut();
    let mut txt = txn.node_mut("test").unwrap();

    txt.insert_text(0, "🙇‍♀️🙇‍♀️⏰👩‍❤️‍💋‍👨");
    txt.insert_text(0, "👯");
    txt.format("👯".len() as u32, "🙇‍♀️🙇‍♀️".len() as u32, HashMap::new());

    // will delete ⏰ and 👩‍❤️‍💋‍👨
    txt.remove("👯🙇‍♀️🙇‍♀️".len() as u32, "⏰".len() as u32); // will delete ⏰ and 👩‍❤️‍💋‍👨

    assert_eq!(&txt.to_string(), "👯🙇‍♀️🙇‍♀️👩‍❤️‍💋‍👨");
}

#[test]
fn delete_multi_byte_character_from_middle_after_insert_and_format() {
    let mut doc = Doc::with_client_id(1);
    let mut txn = doc.transact_mut();
    let mut txt = txn.node_mut("test").unwrap();

    txt.insert_text(0, "❤️❤️🙇‍♀️🙇‍♀️⏰👩‍❤️‍💋‍👨👩‍❤️‍💋‍👨");
    txt.insert_text(0, "👯");
    txt.format("👯".len() as u32, "❤️❤️🙇‍♀️🙇‍♀️⏰".len() as u32, HashMap::new());
    txt.insert_text("👯❤️❤️🙇‍♀️🙇‍♀️⏰".len() as u32, "⏰");
    txt.format(
        "👯❤️❤️🙇‍♀️🙇‍♀️⏰⏰".len() as u32,
        "👩‍❤️‍💋‍👨".len() as u32,
        HashMap::new(),
    );
    txt.remove("👯❤️❤️🙇‍♀️🙇‍♀️⏰⏰👩‍❤️‍💋‍👩".len() as u32, "👩‍❤️‍💋‍👨".len() as u32);
    assert_eq!(txt.to_string().as_str(), "👯❤️❤️🙇‍♀️🙇‍♀️⏰⏰👩‍❤️‍💋‍👨");
}

#[test]
fn insert_string_with_no_attribute() {
    let mut doc = Doc::new();
    let mut txn = doc.transact_mut();
    let mut txt = txn.node_mut("test").unwrap();

    let attrs = Attrs::from([("a".into(), "a".into())]);
    txt.insert_text_with(0, "ac", attrs.clone());
    txt.insert_text_with(1, "b", Attrs::new());

    // TODO(unified-api): Text::diff/Diff are not available yet
    let expect = vec![
        Diff::new("a".into(), Some(Box::new(attrs.clone()))),
        Diff::new("b".into(), None),
        Diff::new("c".into(), Some(Box::new(attrs.clone()))),
    ];
    assert!(txt.diff(&mut txn, YChange::identity).eq(&expect))
}

#[test]
fn insert_empty_string_with_attributes() {
    let mut doc = Doc::new();
    let mut txn = doc.transact_mut();

    {
        let attrs = Attrs::from([("a".into(), "a".into())]);
        let mut txt = txn.node_mut("test").unwrap();
        txt.insert_text(0, "abc");
        txt.insert_text(1, ""); // nothing changes
        txt.insert_text_with(1, "", attrs); // nothing changes

        assert_eq!(txt.to_string(), "abc");
    }

    let bin = txn.encode_state_as_update_v1(&StateVector::default());

    let mut doc = Doc::new();
    let mut txn = doc.transact_mut();
    let update = Update::decode_v1(bin.as_slice()).unwrap();
    txn.apply_update(update).unwrap();

    assert_eq!(txn.node("test").unwrap().to_string(), "abc");
}

#[test]
fn snapshots() {
    let mut doc = Doc::with_client_id(1);
    doc.transact_mut()
        .node_mut("text")
        .unwrap()
        .insert_text(0, "hello");
    let _prev = doc.transact_mut().snapshot();
    doc.transact_mut()
        .node_mut("text")
        .unwrap()
        .insert_text(5, " world");
    let _next = doc.transact_mut().snapshot();

    // TODO(unified-api): Text::diff_range/YChange are not available yet
    let diff = text.diff_range(
        &mut doc.transact_mut(),
        Some(&next),
        Some(&prev),
        YChange::identity,
    );
    assert_eq!(
        diff,
        vec![
            Diff::new("hello".into(), None),
            Diff::with_change(
                " world".into(),
                None,
                Some(YChange::new(
                    ChangeKind::Added,
                    ID::new(ClientID::new(1), 5)
                ))
            )
        ]
    )
}

#[test]
fn diff_with_embedded_items() {
    let mut doc = Doc::new();
    let mut txn = doc.transact_mut();
    let mut text = txn.node_mut("article").unwrap();

    let bold = Attrs::from([("b".into(), true.into())]);
    let italic = Attrs::from([("i".into(), true.into())]);

    text.insert_text_with(0, "hello world", italic.clone()); // "<i>hello world</i>"
    text.format(6, 5, bold.clone()); // "<i>hello <b>world</b></i>"
    let image = Any::Buffer(vec![0, 0, 0, 0].into());
    text.insert(5, image.clone()); // insert binary after "hello"
    text.insert(5, In::Node(Delta::new())); // insert array ref after "hello"

    // TODO(unified-api): Text::diff/Diff are not available yet
    let italic_and_bold = Attrs::from([("b".into(), true.into()), ("i".into(), true.into())]);
    let chunks = text.diff(&txn, YChange::identity);
    assert_eq!(
        chunks,
        vec![
            Diff::new("hello".into(), Some(Box::new(italic.clone()))),
            Diff::new(Out::YArray(array), Some(Box::new(italic.clone()))),
            Diff::new(image.into(), Some(Box::new(italic.clone()))),
            Diff::new(" ".into(), Some(Box::new(italic))),
            Diff::new("world".into(), Some(Box::new(italic_and_bold))),
        ]
    );
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
            let mut txn = doc.transact_mut();
            txn.node_mut("test").unwrap().push_text("a");
        }
    });

    let d3 = doc.clone();
    let h3 = spawn(move || {
        for _ in 0..10 {
            let millis = fastrand::u64(1..20);
            sleep(Duration::from_millis(millis));

            let mut doc = d3.write().unwrap();
            let mut txn = doc.transact_mut();
            txn.node_mut("test").unwrap().push_text("b");
        }
    });

    h3.join().unwrap();
    h2.join().unwrap();

    let doc = doc.write().unwrap();
    let len = doc.transact().node("test").unwrap().len();
    assert_eq!(len, 20);
}

#[test]
fn multiline_format() {
    let mut doc = Doc::with_client_id(1);
    let mut txn = doc.transact_mut();
    let mut txt = txn.node_mut("text").unwrap();
    let bold = Attrs::from([("bold".into(), true.into())]);
    txt.insert_text(0, "Test\nMulti-line\nFormatting");
    txt.apply_delta([Delta::new()
        .retain_with(4, bold.clone())
        .retain(1) // newline character
        .retain_with(10, bold.clone())
        .retain(1) // newline character
        .retain_with(10, bold.clone())]);

    // TODO(unified-api): Text::diff/Diff are not available yet
    let delta = txt.diff(&txn, YChange::identity);
    assert_eq!(
        delta,
        vec![
            Diff::new("Test".into(), bold.clone()),
            Diff::new("\n".into(), None),
            Diff::new("Multi-line".into(), bold.clone()),
            Diff::new("\n".into(), None),
            Diff::new("Formatting".into(), bold),
        ]
    );
}

#[test]
fn delta_with_embeds() {
    let mut doc = Doc::with_client_id(1);
    let mut txn = doc.transact_mut();
    let mut txt = txn.node_mut("text").unwrap();
    let linebreak = any!({
        "linebreak": "s"
    });
    txt.apply_delta([Delta::new().insert(linebreak.clone())]);

    let expected = Delta::new().insert(linebreak).map(|i| match i {
        In::Any(any) => Out::Any(any),
        _ => unreachable!(),
    });
    assert_eq!(txt.to_delta(&DeltaOptions::default()), vec![expected]);
}

#[test]
fn delta_with_shared_ref() {
    let mut d1 = Doc::with_client_id(1);

    let triggered = Arc::new(AtomicBool::new(false));
    let _sub = {
        let triggered = triggered.clone();
        let mut txn = d1.transact_mut();
        txn.node_mut("text").unwrap().observe(move |_txn, e| {
            // TODO(unified-api): the `Delta` event enum and Out::cast are not available yet
            let delta = e.delta(txn).to_vec();
            let d: MapRef = match &delta[0] {
                Delta::Inserted(insert, _) => insert.clone().cast().unwrap(),
                _ => unreachable!("unexpected delta"),
            };
            assert_eq!(d.get(txn, "key").unwrap(), Out::Any("val".into()));
            e.delta(()).for_each(drop);
            triggered.store(true, Ordering::Relaxed);
        })
    };

    let update = {
        let mut txn1 = d1.transact_mut();
        txn1.node_mut("text")
            .unwrap()
            .apply_delta([Delta::new().insert(In::Node(Delta::new().insert_attr("key", "val")))]);
        let update = txn1.encode_update_v1();
        update
    };
    // TODO(unified-api): Text::diff and Out::cast are not available yet
    let delta = txt1.diff(&txn1, YChange::identity);
    let d: MapRef = delta[0].insert.clone().cast().unwrap();
    assert_eq!(d.get(&txn1, "key").unwrap(), Out::Any("val".into()));

    assert!(triggered.load(Ordering::Relaxed), "fired event");

    let mut d2 = Doc::with_client_id(2);
    {
        let mut txn2 = d2.transact_mut();
        txn2.apply_update(Update::decode_v1(&update).unwrap())
            .unwrap();
    }
    // TODO(unified-api): Text::diff and Out::cast are not available yet
    let delta = txt2.diff(&d2.transact(), YChange::identity);
    assert_eq!(delta.len(), 1);
    let d: MapRef = delta[0].insert.clone().cast().unwrap();
    assert_eq!(
        d.get(&d2.transact(), "key").unwrap(),
        Out::Any("val".into())
    );
}

#[test]
fn delta_snapshots() {
    let mut doc = Doc::with_options(Options {
        client_id: ClientID::new(1),
        skip_gc: true,
        ..Default::default()
    });
    let mut txn = doc.transact_mut();
    txn.node_mut("text")
        .unwrap()
        .apply_delta([Delta::new().insert_text("abcd")]);
    let _snapshot1 = txn.snapshot(); // 'abcd'
    txn.node_mut("text")
        .unwrap()
        .apply_delta([Delta::new().retain(1).insert_text("x").remove(1)]);
    let _snapshot2 = txn.snapshot(); // 'axcd'
    txn.node_mut("text").unwrap().apply_delta([Delta::new()
        .retain(2) // ax^cd
        .remove(1) // ax^d
        .insert_text("x") // axx^d
        .remove(1)]); // axx^

    // TODO(unified-api): Text::diff_range/YChange/ChangeKind are not available yet
    let state1 = txt.diff_range(&mut txn, Some(&snapshot1), None, YChange::identity);
    assert_eq!(state1, vec![Diff::new("abcd".into(), None)]);
    let state2 = txt.diff_range(&mut txn, Some(&snapshot2), None, YChange::identity);
    assert_eq!(state2, vec![Diff::new("axcd".into(), None)]);
    let state2_diff = txt.diff_range(
        &mut txn,
        Some(&snapshot2),
        Some(&snapshot1),
        YChange::identity,
    );
    assert_eq!(
        state2_diff,
        vec![
            Diff {
                insert: "a".into(),
                attributes: None,
                ychange: None
            },
            Diff {
                insert: "x".into(),
                attributes: None,
                ychange: Some(YChange {
                    kind: ChangeKind::Added,
                    id: ID::new(ClientID::new(1), 4)
                })
            },
            Diff {
                insert: "b".into(),
                attributes: None,
                ychange: Some(YChange {
                    kind: ChangeKind::Removed,
                    id: ID::new(ClientID::new(1), 1)
                })
            },
            Diff {
                insert: "cd".into(),
                attributes: None,
                ychange: None
            }
        ]
    );
}

#[test]
fn snapshot_delete_after() {
    let mut doc = Doc::with_options(Options {
        client_id: ClientID::new(1),
        skip_gc: true,
        ..Default::default()
    });
    let mut txn = doc.transact_mut();
    txn.node_mut("text")
        .unwrap()
        .apply_delta([Delta::new().insert_text("abcd")]);
    let _snapshot1 = txn.snapshot();
    txn.node_mut("text")
        .unwrap()
        .apply_delta([Delta::new().retain(4).insert_text("e")]);

    // TODO(unified-api): Text::diff_range/YChange are not available yet
    let state1 = txt.diff_range(&mut txn, Some(&snapshot1), None, YChange::identity);
    assert_eq!(state1, vec![Diff::new("abcd".into(), None)]);
}

#[test]
fn empty_delta_chunks() {
    let mut doc = Doc::with_client_id(1);
    let mut txn = doc.transact_mut();

    let bold = Attrs::from([(Arc::from("bold"), Any::from(true))]);
    let delta = Delta::new()
        .insert_text("a")
        .insert_text_with("", bold)
        .insert_text("b");

    {
        let mut txt = txn.node_mut("text").unwrap();
        txt.apply_delta([delta]);
        assert_eq!(txt.to_string(), "ab");
    }

    let bin = txn.encode_state_as_update_v1(&StateVector::default());

    let mut doc2 = Doc::with_client_id(2);
    let mut txn = doc2.transact_mut();
    let update = Update::decode_v1(bin.as_slice()).unwrap();
    txn.apply_update(update).unwrap();
    assert_eq!(txn.node("text").unwrap().to_string(), "ab");
}

/// Remote client 1 makes `total` edits, of which the local peer receives only the
/// first `synced`. Returns the local doc and a snapshot taken on the remote after
/// all edits, i.e. one whose state map points past the local block list.
///
/// Each edit inserts at index 0, so every item lands left of the previous one and
/// the blocks stay unsquashed.
fn partially_synced(total: usize, synced: usize) -> (Doc, Snapshot) {
    let mut remote = Doc::with_options(Options {
        client_id: ClientID::new(1),
        skip_gc: true,
        ..Default::default()
    });
    let mut local = Doc::with_client_id(2);

    for i in 0..total {
        let mut txn = remote.transact_mut();
        txn.node_mut("text").unwrap().insert_text(0, "a");
        let update = txn.encode_update_v1();
        drop(txn);
        if i < synced {
            local
                .transact_mut()
                .apply_update(Update::decode_v1(&update).unwrap())
                .unwrap();
        }
    }
    let snapshot = remote.transact().snapshot();
    (local, snapshot)
}

/// Renders history on a peer that has not caught up to the snapshot, so the clock
/// held by the snapshot is out of range for the local block list.
fn assert_partial_history(total: usize, synced: usize) {
    // TODO(unified-api): Text::diff_range/YChange are not available yet
    let (mut local, snapshot) = partially_synced(total, synced);

    let mut txn = local.transact_mut();
    let txt = txn.node_mut("text").unwrap();

    let diff = txt.diff_range(&mut txn, Some(&snapshot), None, YChange::identity);
    let text: String = diff
        .iter()
        .map(|d| match &d.insert {
            Out::Any(Any::String(s)) => s.to_string(),
            other => panic!("unexpected chunk {:?}", other),
        })
        .collect();
    assert_eq!(text, "a".repeat(synced));
}

/// see: https://github.com/y-crdt/y-crdt/pull/645.
#[test]
fn diff_range_with_snapshot_ahead_single_block() {
    assert_partial_history(6, 1);
}
#[test]
fn diff_range_with_snapshot_ahead_many_blocks() {
    assert_partial_history(20, 3);
}

#[test]
fn apply_update_with_missing_predecessors() {
    let mut remote = Doc::with_client_id(1);
    let mut updates = Vec::new();
    for _ in 0..6 {
        let mut txn = remote.transact_mut();
        txn.node_mut("text").unwrap().insert_text(0, "a");
        updates.push(txn.encode_update_v1());
    }

    // delivered in reverse, so every update but the last references missing clocks
    let mut local = Doc::with_client_id(2);
    for update in updates.iter().rev() {
        local
            .transact_mut()
            .apply_update(Update::decode_v1(update).unwrap())
            .unwrap();
    }
    assert_eq!(local.transact().node("text").unwrap().to_string(), "aaaaaa");
}
