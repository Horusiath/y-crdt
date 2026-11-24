use crate::block::ItemPtr;
use crate::id_set::DeleteSet;
use crate::lib0::EncoderV1;
use crate::slice::BlockSlice;
use crate::transaction::for_each_ancestor;
use crate::{
    Doc, DocHandle, ID, IdSet, NodeID, NodeRef, Reporter, Transaction, TransactionState, Uuid,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt::Formatter;
use std::ops::{Deref, DerefMut};

/// Undo manager is a structure used to perform undo/redo operations over the associated shared
/// type(s), possibly spread over multiple documents.
///
/// Changes are tracked with [UndoManager::track] (also called when a transaction is committed
/// with the manager as its [Reporter]), filtered by its scope (see: [UndoManager::expand_scope]).
///
/// Undo-/redo-able actions (a.k.a. [StackItem]s) are not equivalent to [crate::TransactionMut]
/// unit of work: tracked changes extend the last [StackItem] until [UndoManager::capture] is
/// called, which denotes the end of the batch.
///
/// In order to revert an operation, call [UndoManager::undo], then [UndoManager::redo] to bring it
/// back.
pub struct UndoManager<M> {
    scope: HashMap<Uuid, Scope>,
    undo_stack: UndoStack<M>,
    redo_stack: UndoStack<M>,
    /// Top of the undo stack is extended by tracked changes until captured.
    open: bool,
}

impl<M> UndoManager<M>
where
    M: Default,
{
    /// Creates a new instance of the [UndoManager] without any scope.
    pub fn new() -> Self {
        UndoManager {
            scope: HashMap::new(),
            undo_stack: UndoStack::default(),
            redo_stack: UndoStack::default(),
            open: false,
        }
    }

    /// Extends the scope tracked by current undo manager by a given node or a whole document.
    pub fn expand_scope<S: UndoScope + ?Sized>(&mut self, s: &S) {
        let guid = s.doc_guid().clone();
        match s.node() {
            None => {
                self.scope.insert(guid, Scope::Doc);
            }
            Some(node) => {
                let scope = self.scope.entry(guid);
                if let Scope::Nodes(nodes) = scope.or_insert_with(|| Scope::Nodes(HashSet::new())) {
                    nodes.insert(node);
                }
            }
        }
    }

    /// Guids of the documents tracked by current undo manager.
    pub fn tracked_docs(&self) -> impl Iterator<Item = &Uuid> {
        self.scope.keys()
    }

    /// Scope tracked within a document of a given guid.
    pub fn scope(&self, doc: &Uuid) -> Option<&Scope> {
        self.scope.get(doc)
    }

    /// Clears [StackItem]s of the `txn` document from both undo AND redo stacks. Items of other
    /// documents stay untouched. Returns `true` if any item was removed.
    pub fn clear_all<D: DocHandle + DerefMut>(&mut self, txn: &mut Transaction<D>) -> bool {
        let undo = self.clear_undo(txn);
        self.clear_redo(txn) || undo
    }

    /// Clears [StackItem]s of the `txn` document from the undo stack, leaving redo stack
    /// untouched. Returns `true` if any item was removed.
    pub fn clear_undo<D: DocHandle + DerefMut>(&mut self, txn: &mut Transaction<D>) -> bool {
        let doc = txn.doc_mut();
        if self.undo_stack.last().is_some_and(|i| i.doc == *doc.guid()) {
            self.open = false;
        }
        clear_stack(&mut self.undo_stack, doc, self.scope.get(doc.guid()))
    }

    /// Clears [StackItem]s of the `txn` document from the redo stack, leaving undo stack
    /// untouched. Returns `true` if any item was removed.
    pub fn clear_redo<D: DocHandle + DerefMut>(&mut self, txn: &mut Transaction<D>) -> bool {
        let doc = txn.doc_mut();
        clear_stack(&mut self.redo_stack, doc, self.scope.get(doc.guid()))
    }

    /// Seals the last [StackItem] with given `meta`, so that next tracked changes start a new one.
    /// Returns `false` (and drops `meta`) if there were no tracked changes since the last capture.
    ///
    /// Example:
    /// ```rust
    /// use yrs::{Doc, NoReporter, UndoManager};
    ///
    /// let mut doc = Doc::new();
    /// let insert = |mgr: &mut UndoManager, doc: &mut Doc, name: &str, index: u32, chunk: &str| {
    ///     let mut txn = doc.transact_mut();
    ///     txn.node_mut(name).unwrap().insert_text(index, chunk);
    ///     mgr.track(&txn);
    ///     txn.commit(&mut NoReporter);
    /// };
    /// let undo = |mgr: &mut UndoManager, doc: &mut Doc| {
    ///     let mut txn = doc.transact_mut();
    ///     mgr.undo(&mut txn).unwrap();
    ///     txn.commit(&mut NoReporter);
    /// };
    /// let read = |doc: &Doc, name: &str| doc.transact().node(name).unwrap().to_string();
    ///
    /// // without UndoManager::capture
    /// let mut mgr = UndoManager::new();
    /// mgr.expand_scope(&doc.transact_mut().node_mut("no-capture").unwrap());
    /// insert(&mut mgr, &mut doc, "no-capture", 0, "a");
    /// insert(&mut mgr, &mut doc, "no-capture", 1, "b");
    /// undo(&mut mgr, &mut doc);
    /// assert_eq!(read(&doc, "no-capture"), ""); // note that 'ab' was removed
    ///
    /// // with UndoManager::capture
    /// let mut mgr = UndoManager::new();
    /// mgr.expand_scope(&doc.transact_mut().node_mut("with-capture").unwrap());
    /// insert(&mut mgr, &mut doc, "with-capture", 0, "a");
    /// mgr.capture(());
    /// insert(&mut mgr, &mut doc, "with-capture", 1, "b");
    /// undo(&mut mgr, &mut doc);
    /// assert_eq!(read(&doc, "with-capture"), "a"); // note that only 'b' was removed
    /// ```
    pub fn capture(&mut self, meta: M) -> bool {
        if !self.open {
            return false;
        }
        self.open = false;
        if let Some(item) = self.undo_stack.last_mut() {
            item.meta = meta;
        }
        true
    }

    /// Are there any undo steps available?
    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    /// Returns a list of [StackItem]s stored within current undo manager responsible for performing
    /// potential undo operations.
    pub fn undo_stack(&self) -> &[StackItem<M>] {
        &self.undo_stack.0
    }

    /// Mutable counterpart of [UndoManager::undo_stack]. Until [UndoManager::capture] is called,
    /// tracked changes extend the last item of the same document.
    pub fn undo_stack_mut(&mut self) -> &mut Vec<StackItem<M>> {
        &mut self.undo_stack.0
    }

    /// Returns a list of [StackItem]s stored within current undo manager responsible for performing
    /// potential redo operations.
    pub fn redo_stack(&self) -> &[StackItem<M>] {
        &self.redo_stack.0
    }

    /// Mutable counterpart of [UndoManager::redo_stack].
    pub fn redo_stack_mut(&mut self) -> &mut Vec<StackItem<M>> {
        &mut self.redo_stack.0
    }

    /// Records changes of a read-write `txn` which touch current undo manager scope: they extend
    /// the last [StackItem] (until [UndoManager::capture]) and the redo stack of `txn` document is
    /// cleared. Content deleted within the scope is kept from being garbage collected, so it must
    /// be called before `txn` is committed (eg. from [Reporter::committing]).
    ///
    /// Read-only transactions, ones without changes and ones performing undo/redo are ignored.
    ///
    /// Tracking the same transaction twice only will take note of changes that happened before
    /// first track.
    pub fn track<D: DocHandle>(&mut self, txn: &Transaction<D>) {
        let (doc, Some(state)) = (txn.doc(), txn.state.as_deref()) else {
            return;
        };
        if state.undo_redo || !txn.has_changes() {
            return;
        }
        let guid = doc.guid();
        let Some(scope) = self.scope.get(guid) else {
            return;
        };
        let in_scope = match scope {
            Scope::Doc => true,
            Scope::Nodes(nodes) => changes_any(doc, state, nodes),
        };
        if !in_scope {
            return;
        }
        // neither undoing nor redoing: drop redo stack items of this doc
        clear_stack(&mut self.redo_stack, doc, Some(scope));
        let top = self.undo_stack.last_mut();
        match top {
            Some(item) if self.open && &item.doc == guid => {
                item.deletions.merge_with(state.delete_set.clone());
                item.insertions.merge_with(state.insert_set.clone());
            }
            _ => {
                let (deletes, inserts) = (state.delete_set.clone(), state.insert_set.clone());
                let item = StackItem::new(guid.clone(), deletes, inserts);
                self.undo_stack.push(item);
            }
        }
        self.open = true;
        // make sure that deleted content is not GC'ed
        keep_deleted(doc, scope, &state.delete_set, true);
    }

    /// Undo last action tracked by current undo manager within a given `txn`. Actions (a.k.a.
    /// [StackItem]s) are separated by [UndoManager::capture].
    ///
    /// Returns the reverted [StackItem], or `None` if nothing could be reverted. Fails if `txn`
    /// already has changes, or if the last [StackItem] belongs to another document than `txn`.
    pub fn undo<D: DocHandle + DerefMut>(
        &mut self,
        txn: &mut Transaction<D>,
    ) -> Result<Option<StackItem<M>>, UndoError> {
        self.pop(true, txn)
    }

    /// Are there any redo steps available?
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// Redo'es last action previously undo'ed by current undo manager within a given `txn`.
    ///
    /// Returns the reapplied [StackItem], or `None` if nothing could be reapplied. Fails if `txn`
    /// already has changes, or if the last [StackItem] belongs to another document than `txn`.
    pub fn redo<D: DocHandle + DerefMut>(
        &mut self,
        txn: &mut Transaction<D>,
    ) -> Result<Option<StackItem<M>>, UndoError> {
        self.pop(false, txn)
    }

    fn pop<D: DocHandle + DerefMut>(
        &mut self,
        undoing: bool,
        txn: &mut Transaction<D>,
    ) -> Result<Option<StackItem<M>>, UndoError> {
        if txn.has_changes() {
            return Err(UndoError::DirtyTransaction);
        }
        let (doc, state) = txn.split_mut();
        let guid = doc.guid().clone();
        let none = Scope::Nodes(HashSet::new());
        let scope = self.scope.get(&guid).unwrap_or(&none);
        let (stack, other) = if undoing {
            (&mut self.undo_stack, &mut self.redo_stack)
        } else {
            (&mut self.redo_stack, &mut self.undo_stack)
        };
        // pop until a change is performed, all within the same transaction
        while let Some(top) = stack.last() {
            if top.doc != guid {
                let expected = top.doc.clone();
                return Err(UndoError::DocMismatch {
                    expected,
                    actual: guid,
                });
            }
            let item = stack.pop().unwrap();
            let (ins, del) = (&item.insertions, &item.deletions);
            if revert(doc, state, ins, del, scope, stack, other) {
                keep_deleted(doc, scope, &state.delete_set, true);
                let (deletes, inserts) = (state.delete_set.clone(), state.insert_set.clone());
                other.push(StackItem::new(guid, deletes, inserts));
                if undoing {
                    self.open = false;
                }
                state.undo_redo = true;
                return Ok(Some(item));
            }
        }
        Ok(None)
    }
}

impl<M: Default> Reporter for UndoManager<M> {
    type Encoder = EncoderV1;

    fn committing(&mut self, txn: &Transaction<&Doc>) {
        self.track(txn)
    }
}

/// Checks if any node changed within `state` is nested in (or equal to) one of the `nodes`.
fn changes_any(doc: &Doc, state: &TransactionState, nodes: &HashSet<NodeID>) -> bool {
    let mut visited = HashSet::new();
    state.changed.keys().any(|&node| {
        let mut found = false;
        visited.clear();
        for_each_ancestor(&doc.linked_by, node, &mut visited, &mut |n| {
            found |= nodes.contains(&n.id())
        });
        found
    })
}

fn keep_deleted(doc: &Doc, scope: &Scope, ids: &IdSet, keep: bool) {
    let mut deleted = ids.blocks();
    while let Some(slice) = deleted.next_in(&doc.blocks) {
        if let Some(item) = slice.as_item() {
            if scope.contains(item) {
                item.keep(keep);
            }
        }
    }
}

/// Removes `stack` items of the `doc`, releasing their deleted content in `scope`.
/// Returns `true` if any item was removed.
fn clear_stack<M>(stack: &mut UndoStack<M>, doc: &Doc, scope: Option<&Scope>) -> bool {
    let len = stack.len();
    stack.retain(|item| {
        if item.doc != *doc.guid() {
            return true;
        }
        if let Some(scope) = scope {
            keep_deleted(doc, scope, &item.deletions, false);
        }
        false
    });
    len != stack.len()
}

pub(crate) fn revert<M>(
    doc: &mut Doc,
    state: &mut TransactionState,
    insertions: &IdSet,
    deletions: &IdSet,
    scope: &Scope,
    stack_a: &UndoStack<M>,
    stack_b: &UndoStack<M>,
) -> bool {
    let mut to_redo = HashSet::<ItemPtr>::new();
    let mut to_delete = Vec::<ItemPtr>::new();
    let mut change_performed = false;

    let inserted: Vec<_> = insertions.iter_blocks(&doc.blocks).collect();
    for slice in inserted {
        if let BlockSlice::Item(slice) = slice {
            let mut item = doc.materialize(slice);
            if item.redone.is_some() {
                let slice = match doc.follow_redone(item.id()) {
                    Some(slice) => slice,
                    None => return false,
                };
                item = doc.materialize(slice);
            }
            if !item.is_deleted() && scope.contains(item) {
                to_delete.push(item);
            }
        }
    }

    let mut deleted = deletions.blocks();
    while let Some(slice) = deleted.next_in(&doc.blocks) {
        if let BlockSlice::Item(slice) = slice {
            let ptr = doc.materialize(slice);
            // never redo structs in `insertions`: they were created and deleted in the same change
            if scope.contains(ptr) && !insertions.contains(ptr.id()) {
                to_redo.insert(ptr);
            }
        }
    }

    for &ptr in to_redo.iter() {
        let mut ptr = ptr;
        change_performed |= ptr
            .redo(doc, state, &to_redo, insertions, stack_a, stack_b)
            .is_some();
    }

    // delete in reverse order so that children are deleted before parents,
    // so more information is available when items are filtered
    for &item in to_delete.iter().rev() {
        state.delete(doc, item);
        change_performed = true;
    }
    change_performed
}

/// Part of a document tracked by an [UndoManager].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// The whole document.
    Doc,
    /// Given nodes and everything nested in them.
    Nodes(HashSet<NodeID>),
}

