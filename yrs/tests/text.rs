use fastrand::Rng;
use std::collections::HashMap;
use std::sync::Arc;
use yrs::node::{Attrs, Observable};
use yrs::test_utils::{RngExt, exchange_updates, run_scenario};
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{
    Acquire, AcquireMut, Any, Cell, ClientID, Delta, DeltaOptions, Doc, IdSet, In, OffsetKind, Op,
    Options, Out, Snapshot, StateVector, Update, any,
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
    let delta = Cell::new(Delta::out());
    let delta1 = delta.clone();
    let sub = {
        let mut txn = doc.transact_mut();
        txn.node_mut("text")
            .unwrap()
            .observe(move |e| *delta1.acquire_mut() = e.delta(false))
    };
    let delta = || std::mem::take(&mut *delta.acquire_mut());

    // insert initial data to an empty YText
    {
        let mut txn = doc.transact_mut();
        txn.node_mut("text").unwrap().insert_text(0, "abcd"); // => 'abcd'
    }
    assert_eq!(delta(), Delta::out().insert_text("abcd"));

    // remove 2 chars from the middle
    {
        let mut txn = doc.transact_mut();
        txn.node_mut("text").unwrap().remove(1, 2); // => 'ad'
    }
    assert_eq!(delta(), Delta::out().retain(1).remove(2));

    // insert new item in the middle
    let attrs = Attrs::from([("bold".into(), true.into())]);
    {
        let mut txn = doc.transact_mut();
        txn.node_mut("text")
            .unwrap()
            .insert_text_with(1, "e", attrs.clone()); // => 'a<bold>e</bold>d'
    }
    assert_eq!(delta(), Delta::out().retain(1).insert_text_with("e", attrs));

    // remove formatting
    let attrs = Attrs::from([("bold".into(), Any::Null)]);
    {
        let mut txn = doc.transact_mut();
        txn.node_mut("text").unwrap().format(1, 1, attrs.clone()); // => 'aed'
    }
    assert_eq!(delta(), Delta::out().retain(1).retain_with(2, attrs));

    // free the observer and make sure that callback is no longer called
    drop(sub);
    {
        let mut txn = doc.transact_mut();
        txn.node_mut("text").unwrap().insert_text(1, "fgh"); // => 'afghed'
    }
    assert_eq!(delta(), Delta::out()); // empty after unsubscribing
}

