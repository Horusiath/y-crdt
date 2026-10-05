use crate::node::{Node, NodePtr, Path};
use crate::node_ref::get_delta;
use crate::transaction::Subdocs;
use crate::{Delta, Doc, ID, IdSet, NodeRef, Out, StateVector, Transaction, TransactionMut, Uuid};
use std::collections::HashSet;
use std::sync::Arc;

/// An update event passed to a callback subscribed with [Doc::observe_update_v1]/[Doc::observe_update_v2].
pub struct UpdateEvent {
    /// A binary which contains information about all inserted and deleted changes performed within
    /// the scope of its [TransactionMut].
    pub update: Vec<u8>,
}

impl UpdateEvent {
    pub(crate) fn new_v1(txn: &TransactionMut) -> Self {
        UpdateEvent {
            update: txn.encode_update_v1(),
        }
    }
    pub(crate) fn new_v2(txn: &TransactionMut) -> Self {
        UpdateEvent {
            update: txn.encode_update_v2(),
        }
    }
}

/// Holds transaction update information from a commit after state vectors have been compressed.
#[derive(Debug, Clone)]
pub struct TransactionCleanupEvent {
    pub before_state: StateVector,
    pub after_state: StateVector,
    pub delete_set: IdSet,
}

impl TransactionCleanupEvent {
    pub fn new(txn: &TransactionMut) -> Self {
        TransactionCleanupEvent {
            before_state: txn.before_state().clone(),
            after_state: txn.after_state().clone(),
            delete_set: txn.delete_set().clone(),
        }
    }
}

/// Event used to communicate load requests from the underlying subdocuments.
#[derive(Debug, Clone)]
pub struct SubdocsEvent {
    pub(crate) added: HashSet<Uuid>,
    pub(crate) removed: HashSet<Uuid>,
    pub(crate) loaded: HashSet<Uuid>,
}

impl SubdocsEvent {
    pub(crate) fn new(inner: Box<Subdocs>) -> Self {
        SubdocsEvent {
            added: inner.added,
            removed: inner.removed,
            loaded: inner.loaded,
        }
    }

    /// Returns an iterator over globally unique identifiers of all sub-documents added to a
    /// current document within a scope of committed transaction.
    pub fn added(&self) -> SubdocsEventIter {
        SubdocsEventIter(self.added.iter())
    }

    /// Returns an iterator over globally unique identifiers of all sub-documents removed from a
    /// current document within a scope of committed transaction.
    pub fn removed(&self) -> SubdocsEventIter {
        SubdocsEventIter(self.removed.iter())
    }

    /// Returns an iterator over globally unique identifiers of all sub-documents living in a
    /// parent document, that have requested to be loaded within a scope of committed transaction.
    pub fn loaded(&self) -> SubdocsEventIter {
        SubdocsEventIter(self.loaded.iter())
    }
}

#[repr(transparent)]
pub struct SubdocsEventIter<'a>(std::collections::hash_set::Iter<'a, Uuid>);

impl<'a> Iterator for SubdocsEventIter<'a> {
    type Item = &'a Uuid;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next()
    }
}

impl<'a> ExactSizeIterator for SubdocsEventIter<'a> {
    fn len(&self) -> usize {
        self.0.len()
    }
}

pub struct Event<'txn> {
    target: NodePtr,
    current_target: NodePtr,
    txn: &'txn Transaction<&'txn Doc>,
    delta: Option<Box<Delta>>,
    delta_deep: Option<Box<Delta>>,
    /// Changed attribute keys, where `None` marks a change made to node's children.
    subs: &'txn HashSet<Option<Arc<str>>>,
}

impl<'txn> Event<'txn> {
    pub(crate) fn new(
        target: NodePtr,
        txn: &'txn Transaction<&'txn Doc>,
        subs: &'txn HashSet<Option<Arc<str>>>,
    ) -> Self {
        Event {
            target,
            current_target: target,
            txn,
            delta: None,
            delta_deep: None,
            subs,
        }
    }

    /// Transaction which triggered this event.
    pub fn transaction(&self) -> &'txn Transaction<&'txn Doc> {
        self.txn
    }

    /// Node, where the change has occurred. For shallow events
    /// it's the same as [Event::current_target], but may differ for deep observers.
    pub fn target(&self) -> NodeRef<&'txn Transaction<&'txn Doc>> {
        NodeRef::new(self.target, self.txn)
    }

    /// Node where the observer of this event is registered.
    pub fn current_target(&self) -> NodeRef<&'txn Transaction<&'txn Doc>> {
        NodeRef::new(self.current_target, self.txn)
    }

    pub(crate) fn set_current_target(&mut self, target: NodePtr) {
        self.current_target = target;
    }

