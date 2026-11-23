use crate::lib0::{Encoder, EncoderV1};
use crate::node::NodePtr;
use crate::transaction::Subdocs;
use crate::{Doc, IdSet, Modified, Origin, StateVector, Transaction};
use std::collections::HashSet;

/// Used to report changes made during transactions. Storage accessors returning `None` are skipped.
pub trait Reporter {
    /// lib0 v1 or v2, chosen by the implementor.
    type Encoder: Encoder;
    /// Written after GC and squashing with this transaction's update. `None` skips encoding.
    fn encoder(&mut self) -> Option<&mut Self::Encoder> {
        None
    }
    /// Receives ids inserted by the transaction.
    fn inserts(&mut self) -> Option<&mut IdSet> {
        None
    }
    /// Receives ids deleted by the transaction (squashed).
    fn deletes(&mut self) -> Option<&mut IdSet> {
        None
    }
    /// Directly modified nodes mapped to their changed keys (`None` key = children changed).
    fn modified(&mut self) -> Option<&mut Modified> {
        None
    }
    /// Every ancestor of a modified node, weak-link parents included.
    fn changed_parent_types(&mut self) -> Option<&mut Vec<NodePtr>> {
        None
    }
    /// Receives the document state vector from before the transaction.
    fn before_state(&mut self) -> Option<&mut StateVector> {
        None
    }
    /// Receives the document state vector from after the transaction.
    fn after_state(&mut self) -> Option<&mut StateVector> {
        None
    }
    /// Receives subdocuments added, removed and loaded by the transaction.
    fn subdocs(&mut self) -> Option<&mut Subdocs> {
        None
    }
    /// Receives the transaction origin and whether it was local.
    fn meta(&mut self, _origin: Option<&Origin>, _local: bool) {}
    /// Asked before GC: nodes whose content deleted by `txn` must stay in the block store.
    fn keep(&self, _txn: &Transaction<&Doc>) -> Option<&HashSet<NodePtr>> {
        None
    }
    /// Called after changes are collected and before GC, with storages (except `encoder`) already filled.
    /// This is where deltas of the transaction should be read: GC'ed content doesn't render.
    fn committing(&mut self, _txn: &Transaction<&Doc>) {}
    /// Called last with the committed transaction, after GC, before its state is cleared.
    fn committed(&mut self, _txn: &Transaction<&Doc>) {}

    #[doc(hidden)]
    fn fill(&mut self, txn: &Transaction<&Doc>) {
        fill_report(self, txn)
    }
    #[doc(hidden)]
    fn encode(&mut self, txn: &Transaction<&Doc>) {
        encode_report(self, txn)
    }
}

/// Fills every storage requested by `r` except `encoder` (see [encode_report]).
fn fill_report<R: Reporter + ?Sized>(r: &mut R, txn: &Transaction<&Doc>) {
    let (doc, Some(state)) = (txn.doc, txn.state.as_deref()) else {
        return;
    };
    if let Some(ids) = r.inserts() {
        ids.clone_from(&state.insert_set);
    }
    if let Some(ids) = r.deletes() {
        ids.clone_from(&state.delete_set);
    }
    if let Some(modified) = r.modified() {
        modified.clone_from(&state.changed);
    }
    if let Some(parents) = r.changed_parent_types() {
        parents.clone_from(&state.changed_parent_types);
    }
    if let Some(sv) = r.before_state() {
        sv.clone_from(state.before_state(doc));
    }
    if let Some(sv) = r.after_state() {
        sv.clone_from(state.after_state(doc));
    }
    if let Some(subdocs) = r.subdocs() {
        match state.subdocs.as_deref() {
            Some(s) => subdocs.clone_from(s),
            None => *subdocs = Subdocs::default(),
        }
    }
    r.meta(state.origin.as_ref(), state.local);
}