impl Scope {
    /// Checks if a node of a given `id` is tracked. Its ancestors are not taken into account.
    pub fn contains_node(&self, id: &NodeID) -> bool {
        match self {
            Scope::Doc => true,
            Scope::Nodes(nodes) => nodes.contains(id),
        }
    }

    /// Checks if an `item` is nested (at any depth) in a tracked node.
    pub(crate) fn contains(&self, item: ItemPtr) -> bool {
        let nodes = match self {
            Scope::Doc => return true,
            Scope::Nodes(nodes) if nodes.is_empty() => return false,
            Scope::Nodes(nodes) => nodes,
        };
        let mut parent = item.parent.as_node().copied();
        while let Some(node) = parent {
            if nodes.contains(&node.id()) {
                return true;
            }
            parent = node.item.and_then(|item| item.parent.as_node().copied());
        }
        false
    }
}

/// Something which can be added to an [UndoManager] scope (see: [UndoManager::expand_scope]).
pub trait UndoScope {
    /// Guid of the document this scope belongs to.
    fn doc_guid(&self) -> &Uuid;
    /// Tracked node or `None` if the whole document is tracked.
    fn node(&self) -> Option<NodeID>;
}

impl UndoScope for Doc {
    fn doc_guid(&self) -> &Uuid {
        self.guid()
    }

    fn node(&self) -> Option<NodeID> {
        None
    }
}

impl<D: DocHandle> UndoScope for Transaction<D> {
    fn doc_guid(&self) -> &Uuid {
        self.doc().guid()
    }

    fn node(&self) -> Option<NodeID> {
        None
    }
}

impl<D: DocHandle> UndoScope for NodeRef<&Transaction<D>> {
    fn doc_guid(&self) -> &Uuid {
        self.txn().doc().guid()
    }

    fn node(&self) -> Option<NodeID> {
        Some(self.id())
    }
}

impl<D: DocHandle> UndoScope for NodeRef<&mut Transaction<D>> {
    fn doc_guid(&self) -> &Uuid {
        self.txn().doc().guid()
    }

    fn node(&self) -> Option<NodeID> {
        Some(self.id())
    }
}

/// Errors returned by [UndoManager] operations.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UndoError {
    /// The [StackItem] to apply belongs to another document than the transaction.
    #[error("stack item belongs to document '{expected}', but transaction is over '{actual}'")]
    DocMismatch { expected: Uuid, actual: Uuid },
    /// Undo/redo must run within a transaction without prior changes.
    #[error("transaction already contains changes")]
    DirtyTransaction,
}

#[repr(transparent)]
#[derive(Debug, Clone, Eq, PartialEq, Default)]
pub(crate) struct UndoStack<M>(Vec<StackItem<M>>);

impl<M> Deref for UndoStack<M> {
    type Target = Vec<StackItem<M>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<M> DerefMut for UndoStack<M> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<M> UndoStack<M> {
    pub fn is_deleted(&self, id: &ID) -> bool {
        for item in self.0.iter() {
            if item.deletions.contains(id) {
                return true;
            }
        }
        false
    }
}

/// A unit of work for the [UndoManager]. It contains a compressed information about all updates and
/// deletions tracked by a corresponding undo manager. Whenever an [UndoManager::undo] or
/// [UndoManager::redo] methods are called a last [StackItem] is being used to modify a state of
/// the document.
///
/// Stack items are stored internally by undo manager and created automatically whenever changes
/// of a tracked shared type are [tracked](UndoManager::track) after the last
/// [UndoManager::capture], which marks the end of the last stack item batch.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct StackItem<T> {
    doc: Uuid,
    deletions: IdSet,
    insertions: IdSet,

    /// A custom user metadata that can be attached to a particular [StackItem]. It can be used
    /// to carry over the additional information (such as ie. user cursor position) between
    /// undo/redo operations.
    pub meta: T,
}

impl<M> StackItem<M> {
    pub fn with_meta(doc: Uuid, deletions: IdSet, insertions: IdSet, meta: M) -> Self {
        StackItem {
            doc,
            deletions,
            insertions,
            meta,
        }
    }

    /// Guid of the document this [StackItem] belongs to.
    pub fn doc(&self) -> &Uuid {
        &self.doc
    }

    /// A set of identifiers of element deleted at part of the timeframe current [StackItem] is
    /// responsible for.
    pub fn deletions(&self) -> &IdSet {
        &self.deletions
    }

    /// A set of identifiers of element inserted at part of the timeframe current [StackItem] is
    /// responsible for.
    pub fn insertions(&self) -> &IdSet {
        &self.insertions
    }

    /// Returns metaobject reference associated with this stack item.
    pub fn meta(&self) -> &M {
        &self.meta
    }

    /// Merged another [StackItem] into current one. `merge_meta` function is used to merge user's
    /// custom metadata structures together. Fails if `other` belongs to another document.
    pub fn merge<F>(&mut self, other: Self, merge_meta: F) -> Result<(), UndoError>
    where
        F: FnOnce(&mut M, M),
    {
        if other.doc != self.doc {
            return Err(UndoError::DocMismatch {
                expected: self.doc.clone(),
                actual: other.doc,
            });
        }
        self.insertions.merge_with(other.insertions);
        self.deletions.merge_with(other.deletions);
        merge_meta(&mut self.meta, other.meta);
        Ok(())
    }
}

impl<M: Default> StackItem<M> {
    pub fn new(doc_id: Uuid, deletions: IdSet, insertions: IdSet) -> Self {
        Self::with_meta(doc_id, deletions, insertions, M::default())
    }
}

impl<M> std::fmt::Display for StackItem<M> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "StackItem(")?;
        if !self.deletions.is_empty() {
            write!(f, "-{}", self.deletions)?;
        }
        if !self.insertions.is_empty() {
            write!(f, "+{}", self.insertions)?;
        }
        write!(f, ")")
    }
}

#[cfg(test)]
mod test {
    use std::collections::HashMap;
    use std::convert::TryInto;

    use crate::block::ClientID;

    use crate::lib0::{Decode, EncoderV1};
    use crate::node::{Attrs, NodePtr};
    use crate::test_utils::{Report, exchange_updates};
    use crate::undo::{Scope, StackItem, UndoError};
    use crate::{
        Any, Delta, DeltaOptions, Doc, ID, IdSet, In, NoReporter, NodeID, NodeRef, Origin, Out,
        Reporter, StateVector, Transaction, UndoManager, Update, any,
    };
    use serde::Deserialize;

    #[test]
    fn selective_clear_preserves_other_stack_and_reports_actual_changes() {
        for (clear_undo, clear_redo) in [(true, false), (false, true), (true, true)] {
            let mut doc = Doc::new();
            let mut mgr = UndoManager::new();
            node(&mut doc, "text", &mut NoReporter, |n| mgr.expand_scope(&n));
            node(&mut doc, "text", &mut mgr, |mut n| n.insert_text(0, "a"));
            mgr.capture(());
            node(&mut doc, "text", &mut mgr, |mut n| n.insert_text(1, "b"));
            assert!(undo(&mut mgr, &mut doc).is_some());
            assert!(mgr.can_undo() && mgr.can_redo());
            let text = |doc: &Doc| doc.transact().node("text").unwrap().to_string();
            for i in 0..2 {
                let cleared = {
                    let mut txn = doc.transact_mut();
                    match (clear_undo, clear_redo) {
                        (true, false) => mgr.clear_undo(&mut txn),
                        (false, true) => mgr.clear_redo(&mut txn),
                        _ => mgr.clear_all(&mut txn),
                    }
                };
                assert_eq!(mgr.can_undo(), !clear_undo);
                assert_eq!(mgr.can_redo(), !clear_redo);
                assert_eq!(text(&doc), "a");
                // was: stack_cleared event fired once, with (clear_undo, clear_redo)
                assert_eq!(cleared, i == 0);
            }
            if !clear_undo {
                assert!(undo(&mut mgr, &mut doc).is_some());
                assert_eq!(text(&doc), "");
            } else if !clear_redo {
                assert!(redo(&mut mgr, &mut doc).is_some());
                assert_eq!(text(&doc), "ab");
            }
        }
    }

