use crate::node::{NodePtr, Path};
use crate::transaction::Subdocs;
use crate::{
    Delta, Doc, IdSet, NodeRef, StateVector, Transaction, TransactionMut, TransactionState, Uuid,
};
use std::collections::HashSet;

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
    doc: &'txn Doc,
    state: &'txn TransactionState,
}

impl<'txn> Event<'txn> {
    pub fn transaction(&self) -> Transaction<&'txn Doc> {
        todo!()
    }

    /// Node, where the change has occurred. For shallow events
    /// it's the same as [Event::current_target], but may differ for deep observers.
    pub fn target(&self) -> NodeRef<Transaction<&'txn Doc>> {
        todo!()
    }

    /// Node where the observer of this event is registered.
    pub fn current_target(&self) -> NodeRef<Transaction<&'txn Doc>> {
        todo!()
    }

    pub(crate) fn set_current_target(&mut self, target: NodePtr) {
        todo!()
    }

    /// A path from the node this event's observer is registered on, down to [Event::target].
    pub fn path(&self) -> Path {
        todo!()
    }

    /// Whether children changed.
    pub fn children_changed(&self) -> bool {
        todo!()
    }

    /// Attribute keys that changed.
    pub fn keys_changed(&self) -> bool {
        todo!()
    }

    pub fn delta(&self, options: /* todo */ ()) -> impl Iterator<Item = Delta> {
        todo!();
        #[allow(unreachable_code)]
        std::iter::empty()
    }

    /// Was this node deleted?
    pub fn is_deleted(&self) -> bool {
        todo!()
    }

    /// Was this node added?
    pub fn is_added(&self) -> bool {
        todo!()
    }
}