/// Writes the transaction's update into the encoder requested by `r`. Called after GC and squashing.
fn encode_report<R: Reporter + ?Sized>(r: &mut R, txn: &Transaction<&Doc>) {
    let (doc, Some(state)) = (txn.doc, txn.state.as_deref()) else {
        return;
    };
    if let Some(encoder) = r.encoder() {
        if !state.delete_set.is_empty() || state.after_state(doc) != state.before_state(doc) {
            txn.encode_update(encoder);
        }
    }
}

/// Reporter which doesn't collect anything.
pub struct NoReporter;

impl Reporter for NoReporter {
    type Encoder = EncoderV1;

    #[inline]
    fn committed(&mut self, txn: &Transaction<&Doc>) {
        #[cfg(feature = "loud-report")]
        {
            if !txn.insert_set().is_empty() || !txn.delete_set().is_empty() {
                eprintln!(
                    "yrs: transaction with changes committed without a reporter (doc: {}, origin: {:?})",
                    txn.doc().guid(),
                    txn.origin()
                );
            }
        }
    }
}

impl<R: Reporter + ?Sized> Reporter for &mut R {
    type Encoder = R::Encoder;

    fn encoder(&mut self) -> Option<&mut Self::Encoder> {
        (**self).encoder()
    }
    fn inserts(&mut self) -> Option<&mut IdSet> {
        (**self).inserts()
    }
    fn deletes(&mut self) -> Option<&mut IdSet> {
        (**self).deletes()
    }
    fn modified(&mut self) -> Option<&mut Modified> {
        (**self).modified()
    }
    fn changed_parent_types(&mut self) -> Option<&mut Vec<NodePtr>> {
        (**self).changed_parent_types()
    }
    fn before_state(&mut self) -> Option<&mut StateVector> {
        (**self).before_state()
    }
    fn after_state(&mut self) -> Option<&mut StateVector> {
        (**self).after_state()
    }
    fn subdocs(&mut self) -> Option<&mut Subdocs> {
        (**self).subdocs()
    }
    fn meta(&mut self, origin: Option<&Origin>, local: bool) {
        (**self).meta(origin, local)
    }
    fn keep(&self, txn: &Transaction<&Doc>) -> Option<&HashSet<NodePtr>> {
        (**self).keep(txn)
    }
    fn committing(&mut self, txn: &Transaction<&Doc>) {
        (**self).committing(txn)
    }
    fn committed(&mut self, txn: &Transaction<&Doc>) {
        (**self).committed(txn)
    }
    fn fill(&mut self, txn: &Transaction<&Doc>) {
        (**self).fill(txn)
    }
    fn encode(&mut self, txn: &Transaction<&Doc>) {
        (**self).encode(txn)
    }
}

/// Both reporters are filled by a commit. Storage accessors and `encoder` expose `A` only,
/// and if both return a `keep` scope, `A`'s wins.
impl<A: Reporter, B: Reporter> Reporter for (A, B) {
    type Encoder = A::Encoder;

    fn encoder(&mut self) -> Option<&mut Self::Encoder> {
        self.0.encoder()
    }
    fn inserts(&mut self) -> Option<&mut IdSet> {
        self.0.inserts()
    }
    fn deletes(&mut self) -> Option<&mut IdSet> {
        self.0.deletes()
    }
    fn modified(&mut self) -> Option<&mut Modified> {
        self.0.modified()
    }
    fn changed_parent_types(&mut self) -> Option<&mut Vec<NodePtr>> {
        self.0.changed_parent_types()
    }
    fn before_state(&mut self) -> Option<&mut StateVector> {
        self.0.before_state()
    }
    fn after_state(&mut self) -> Option<&mut StateVector> {
        self.0.after_state()
    }
    fn subdocs(&mut self) -> Option<&mut Subdocs> {
        self.0.subdocs()
    }
    fn meta(&mut self, origin: Option<&Origin>, local: bool) {
        self.0.meta(origin, local);
        self.1.meta(origin, local);
    }
    fn keep(&self, txn: &Transaction<&Doc>) -> Option<&HashSet<NodePtr>> {
        self.0.keep(txn).or_else(|| self.1.keep(txn))
    }
    fn committing(&mut self, txn: &Transaction<&Doc>) {
        self.0.committing(txn);
        self.1.committing(txn);
    }
    fn committed(&mut self, txn: &Transaction<&Doc>) {
        self.0.committed(txn);
        self.1.committed(txn);
    }
    fn fill(&mut self, txn: &Transaction<&Doc>) {
        self.0.fill(txn);
        self.1.fill(txn);
    }
    fn encode(&mut self, txn: &Transaction<&Doc>) {
        self.0.encode(txn);
        self.1.encode(txn);
    }
}