    #[test]
    fn undo_text() {
        let mut d1 = Doc::with_client_id(1);
        let txt1 = NodePtr::from(d1.transact_mut().node_mut("test").unwrap().as_ref());
        let mut mgr = UndoManager::new();
        node(&mut d1, "test", &mut NoReporter, |n| mgr.expand_scope(&n));

        let mut d2 = Doc::with_client_id(2);

        // items that are added & deleted in the same transaction won't be undo
        node(&mut d1, "test", &mut mgr, |mut n| n.insert_text(0, "test"));
        node(&mut d1, "test", &mut mgr, |mut n| n.remove(0, 4));
        undo(&mut mgr, &mut d1);
        assert_eq!(d1.transact().node("test").unwrap().to_string(), "");

        // follow redone items
        node(&mut d1, "test", &mut mgr, |mut n| n.insert_text(0, "a"));
        mgr.capture(());
        node(&mut d1, "test", &mut mgr, |mut n| n.remove(0, 1));
        mgr.capture(());
        undo(&mut mgr, &mut d1);
        assert_eq!(d1.transact().node("test").unwrap().to_string(), "a");
        undo(&mut mgr, &mut d1);
        assert_eq!(d1.transact().node("test").unwrap().to_string(), "");

        node(&mut d1, "test", &mut mgr, |mut n| n.insert_text(0, "abc"));
        node(&mut d2, "test", &mut NoReporter, |mut n| {
            n.insert_text(0, "xyz")
        });

        exchange_updates(&mut [&mut d1, &mut d2]);
        undo(&mut mgr, &mut d1);
        assert_eq!(d1.transact().node("test").unwrap().to_string(), "xyz");
        redo(&mut mgr, &mut d1);
        assert_eq!(d1.transact().node("test").unwrap().to_string(), "abcxyz");

        exchange_updates(&mut [&mut d1, &mut d2]);

        node(&mut d2, "test", &mut NoReporter, |mut n| n.remove(0, 1));

        exchange_updates(&mut [&mut d1, &mut d2]);

        undo(&mut mgr, &mut d1);
        assert_eq!(d1.transact().node("test").unwrap().to_string(), "xyz");
        redo(&mut mgr, &mut d1);
        {
            let mut t1 = d1.transact_mut();
            let mut txt1 = t1.node_mut("test").unwrap();
            assert_eq!(txt1.to_string(), "bcxyz");

            let bold = Attrs::from([("bold".into(), true.into())]);
            txt1.format(1, 3, &bold);
            let diff = txt1.delta(&DeltaOptions::default());
            assert_eq!(
                diff,
                Delta::out()
                    .insert_text("b")
                    .insert_text_fmt("cxy", &bold)
                    .insert_text("z"),
            );
            t1.commit(&mut mgr);
        }
        undo(&mut mgr, &mut d1);
        redo(&mut mgr, &mut d1);
    }

    #[test]
    fn double_undo() {
        let mut doc = Doc::with_client_id(1);
        node(&mut doc, "test", &mut NoReporter, |mut n| {
            n.insert_text(0, "1221")
        });

        let mut mgr = UndoManager::new();
        node(&mut doc, "test", &mut NoReporter, |n| mgr.expand_scope(&n));
        node(&mut doc, "test", &mut mgr, |mut n| n.insert_text(2, "3"));
        node(&mut doc, "test", &mut mgr, |mut n| n.insert_text(3, "3"));

        undo(&mut mgr, &mut doc);
        undo(&mut mgr, &mut doc);

        node(&mut doc, "test", &mut mgr, |mut n| n.insert_text(2, "3"));
        assert_eq!(doc.transact().node("test").unwrap().to_string(), "12321");
    }

    #[test]
    fn undo_map() {
        let mut d1 = Doc::with_client_id(1);

        node(&mut d1, "test", &mut NoReporter, |mut n| {
            n.insert_attr("a", 0)
        });
        let mut mgr = UndoManager::new();
        node(&mut d1, "test", &mut NoReporter, |n| mgr.expand_scope(&n));
        node(&mut d1, "test", &mut mgr, |mut n| n.insert_attr("a", 1));
        undo(&mut mgr, &mut d1);
        assert_eq!(
            d1.transact().node("test").unwrap().attr("a").unwrap(),
            0.into()
        );
        redo(&mut mgr, &mut d1);
        assert_eq!(
            d1.transact().node("test").unwrap().attr("a").unwrap(),
            1.into()
        );

        // TODO(unified-api): nested `MapPrelim` sub-types + cross-peer overwrite have no NodeRef
        // equivalent yet. Original block: inserted a nested map under "a", set "x"=42, checked
        // that undo/redo restores the whole nested type, then had a second peer overwrite key "a"
        // with 44 (concurrent overwrite makes undo skip).

        // test setting value multiple times
        node(&mut d1, "test", &mut mgr, |mut n| {
            n.insert_attr("b", "initial".to_string())
        });
        mgr.capture(());
        node(&mut d1, "test", &mut mgr, |mut n| {
            n.insert_attr("b", "val1".to_string())
        });
        node(&mut d1, "test", &mut mgr, |mut n| {
            n.insert_attr("b", "val2".to_string())
        });
        mgr.capture(());
        undo(&mut mgr, &mut d1);
        assert_eq!(
            d1.transact().node("test").unwrap().attr("b").unwrap(),
            "initial".into()
        );
    }

    /// Runs `f` over a `node` in a new transaction committed with `r`.
    fn node<F, T>(doc: &mut Doc, node: impl Into<NodeID>, r: &mut impl Reporter, f: F) -> T
    where
        F: FnOnce(NodeRef<&mut Transaction<&mut Doc>>) -> T,
    {
        let mut txn = doc.transact_mut();
        let result = f(txn.node_mut(node.into()).unwrap());
        txn.commit(r);
        result
    }

    /// Undoes the last change within a new transaction over `doc`, committed with [NoReporter].
    fn undo<M: Default>(mgr: &mut super::UndoManager<M>, doc: &mut Doc) -> Option<StackItem<M>> {
        let mut txn = doc.transact_mut();
        let item = mgr.undo(&mut txn).unwrap();
        txn.commit(&mut NoReporter);
        item
    }

    /// Redoes the last undone change within a new transaction over `doc`, committed with
    /// [NoReporter].
    fn redo<M: Default>(mgr: &mut super::UndoManager<M>, doc: &mut Doc) -> Option<StackItem<M>> {
        let mut txn = doc.transact_mut();
        let item = mgr.redo(&mut txn).unwrap();
        txn.commit(&mut NoReporter);
        item
    }

    /// Same as [node], but the underlying transaction is tagged with a given `origin`.
    fn node_with<F, T>(
        doc: &mut Doc,
        origin: impl Into<Origin>,
        node: impl Into<NodeID>,
        r: &mut impl Reporter,
        f: F,
    ) -> T
    where
        F: FnOnce(NodeRef<&mut Transaction<&mut Doc>>) -> T,
    {
        let mut txn = doc.transact_mut_with(origin);
        let result = f(txn.node_mut(node.into()).unwrap());
        txn.commit(r);
        result
    }

    #[test]
    // TODO(unified-api): nested MapPrelim/ArrayPrelim + Array::insert_range + Out::cast have no NodeRef equivalent yet
    fn undo_array() {
        #[derive(Debug, Default, Deserialize, PartialEq)]
        struct Entry {
            a: Option<i32>,
            b: Option<i32>,
        }
        impl Entry {
            fn new(a: Option<i32>, b: Option<i32>) -> Self {
                Entry { a, b }
            }
        }

        let array = NodeID::root("test");
        let mut d1 = Doc::with_client_id(1);

        let mut d2 = Doc::with_client_id(2);

        let mut mgr = UndoManager::new();
        node(&mut d1, array.clone(), &mut NoReporter, |n| {
            mgr.expand_scope(&n)
        });
        node(&mut d1, "test", &mut mgr, |mut array| {
            array.insert_range(0, [1, 2, 3])
        });
        node(&mut d2, "test", &mut NoReporter, |mut array| {
            array.insert_range(0, [4, 5, 6])
        });

        exchange_updates(&mut [&mut d1, &mut d2]);

        node(&mut d1, "test", &mut NoReporter, |mut array| {
            assert_eq!(
                array.deserialize::<Vec<i32>>().unwrap(),
                vec![1, 2, 3, 4, 5, 6]
            )
        });

        undo(&mut mgr, &mut d1);

        node(&mut d1, "test", &mut NoReporter, |mut array| {
            assert_eq!(array.deserialize::<Vec<i32>>().unwrap(), vec![4, 5, 6])
        });

        redo(&mut mgr, &mut d1);

        node(&mut d1, "test", &mut NoReporter, |mut array| {
            assert_eq!(
                array.deserialize::<Vec<i32>>().unwrap(),
                vec![1, 2, 3, 4, 5, 6]
            )
        });

        exchange_updates(&mut [&mut d1, &mut d2]);

        node(&mut d2, "test", &mut NoReporter, |mut array| {
            array.remove(0, 1); // user2 deletes [1]
        });

        exchange_updates(&mut [&mut d1, &mut d2]);

        undo(&mut mgr, &mut d1);
        node(&mut d1, "test", &mut NoReporter, |mut array| {
            assert_eq!(array.deserialize::<Vec<i32>>().unwrap(), vec![4, 5, 6]);
        });

        redo(&mut mgr, &mut d1);

        node(&mut d1, "test", &mut mgr, |mut array| {
            assert_eq!(
                array.deserialize::<Vec<i32>>().unwrap(),
                vec![2, 3, 4, 5, 6]
            );
            array.remove(0, 5);
        });

        // test nested structure
        let map = node(&mut d1, "test", &mut mgr, |mut array| {
            let Out::Node(map) = array.insert(0, Delta::new()) else {
                unreachable!()
            };
            assert_eq!(
                array.deserialize::<Vec<Entry>>().unwrap(),
                vec![Entry::new(None, None)]
            );
            map
        });

        mgr.capture(());

        node(&mut d1, map.id.clone(), &mut mgr, |mut map| {
            map.insert_attr("a", 1);
        });
        node(&mut d1, "test", &mut NoReporter, |mut array| {
            assert_eq!(
                array.deserialize::<Vec<Entry>>().unwrap(),
                vec![Entry::new(Some(1), None)]
            );
        });

        undo(&mut mgr, &mut d1);

        node(&mut d1, "test", &mut NoReporter, |mut array| {
            assert_eq!(
                array.deserialize::<Vec<Entry>>().unwrap(),
                vec![Entry::new(None, None)]
            );
        });

        undo(&mut mgr, &mut d1);

        node(&mut d1, "test", &mut NoReporter, |mut array| {
            assert_eq!(
                array.deserialize::<Vec<i32>>().unwrap(),
                vec![2, 3, 4, 5, 6]
            );
        });

        redo(&mut mgr, &mut d1);

        node(&mut d1, "test", &mut NoReporter, |mut array| {
            assert_eq!(
                array.deserialize::<Vec<Entry>>().unwrap(),
                vec![Entry::new(None, None)]
            );
        });

        redo(&mut mgr, &mut d1);
        node(&mut d1, "test", &mut NoReporter, |array| {
            assert_eq!(
                array.deserialize::<Vec<Entry>>().unwrap(),
                vec![Entry::new(Some(1), None)]
            );
        });

        exchange_updates(&mut [&mut d1, &mut d2]);

        let map2 = node(&mut d2, "test", &mut NoReporter, |array| {
            array.get(0).unwrap().node_id().unwrap()
        });
        node(&mut d2, map2, &mut NoReporter, |mut map| {
            map.insert_attr("b", 2);
        });

        exchange_updates(&mut [&mut d1, &mut d2]);

        node(&mut d1, "test", &mut NoReporter, |array| {
            assert_eq!(
                array.deserialize::<Vec<Entry>>().unwrap(),
                vec![Entry::new(Some(1), Some(2))]
            );
        });

        undo(&mut mgr, &mut d1);
        node(&mut d1, "test", &mut NoReporter, |array| {
            assert_eq!(
                array.deserialize::<Vec<Entry>>().unwrap(),
                vec![Entry::new(None, Some(2))]
            );
        });

        undo(&mut mgr, &mut d1);
        node(&mut d1, "test", &mut NoReporter, |array| {
            assert_eq!(
                array.deserialize::<Vec<i32>>().unwrap(),
                vec![2, 3, 4, 5, 6]
            );
        });

        redo(&mut mgr, &mut d1);
        node(&mut d1, "test", &mut NoReporter, |array| {
            assert_eq!(
                array.deserialize::<Vec<Entry>>().unwrap(),
                vec![Entry::new(None, Some(2))]
            );
        });

        redo(&mut mgr, &mut d1);
        node(&mut d1, "test", &mut NoReporter, |array| {
            assert_eq!(
                array.deserialize::<Vec<Entry>>().unwrap(),
                vec![Entry::new(Some(1), Some(2))]
            );
        });
    }