#[test]
fn insert_and_remove_event_changes() {
    let mut d1 = Doc::with_client_id(1);
    let delta = Cell::new(Delta::out());
    let delta1 = delta.clone();
    let delta2 = delta.clone();
    let _sub = {
        let mut txn = d1.transact_mut();
        txn.node_mut("text")
            .unwrap()
            .observe(move |e| *delta1.acquire_mut() = e.delta(true))
    };
    let delta = || std::mem::take(&mut *delta.acquire_mut());

    // insert initial string
    {
        let mut txn = d1.transact_mut();
        txn.node_mut("text").unwrap().insert_text(0, "abcd");
    }
    assert_eq!(delta(), Delta::out().insert_text("abcd"));

    // remove middle
    {
        let mut txn = d1.transact_mut();
        txn.node_mut("text").unwrap().remove(1, 2);
    }
    assert_eq!(delta(), Delta::out().retain(1).remove(2));

    // insert again
    {
        let mut txn = d1.transact_mut();
        txn.node_mut("text").unwrap().insert_text(1, "ef");
    }
    assert_eq!(delta(), Delta::out().retain(1).insert_text("ef"));

    // replicate data to another peer
    let mut d2 = Doc::with_client_id(2);
    let _sub = {
        let mut txn = d2.transact_mut();
        txn.node_mut("text")
            .unwrap()
            .observe(move |e| *delta2.acquire_mut() = e.delta(true))
    };

    {
        let t1 = d1.transact();
        let mut t2 = d2.transact_mut();

        let sv = t2.state_vector();
        let update = t1.encode_diff_v1(&sv);
        t2.apply_update(Update::decode_v1(update.as_slice()).unwrap())
            .unwrap();
    }
    assert_eq!(delta(), Delta::out().insert_text("aefd"));
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

    let delta1 = Cell::new(Delta::out());
    let delta_clone = delta1.clone();
    let _sub1 = {
        let mut txn = d1.transact_mut();
        txn.node_mut("text")
            .unwrap()
            .observe(move |e| *delta_clone.acquire_mut() = e.delta(true))
    };

    let delta2 = Cell::new(Delta::out());
    let delta_clone = delta2.clone();
    let _sub2 = {
        let mut txn = d2.transact_mut();
        txn.node_mut("text")
            .unwrap()
            .observe(move |e| *delta_clone.acquire_mut() = e.delta(true))
    };

    let a: Attrs = HashMap::from([("bold".into(), Any::Bool(true))]);

    // step 1
    {
        let mut txn = d1.transact_mut();
        let mut txt1 = txn.node_mut("text").unwrap();
        txt1.insert_text_with(0, "abc", a.clone());
        let expected = Delta::out().insert_text_with("abc", a.clone());
        assert_eq!(txt1.delta(&DeltaOptions::default()), expected);
        let update = txn.encode_update_v1();
        drop(txn);

        assert_eq!(&*delta1.acquire(), &expected);
        assert_eq!(&*delta2.acquire(), &expected);
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
        let mut txt1 = txn.node_mut("text").unwrap();
        txt1.remove(0, 1);
        let actual = txt1.delta(&DeltaOptions::default());
        assert_eq!(actual, Delta::out().insert_text_with("bc", a.clone()));
        let update = txn.encode_update_v1();
        drop(txn);

        let expected = Delta::out().remove(1);
        assert_eq!(&*delta1.acquire(), &expected);
        assert_eq!(&*delta2.acquire(), &expected);
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
        let mut txt1 = txn.node_mut("text").unwrap();
        txt1.remove(1, 1);
        assert_eq!(
            txt1.delta(&DeltaOptions::default()),
            Delta::out().insert_text_with("b", a.clone())
        );
        let update = txn.encode_update_v1();
        drop(txn);

        let expected = Delta::out().retain(1).remove(1);
        assert_eq!(&*delta1.acquire(), &expected);
        assert_eq!(&*delta2.acquire(), &expected);
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
        let mut txt1 = txn.node_mut("text").unwrap();
        txt1.insert_text_with(0, "z", a.clone());
        let actual = txt1.delta(&DeltaOptions::default());
        assert_eq!(actual, Delta::out().insert_text_with("zb", a.clone()));
        let update = txn.encode_update_v1();
        drop(txn);

        // TODO(unified-api): Text::diff/Diff and the `Delta` event enum are not available yet
        let expected = Delta::out().insert_text_with("z", a.clone());
        assert_eq!(&*delta1.acquire(), &expected);
        assert_eq!(&*delta2.acquire(), &expected);
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
        let mut txt1 = txn.node_mut("text").unwrap();
        txt1.insert_text(0, "y");
        let actual = txt1.delta(&DeltaOptions::default());
        assert_eq!(
            actual,
            Delta::out()
                .insert_text("y")
                .insert_text_with("ab", a.clone())
        );
        let update = txn.encode_update_v1();
        drop(txn);

        let expected = Delta::out().insert_text("y");
        assert_eq!(&*delta1.acquire(), &expected);
        assert_eq!(&*delta2.acquire(), &expected);
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
        let mut txt1 = txn.node_mut("text").unwrap();
        txt1.format(0, 2, b.clone());
        let actual = txt1.delta(&DeltaOptions::default());
        assert_eq!(
            actual,
            Delta::out()
                .insert_text("yz")
                .insert_text_with("b", b.clone())
        );
        let update = txn.encode_update_v1();
        drop(txn);

        let expected = Delta::out().retain(1).retain_with(1, b.clone());
        assert_eq!(&*delta1.acquire(), &expected);
        assert_eq!(&*delta2.acquire(), &expected);
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

    let delta1 = Cell::new(Delta::out());
    let delta_clone = delta1.clone();
    let _sub1 = {
        let mut txn = d1.transact_mut();
        txn.node_mut("text")
            .unwrap()
            .observe(move |e| *delta_clone.acquire_mut() = e.delta(true))
    };

    let a1: Attrs = HashMap::from([("bold".into(), true.into())]);
    let a2: Attrs = HashMap::from([("width".into(), Any::Number(100.0))]);
    let embed = any!({
        "image": "imageSrc.png"
    });
    let expected = Delta::out()
        .insert_text_with("a", a1.clone())
        .insert_with(embed.clone(), a2.clone())
        .insert_text_with("b", a1.clone());

    let (update_v1, update_v2) = {
        let mut txn = d1.transact_mut();
        let mut txt1 = txn.node_mut("text").unwrap();
        txt1.insert_text_with(0, "ab", a1.clone());
        txt1.apply_delta([Delta::new().retain(1).insert_with(embed.clone(), a2)]);
        assert_eq!(txt1.delta(&DeltaOptions::default()), expected);

        let update_v1 = txn.encode_state_as_update_v1(&StateVector::default());
        let update_v2 = txn.encode_state_as_update_v2(&StateVector::default());
        (update_v1, update_v2)
    };
    assert_eq!(&*delta1.acquire(), &expected);

    let mut d2 = Doc::new();
    {
        let mut txn = d2.transact_mut();
        txn.apply_update(Update::decode_v1(&update_v1).unwrap())
            .unwrap();
        let actual = txn.node("text").unwrap().delta(&DeltaOptions::default());
        assert_eq!(actual, expected);
    }

    let mut d3 = Doc::new();
    {
        let mut txn = d3.transact_mut();
        txn.apply_update(Update::decode_v2(&update_v2).unwrap())
            .unwrap();
        let actual = txn.node("text").unwrap().delta(&DeltaOptions::default());
        assert_eq!(actual, expected);
    }
}

#[test]
fn issue_101() {
    let mut d1 = Doc::with_client_id(1);
    let delta = Cell::new(Delta::out());
    let delta_copy = delta.clone();

    let attrs: Attrs = HashMap::from([("bold".into(), true.into())]);

    {
        let mut txn = d1.transact_mut();
        txn.node_mut("text").unwrap().insert_text(0, "abcd");
    }

    let _sub = {
        let txn = d1.transact();
        txn.node("text")
            .unwrap()
            .observe(move |e| *delta_copy.acquire_mut() = e.delta(false))
    };
    {
        let mut txn = d1.transact_mut();
        txn.node_mut("text").unwrap().format(1, 2, attrs.clone());
    }
    assert_eq!(
        &*delta.acquire(),
        &Delta::out().retain(1).retain_with(2, attrs)
    );
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
    }

    {
        let mut txn = doc.transact_mut();
        txn.node_mut("content").unwrap().insert_text(100, text2);
    }

    {
        let c1 = text1.chars().count();
        let c2 = text2.chars().count();
        let count = c1 as u32 + c2 as u32;

        let delta = Cell::new(Delta::out());
        let delta_copy = delta.clone();
        let _observer = {
            let txn = doc.transact();
            txn.node("content")
                .unwrap()
                .observe(move |e| *delta_copy.acquire_mut() = e.delta(false))
        };

        {
            let mut txn = doc.transact_mut();
            txn.node_mut("content").unwrap().remove(0, count);
        }
        assert_eq!(&*delta.acquire(), &Delta::out().remove(count));
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

    let expected = Delta::out()
        .insert_text_with("abc", attrs1)
        .insert_text_with("def", attrs2);
    assert_eq!(txt.delta(&DeltaOptions::default()), expected);
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

    let expected = Delta::out()
        .insert_text_with("a", attrs.clone())
        .insert_text("b")
        .insert_text_with("c", attrs);
    assert_eq!(txt.delta(&DeltaOptions::default()), expected);
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
    let mut txn = doc.transact_mut();
    txn.node_mut("text").unwrap().insert_text(0, "hello");
    let prev = txn.snapshot();
    txn.node_mut("text").unwrap().insert_text(5, " world");
    let next = txn.snapshot();
    drop(txn);

    let txn = doc.transact();
    let text = txn.node("text").unwrap();
    let diff = text.delta(&DeltaOptions {
        retain_inserts: true,
        retain_deletes: false,
        deep: false,
        items_to_render: Some(IdSet::from(next.state_map).diff(&IdSet::from(prev.state_map))),
        deleted_items: Some(next.delete_set.diff(&prev.delete_set)),
    });
    assert_eq!(diff, Delta::out().retain(5).insert_text(" world"))
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
    let array = text.insert(5, In::Node(Delta::new())); // insert array ref after "hello"

    let italic_and_bold = Attrs::from([("b".into(), true.into()), ("i".into(), true.into())]);
    let expected = Delta::out()
        .insert_text_with("hello", italic.clone())
        .insert_with(array, italic.clone())
        .insert_with(image, italic.clone())
        .insert_text_with(" ", italic)
        .insert_text_with("world", italic_and_bold);
    assert_eq!(text.delta(&DeltaOptions::default()), expected);
}

#[test]
#[cfg(feature = "sync")]
fn multi_threading() {
    use std::sync::{Arc, RwLock};
    use std::thread::{sleep, spawn};
    use std::time::Duration;

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

    let expected = Delta::out()
        .insert_text_with("Test", bold.clone())
        .insert_text("\n")
        .insert_text_with("Multi-line", bold.clone())
        .insert_text("\n")
        .insert_text_with("Formatting", bold);
    assert_eq!(txt.delta(&DeltaOptions::default()), expected);
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
    assert_eq!(txt.delta(&DeltaOptions::default()), expected);
}

#[test]
fn delta_with_shared_ref() {
    let mut d1 = Doc::with_client_id(1);

    let delta = Cell::new(Delta::out());
    let delta_copy = delta.clone();
    let _sub = {
        let mut txn = d1.transact_mut();
        txn.node_mut("text")
            .unwrap()
            .observe(move |e| *delta_copy.acquire_mut() = e.delta(true))
    };

    let update = {
        let mut txn1 = d1.transact_mut();
        txn1.node_mut("text")
            .unwrap()
            .apply_delta([Delta::new().insert(In::Node(Delta::new().insert_attr("key", "val")))]);
        txn1.encode_update_v1()
    };

    // the embedded node is reachable and carries its attributes
    let embedded_attr = |doc: &Doc| {
        let txn = doc.transact();
        let id = txn.node("text").unwrap().get(0).unwrap().node_id().unwrap();
        txn.node(id).unwrap().attr("key")
    };
    assert_eq!(embedded_attr(&d1), Some(Out::Any("val".into())));

    let id = d1
        .transact()
        .node("text")
        .unwrap()
        .get(0)
        .unwrap()
        .node_id()
        .unwrap();
    let expected = Delta::out().insert(Out::node_with_delta(
        id,
        Delta::out().insert_attr("key", "val"),
    ));
    assert_eq!(&*delta.acquire(), &expected, "fired event");

    let mut d2 = Doc::with_client_id(2);
    d2.transact_mut()
        .apply_update(Update::decode_v1(&update).unwrap())
        .unwrap();
    assert_eq!(embedded_attr(&d2), Some(Out::Any("val".into())));
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
    let snapshot1 = txn.snapshot(); // 'abcd'
    txn.node_mut("text")
        .unwrap()
        .apply_delta([Delta::new().retain(1).insert_text("x").remove(1)]);
    let snapshot2 = txn.snapshot(); // 'axcd'
    txn.node_mut("text").unwrap().apply_delta([Delta::new()
        .retain(2) // ax^cd
        .remove(1) // ax^d
        .insert_text("x") // axx^d
        .remove(1)]); // axx^
    drop(txn);

    let delta_opts = |to: &Snapshot, from: Option<&Snapshot>| {
        let mut ins = IdSet::from(to.state_map.clone());
        let mut del = to.delete_set.clone();
        if let Some(from) = from {
            ins = ins.diff(&IdSet::from(from.state_map.clone()));
            del = del.diff(&from.delete_set);
        }
        DeltaOptions {
            retain_inserts: false,
            retain_deletes: false,
            deep: true,
            items_to_render: Some(ins),
            deleted_items: Some(del),
        }
    };

    let txn = doc.transact();
    let txt = txn.node("text").unwrap();
    let state1 = txt.delta(&delta_opts(&snapshot1, None));
    assert_eq!(state1, Delta::out().insert_text("abcd"));
    let state2 = txt.delta(&delta_opts(&snapshot2, None));
    assert_eq!(state2, Delta::out().insert_text("axcd"));
    let state2_diff = txt.delta(&delta_opts(&snapshot1, Some(&snapshot2)));
    assert_eq!(
        state2_diff,
        Delta::out()
            .insert_text("a")
            .insert_text("x") // added
            .insert_text("b") // removed
            .insert_text("cd"),
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
    let snapshot1 = txn.snapshot();
    let mut text = txn.node_mut("text").unwrap();
    text.apply_delta([Delta::new().retain(4).insert_text("e")]);

    // TODO(unified-api): snapshot-scoped rendering (`Text::diff_range`) has no `DeltaOptions`
    // equivalent yet - `items_to_render`/`deleted_items` take an `IdSet`, not a `Snapshot`.
    let state1 = text.delta(&DeltaOptions {
        retain_inserts: false,
        retain_deletes: false,
        deep: false,
        items_to_render: Some(IdSet::from(snapshot1.state_map)),
        deleted_items: Some(snapshot1.delete_set),
    });
    assert_eq!(state1, Delta::out().insert_text("abcd"));
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
    let (mut local, snapshot) = partially_synced(total, synced);

    let mut txn = local.transact_mut();
    let txt = txn.node_mut("text").unwrap();

    // TODO(unified-api): snapshot-scoped rendering (`Text::diff_range`) has no `DeltaOptions`
    // equivalent yet - `items_to_render`/`deleted_items` take an `IdSet`, not a `Snapshot`.
    let diff = txt.delta(&DeltaOptions {
        retain_inserts: false,
        retain_deletes: false,
        deep: false,
        items_to_render: Some(snapshot.state_map.into()),
        deleted_items: Some(snapshot.delete_set),
    });
    let text: String = diff
        .children
        .iter()
        .map(|op| match op {
            Op::InsertText { text, format } => text.clone(),
            _ => unreachable!(),
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