#[cfg(test)]
mod test {
    use super::NoReporter;
    use crate::block::{ClientID, ItemContent};
    use crate::lib0::{Decode, Encoder, EncoderV1, EncoderV2};
    use crate::node::PathSegment;
    use crate::test_utils::{Keep, Report, on_commit};
    use crate::{Delta, Doc, ID, IdSet, NodeID, Origin, Update};
    use std::collections::{HashMap, HashSet};

    fn insert_text(doc: &mut Doc, text: &str) {
        doc.transact_mut()
            .node_mut("text")
            .unwrap()
            .insert_text(0, text);
    }

    fn text(doc: &Doc) -> String {
        doc.transact().node("text").unwrap().to_string()
    }

    fn nested_map(doc: &mut Doc) -> NodeID {
        let mut txn = doc.transact_mut();
        let mut map = txn.node_mut("map").unwrap();
        map.insert_attr("nested", Delta::new()).node_id().unwrap()
    }

    #[test]
    fn encoder_v1_update() {
        let mut d1 = Doc::with_client_id(1);
        let mut r = Report::new(EncoderV1::new());
        let mut txn = d1.transact_mut();
        txn.node_mut("text").unwrap().insert_text(0, "hello");
        txn.commit(&mut r);
        let mut d2 = Doc::with_client_id(2);
        let update = Update::decode_v1(&r.encoder.to_vec()).unwrap();
        d2.transact_mut().apply_update(update).unwrap();
        assert_eq!(text(&d2), "hello");
    }

    #[test]
    fn encoder_v2_update() {
        let mut d1 = Doc::with_client_id(1);
        let mut r = Report::new(EncoderV2::new());
        let mut txn = d1.transact_mut();
        txn.node_mut("text").unwrap().insert_text(0, "hello");
        txn.commit(&mut r);
        let mut d2 = Doc::with_client_id(2);
        let update = Update::decode_v2(&r.encoder.to_vec()).unwrap();
        d2.transact_mut().apply_update(update).unwrap();
        assert_eq!(text(&d2), "hello");
    }

    #[test]
    fn encoder_skips_empty_transaction() {
        let mut doc = Doc::with_client_id(1);
        let mut r = Report::new(EncoderV1::new());
        let txn = doc.transact_mut_with("origin");
        txn.commit(&mut r);
        assert!(r.encoder.to_vec().is_empty());
    }

    #[test]
    fn fills_requested_data() {
        let mut doc = Doc::with_client_id(1);
        insert_text(&mut doc, "abc");
        let text = doc.node(NodeID::from("text")).unwrap();
        let mut r = Report::new(EncoderV1::new());
        let mut txn = doc.transact_mut();
        let mut node = txn.node_mut("text").unwrap();
        node.insert_text(3, "d");
        node.remove(0, 1);
        txn.commit(&mut r);
        let client = ClientID::new(1);
        let mut inserts = IdSet::new();
        inserts.insert(ID::new(client, 3), 1);
        let mut deletes = IdSet::new();
        deletes.insert(ID::new(client, 0), 1);
        assert_eq!(r.inserts, inserts);
        assert_eq!(r.deletes, deletes);
        assert_eq!(r.modified, HashMap::from([(text, HashSet::from([None]))]));
        assert_eq!(r.parents, vec![text]);
        assert_eq!(r.before.get(&client), 3);
        assert_eq!(r.after.get(&client), 4);
    }