    #[test]
    fn undo_xml() {
        let mut d1 = Doc::with_client_id(1);
        let xml1 = node(&mut d1, "xml", &mut NoReporter, |mut frag| {
            frag.insert(0, Delta::with_name("undefined"))
                .node_id()
                .unwrap()
        });

        let mut mgr = UndoManager::new();
        node(&mut d1, xml1.clone(), &mut NoReporter, |n| {
            mgr.expand_scope(&n)
        });
        let child = node(&mut d1, xml1.clone(), &mut mgr, |mut xml| {
            xml.insert(0, Delta::with_name("p")).node_id().unwrap()
        });
        let text_child = node(&mut d1, child, &mut mgr, |mut child| {
            child
                .insert(0, Delta::new().insert_text("content"))
                .node_id()
                .unwrap()
        });

        let as_string = |d1: &mut Doc| {
            node(d1, xml1.clone(), &mut NoReporter, |xml| {
                xml.format_xml().to_string()
            })
        };

        assert_eq!(as_string(&mut d1), "<undefined><p>content</p></undefined>");
        // format textchild and revert that change
        mgr.capture(());
        node(&mut d1, text_child, &mut mgr, |mut text| {
            text.format(3, 4, &Attrs::from([("bold".into(), any!({}))]))
        });
        assert_eq!(
            as_string(&mut d1),
            "<undefined><p>con<bold>tent</bold></p></undefined>"
        );
        undo(&mut mgr, &mut d1);
        assert_eq!(as_string(&mut d1), "<undefined><p>content</p></undefined>");
        redo(&mut mgr, &mut d1);
        assert_eq!(
            as_string(&mut d1),
            "<undefined><p>con<bold>tent</bold></p></undefined>"
        );
        node(&mut d1, xml1.clone(), &mut mgr, |mut xml| xml.remove(0, 1));
        assert_eq!(as_string(&mut d1), "<undefined></undefined>");
        undo(&mut mgr, &mut d1);
        assert_eq!(
            as_string(&mut d1),
            "<undefined><p>con<bold>tent</bold></p></undefined>"
        );
    }

    #[test]
    fn undo_events() {
        use crate::undo::UndoManager;
        type Metadata = HashMap<String, usize>;

        let mut doc = Doc::with_client_id(1);
        let txt = NodePtr::from(doc.transact_mut().node_mut("test").unwrap().as_ref());
        let mut mgr: UndoManager<Metadata> = UndoManager::new();
        node(&mut doc, "test", &mut NoReporter, |n| mgr.expand_scope(&n));

        let mut r = Report::new(EncoderV1::new());
        node(&mut doc, "test", &mut (&mut mgr, &mut r), |mut n| {
            n.insert_text(0, "abc")
        });
        assert!(r.parents.contains(&txt));
        assert!(mgr.capture(Metadata::from([("test".to_string(), 1)])));

        let mut txn = doc.transact_mut();
        let item = mgr.undo(&mut txn).unwrap().unwrap();
        txn.commit(&mut r);
        assert!(r.parents.contains(&txt));
        assert_eq!(item.meta().get("test"), Some(&1));
        let mut txn = doc.transact_mut();
        let item = mgr.redo(&mut txn).unwrap().unwrap();
        txn.commit(&mut r);
        assert!(r.parents.contains(&txt));
        // was 2: meta was set on redo stack items by observers, now they get `M::default()`
        assert_eq!(item.meta().get("test"), None);
    }

    #[test]
    fn undo_stack_serialization() {
        use crate::undo::UndoManager;
        type Metadata = HashMap<String, usize>;
        let (undo_stack_json, redo_stack_json, state) = {
            let mut d1 = Doc::with_options(crate::Options {
                client_id: ClientID::new(1),
                guid: "A".into(),
                ..crate::Options::default()
            });
            let mut m1: UndoManager<Metadata> = UndoManager::new();
            node(&mut d1, "test", &mut NoReporter, |n| m1.expand_scope(&n));

            let meta = || Metadata::from([("test".to_string(), 0)]);

            node(&mut d1, "test", &mut m1, |mut n| n.insert_text(0, "c"));
            m1.capture(meta());
            node(&mut d1, "test", &mut m1, |mut n| n.insert_text(0, "b"));
            m1.capture(meta());
            node(&mut d1, "test", &mut m1, |mut n| n.insert_text(0, "a"));
            m1.capture(meta());

            undo(&mut m1, &mut d1);
            assert_eq!(d1.transact().node("test").unwrap().to_string(), "bc");
            redo(&mut m1, &mut d1);
            assert_eq!(d1.transact().node("test").unwrap().to_string(), "abc");

            let undo_stack_json = serde_json::to_string(m1.undo_stack()).unwrap();
            let redo_stack_json = serde_json::to_string(m1.redo_stack()).unwrap();
            let doc_state = d1.transact().encode_diff_v1(&StateVector::default());
            (undo_stack_json, redo_stack_json, doc_state)
        };

        let undo_stack: Vec<StackItem<Metadata>> = serde_json::from_str(&undo_stack_json).unwrap();
        let redo_stack: Vec<StackItem<Metadata>> = serde_json::from_str(&redo_stack_json).unwrap();

        // try to recreate the stack
        let mut d2 = Doc::with_options(crate::Options {
            client_id: ClientID::new(2),
            guid: "A".into(),
            ..crate::Options::default()
        });
        d2.transact_mut()
            .apply_update(Update::decode_v1(&state).unwrap())
            .unwrap();
        let mut m2: UndoManager<Metadata> = UndoManager::new();
        *m2.undo_stack_mut() = undo_stack;
        *m2.redo_stack_mut() = redo_stack;
        node(&mut d2, "test", &mut NoReporter, |n| m2.expand_scope(&n));

        undo(&mut m2, &mut d2);
        assert_eq!(d2.transact().node("test").unwrap().to_string(), "bc");
        redo(&mut m2, &mut d2);
        assert_eq!(d2.transact().node("test").unwrap().to_string(), "abc");
    }

    #[test]
    fn undo_until_change_performed() {
        let mut d1 = Doc::with_client_id(1);
        let mut d2 = Doc::with_client_id(2);

        let map1a = node(&mut d1, "array", &mut NoReporter, |mut arr| {
            arr.push_back(Delta::new().insert_attr("hello", "world".to_string()))
                .node_id()
                .unwrap()
        });
        let map1b = node(&mut d1, "array", &mut NoReporter, |mut arr| {
            arr.push_back(Delta::new().insert_attr("key", "value".to_string()))
                .node_id()
                .unwrap()
        });

        exchange_updates(&mut [&mut d1, &mut d2]);

        let mut mgr1 = UndoManager::new();
        node(&mut d1, "array", &mut NoReporter, |n| mgr1.expand_scope(&n));
        let d1_client_id = d1.client_id();

        let mut mgr2 = UndoManager::new();
        node(&mut d2, "array", &mut NoReporter, |n| mgr2.expand_scope(&n));
        let d2_client_id = d2.client_id();

        node_with(
            &mut d1,
            d1_client_id,
            map1b.clone(),
            &mut mgr1,
            |mut map| {
                map.insert_attr("key", "value modified".to_string());
            },
        );

        exchange_updates(&mut [&mut d1, &mut d2]);
        mgr1.capture(());

        node_with(&mut d1, d1_client_id, map1a, &mut mgr1, |mut map| {
            map.insert_attr("hello", "world modified".to_string());
        });

        exchange_updates(&mut [&mut d1, &mut d2]);

        node_with(&mut d2, d2_client_id, "array", &mut mgr2, |mut arr| {
            arr.remove(0, 1)
        });

        exchange_updates(&mut [&mut d1, &mut d2]);
        undo(&mut mgr2, &mut d2);

        exchange_updates(&mut [&mut d1, &mut d2]);
        undo(&mut mgr1, &mut d1);
        exchange_updates(&mut [&mut d1, &mut d2]);

        node(&mut d1, map1b, &mut NoReporter, |map| {
            assert_eq!(map.attr("key"), Some("value".into()));
        });
    }