    /// A path from the node this event's observer is registered on, down to [Event::target].
    pub fn path(&self) -> Path {
        Node::path(self.current_target, self.target)
    }

    /// Whether children changed.
    pub fn children_changed(&self) -> bool {
        self.subs.contains(&None)
    }

    /// Attribute keys that changed.
    pub fn keys_changed(&self) -> impl Iterator<Item = &'txn Arc<str>> + use<'txn> {
        self.subs.iter().flatten()
    }

    /// Check if given key has been changed within that event's node.
    pub fn key_changed<S: Into<Arc<str>>>(&self, key: S) -> bool {
        self.subs.contains(&Some(key.into()))
    }

    /// Changes made to [Event::target] within this transaction. If `deep` is set, changes made to
    /// nested nodes are included as well.
    pub fn delta(&mut self, deep: bool) -> Delta<Out> {
        let (txn, target) = (self.txn, self.target);
        let cache = if deep {
            &mut self.delta_deep
        } else {
            &mut self.delta
        };
        let delta = cache.get_or_insert_with(|| {
            // events are only created for committed transactions, which always have a state
            let state = txn.state.as_deref().unwrap();
            Box::new(get_delta(txn.doc, state, target, deep))
        });
        (**delta).clone()
    }

    /// Check if given element was deleted by this event.
    pub fn is_deleted(&self, id: &ID) -> bool {
        self.txn.has_deleted(id)
    }

    /// Check if given element was added by this event.
    pub fn is_added(&self, id: &ID) -> bool {
        self.txn.has_added(id)
    }
}

#[cfg(test)]
mod test {
    use crate::node::{DeepObservable, Observable};
    use crate::{Acquire, AcquireMut, Cell, ClientID, Delta, Doc, ID, NodeID, Origin};

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
        let deltas = Cell::new(vec![]);
        let deltas_c = deltas.clone();
        let _sub = {
            let txn = doc.transact();
            txn.node("map").unwrap().observe(move |e| {
                let delta = (e.delta(false), e.delta(true));
                deltas_c.acquire_mut().push(delta)
            })
        };
        {
            let mut txn = doc.transact_mut();
            txn.node_mut("map").unwrap().insert_attr("a", 1);
            txn.node_mut(nested).unwrap().push_back(2);
        }
        let shallow = Delta::out().insert_attr("a", 1);
        let deep = shallow
            .clone()
            .modify_attr("nested", Delta::out().retain(1).insert(2));
        assert_eq!(*deltas.acquire(), vec![(shallow, deep)]);
    }

    #[test]
    fn is_added_and_is_deleted() {
        let mut doc = Doc::with_client_id(1);
        let flags = Cell::new([(false, false); 2]);
        let flags_c = flags.clone();
        let _sub = {
            let mut txn = doc.transact_mut();
            txn.node_mut("array").unwrap().observe(move |e| {
                let ids = [ID::new(ClientID::new(1), 0), ID::new(ClientID::new(1), 1)];
                *flags_c.acquire_mut() = ids.map(|id| (e.is_added(&id), e.is_deleted(&id)));
            })
        };
        {
            let mut txn = doc.transact_mut();
            let mut array = txn.node_mut("array").unwrap();
            array.push_back(1);
            array.push_back(2);
            array.remove(0, 1);
        }
        assert_eq!(*flags.acquire(), [(true, true), (true, false)]);
    }

    #[test]
    fn observe_deep_receives_only_own_subtree_events() {
        let mut doc = Doc::with_client_id(1);
        let targets = Cell::new(vec![]);
        let targets_c = targets.clone();
        let _sub = {
            let mut txn = doc.transact_mut();
            txn.node_mut("a").unwrap().observe_deep(move |events| {
                let ids = events.iter().map(|e| e.target().id());
                targets_c.acquire_mut().extend(ids);
            })
        };
        {
            let mut txn = doc.transact_mut();
            txn.node_mut("a").unwrap().insert_attr("key", 1);
            txn.node_mut("b").unwrap().insert_attr("key", 1);
        }
        assert_eq!(*targets.acquire(), vec![NodeID::from("a")]);
    }

    #[test]
    fn transaction_origin() {
        let mut doc = Doc::new();
        let origin = Cell::new(None);
        let origin_c = origin.clone();
        let _sub = {
            let mut txn = doc.transact_mut();
            txn.node_mut("map").unwrap().observe(move |e| {
                *origin_c.acquire_mut() = e.transaction().origin().cloned();
            })
        };
        let mut txn = doc.transact_mut_with("test");
        txn.node_mut("map").unwrap().insert_attr("key", 1);
        drop(txn);
        assert_eq!(*origin.acquire(), Some(Origin::from("test")));
    }
}