    #[test]
    fn no_reporter_commit() {
        let mut doc = Doc::with_client_id(1);
        let mut txn = doc.transact_mut();
        txn.node_mut("text").unwrap().insert_text(0, "abc");
        txn.commit(&mut NoReporter);
        assert_eq!(text(&doc), "abc");
    }

    #[test]
    fn keep_prevents_gc() {
        let mut doc = Doc::with_client_id(1);
        insert_text(&mut doc, "abc");
        let text = doc.node(NodeID::from("text")).unwrap();
        let mut txn = doc.transact_mut();
        txn.node_mut("text").unwrap().remove(0, 3);
        txn.commit(&mut Keep(HashSet::from([text])));
        let item = doc.blocks.get_item(&ID::new(ClientID::new(1), 0)).unwrap();
        assert!(matches!(item.content, ItemContent::String(_)));
    }

    #[test]
    fn no_keep_allows_gc() {
        let mut doc = Doc::with_client_id(1);
        insert_text(&mut doc, "abc");
        let mut txn = doc.transact_mut();
        txn.node_mut("text").unwrap().remove(0, 3);
        txn.commit(&mut NoReporter);
        let item = doc.blocks.get_item(&ID::new(ClientID::new(1), 0)).unwrap();
        assert!(matches!(item.content, ItemContent::Deleted(_)));
    }

    #[test]
    fn committed_delta() {
        let mut doc = Doc::with_client_id(1);
        let nested = nested_map(&mut doc);
        let mut deltas = None;
        let mut txn = doc.transact_mut();
        txn.node_mut("map").unwrap().insert_attr("a", 1);
        txn.node_mut(nested).unwrap().insert_attr("b", 2);
        txn.commit(&mut on_commit(|t| {
            deltas = Some((t.delta("map", false), t.delta("map", true)))
        }));
        let shallow = Delta::out().insert_attr("a", 1);
        let deep = shallow
            .clone()
            .modify_attr("nested", Delta::out().insert_attr("b", 2));
        assert_eq!(deltas, Some((shallow, deep)));
    }

    #[test]
    fn committed_delta_of_gc_content() {
        let mut doc = Doc::with_client_id(1);
        insert_text(&mut doc, "abc");
        let mut delta = None;
        let mut txn = doc.transact_mut();
        txn.node_mut("text").unwrap().remove(1, 2);
        txn.commit(&mut on_commit(|t| delta = Some(t.delta("text", false))));
        assert_eq!(delta, Some(Delta::out().retain(1).remove(2)));
    }

    #[test]
    fn committed_modified_paths() {
        let mut doc = Doc::with_client_id(1);
        let nested = nested_map(&mut doc);
        let mut paths = Vec::new();
        let mut txn = doc.transact_mut();
        txn.node_mut(nested.clone()).unwrap().insert_attr("b", 1);
        txn.node_mut("map").unwrap().insert_attr("a", 1);
        txn.node_mut("other").unwrap().insert_attr("c", 1);
        txn.commit(&mut on_commit(|t| {
            paths = t.modified().map(|(n, _)| (n.id(), n.path())).collect();
            paths.sort_by(|(a, _), (b, _)| a.cmp(b));
        }));
        let key = |s: &str| PathSegment::Key(s.into());
        let mut expected = vec![
            (NodeID::from("map"), [key("map")].into()),
            (NodeID::from("other"), [key("other")].into()),
            (nested, [key("map"), key("nested")].into()),
        ];
        expected.sort_by(|(a, _), (b, _)| a.cmp(b));
        assert_eq!(paths, expected);
    }