    /// `{ text?: { blocks: { text } } }` shape used by [nested_undo] and [undo_block_bug].
    #[derive(Debug, Deserialize, PartialEq)]
    struct Design {
        text: Option<DesignText>,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    struct DesignText {
        blocks: Block,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    struct Block {
        text: String,
    }

    fn design(text: &str) -> Design {
        Design {
            text: Some(DesignText {
                blocks: Block { text: text.into() },
            }),
        }
    }

    #[test]
    fn nested_undo() {
        // This issue has been reported in https://github.com/yjs/yjs/issues/317
        let mut doc = Doc::with_options(crate::doc::Options {
            skip_gc: true,
            client_id: ClientID::new(1),
            ..crate::doc::Options::default()
        });
        let mut mgr = UndoManager::new();
        node(&mut doc, "map", &mut NoReporter, |n| mgr.expand_scope(&n));

        let blocks = |text: &str| Delta::new().insert_attr("text", text.to_owned());
        let text = node(&mut doc, "map", &mut mgr, |mut design| {
            let content = Delta::new().insert_attr("blocks", blocks("Type something"));
            design.insert_attr("text", content).node_id().unwrap()
        });
        mgr.capture(());

        node(&mut doc, text.clone(), &mut mgr, |mut text| {
            text.insert_attr("blocks", blocks("Something"));
        });
        mgr.capture(());

        node(&mut doc, text, &mut mgr, |mut text| {
            text.insert_attr("blocks", blocks("Something else"));
        });
        mgr.capture(());

        let design_json = |doc: &mut Doc| {
            node(doc, "map", &mut NoReporter, |design| {
                design.deserialize::<Design>().unwrap()
            })
        };

        assert_eq!(design_json(&mut doc), design("Something else"));
        undo(&mut mgr, &mut doc);
        assert_eq!(design_json(&mut doc), design("Something"));
        undo(&mut mgr, &mut doc);
        assert_eq!(design_json(&mut doc), design("Type something"));
        undo(&mut mgr, &mut doc);
        assert_eq!(design_json(&mut doc), Design { text: None });
        redo(&mut mgr, &mut doc);
        assert_eq!(design_json(&mut doc), design("Type something"));
        redo(&mut mgr, &mut doc);
        assert_eq!(design_json(&mut doc), design("Something"));
        redo(&mut mgr, &mut doc);
        assert_eq!(design_json(&mut doc), design("Something else"));
    }

    #[test]
    fn consecutive_redo_bug() {
        // https://github.com/yjs/yjs/issues/355
        let mut doc = Doc::with_client_id(1);
        let mut mgr = UndoManager::new();
        node(&mut doc, "root", &mut NoReporter, |n| mgr.expand_scope(&n));

        let point = node(&mut doc, "root", &mut mgr, |mut root| {
            let content = Delta::new().insert_attr("x", 0).insert_attr("y", 0);
            root.insert_attr("a", content).node_id().unwrap()
        });
        mgr.capture(());

        let mut set_point = |mgr: &mut UndoManager, x: i32, y: i32| {
            node(&mut doc, point.clone(), mgr, |mut point| {
                point.insert_attr("x", x)
            });
            node(&mut doc, point.clone(), mgr, |mut point| {
                point.insert_attr("y", y)
            });
        };

        set_point(&mut mgr, 100, 100);
        mgr.capture(());
        set_point(&mut mgr, 200, 200);
        mgr.capture(());
        set_point(&mut mgr, 300, 300);
        mgr.capture(());

        #[derive(Debug, Deserialize, PartialEq)]
        struct Point {
            x: i32,
            y: i32,
        }
        #[derive(Debug, Deserialize, PartialEq)]
        struct R {
            a: Point,
        }
        // read through root: redo restores the point as a new node
        let point_json = |doc: &mut Doc| {
            node(doc, "root", &mut NoReporter, |r| {
                r.deserialize::<R>().unwrap().a
            })
        };

        assert_eq!(point_json(&mut doc), Point { x: 300, y: 300 });

        undo(&mut mgr, &mut doc); // x=200, y=200
        assert_eq!(point_json(&mut doc), Point { x: 200, y: 200 });

        undo(&mut mgr, &mut doc); // x=100, y=100
        assert_eq!(point_json(&mut doc), Point { x: 100, y: 100 });

        undo(&mut mgr, &mut doc); // x=0, y=0
        assert_eq!(point_json(&mut doc), Point { x: 0, y: 0 });

        undo(&mut mgr, &mut doc); // null
        node(&mut doc, "root", &mut NoReporter, |root| {
            assert_eq!(root.attr("a"), None)
        });

        redo(&mut mgr, &mut doc); // x=0, y=0
        assert_eq!(point_json(&mut doc), Point { x: 0, y: 0 });

        redo(&mut mgr, &mut doc); // x=100, y=100
        assert_eq!(point_json(&mut doc), Point { x: 100, y: 100 });

        redo(&mut mgr, &mut doc); // x=200, y=200
        assert_eq!(point_json(&mut doc), Point { x: 200, y: 200 });

        redo(&mut mgr, &mut doc); // x=300, y=300
        assert_eq!(point_json(&mut doc), Point { x: 300, y: 300 });
    }

    #[test]
    fn undo_xml_bug() {
        // https://github.com/yjs/yjs/issues/304
        const ORIGIN: &str = "origin";
        let mut doc = Doc::with_client_id(1);
        let mut mgr = UndoManager::new();
        node(&mut doc, "t", &mut NoReporter, |n| mgr.expand_scope(&n));

        // create element
        let e = node_with(&mut doc, ORIGIN, "t", &mut mgr, |mut f| {
            let content = Delta::with_name("test-node")
                .insert_attr("a", "100".to_string())
                .insert_attr("b", "0".to_string());
            f.insert(0, content).node_id().unwrap()
        });
        mgr.capture(());

        // change one attribute
        node_with(&mut doc, ORIGIN, e.clone(), &mut mgr, |mut e| {
            e.insert_attr("a", "200".to_string());
        });
        mgr.capture(());

        // change both attributes
        node_with(&mut doc, ORIGIN, e, &mut mgr, |mut e| {
            e.insert_attr("a", "180".to_string());
            e.insert_attr("b", "50".to_string());
        });
        mgr.capture(());

        undo(&mut mgr, &mut doc);
        undo(&mut mgr, &mut doc);
        undo(&mut mgr, &mut doc);

        redo(&mut mgr, &mut doc);
        redo(&mut mgr, &mut doc);
        redo(&mut mgr, &mut doc);

        let str = node(&mut doc, "t", &mut NoReporter, |f| {
            f.format_xml().to_string()
        });
        assert!(
            str == r#"<test-node a="180" b="50"></test-node>"#
                || str == r#"<test-node b="50" a="180"></test-node>"#
        );
    }

    #[test]
    fn undo_block_bug() {
        // https://github.com/yjs/yjs/issues/343
        let mut doc = Doc::with_options({
            let mut o = crate::doc::Options::default();
            o.client_id = ClientID::new(1);
            o.skip_gc = true;
            o
        });
        let mut mgr = UndoManager::new();
        node(&mut doc, "map", &mut NoReporter, |n| mgr.expand_scope(&n));

        let blocks = |text: &str| Delta::new().insert_attr("text", text.to_owned());
        let text = node(&mut doc, "map", &mut mgr, |mut design| {
            let content = Delta::new().insert_attr("blocks", blocks("1"));
            design.insert_attr("text", content).node_id().unwrap()
        });
        mgr.capture(());
        for content in ["1", "3", "4"] {
            node(&mut doc, text.clone(), &mut mgr, |mut text| {
                text.insert_attr("blocks", blocks(content));
            });
            mgr.capture(());
        }
        // {"text":{"blocks":{"text":"4"}}}
        undo(&mut mgr, &mut doc); // {"text":{"blocks":{"3"}}}
        undo(&mut mgr, &mut doc); // {"text":{"blocks":{"text":"2"}}}
        undo(&mut mgr, &mut doc); // {"text":{"blocks":{"text":"1"}}}
        undo(&mut mgr, &mut doc); // {}
        redo(&mut mgr, &mut doc); // {"text":{"blocks":{"text":"1"}}}
        redo(&mut mgr, &mut doc); // {"text":{"blocks":{"text":"2"}}}
        redo(&mut mgr, &mut doc); // {"text":{"blocks":{"text":"3"}}}
        redo(&mut mgr, &mut doc); // {"text":{}}
        let actual = node(&mut doc, "map", &mut NoReporter, |design| {
            design.deserialize::<Design>().unwrap()
        });
        assert_eq!(actual, design("4"));
    }

    #[test]
    fn undo_delete_text_format() {
        // https://github.com/yrs/yjs/issues/392
        fn send(src: &Doc, dst: &mut Doc) {
            let update = Update::decode_v1(
                &src.transact()
                    .encode_state_as_update_v1(&StateVector::default()),
            )
            .unwrap();
            dst.transact_mut().apply_update(update).unwrap();
        }

        let mut doc1 = Doc::with_client_id(1);
        node(&mut doc1, "test", &mut NoReporter, |mut n| {
            n.insert_text(0, "Attack ships on fire off the shoulder of Orion.")
        }); // D1: 'Attack ships on fire off the shoulder of Orion.'
        let mut doc2 = Doc::with_client_id(2);

        send(&doc1, &mut doc2); // D2: 'Attack ships on fire off the shoulder of Orion.'
        let mut mgr = UndoManager::new();
        node(&mut doc1, "test", &mut NoReporter, |n| mgr.expand_scope(&n));

        let attrs = Attrs::from([("bold".into(), true.into())]);
        node(&mut doc1, "test", &mut mgr, |mut n| n.format(13, 7, &attrs)); // D1: 'Attack ships <b>on fire</b> off the shoulder of Orion.'

        mgr.capture(());

        send(&doc1, &mut doc2); // D2: 'Attack ships <b>on fire</b> off the shoulder of Orion.'

        let attrs2 = Attrs::from([("bold".into(), Any::Null)]);
        node(&mut doc1, "test", &mut mgr, |mut n| {
            n.format(16, 4, &attrs2)
        }); // D1: 'Attack ships <b>on </b>fire off the shoulder of Orion.'

        // TODO(unified-api): rich-text `diff`/`Diff`/`YChange` assertions have no NodeRef
        // equivalent yet (`to_delta` is not implemented). Original expected the delta:
        //   ["Attack ships ", <b>"on "</b>, "fire off the shoulder of Orion."]

        mgr.capture(());
        send(&doc1, &mut doc2); // D2: 'Attack ships <b>on </b>fire off the shoulder of Orion.'

        undo(&mut mgr, &mut doc1); // D1: 'Attack ships <b>on fire</b> off the shoulder of Orion.'
        send(&doc1, &mut doc2); // D2: 'Attack ships <b>on fire</b> off the shoulder of Orion.'

        // TODO(unified-api): rich-text `diff`/`Diff`/`YChange` assertions have no NodeRef
        // equivalent yet. Original expected on both docs the delta:
        //   ["Attack ships ", <b>"on fire"</b>, " off the shoulder of Orion."]
    }

    #[test]
    fn special_deletion_case() {
        // https://github.com/yjs/yjs/issues/447
        let mut doc = Doc::with_client_id(1);
        let mut mgr = UndoManager::new();
        node(&mut doc, "test", &mut NoReporter, |n| mgr.expand_scope(&n));

        // setup is not undoable
        let e = node(&mut doc, "test", &mut NoReporter, |mut f| {
            let content = Delta::with_name("test")
                .insert_attr("a", "1".to_string())
                .insert_attr("b", "2".to_string());
            f.insert(0, content).node_id().unwrap()
        });
        let as_string =
            |doc: &mut Doc| node(doc, "test", &mut NoReporter, |f| f.format_xml().to_string());

        let s = as_string(&mut doc);
        assert!(s == r#"<test a="1" b="2"></test>"# || s == r#"<test b="2" a="1"></test>"#);

        {
            // change attribute "b" and delete test-node within a single transaction
            let mut txn = doc.transact_mut();
            txn.node_mut(e).unwrap().insert_attr("b", "3".to_string());
            txn.node_mut("test").unwrap().remove(0, 1);
            txn.commit(&mut mgr);
        }
        assert_eq!(as_string(&mut doc), "");

        undo(&mut mgr, &mut doc);
        let s = as_string(&mut doc);
        assert!(s == r#"<test a="1" b="2"></test>"# || s == r#"<test b="2" a="1"></test>"#);
    }

    #[test]
    fn undo_in_embed() {
        let mut d1 = Doc::with_client_id(1);
        let mut mgr = UndoManager::new();
        node(&mut d1, "test", &mut NoReporter, |n| mgr.expand_scope(&n));

        let mut d2 = Doc::with_client_id(2);

        let attrs = Attrs::from([("bold".into(), true.into())]);
        let nested = node(&mut d1, "test", &mut mgr, |mut txt1| {
            let embed = Delta::new().insert_text("initial text");
            txt1.apply_delta(Delta::new().insert_fmt(embed, &attrs));
            txt1.get(0).unwrap().node_id().unwrap()
        });
        let nested_string = |d1: &mut Doc| {
            node(d1, nested.clone(), &mut NoReporter, |nested| {
                nested.to_string()
            })
        };

        assert_eq!(nested_string(&mut d1), "initial text".to_string());
        mgr.capture(());
        node(&mut d1, nested.clone(), &mut mgr, |mut nested| {
            let len = nested.len();
            nested.remove(0, len);
        });
        node(&mut d1, nested.clone(), &mut mgr, |mut nested| {
            nested.insert_text(0, "other text")
        });
        assert_eq!(nested_string(&mut d1), "other text".to_string());
        undo(&mut mgr, &mut d1);
        assert_eq!(nested_string(&mut d1), "initial text".to_string());

        exchange_updates(&mut [&mut d1, &mut d2]);

        let nested2 = node(&mut d2, "test", &mut NoReporter, |txt2| {
            txt2.get(0).unwrap().node_id().unwrap()
        });
        assert_eq!(
            node(&mut d2, nested2, &mut NoReporter, |nested2| {
                nested2.to_string()
            }),
            "initial text".to_string()
        );

        undo(&mut mgr, &mut d1);
        assert_eq!(node(&mut d1, "test", &mut NoReporter, |txt1| txt1.len()), 0);
    }

    /// `{ s1: T }` root shape shared by github issue regression tests.
    #[derive(Debug, Deserialize, PartialEq)]
    struct Root<T> {
        s1: T,
    }

    #[test]
    fn github_issue_345() {
        // https://github.com/y-crdt/y-crdt/issues/345
        let mut doc = Doc::new();
        let mut mgr = UndoManager::new();
        node(&mut doc, "r", &mut NoReporter, |n| mgr.expand_scope(&n));
        let client_id = doc.client_id();

        let s1 = node_with(&mut doc, client_id, "r", &mut mgr, |mut map| {
            map.insert_attr("s1", Delta::new()).node_id().unwrap()
        });
        mgr.capture(());

        let a = node_with(&mut doc, client_id, s1.clone(), &mut mgr, |mut s1| {
            s1.insert_attr("a", Delta::new().insert("a1".to_string()))
                .node_id()
                .unwrap()
        });
        mgr.capture(());

        let b = node_with(&mut doc, client_id, s1, &mut mgr, |mut s1| {
            s1.insert_attr("b", Delta::new().insert("b1".to_string()))
                .node_id()
                .unwrap()
        });
        mgr.capture(());

        node_with(&mut doc, client_id, b, &mut mgr, |mut b| {
            b.insert(1, "b2".to_string())
        });
        mgr.capture(());

        for (index, value) in [(1, "a3"), (2, "a4"), (3, "a5")] {
            node_with(&mut doc, client_id, a.clone(), &mut mgr, |mut a| {
                a.insert(index, value.to_string())
            });
            mgr.capture(());
        }

        #[derive(Debug, Deserialize, PartialEq)]
        struct S1 {
            a: Vec<String>,
            b: Vec<String>,
        }
        let s1 = |a: &[&str], b: &[&str]| S1 {
            a: a.iter().map(|s| s.to_string()).collect(),
            b: b.iter().map(|s| s.to_string()).collect(),
        };
        let map_json = |doc: &mut Doc| {
            node(doc, "r", &mut NoReporter, |map| {
                map.deserialize::<Root<S1>>().unwrap().s1
            })
        };

        assert_eq!(
            map_json(&mut doc),
            s1(&["a1", "a3", "a4", "a5"], &["b1", "b2"])
        );

        undo(&mut mgr, &mut doc); // {"s1": {"a": ["a1", "a3", "a4"], "b": ["b1", "b2"]}}
        undo(&mut mgr, &mut doc); // {"s1": {"a": ["a1", "a3"], "b": ["b1", "b2"]}}
        undo(&mut mgr, &mut doc); // {"s1": {"a": ["a1"], "b": ["b1", "b2"]}}
        undo(&mut mgr, &mut doc); // {"s1": {"a": ["a1"], "b": ["b1"]}}
        assert_eq!(map_json(&mut doc), s1(&["a1"], &["b1"]));

        redo(&mut mgr, &mut doc);
        assert_eq!(map_json(&mut doc), s1(&["a1"], &["b1", "b2"]));

        redo(&mut mgr, &mut doc);
        assert_eq!(map_json(&mut doc), s1(&["a1", "a3"], &["b1", "b2"]));

        redo(&mut mgr, &mut doc);
        assert_eq!(map_json(&mut doc), s1(&["a1", "a3", "a4"], &["b1", "b2"]));

        redo(&mut mgr, &mut doc);
        assert_eq!(
            map_json(&mut doc),
            s1(&["a1", "a3", "a4", "a5"], &["b1", "b2"])
        );
    }

    #[test]
    fn github_issue_345_part_2() {
        // https://github.com/y-crdt/y-crdt/issues/345
        let mut d = Doc::new();
        let s1 = node(&mut d, "r", &mut NoReporter, |mut r| {
            r.insert_attr("s1", Delta::new()).node_id().unwrap()
        });

        let mut mgr = UndoManager::new();
        node(&mut d, "r", &mut NoReporter, |n| mgr.expand_scope(&n));
        node(&mut d, s1.clone(), &mut mgr, |mut s1| {
            s1.insert_attr("b1", Delta::new().insert_attr("f1", 11));
        });
        mgr.capture(());

        node(&mut d, s1.clone(), &mut mgr, |mut s1| s1.remove_attr("b1"));
        mgr.capture(());

        node(&mut d, s1, &mut mgr, |mut s1| {
            s1.insert_attr("b1", Delta::new().insert_attr("f1", 20));
        });
        mgr.capture(());

        #[derive(Debug, Deserialize, PartialEq)]
        struct S1 {
            b1: Option<B1>,
        }
        #[derive(Debug, Deserialize, PartialEq)]
        struct B1 {
            f1: i32,
        }
        let r_json = |d: &mut Doc| {
            node(d, "r", &mut NoReporter, |r| {
                r.deserialize::<Root<S1>>().unwrap().s1.b1
            })
        };

        assert_eq!(r_json(&mut d), Some(B1 { f1: 20 }));

        undo(&mut mgr, &mut d);
        assert_eq!(r_json(&mut d), None);
        assert!(mgr.can_undo(), "should be able to undo");

        undo(&mut mgr, &mut d);
        assert_eq!(r_json(&mut d), Some(B1 { f1: 11 }));
        assert!(mgr.can_undo(), "should be able to undo to the init state");
    }

    #[test]
    fn issue_371() {
        let mut doc = Doc::with_client_id(1);

        let s1 = node(&mut doc, "r", &mut NoReporter, |mut r| {
            r.insert_attr("s1", Delta::new()).node_id().unwrap()
        }); // { s1: {} }
        let b1_arr = node(&mut doc, s1, &mut NoReporter, |mut s1| {
            s1.insert_attr("b1", Delta::new()).node_id().unwrap()
        }); // { s1: { b1: [] } }
        let el1 = node(&mut doc, b1_arr.clone(), &mut NoReporter, |mut b1_arr| {
            b1_arr.insert(0, Delta::new()).node_id().unwrap()
        }); // { s1: { b1: [{}] } }
        node(&mut doc, el1.clone(), &mut NoReporter, |mut el1| {
            el1.insert_attr("f1", 8)
        }); // { s1: { b1: [{ f1: 8 }] } }
        node(&mut doc, el1.clone(), &mut NoReporter, |mut el1| {
            el1.insert_attr("f2", true)
        }); // { s1: { b1: [{ f1: 8, f2: true }] } }

        let mut mgr = UndoManager::new();
        node(&mut doc, "r", &mut NoReporter, |n| mgr.expand_scope(&n));
        {
            let mut txn = doc.transact_mut();
            // { s1: { b1: [{ f1: 8, f2: false }, { f1: 8, f2: true }] } }
            let el0 = Delta::new().insert_attr("f1", 8).insert_attr("f2", false);
            txn.node_mut(b1_arr).unwrap().insert(0, el0);

            let mut el1 = txn.node_mut(el1.clone()).unwrap();
            el1.insert_attr("f1", 13); // { s1: { b1: [{ f1: 8, f2: false }, { f1: 13, f2: true }] } }
            el1.remove_attr("f2"); // { s1: { b1: [{ f1: 8, f2: false }, { f1: 13 }] } }
            txn.commit(&mut mgr);
        }
        mgr.capture(());

        // { s1: { b1: [{ f1: 8, f2: false }, { f1: 13, f2: false }] } }
        node(&mut doc, el1.clone(), &mut mgr, |mut el1| {
            el1.insert_attr("f2", false)
        });
        mgr.capture(());

        // { s1: { b1: [{ f1: 8, f2: false }, { f1: 13, f2: true }] } }
        node(&mut doc, el1, &mut mgr, |mut el1| {
            el1.insert_attr("f2", true)
        });
        mgr.capture(());

        #[derive(Debug, Deserialize, PartialEq)]
        struct S1 {
            b1: Vec<El>,
        }
        #[derive(Debug, Deserialize, PartialEq)]
        struct El {
            f1: i32,
            f2: Option<bool>,
        }
        let el = |f1, f2| El { f1, f2 };
        let r_json = |doc: &mut Doc| {
            node(doc, "r", &mut NoReporter, |r| {
                r.deserialize::<Root<S1>>().unwrap().s1.b1
            })
        };

        undo(&mut mgr, &mut doc); // { s1: { b1: [{ f1: 8, f2: false }, { f1: 13, f2: false }] } }
        assert_eq!(r_json(&mut doc), [el(8, Some(false)), el(13, Some(false))]);
        undo(&mut mgr, &mut doc); // { s1: { b1: [{ f1: 8, f2: false }, { f1: 13 }] } }
        assert_eq!(r_json(&mut doc), [el(8, Some(false)), el(13, None)]);
        undo(&mut mgr, &mut doc); // { s1: { b1: [{ f1: 8, f2: true }] } }
        assert_eq!(r_json(&mut doc), [el(8, Some(true))]);
        assert!(!undo(&mut mgr, &mut doc).is_some()); // no more changes tracked by undo manager
    }

    #[test]
    fn issue_371_2() {
        let mut doc = Doc::with_client_id(1);
        let s1 = node(&mut doc, "r", &mut NoReporter, |mut r| {
            r.insert_attr("s1", Delta::new()).node_id().unwrap()
        }); // { s1:{} }
        node(&mut doc, s1.clone(), &mut NoReporter, |mut s1| {
            s1.insert_attr("f2", "AAA".to_string())
        }); // { s1: { f2: AAA } }
        node(&mut doc, s1.clone(), &mut NoReporter, |mut s1| {
            s1.insert_attr("f1", false)
        }); // { s1: { f1: false, f2: AAA } }

        let mut mgr = UndoManager::new();
        node(&mut doc, "r", &mut NoReporter, |n| mgr.expand_scope(&n));
        node(&mut doc, s1.clone(), &mut mgr, |mut s1| {
            s1.remove_attr("f2")
        }); // { s1: { f1: false } }
        mgr.capture(());

        node(&mut doc, s1.clone(), &mut mgr, |mut s1| {
            s1.insert_attr("f2", "C1".to_string())
        }); // { s1: { f1: false, f2: C1 } }
        mgr.capture(());

        node(&mut doc, s1, &mut mgr, |mut s1| {
            s1.insert_attr("f2", "C2".to_string())
        }); // { s1: { f1: false, f2: C2 } }
        mgr.capture(());

        #[derive(Debug, Deserialize, PartialEq)]
        struct S1 {
            f1: bool,
            f2: Option<String>,
        }
        let s1 = |f2: Option<&str>| S1 {
            f1: false,
            f2: f2.map(String::from),
        };
        let r_json = |doc: &mut Doc| {
            node(doc, "r", &mut NoReporter, |r| {
                r.deserialize::<Root<S1>>().unwrap().s1
            })
        };

        undo(&mut mgr, &mut doc); // { s1: { f1: false, f2: C1 } }
        assert_eq!(r_json(&mut doc), s1(Some("C1")));
        undo(&mut mgr, &mut doc); // { s1: { f1: false } }
        assert_eq!(r_json(&mut doc), s1(None));
        undo(&mut mgr, &mut doc); // { s1: { f1: false, f2: AAA } }
        assert_eq!(r_json(&mut doc), s1(Some("AAA")));
        assert!(!undo(&mut mgr, &mut doc).is_some()); // no more changes tracked by undo manager
    }

    #[test]
    fn issue_380() {
        let mut d = Doc::with_client_id(1);
        let s1 = node(&mut d, "r", &mut NoReporter, |mut r| {
            r.insert_attr("s1", Delta::new()).node_id().unwrap()
        }); // {r:{s1:{}}
        let b1_arr = node(&mut d, s1, &mut NoReporter, |mut s1| {
            s1.insert_attr("b1", Delta::new()).node_id().unwrap()
        }); // {r:{s1:{b1:[]}}

        let b1_el1 = node(&mut d, b1_arr.clone(), &mut NoReporter, |mut b1_arr| {
            b1_arr.insert(0, Delta::new()).node_id().unwrap()
        }); // {r:{s1:{b1:[{}]}}
        let b2_arr = node(&mut d, b1_el1, &mut NoReporter, |mut b1_el1| {
            b1_el1.insert_attr("b2", Delta::new()).node_id().unwrap()
        }); // {r:{s1:{b1:[{b2:[]}]}}
        let b2_arr_nest = node(&mut d, b2_arr, &mut NoReporter, |mut b2_arr| {
            b2_arr.insert(0, Delta::new()).node_id().unwrap()
        }); // {r:{s1:{b1:[{b2:[[]]}]}}
        node(&mut d, b2_arr_nest.clone(), &mut NoReporter, |mut nest| {
            nest.insert(0, 232291652)
        }); // {r:{s1:{b1:[{b2:[[232291652]]}]}}
        node(&mut d, b2_arr_nest.clone(), &mut NoReporter, |mut nest| {
            nest.insert(1, -30)
        }); // {r:{s1:{b1:[{b2:[[232291652, -30]]}]}}

        let mut mgr = UndoManager::new();
        node(&mut d, "r", &mut NoReporter, |n| mgr.expand_scope(&n));

        node(&mut d, b2_arr_nest, &mut mgr, |mut nest| {
            nest.remove(1, 1); // {r:{s1:{b1:[{b2:[[232291652]]}]}}
            nest.insert(1, -5); // {r:{s1:{b1:[{b2:[[232291652, -5]]}]}}
        });
        mgr.capture(());

        {
            let mut txn = d.transact_mut();

            // {r:{s1:{b1:[{b2:[[232291652, -6]]},{b2:[[232291652, -5]]}]}}
            let b2_0_arr_nest = Delta::new().insert(232291652).insert(-6);
            let b2_0_arr = Delta::new().insert(b2_0_arr_nest);
            let b1_el0 = Delta::new().insert_attr("b2", b2_0_arr);
            txn.node_mut(b1_arr.clone()).unwrap().insert(0, b1_el0);
            txn.node_mut(b1_arr.clone()).unwrap().remove(1, 1); // {r:{s1:{b1:[{b2:[[232291652, -6]]}]}}

            // {r:{s1:{b1:[{b2:[[232291652, -6]]}, {f2:C1}]}}
            let b1_el1 = Delta::new().insert_attr("f2", "C1".to_string());
            txn.node_mut(b1_arr).unwrap().insert(1, b1_el1);
            txn.commit(&mut mgr);
        }
        mgr.capture(());

        #[derive(Debug, Deserialize, PartialEq)]
        struct S1 {
            b1: Vec<El>,
        }
        #[derive(Debug, Deserialize, PartialEq)]
        struct El {
            b2: Option<Vec<Vec<i64>>>,
            f2: Option<String>,
        }
        let b2 = |n| El {
            b2: Some(vec![vec![232291652, n]]),
            f2: None,
        };
        let f2 = |s: &str| El {
            b2: None,
            f2: Some(s.into()),
        };
        let r_json = |d: &mut Doc| {
            node(d, "r", &mut NoReporter, |r| {
                r.deserialize::<Root<S1>>().unwrap().s1.b1
            })
        };

        assert_eq!(r_json(&mut d), [b2(-6), f2("C1")]);

        undo(&mut mgr, &mut d); // {r:{s1:{b1:[{b2:[[232291652, -5]]}]}}
        assert_eq!(r_json(&mut d), [b2(-5)]);

        undo(&mut mgr, &mut d); // {r:{s1:{b1:[{b2:[[232291652, -30]]}]}}
        assert_eq!(r_json(&mut d), [b2(-30)]);
    }

    #[test]
    fn multi_doc_undo() {
        let mut um = UndoManager::new();
        let mut d1 = Doc::new();
        let mut d2 = Doc::new();
        node(&mut d1, "text", &mut NoReporter, |n| um.expand_scope(&n));
        node(&mut d2, "text", &mut NoReporter, |n| um.expand_scope(&n));

        node(&mut d1, "text", &mut um, |mut n| n.insert_text(0, "abc"));
        node(&mut d2, "text", &mut um, |mut n| n.insert_text(0, "xyz"));

        assert_eq!(um.undo_stack().len(), 2);
        assert!(um.can_undo(), "should be undoable (1)");
        assert!(!um.can_redo(), "should not be redoable (1)");

        undo(&mut um, &mut d2);

        assert!(um.can_undo(), "should be undoable (2)");
        assert!(um.can_redo(), "should be redoable (2)");
        assert_eq!(d1.transact().node("text").unwrap().to_string(), "abc");
        assert_eq!(d2.transact().node("text").unwrap().to_string(), "");

        undo(&mut um, &mut d1);

        assert!(!um.can_undo(), "should not be undoable (3)");
        assert!(um.can_redo(), "should be redoable (3)");
        assert_eq!(d1.transact().node("text").unwrap().to_string(), "");
        assert_eq!(d2.transact().node("text").unwrap().to_string(), "");

        // shouldn't have any effect
        assert!(
            !undo(&mut um, &mut d1).is_some(),
            "undo should have no effect"
        );
        assert!(!um.can_undo(), "should not be undoable (4)");
        assert!(um.can_redo(), "should be redoable (4)");
        assert_eq!(d1.transact().node("text").unwrap().to_string(), "");
        assert_eq!(d2.transact().node("text").unwrap().to_string(), "");

        redo(&mut um, &mut d1);

        assert!(um.can_undo(), "should be undoable (5)");
        assert!(um.can_redo(), "should be redoable (5)");
        assert_eq!(d1.transact().node("text").unwrap().to_string(), "abc");
        assert_eq!(d2.transact().node("text").unwrap().to_string(), "");

        redo(&mut um, &mut d2);

        assert_eq!(um.undo_stack().len(), 2);
        assert!(um.can_undo(), "should be undoable (6)");
        assert!(!um.can_redo(), "should not be redoable (6)");
        assert_eq!(d1.transact().node("text").unwrap().to_string(), "abc");
        assert_eq!(d2.transact().node("text").unwrap().to_string(), "xyz");
    }

    #[test]
    fn gc_collects_kept_children_of_a_collected_type() {
        // #667, #676: clearing the redo stack un-keeps map "p" though "number", which the undo
        // stack still holds, stays kept. Collecting "p" after a remote removal freed its branch
        // and left "number" pointing into it, so encoding read freed memory.
        let mut d1 = Doc::with_client_id(1);
        let mut d2 = Doc::with_client_id(2);
        let exchange = |d1: &mut Doc, d2: &mut Doc| exchange_updates(&mut [d1, d2]);
        node(&mut d1, "styles", &mut NoReporter, |mut styles| {
            styles.insert_attr(
                "p",
                In::Node(Box::new(Delta::new().insert_attr("kind", "number"))),
            );
        });
        exchange(&mut d1, &mut d2);
        let number = ID::new(ClientID::new(1), 1);

        let mut mgr = UndoManager::new();
        node(&mut d2, "styles", &mut NoReporter, |n| mgr.expand_scope(&n));
        {
            let mut txn = d2.transact_mut();
            let Some(Out::Node(p)) = txn.node("styles").unwrap().attr("p") else {
                panic!("expected a nested map")
            };
            txn.node_mut(p.id).unwrap().insert_attr("kind", "bullet");
            txn.commit(&mut mgr);
        }
        undo(&mut mgr, &mut d2);
        mgr.capture(());
        node(&mut d2, "styles", &mut mgr, |mut n| {
            n.insert_attr("q", In::Node(Box::new(Delta::new()))); // clears the redo stack
        });

        node(&mut d1, "styles", &mut NoReporter, |mut n| {
            n.remove_attr("p");
        });
        exchange(&mut d1, &mut d2);

        // "number" went with its map, as in Yjs.
        assert!(d2.blocks.get_item(&number).is_none());
        d2.transact()
            .encode_state_as_update_v1(&StateVector::default());
        while undo(&mut mgr, &mut d2).is_some() {}
        exchange(&mut d1, &mut d2);
        assert_eq!(
            d1.transact().node("styles").unwrap().to_json(),
            d2.transact().node("styles").unwrap().to_json()
        );
    }

    #[test]
    fn sticky_index_inside_redone_text_keeps_its_offset() {
        use crate::Assoc;

        let mut doc = Doc::with_client_id(1);
        let mut mgr = UndoManager::new();
        node(&mut doc, "test", &mut NoReporter, |n| mgr.expand_scope(&n));
        node(&mut doc, "test", &mut mgr, |mut n| {
            n.insert_text(0, "hello world")
        });
        let pos = doc
            .transact()
            .node("test")
            .unwrap()
            .sticky_index(5, Assoc::After)
            .unwrap();
        let text = |doc: &Doc| doc.transact().node("test").unwrap().to_string();

        undo(&mut mgr, &mut doc);
        assert_eq!(text(&doc), "");
        redo(&mut mgr, &mut doc);
        assert_eq!(text(&doc), "hello world");

        let offset = pos.get_offset(&doc.transact()).unwrap();
        assert_eq!(offset.index, 5, "Yjs resolves the same position to 5");
    }

    fn text(doc: &Doc, name: &str) -> String {
        doc.transact().node(name).unwrap().to_string()
    }

    /// Deletion tracked explicitly and committed without the manager can be undone.
    #[test]
    fn track_without_committing_with_manager() {
        let mut doc = Doc::new();
        node(&mut doc, "text", &mut NoReporter, |mut n| {
            n.insert_text(0, "abc")
        });
        let mut mgr = UndoManager::new();
        node(&mut doc, "text", &mut NoReporter, |n| mgr.expand_scope(&n));
        {
            let mut txn = doc.transact_mut();
            txn.node_mut("text").unwrap().remove(0, 3);
            mgr.track(&txn);
            txn.commit(&mut NoReporter);
        }
        assert!(undo(&mut mgr, &mut doc).is_some());
        assert_eq!(text(&doc, "text"), "abc");
    }

    /// Tracking a transaction and then committing it with the manager pushes a single change.
    #[test]
    fn track_then_commit_with_manager() {
        let mut doc = Doc::new();
        let mut mgr = UndoManager::new();
        node(&mut doc, "text", &mut NoReporter, |n| mgr.expand_scope(&n));
        let mut txn = doc.transact_mut();
        txn.node_mut("text").unwrap().insert_text(0, "abc");
        mgr.track(&txn);
        let expected = txn.insert_set().clone();
        txn.commit(&mut mgr);
        assert_eq!(mgr.undo_stack().len(), 1);
        assert_eq!(mgr.undo_stack()[0].insertions(), &expected);
    }

    /// Undo/redo transactions committed with the manager are not tracked.
    #[test]
    fn undo_committed_with_manager() {
        let mut doc = Doc::new();
        let mut mgr = UndoManager::new();
        node(&mut doc, "text", &mut NoReporter, |n| mgr.expand_scope(&n));
        node(&mut doc, "text", &mut mgr, |mut n| n.insert_text(0, "abc"));
        {
            let mut txn = doc.transact_mut();
            mgr.undo(&mut txn).unwrap().unwrap();
            txn.commit(&mut mgr);
            assert_eq!((mgr.undo_stack().len(), mgr.redo_stack().len()), (0, 1));
            let mut txn = doc.transact_mut();
            mgr.redo(&mut txn).unwrap().unwrap();
            txn.commit(&mut mgr);
            assert_eq!((mgr.undo_stack().len(), mgr.redo_stack().len()), (1, 0));
        }
        assert_eq!(text(&doc, "text"), "abc");
    }

    /// Undo over another document than the last stack item fails and keeps the item.
    #[test]
    fn undo_doc_mismatch() {
        let (mut d1, mut d2) = (Doc::new(), Doc::new());
        let mut mgr = UndoManager::new();
        node(&mut d1, "text", &mut NoReporter, |n| mgr.expand_scope(&n));
        node(&mut d1, "text", &mut mgr, |mut n| n.insert_text(0, "abc"));
        let mut txn = d2.transact_mut();
        let expected = d1.guid().clone();
        let actual = txn.doc().guid().clone();
        let err = mgr.undo(&mut txn).unwrap_err();
        assert_eq!(err, UndoError::DocMismatch { expected, actual });
        assert_eq!(mgr.undo_stack().len(), 1);
    }

    /// Undo within a transaction which already has changes fails.
    #[test]
    fn undo_dirty_transaction() {
        let mut doc = Doc::new();
        let mut mgr = UndoManager::new();
        node(&mut doc, "text", &mut NoReporter, |n| mgr.expand_scope(&n));
        node(&mut doc, "text", &mut mgr, |mut n| n.insert_text(0, "abc"));
        let mut txn = doc.transact_mut();
        txn.node_mut("text").unwrap().insert_text(0, "x");
        assert_eq!(mgr.undo(&mut txn).unwrap_err(), UndoError::DirtyTransaction);
    }

    /// Whole-document scope tracks changes of every root, but not transactions without changes.
    #[test]
    fn doc_scope() {
        let mut doc = Doc::new();
        let mut mgr = UndoManager::new();
        mgr.expand_scope(&doc);
        node(&mut doc, "a", &mut NoReporter, |n| mgr.expand_scope(&n)); // no-op for whole doc
        assert_eq!(mgr.scope(doc.guid()), Some(&Scope::Doc));
        {
            let mut txn = doc.transact_mut();
            txn.node_mut("a").unwrap().insert_text(0, "a");
            txn.node_mut("b").unwrap().insert_text(0, "b");
            txn.commit(&mut mgr);
            mgr.capture(());
            doc.transact_mut().commit(&mut mgr);
        }
        assert_eq!(mgr.undo_stack().len(), 1);
        undo(&mut mgr, &mut doc);
        assert_eq!((text(&doc, "a"), text(&doc, "b")), ("".into(), "".into()));
    }

    /// Stacks interleave items of many docs, while clearing only affects the given doc.
    #[test]
    fn multi_doc_clear() {
        let (mut d1, mut d2) = (Doc::new(), Doc::new());
        let mut mgr = UndoManager::new();
        node(&mut d1, "text", &mut NoReporter, |n| mgr.expand_scope(&n));
        node(&mut d2, "text", &mut NoReporter, |n| mgr.expand_scope(&n));
        let mut docs: Vec<_> = mgr.tracked_docs().cloned().collect();
        docs.sort();
        let mut expected = vec![d1.guid().clone(), d2.guid().clone()];
        expected.sort();
        assert_eq!(docs, expected);
        node(&mut d1, "text", &mut mgr, |mut n| n.insert_text(0, "a"));
        node(&mut d2, "text", &mut mgr, |mut n| n.insert_text(0, "b"));
        node(&mut d1, "text", &mut mgr, |mut n| n.insert_text(1, "c"));
        assert_eq!(mgr.undo_stack().len(), 3);
        assert!(mgr.clear_undo(&mut d1.transact_mut()));
        assert!(!mgr.clear_all(&mut d1.transact_mut()));
        assert_eq!(mgr.undo_stack().len(), 1);
        assert_eq!(mgr.undo_stack()[0].doc(), d2.guid());
        undo(&mut mgr, &mut d2);
        assert_eq!(
            (text(&d1, "text"), text(&d2, "text")),
            ("ac".into(), "".into())
        );
    }

    /// Dropping a tracked document doesn't affect tracking and undoing changes of another one.
    #[test]
    fn drop_tracked_doc() {
        let (mut d1, mut d2) = (Doc::new(), Doc::new());
        let mut mgr = UndoManager::new();
        node(&mut d1, "text", &mut NoReporter, |n| mgr.expand_scope(&n));
        node(&mut d2, "text", &mut NoReporter, |n| mgr.expand_scope(&n));
        node(&mut d2, "text", &mut mgr, |mut n| n.insert_text(0, "b"));
        node(&mut d1, "text", &mut mgr, |mut n| n.insert_text(0, "a"));
        drop(d2);
        mgr.capture(());
        node(&mut d1, "text", &mut mgr, |mut n| n.insert_text(1, "c"));
        undo(&mut mgr, &mut d1);
        assert_eq!(text(&d1, "text"), "a");
        undo(&mut mgr, &mut d1);
        assert_eq!(text(&d1, "text"), "");
    }

    /// Items of a dropped document on top of the stack can be removed to unblock undo.
    #[test]
    fn remove_dropped_doc_items() {
        let (mut d1, mut d2) = (Doc::new(), Doc::new());
        let mut mgr = UndoManager::new();
        node(&mut d1, "text", &mut NoReporter, |n| mgr.expand_scope(&n));
        node(&mut d2, "text", &mut NoReporter, |n| mgr.expand_scope(&n));
        node(&mut d1, "text", &mut mgr, |mut n| n.insert_text(0, "a"));
        node(&mut d2, "text", &mut mgr, |mut n| n.insert_text(0, "b"));
        let guid = d2.guid().clone();
        drop(d2);
        mgr.undo_stack_mut().retain(|item| item.doc() != &guid);
        assert!(undo(&mut mgr, &mut d1).is_some());
        assert_eq!(text(&d1, "text"), "");
    }

    /// Garbage collected node in scope doesn't affect tracking and undoing other nodes.
    #[test]
    fn collected_node_in_scope() {
        let mut doc = Doc::new();
        let map = node(&mut doc, "root", &mut NoReporter, |mut n| {
            n.insert_attr("map", Delta::new()).node_id().unwrap()
        });
        let mut mgr = UndoManager::new();
        node(&mut doc, map, &mut NoReporter, |n| mgr.expand_scope(&n));
        node(&mut doc, "text", &mut NoReporter, |n| mgr.expand_scope(&n));
        node(&mut doc, "root", &mut NoReporter, |mut n| {
            n.remove_attr("map")
        });
        node(&mut doc, "text", &mut mgr, |mut n| n.insert_text(0, "abc"));
        undo(&mut mgr, &mut doc);
        assert_eq!(text(&doc, "text"), "");
    }

    /// Stack items of different documents can't be merged.
    #[test]
    fn stack_item_merge_foreign_doc() {
        let mut a = StackItem::<()>::new("a".into(), IdSet::new(), IdSet::new());
        let b = StackItem::new("b".into(), IdSet::new(), IdSet::new());
        let expected = UndoError::DocMismatch {
            expected: "a".into(),
            actual: "b".into(),
        };
        assert_eq!(a.merge(b, |_, _| {}), Err(expected));
    }
}