    #[test]
    fn committed_keys_and_children_changed() {
        let mut doc = Doc::with_client_id(1);
        let mut seen = None;
        let mut txn = doc.transact_mut();
        txn.node_mut("map").unwrap().insert_attr("a", 1);
        txn.node_mut("array").unwrap().push_back(1);
        txn.commit(&mut on_commit(|t| {
            let keys: Vec<_> = t.keys_changed("map").cloned().collect();
            seen = Some((keys, t.children_changed("map"), t.children_changed("array")))
        }));
        assert_eq!(seen, Some((vec!["a".into()], false, true)));
    }

    #[test]
    fn committed_is_modified_deep() {
        let mut doc = Doc::with_client_id(1);
        let nested = nested_map(&mut doc);
        doc.transact_mut()
            .node_mut("other")
            .unwrap()
            .insert_attr("a", 1);
        let mut seen = None;
        let mut txn = doc.transact_mut();
        txn.node_mut(nested).unwrap().insert_attr("b", 1);
        txn.commit(&mut on_commit(|t| {
            let map = (t.is_modified("map", true), t.is_modified("map", false));
            seen = Some((map, t.is_modified("other", true)))
        }));
        assert_eq!(seen, Some(((true, false), false)));
    }

    /// Both halves of a tuple reporter are filled and see `committing`.
    #[test]
    fn tuple_reporter() {
        let mut doc = Doc::with_client_id(1);
        let (mut a, mut b) = (0, 0);
        let mut r = (
            (Report::new(EncoderV1::new()), on_commit(|_| a += 1)),
            (Report::new(EncoderV2::new()), on_commit(|_| b += 1)),
        );
        let mut txn = doc.transact_mut();
        txn.node_mut("text").unwrap().insert_text(0, "abc");
        txn.commit(&mut r);
        let ((ra, _), (rb, _)) = r;
        assert!(!ra.inserts.is_empty() && ra.inserts == rb.inserts);
        assert!(!rb.encoder.to_vec().is_empty());
        assert_eq!((a, b), (1, 1));
    }

    #[test]
    fn delta_deep_includes_nested_changes() {
        let mut doc = Doc::with_client_id(1);
        let nested = {
            let mut txn = doc.transact_mut();
            let mut map = txn.node_mut("map").unwrap();
            map.insert_attr("nested", Delta::new().insert(1))
                .node_id()
                .unwrap()
        };
        let mut deltas = vec![];
        let mut txn = doc.transact_mut();
        txn.node_mut("map").unwrap().insert_attr("a", 1);
        txn.node_mut(nested).unwrap().push_back(2);
        txn.commit(&mut on_commit(|t| {
            deltas.push((t.delta("map", false), t.delta("map", true)))
        }));
        let shallow = Delta::out().insert_attr("a", 1);
        let deep = shallow
            .clone()
            .modify_attr("nested", Delta::out().retain(1).insert(2));
        assert_eq!(deltas, vec![(shallow, deep)]);
    }

    #[test]
    fn is_added_and_is_deleted() {
        let mut doc = Doc::with_client_id(1);
        let mut flags = [(false, false); 2];
        let mut txn = doc.transact_mut();
        let mut array = txn.node_mut("array").unwrap();
        array.push_back(1);
        array.push_back(2);
        array.remove(0, 1);
        txn.commit(&mut on_commit(|t| {
            let ids = [ID::new(ClientID::new(1), 0), ID::new(ClientID::new(1), 1)];
            flags = ids.map(|id| (t.has_added(&id), t.has_deleted(&id)));
        }));
        assert_eq!(flags, [(true, true), (true, false)]);
    }

    #[test]
    fn observe_deep_receives_only_own_subtree_events() {
        let mut doc = Doc::with_client_id(1);
        let mut targets = vec![];
        let mut txn = doc.transact_mut();
        txn.node_mut("a").unwrap().insert_attr("key", 1);
        txn.node_mut("b").unwrap().insert_attr("key", 1);
        txn.commit(&mut on_commit(|t| {
            targets.extend(t.modified_within("a").map(|n| n.id()))
        }));
        assert_eq!(targets, vec![NodeID::from("a")]);
    }
}
