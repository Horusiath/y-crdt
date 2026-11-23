use crate::block::ItemPtr;
use crate::cell::{Acquire, AcquireMut};
use crate::id_set::DeleteSet;
use crate::iter::TxnIterator;
use crate::lib0::EncoderV1;
use crate::node::NodePtr;
use crate::slice::BlockSlice;
use crate::transaction::Origin;
use crate::{
    Cell, Doc, ID, IdSet, NodeRef, OwnedMut, Reporter, Transaction, TransactionState, Uuid,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt::Formatter;
use std::ops::{Deref, DerefMut};
use std::pin::Pin;
use std::sync::Arc;

/// Undo manager is a structure used to perform undo/redo operations over the associated shared
/// type(s).
///
/// Undo manager is a [Reporter]: it only sees changes of transactions committed with it
/// (see [Transaction::commit]), filtered by their origins (see: [Doc::transact_mut_with] and
/// [UndoManager::include_origin]) and scope (see: [UndoManager::expand_scope]).
///
/// Undo-/redo-able actions (a.k.a. [StackItem]s) are not equivalent to [crate::TransactionMut]
/// unit of work: tracked changes extend the last [StackItem] until [UndoManager::capture] is
/// called, which denotes the end of the batch.
///
/// In order to revert an operation, call [UndoManager::undo], then [UndoManager::redo] to bring it
/// back.
pub struct UndoManager<M> {
    docs: HashMap<Arc<str>, Cell<Doc>>,
    scope: HashSet<NodePtr>,
    options: Options<M>,
    undo_stack: UndoStack<M>,
    redo_stack: UndoStack<M>,
    undoing: bool,
    redoing: bool,
    /// Top of the undo stack is extended by tracked changes until captured.
    open: bool,
}

pub trait Meta: Default {}
impl<M> Meta for M where M: Default {}

impl<M> UndoManager<M>
where
    M: Meta,
{
    /// Creates a new instance of the [UndoManager]. While it's possible for undo manager to
    /// observe multiple shared types (see: [UndoManager::expand_scope]), it can only work with
    /// a single document at the same time.
    pub fn new() -> Self {
        Self::with_options(Options::default())
    }

    /// Creates a new instance of the [UndoManager] without any pre-initialized scope.
    /// While it's possible for undo manager to observe multiple shared types
    /// (see: [UndoManager::expand_scope]), it can only work with a single document at the same time.
    pub fn with_options(mut options: Options<M>) -> Self {
        let undo_stack = UndoStack(std::mem::take(&mut options.init_undo_stack));
        let redo_stack = UndoStack(std::mem::take(&mut options.init_redo_stack));
        UndoManager {
            scope: HashSet::new(),
            options,
            undo_stack,
            redo_stack,
            undoing: false,
            redoing: false,
            open: false,
            docs: HashMap::new(),
        }
    }

    /// Extends a list of shared types tracked by current undo manager by a given `node`.
    pub fn expand_scope<T>(&mut self, node: &mut NodeRef<T>)
    where
        T: DerefMut<Target = Transaction<OwnedMut<Doc>>>,
    {
        let node_ptr = NodePtr::from(node.as_ref());
        let txn = node.txn_mut();
        let guid = txn.doc().guid().clone();
        if !self.docs.contains_key(&guid) {
            self.docs.insert(guid, txn.doc.cell().clone());
        }
        self.scope.insert(node_ptr);
    }

    pub fn docs(&self) -> impl Iterator<Item = &Cell<Doc>> {
        self.docs.values()
    }

    /// Clears all [StackItem]s stored within current UndoManager undo AND redo stacks, effectively
    /// resetting its state. Returns `true` if any stack had items.
    ///
    /// # Deadlocks
    ///
    /// In order to perform its function, this method must guarantee that underlying document store
    /// is not being modified by another running `TransactionMut`. It does so by acquiring
    /// a read-only transaction itself. If transaction couldn't be acquired (because another
    /// read-write transaction is in progress), it will hold current thread until, acquisition is
    /// available.
    pub fn clear_all(&mut self) -> bool {
        let undo = self.clear_undo();
        self.clear_redo() || undo
    }

    /// Clears all [StackItem]s stored within current UndoManager undo stack alone, leaving redo
    /// stack untouched. Returns `true` if the stack had items.
    ///
    /// # Deadlocks
    ///
    /// See [UndoManager::clear_all].
    pub fn clear_undo(&mut self) -> bool {
        self.open = false;
        Self::clear_stack(&self.scope, &self.docs, &mut self.undo_stack)
    }

    /// Clears all [StackItem]s stored within current UndoManager redo stack alone, leaving undo
    /// stack untouched. Returns `true` if the stack had items.
    ///
    /// # Deadlocks
    ///
    /// See [UndoManager::clear_all].
    pub fn clear_redo(&mut self) -> bool {
        Self::clear_stack(&self.scope, &self.docs, &mut self.redo_stack)
    }

    fn clear_stack(
        scope: &HashSet<NodePtr>,
        docs: &HashMap<Uuid, Cell<Doc>>,
        stack: &mut UndoStack<M>,
    ) -> bool {
        let len = stack.len();
        for stack_item in stack.drain(0..len) {
            let mut deleted = stack_item.deletions.blocks();
            if let Some(cell) = docs.get(&stack_item.doc) {
                let doc = cell.acquire();
                let txn = doc.transact();
                while let Some(slice) = deleted.next(&txn) {
                    if let Some(item) = slice.as_item() {
                        if scope.iter().any(|b| b.is_parent_of(Some(item))) {
                            item.keep(false);
                        }
                    }
                }
            }
        }
        len != 0
    }

    /// Seals the last [StackItem] with given `meta`, so that next tracked changes start a new one.
    /// Returns `false` (and drops `meta`) if there were no tracked changes since the last capture.
    ///
    /// Example:
    /// ```rust
    /// use yrs::{Acquire, AcquireMut, Cell, Doc, NoReporter, UndoManager};
    ///
    /// let doc = Cell::new(Doc::new());
    /// // changes are tracked only if committed with the manager
    /// let insert = |mgr: &mut UndoManager, name: &str, index: u32, chunk: &str| {
    ///     let mut doc = doc.acquire_mut();
    ///     let mut txn = doc.transact_mut();
    ///     txn.node_mut(name).unwrap().insert_text(index, chunk);
    ///     txn.commit(mgr);
    /// };
    /// let read = |name: &str| doc.acquire().transact().node(name).unwrap().to_string();
    ///
    /// // without UndoManager::capture
    /// let mut mgr = UndoManager::new();
    /// mgr.expand_scope(&mut doc.transact_owned_mut().node_mut("no-capture").unwrap());
    /// insert(&mut mgr, "no-capture", 0, "a");
    /// insert(&mut mgr, "no-capture", 1, "b");
    /// mgr.undo(&mut NoReporter);
    /// assert_eq!(read("no-capture"), ""); // note that 'ab' was removed
    ///
    /// // with UndoManager::capture
    /// let mut mgr = UndoManager::new();
    /// mgr.expand_scope(&mut doc.transact_owned_mut().node_mut("with-capture").unwrap());
    /// insert(&mut mgr, "with-capture", 0, "a");
    /// mgr.capture(());
    /// insert(&mut mgr, "with-capture", 1, "b");
    /// mgr.undo(&mut NoReporter);
    /// assert_eq!(read("with-capture"), "a"); // note that only 'b' was removed
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

    /// Returns a list of [StackItem]s stored within current undo manager responsible for performing
    /// potential redo operations.
    pub fn redo_stack(&self) -> &[StackItem<M>] {
        &self.redo_stack.0
    }

    /// Undo last action tracked by current undo manager. Actions (a.k.a. [StackItem]s) are
    /// separated by [UndoManager::capture]. The reverting transaction is committed with `r`.
    ///
    /// Returns the reverted [StackItem], or `None` if nothing could be reverted.
    ///
    /// # Deadlocks
    ///
    /// This method requires exclusive access to underlying document store. This means that
    /// no other transaction on that same document can be active while calling this method.
    /// Otherwise, it may cause a deadlock.
    pub fn undo<R: Reporter>(&mut self, r: &mut R) -> Option<StackItem<M>> {
        self.pop(true, r)
    }

    /// Are there any redo steps available?
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// Redo'es last action previously undo'ed by current undo manager. The reapplying transaction
    /// is committed with `r`.
    ///
    /// Returns the reapplied [StackItem], or `None` if nothing could be reapplied.
    ///
    /// # Deadlocks
    ///
    /// This method requires exclusive access to underlying document store. This means that
    /// no other transaction on that same document can be active while calling this method.
    /// Otherwise, it may cause a deadlock.
    pub fn redo<R: Reporter>(&mut self, r: &mut R) -> Option<StackItem<M>> {
        self.pop(false, r)
    }

    fn pop<R: Reporter>(&mut self, undoing: bool, r: &mut R) -> Option<StackItem<M>> {
        self.undoing = undoing;
        self.redoing = !undoing;
        let mut result = None;
        loop {
            let stack = if undoing {
                &mut self.undo_stack
            } else {
                &mut self.redo_stack
            };
            let Some(item) = stack.pop() else { break };
            // cloned, so that `inner` isn't borrowed by the transaction
            let Some(cell) = self.docs.get(&item.doc).cloned() else {
                continue;
            };
            let mut doc = cell.acquire_mut();
            let origin = if undoing { "undo" } else { "redo" };
            let mut txn = doc.transact_mut_with(origin);
            let (stack, other) = if undoing {
                (&self.undo_stack, &self.redo_stack)
            } else {
                (&self.redo_stack, &self.undo_stack)
            };
            let (doc_ref, state) = txn.split_mut();
            let changed = revert(
                doc_ref,
                state,
                &item.insertions,
                &item.deletions,
                Some(&self.scope),
                stack,
                other,
            );
            // the manager fills the opposite stack
            txn.commit(&mut (&mut *self, &mut *r));
            if changed {
                result = Some(item);
                break;
            }
        }
        self.undoing = false;
        self.redoing = false;
        result
    }

    fn should_skip(&self, txn: &Transaction<&Doc>) -> bool {
        if let Some(capture_transaction) = &self.options.capture_transaction {
            if !capture_transaction(txn) {
                return true;
            }
        }
        let parents = txn.changed_parent_types();
        !self.scope.iter().any(|node| parents.contains(node))
    }
}

impl<M: Meta> Reporter for UndoManager<M> {
    type Encoder = EncoderV1;

    fn keep(&self, txn: &Transaction<&Doc>) -> Option<&HashSet<NodePtr>> {
        (!self.should_skip(txn)).then_some(&self.scope)
    }

    fn committed(&mut self, txn: &Transaction<&Doc>) {
        if self.should_skip(txn) {
            return;
        }
        let target = txn.doc().guid();
        let (undoing, redoing) = (self.undoing, self.redoing);
        if !undoing && !redoing {
            // neither undoing nor redoing: drop redo stack items of this doc
            let scope = &self.scope;
            self.redo_stack.0.retain(|stack_item| {
                if stack_item.doc != *target {
                    return true;
                }
                let mut deleted = stack_item.deletions.blocks();
                while let Some(slice) = deleted.next(txn) {
                    if let Some(item) = slice.as_item() {
                        if scope.iter().any(|b| b.is_parent_of(Some(item))) {
                            item.keep(false);
                        }
                    }
                }
                false
            });
        }
        let stack = if undoing {
            &mut self.redo_stack
        } else {
            &mut self.undo_stack
        };
        let same_doc = stack.last().is_some_and(|item| item.doc == *target);
        let extend = !undoing && !redoing && same_doc && self.open;
        if extend {
            let last = stack.last_mut().unwrap();
            last.deletions.merge_with(txn.delete_set().clone());
            last.insertions.merge_with(txn.insert_set().clone());
        } else {
            let deletes = txn.delete_set().clone();
            let item = StackItem::new(target.clone(), deletes, txn.insert_set().clone());
            stack.push(item);
        }
        if undoing {
            self.open = false;
        } else if !redoing {
            self.open = true;
        }
    }
}

pub(crate) fn revert<M>(
    doc: &mut Doc,
    state: &mut TransactionState,
    insertions: &IdSet,
    deletions: &IdSet,
    scope: Option<&HashSet<NodePtr>>,
    stack_a: &UndoStack<M>,
    stack_b: &UndoStack<M>,
) -> bool {
    let in_scope =
        |item: ItemPtr| scope.is_none_or(|s| s.iter().any(|b| b.is_parent_of(Some(item))));
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
            if !item.is_deleted() && in_scope(item) {
                to_delete.push(item);
            }
        }
    }

    let mut deleted = deletions.blocks();
    while let Some(slice) = deleted.next_in(&doc.blocks) {
        if let BlockSlice::Item(slice) = slice {
            let ptr = doc.materialize(slice);
            // never redo structs in `insertions`: they were created and deleted in the same change
            if in_scope(ptr) && !insertions.contains(ptr.id()) {
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

/// Set of options used to configure [UndoManager].
pub struct Options<M> {
    /// Custom logic decider, that along with [tracked_origins] can be used to determine if
    /// transaction changes should be captured or not.
    pub capture_transaction: Option<CaptureTransactionFn>,

    /// Initial undo stack that can be pre-filled with some operations that can be undone.
    pub init_undo_stack: Vec<StackItem<M>>,

    /// Initial redo stack that can be pre-filled with some operations that can be redone.
    pub init_redo_stack: Vec<StackItem<M>>,
}

pub type CaptureTransactionFn = Arc<dyn Fn(&Transaction<&Doc>) -> bool + Send + Sync + 'static>;

impl<M> Default for Options<M> {
    fn default() -> Self {
        Options {
            capture_transaction: None,
            init_undo_stack: Vec::new(),
            init_redo_stack: Vec::new(),
        }
    }
}

/// A unit of work for the [UndoManager]. It contains a compressed information about all updates and
/// deletions tracked by a corresponding undo manager. Whenever an [UndoManager::undo] or
/// [UndoManager::redo] methods are called a last [StackItem] is being used to modify a state of
/// the document.
///
/// Stack items are stored internally by undo manager and created automatically whenever a new
/// update from tracked shared type and transaction of tracked origin has been committed with the
/// manager after the last [UndoManager::capture], which marks the end of the last stack item batch.
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
    /// custom metadata structures together.
    pub fn merge<F>(&mut self, other: Self, merge_meta: F)
    where
        F: FnOnce(&mut M, M),
    {
        self.insertions.merge_with(other.insertions);
        self.deletions.merge_with(other.deletions);
        merge_meta(&mut self.meta, other.meta);
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
    use crate::cell::{Acquire, AcquireMut};

    use crate::lib0::{Decode, EncoderV1};
    use crate::node::{Attrs, NodePtr};
    use crate::test_utils::{Report, exchange_updates};
    use crate::undo::{Options, StackItem};
    use crate::{
        Any, Cell, Delta, DeltaOptions, Doc, ID, In, NoReporter, NodeID, NodeRef, Origin, Out,
        OwnedMut, Reporter, StateVector, Transaction, UndoManager, Update, any,
    };
    use serde::Deserialize;

    #[test]
    fn selective_clear_preserves_other_stack_and_reports_actual_changes() {
        for (clear_undo, clear_redo) in [(true, false), (false, true), (true, true)] {
            let doc = Cell::new(Doc::new());
            let mut undo = UndoManager::new();
            node(&doc, "text", &mut NoReporter, |mut n| {
                undo.expand_scope(&mut n)
            });
            node(&doc, "text", &mut undo, |mut n| n.insert_text(0, "a"));
            undo.capture(());
            node(&doc, "text", &mut undo, |mut n| n.insert_text(1, "b"));
            assert!(undo.undo(&mut NoReporter).is_some());
            assert!(undo.can_undo() && undo.can_redo());
            let text = || doc.acquire().transact().node("text").unwrap().to_string();
            for i in 0..2 {
                let cleared = match (clear_undo, clear_redo) {
                    (true, false) => undo.clear_undo(),
                    (false, true) => undo.clear_redo(),
                    _ => undo.clear_all(),
                };
                assert_eq!(undo.can_undo(), !clear_undo);
                assert_eq!(undo.can_redo(), !clear_redo);
                assert_eq!(text(), "a");
                // was: stack_cleared event fired once, with (clear_undo, clear_redo)
                assert_eq!(cleared, i == 0);
            }
            if !clear_undo {
                assert!(undo.undo(&mut NoReporter).is_some());
                assert_eq!(text(), "");
            } else if !clear_redo {
                assert!(undo.redo(&mut NoReporter).is_some());
                assert_eq!(text(), "ab");
            }
        }
    }

    #[test]
    fn undo_text() {
        let d1 = Cell::new(Doc::with_client_id(1));
        let txt1 = NodePtr::from(
            d1.acquire_mut()
                .transact_mut()
                .node_mut("test")
                .unwrap()
                .as_ref(),
        );
        let mut mgr = UndoManager::new();
        node(&d1, "test", &mut NoReporter, |mut n| {
            mgr.expand_scope(&mut n)
        });

        let d2 = Cell::new(Doc::with_client_id(2));

        // items that are added & deleted in the same transaction won't be undo
        node(&d1, "test", &mut mgr, |mut n| n.insert_text(0, "test"));
        node(&d1, "test", &mut mgr, |mut n| n.remove(0, 4));
        mgr.undo(&mut NoReporter);
        assert_eq!(
            d1.acquire().transact().node("test").unwrap().to_string(),
            ""
        );

        // follow redone items
        node(&d1, "test", &mut mgr, |mut n| n.insert_text(0, "a"));
        mgr.capture(());
        node(&d1, "test", &mut mgr, |mut n| n.remove(0, 1));
        mgr.capture(());
        mgr.undo(&mut NoReporter);
        assert_eq!(
            d1.acquire().transact().node("test").unwrap().to_string(),
            "a"
        );
        mgr.undo(&mut NoReporter);
        assert_eq!(
            d1.acquire().transact().node("test").unwrap().to_string(),
            ""
        );

        node(&d1, "test", &mut mgr, |mut n| n.insert_text(0, "abc"));
        node(&d2, "test", &mut NoReporter, |mut n| {
            n.insert_text(0, "xyz")
        });

        {
            let mut g1 = d1.acquire_mut();
            let mut g2 = d2.acquire_mut();
            exchange_updates(&mut [&mut *g1, &mut *g2]);
        }
        mgr.undo(&mut NoReporter);
        assert_eq!(
            d1.acquire().transact().node("test").unwrap().to_string(),
            "xyz"
        );
        mgr.redo(&mut NoReporter);
        assert_eq!(
            d1.acquire().transact().node("test").unwrap().to_string(),
            "abcxyz"
        );

        {
            let mut g1 = d1.acquire_mut();
            let mut g2 = d2.acquire_mut();
            exchange_updates(&mut [&mut *g1, &mut *g2]);
        }

        node(&d2, "test", &mut NoReporter, |mut n| n.remove(0, 1));

        {
            let mut g1 = d1.acquire_mut();
            let mut g2 = d2.acquire_mut();
            exchange_updates(&mut [&mut *g1, &mut *g2]);
        }

        mgr.undo(&mut NoReporter);
        assert_eq!(
            d1.acquire().transact().node("test").unwrap().to_string(),
            "xyz"
        );
        mgr.redo(&mut NoReporter);
        {
            let mut d1 = d1.acquire_mut();
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
        mgr.undo(&mut NoReporter);
        mgr.redo(&mut NoReporter);
    }

    #[test]
    fn double_undo() {
        let doc = Cell::new(Doc::with_client_id(1));
        node(&doc, "test", &mut NoReporter, |mut n| {
            n.insert_text(0, "1221")
        });

        let mut mgr = UndoManager::new();
        node(&doc, "test", &mut NoReporter, |mut n| {
            mgr.expand_scope(&mut n)
        });
        node(&doc, "test", &mut mgr, |mut n| n.insert_text(2, "3"));
        node(&doc, "test", &mut mgr, |mut n| n.insert_text(3, "3"));

        mgr.undo(&mut NoReporter);
        mgr.undo(&mut NoReporter);

        node(&doc, "test", &mut mgr, |mut n| n.insert_text(2, "3"));
        assert_eq!(
            doc.acquire().transact().node("test").unwrap().to_string(),
            "12321"
        );
    }

    #[test]
    fn undo_map() {
        let d1 = Cell::new(Doc::with_client_id(1));

        node(&d1, "test", &mut NoReporter, |mut n| n.insert_attr("a", 0));
        let mut mgr = UndoManager::new();
        node(&d1, "test", &mut NoReporter, |mut n| {
            mgr.expand_scope(&mut n)
        });
        node(&d1, "test", &mut mgr, |mut n| n.insert_attr("a", 1));
        mgr.undo(&mut NoReporter);
        assert_eq!(
            d1.acquire()
                .transact()
                .node("test")
                .unwrap()
                .attr("a")
                .unwrap(),
            0.into()
        );
        mgr.redo(&mut NoReporter);
        assert_eq!(
            d1.acquire()
                .transact()
                .node("test")
                .unwrap()
                .attr("a")
                .unwrap(),
            1.into()
        );

        // TODO(unified-api): nested `MapPrelim` sub-types + cross-peer overwrite have no NodeRef
        // equivalent yet. Original block: inserted a nested map under "a", set "x"=42, checked
        // that undo/redo restores the whole nested type, then had a second peer overwrite key "a"
        // with 44 (concurrent overwrite makes undo skip).

        // test setting value multiple times
        node(&d1, "test", &mut mgr, |mut n| {
            n.insert_attr("b", "initial".to_string())
        });
        mgr.capture(());
        node(&d1, "test", &mut mgr, |mut n| {
            n.insert_attr("b", "val1".to_string())
        });
        node(&d1, "test", &mut mgr, |mut n| {
            n.insert_attr("b", "val2".to_string())
        });
        mgr.capture(());
        mgr.undo(&mut NoReporter);
        assert_eq!(
            d1.acquire()
                .transact()
                .node("test")
                .unwrap()
                .attr("b")
                .unwrap(),
            "initial".into()
        );
    }

    /// Runs `f` over a `node` in a new transaction committed with `r`.
    fn node<F, T>(cell: &Cell<Doc>, node: impl Into<NodeID>, r: &mut impl Reporter, f: F) -> T
    where
        F: FnOnce(NodeRef<&mut Transaction<OwnedMut<Doc>>>) -> T,
    {
        let mut txn = cell.transact_owned_mut();
        let result = f(txn.node_mut(node.into()).unwrap());
        txn.commit(r);
        result
    }

    /// Same as [node], but the underlying transaction is tagged with a given `origin`.
    fn node_with<F, T>(
        cell: &Cell<Doc>,
        origin: impl Into<Origin>,
        node: impl Into<NodeID>,
        r: &mut impl Reporter,
        f: F,
    ) -> T
    where
        F: FnOnce(NodeRef<&mut Transaction<OwnedMut<Doc>>>) -> T,
    {
        let mut txn = cell.transact_owned_mut_with(origin);
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
        let d1 = Cell::new(Doc::with_client_id(1));

        let d2 = Cell::new(Doc::with_client_id(2));

        let mut mgr = UndoManager::new();
        node(&d1, array.clone(), &mut NoReporter, |mut n| {
            mgr.expand_scope(&mut n)
        });
        node(&d1, "test", &mut mgr, |mut array| {
            array.insert_range(0, [1, 2, 3])
        });
        node(&d2, "test", &mut NoReporter, |mut array| {
            array.insert_range(0, [4, 5, 6])
        });

        exchange_updates(&mut [&mut *d1.acquire_mut(), &mut *d2.acquire_mut()]);

        node(&d1, "test", &mut NoReporter, |mut array| {
            assert_eq!(
                array.deserialize::<Vec<i32>>().unwrap(),
                vec![1, 2, 3, 4, 5, 6]
            )
        });

        mgr.undo(&mut NoReporter);

        node(&d1, "test", &mut NoReporter, |mut array| {
            assert_eq!(array.deserialize::<Vec<i32>>().unwrap(), vec![4, 5, 6])
        });

        mgr.redo(&mut NoReporter);

        node(&d1, "test", &mut NoReporter, |mut array| {
            assert_eq!(
                array.deserialize::<Vec<i32>>().unwrap(),
                vec![1, 2, 3, 4, 5, 6]
            )
        });

        exchange_updates(&mut [&mut *d1.acquire_mut(), &mut *d2.acquire_mut()]);

        node(&d2, "test", &mut NoReporter, |mut array| {
            array.remove(0, 1); // user2 deletes [1]
        });

        exchange_updates(&mut [&mut *d1.acquire_mut(), &mut *d2.acquire_mut()]);

        mgr.undo(&mut NoReporter);
        node(&d1, "test", &mut NoReporter, |mut array| {
            assert_eq!(array.deserialize::<Vec<i32>>().unwrap(), vec![4, 5, 6]);
        });

        mgr.redo(&mut NoReporter);

        node(&d1, "test", &mut mgr, |mut array| {
            assert_eq!(
                array.deserialize::<Vec<i32>>().unwrap(),
                vec![2, 3, 4, 5, 6]
            );
            array.remove(0, 5);
        });

        // test nested structure
        let map = node(&d1, "test", &mut mgr, |mut array| {
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

        node(&d1, map.id.clone(), &mut mgr, |mut map| {
            map.insert_attr("a", 1);
        });
        node(&d1, "test", &mut NoReporter, |mut array| {
            assert_eq!(
                array.deserialize::<Vec<Entry>>().unwrap(),
                vec![Entry::new(Some(1), None)]
            );
        });

        mgr.undo(&mut NoReporter);

        node(&d1, "test", &mut NoReporter, |mut array| {
            assert_eq!(
                array.deserialize::<Vec<Entry>>().unwrap(),
                vec![Entry::new(None, None)]
            );
        });

        mgr.undo(&mut NoReporter);

        node(&d1, "test", &mut NoReporter, |mut array| {
            assert_eq!(
                array.deserialize::<Vec<i32>>().unwrap(),
                vec![2, 3, 4, 5, 6]
            );
        });

        mgr.redo(&mut NoReporter);

        node(&d1, "test", &mut NoReporter, |mut array| {
            assert_eq!(
                array.deserialize::<Vec<Entry>>().unwrap(),
                vec![Entry::new(None, None)]
            );
        });

        mgr.redo(&mut NoReporter);
        node(&d1, "test", &mut NoReporter, |array| {
            assert_eq!(
                array.deserialize::<Vec<Entry>>().unwrap(),
                vec![Entry::new(Some(1), None)]
            );
        });

        exchange_updates(&mut [&mut *d1.acquire_mut(), &mut *d2.acquire_mut()]);

        let map2 = node(&d2, "test", &mut NoReporter, |array| {
            array.get(0).unwrap().node_id().unwrap()
        });
        node(&d2, map2, &mut NoReporter, |mut map| {
            map.insert_attr("b", 2);
        });

        exchange_updates(&mut [&mut *d1.acquire_mut(), &mut *d2.acquire_mut()]);

        node(&d1, "test", &mut NoReporter, |array| {
            assert_eq!(
                array.deserialize::<Vec<Entry>>().unwrap(),
                vec![Entry::new(Some(1), Some(2))]
            );
        });

        mgr.undo(&mut NoReporter);
        node(&d1, "test", &mut NoReporter, |array| {
            assert_eq!(
                array.deserialize::<Vec<Entry>>().unwrap(),
                vec![Entry::new(None, Some(2))]
            );
        });

        mgr.undo(&mut NoReporter);
        node(&d1, "test", &mut NoReporter, |array| {
            assert_eq!(
                array.deserialize::<Vec<i32>>().unwrap(),
                vec![2, 3, 4, 5, 6]
            );
        });

        mgr.redo(&mut NoReporter);
        node(&d1, "test", &mut NoReporter, |array| {
            assert_eq!(
                array.deserialize::<Vec<Entry>>().unwrap(),
                vec![Entry::new(None, Some(2))]
            );
        });

        mgr.redo(&mut NoReporter);
        node(&d1, "test", &mut NoReporter, |array| {
            assert_eq!(
                array.deserialize::<Vec<Entry>>().unwrap(),
                vec![Entry::new(Some(1), Some(2))]
            );
        });
    }

    #[test]
    fn undo_xml() {
        let d1 = Cell::new(Doc::with_client_id(1));
        let xml1 = node(&d1, "xml", &mut NoReporter, |mut frag| {
            frag.insert(0, Delta::with_name("undefined"))
                .node_id()
                .unwrap()
        });

        let mut mgr = UndoManager::new();
        node(&d1, xml1.clone(), &mut NoReporter, |mut n| {
            mgr.expand_scope(&mut n)
        });
        let child = node(&d1, xml1.clone(), &mut mgr, |mut xml| {
            xml.insert(0, Delta::with_name("p")).node_id().unwrap()
        });
        let text_child = node(&d1, child, &mut mgr, |mut child| {
            child
                .insert(0, Delta::new().insert_text("content"))
                .node_id()
                .unwrap()
        });

        let as_string = || {
            node(&d1, xml1.clone(), &mut NoReporter, |xml| {
                xml.format_xml().to_string()
            })
        };

        assert_eq!(as_string(), "<undefined><p>content</p></undefined>");
        // format textchild and revert that change
        mgr.capture(());
        node(&d1, text_child, &mut mgr, |mut text| {
            text.format(3, 4, &Attrs::from([("bold".into(), any!({}))]))
        });
        assert_eq!(
            as_string(),
            "<undefined><p>con<bold>tent</bold></p></undefined>"
        );
        mgr.undo(&mut NoReporter);
        assert_eq!(as_string(), "<undefined><p>content</p></undefined>");
        mgr.redo(&mut NoReporter);
        assert_eq!(
            as_string(),
            "<undefined><p>con<bold>tent</bold></p></undefined>"
        );
        node(&d1, xml1.clone(), &mut mgr, |mut xml| xml.remove(0, 1));
        assert_eq!(as_string(), "<undefined></undefined>");
        mgr.undo(&mut NoReporter);
        assert_eq!(
            as_string(),
            "<undefined><p>con<bold>tent</bold></p></undefined>"
        );
    }

    #[test]
    fn undo_events() {
        use crate::undo::UndoManager;
        type Metadata = HashMap<String, usize>;

        let doc = Cell::new(Doc::with_client_id(1));
        let txt = NodePtr::from(
            doc.acquire_mut()
                .transact_mut()
                .node_mut("test")
                .unwrap()
                .as_ref(),
        );
        let mut mgr: UndoManager<Metadata> = UndoManager::new();
        node(&doc, "test", &mut NoReporter, |mut n| {
            mgr.expand_scope(&mut n)
        });

        let mut r = Report::new(EncoderV1::new());
        node(&doc, "test", &mut (&mut mgr, &mut r), |mut n| {
            n.insert_text(0, "abc")
        });
        assert!(r.parents.contains(&txt));
        assert!(mgr.capture(Metadata::from([("test".to_string(), 1)])));

        let item = mgr.undo(&mut r).unwrap();
        assert!(r.parents.contains(&txt));
        assert_eq!(item.meta().get("test"), Some(&1));
        let item = mgr.redo(&mut r).unwrap();
        assert!(r.parents.contains(&txt));
        // was 2: meta was set on redo stack items by observers, now they get `M::default()`
        assert_eq!(item.meta().get("test"), None);
    }

    #[test]
    fn undo_stack_serialization() {
        use crate::undo::UndoManager;
        type Metadata = HashMap<String, usize>;
        let (undo_stack_json, redo_stack_json, state) = {
            let d1 = Cell::new(Doc::with_options(crate::Options {
                client_id: ClientID::new(1),
                guid: "A".into(),
                ..crate::Options::default()
            }));
            let mut m1: UndoManager<Metadata> = UndoManager::new();
            node(&d1, "test", &mut NoReporter, |mut n| {
                m1.expand_scope(&mut n)
            });

            let meta = || Metadata::from([("test".to_string(), 0)]);

            node(&d1, "test", &mut m1, |mut n| n.insert_text(0, "c"));
            m1.capture(meta());
            node(&d1, "test", &mut m1, |mut n| n.insert_text(0, "b"));
            m1.capture(meta());
            node(&d1, "test", &mut m1, |mut n| n.insert_text(0, "a"));
            m1.capture(meta());

            m1.undo(&mut NoReporter);
            assert_eq!(
                d1.acquire().transact().node("test").unwrap().to_string(),
                "bc"
            );
            m1.redo(&mut NoReporter);
            assert_eq!(
                d1.acquire().transact().node("test").unwrap().to_string(),
                "abc"
            );

            let undo_stack_json = serde_json::to_string(m1.undo_stack()).unwrap();
            let redo_stack_json = serde_json::to_string(m1.redo_stack()).unwrap();
            let doc_state = d1
                .acquire()
                .transact()
                .encode_diff_v1(&StateVector::default());
            (undo_stack_json, redo_stack_json, doc_state)
        };

        let undo_stack: Vec<StackItem<Metadata>> = serde_json::from_str(&undo_stack_json).unwrap();
        let redo_stack: Vec<StackItem<Metadata>> = serde_json::from_str(&redo_stack_json).unwrap();

        // try to recreate the stack
        let d2 = Cell::new(Doc::with_options(crate::Options {
            client_id: ClientID::new(2),
            guid: "A".into(),
            ..crate::Options::default()
        }));
        let undo_options = Options {
            init_undo_stack: undo_stack,
            init_redo_stack: redo_stack,
            ..Default::default()
        };
        d2.acquire_mut()
            .transact_mut()
            .apply_update(Update::decode_v1(&state).unwrap())
            .unwrap();
        let mut m2: UndoManager<Metadata> = UndoManager::with_options(undo_options);
        node(&d2, "test", &mut NoReporter, |mut n| {
            m2.expand_scope(&mut n)
        });

        m2.undo(&mut NoReporter);
        assert_eq!(
            d2.acquire().transact().node("test").unwrap().to_string(),
            "bc"
        );
        m2.redo(&mut NoReporter);
        assert_eq!(
            d2.acquire().transact().node("test").unwrap().to_string(),
            "abc"
        );
    }

    #[test]
    fn undo_until_change_performed() {
        let d1 = Cell::new(Doc::with_client_id(1));
        let d2 = Cell::new(Doc::with_client_id(2));

        let map1a = node(&d1, "array", &mut NoReporter, |mut arr| {
            arr.push_back(Delta::new().insert_attr("hello", "world".to_string()))
                .node_id()
                .unwrap()
        });
        let map1b = node(&d1, "array", &mut NoReporter, |mut arr| {
            arr.push_back(Delta::new().insert_attr("key", "value".to_string()))
                .node_id()
                .unwrap()
        });

        exchange_updates(&mut [&mut *d1.acquire_mut(), &mut *d2.acquire_mut()]);

        let mut mgr1 = UndoManager::new();
        node(&d1, "array", &mut NoReporter, |mut n| {
            mgr1.expand_scope(&mut n)
        });
        let d1_client_id = d1.acquire().client_id();

        let mut mgr2 = UndoManager::new();
        node(&d2, "array", &mut NoReporter, |mut n| {
            mgr2.expand_scope(&mut n)
        });
        let d2_client_id = d2.acquire().client_id();

        node_with(&d1, d1_client_id, map1b.clone(), &mut mgr1, |mut map| {
            map.insert_attr("key", "value modified".to_string());
        });

        exchange_updates(&mut [&mut *d1.acquire_mut(), &mut *d2.acquire_mut()]);
        mgr1.capture(());

        node_with(&d1, d1_client_id, map1a, &mut mgr1, |mut map| {
            map.insert_attr("hello", "world modified".to_string());
        });

        exchange_updates(&mut [&mut *d1.acquire_mut(), &mut *d2.acquire_mut()]);

        node_with(&d2, d2_client_id, "array", &mut mgr2, |mut arr| {
            arr.remove(0, 1)
        });

        exchange_updates(&mut [&mut *d1.acquire_mut(), &mut *d2.acquire_mut()]);
        mgr2.undo(&mut NoReporter);

        exchange_updates(&mut [&mut *d1.acquire_mut(), &mut *d2.acquire_mut()]);
        mgr1.undo(&mut NoReporter);
        exchange_updates(&mut [&mut *d1.acquire_mut(), &mut *d2.acquire_mut()]);

        node(&d1, map1b, &mut NoReporter, |map| {
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
        let doc = Cell::new(Doc::with_options(crate::doc::Options {
            skip_gc: true,
            client_id: ClientID::new(1),
            ..crate::doc::Options::default()
        }));
        let mut mgr = UndoManager::new();
        node(&doc, "map", &mut NoReporter, |mut n| {
            mgr.expand_scope(&mut n)
        });

        let blocks = |text: &str| Delta::new().insert_attr("text", text.to_owned());
        let text = node(&doc, "map", &mut mgr, |mut design| {
            let content = Delta::new().insert_attr("blocks", blocks("Type something"));
            design.insert_attr("text", content).node_id().unwrap()
        });
        mgr.capture(());

        node(&doc, text.clone(), &mut mgr, |mut text| {
            text.insert_attr("blocks", blocks("Something"));
        });
        mgr.capture(());

        node(&doc, text, &mut mgr, |mut text| {
            text.insert_attr("blocks", blocks("Something else"));
        });
        mgr.capture(());

        let design_json = || {
            node(&doc, "map", &mut NoReporter, |design| {
                design.deserialize::<Design>().unwrap()
            })
        };

        assert_eq!(design_json(), design("Something else"));
        mgr.undo(&mut NoReporter);
        assert_eq!(design_json(), design("Something"));
        mgr.undo(&mut NoReporter);
        assert_eq!(design_json(), design("Type something"));
        mgr.undo(&mut NoReporter);
        assert_eq!(design_json(), Design { text: None });
        mgr.redo(&mut NoReporter);
        assert_eq!(design_json(), design("Type something"));
        mgr.redo(&mut NoReporter);
        assert_eq!(design_json(), design("Something"));
        mgr.redo(&mut NoReporter);
        assert_eq!(design_json(), design("Something else"));
    }

    #[test]
    fn consecutive_redo_bug() {
        // https://github.com/yjs/yjs/issues/355
        let doc = Cell::new(Doc::with_client_id(1));
        let mut mgr = UndoManager::new();
        node(&doc, "root", &mut NoReporter, |mut n| {
            mgr.expand_scope(&mut n)
        });

        let point = node(&doc, "root", &mut mgr, |mut root| {
            let content = Delta::new().insert_attr("x", 0).insert_attr("y", 0);
            root.insert_attr("a", content).node_id().unwrap()
        });
        mgr.capture(());

        let set_point = |mgr: &mut UndoManager, x: i32, y: i32| {
            node(&doc, point.clone(), mgr, |mut point| {
                point.insert_attr("x", x)
            });
            node(&doc, point.clone(), mgr, |mut point| {
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
        let point_json = || {
            node(&doc, "root", &mut NoReporter, |r| {
                r.deserialize::<R>().unwrap().a
            })
        };

        assert_eq!(point_json(), Point { x: 300, y: 300 });

        mgr.undo(&mut NoReporter); // x=200, y=200
        assert_eq!(point_json(), Point { x: 200, y: 200 });

        mgr.undo(&mut NoReporter); // x=100, y=100
        assert_eq!(point_json(), Point { x: 100, y: 100 });

        mgr.undo(&mut NoReporter); // x=0, y=0
        assert_eq!(point_json(), Point { x: 0, y: 0 });

        mgr.undo(&mut NoReporter); // null
        node(&doc, "root", &mut NoReporter, |root| {
            assert_eq!(root.attr("a"), None)
        });

        mgr.redo(&mut NoReporter); // x=0, y=0
        assert_eq!(point_json(), Point { x: 0, y: 0 });

        mgr.redo(&mut NoReporter); // x=100, y=100
        assert_eq!(point_json(), Point { x: 100, y: 100 });

        mgr.redo(&mut NoReporter); // x=200, y=200
        assert_eq!(point_json(), Point { x: 200, y: 200 });

        mgr.redo(&mut NoReporter); // x=300, y=300
        assert_eq!(point_json(), Point { x: 300, y: 300 });
    }

    #[test]
    fn undo_xml_bug() {
        // https://github.com/yjs/yjs/issues/304
        const ORIGIN: &str = "origin";
        let doc = Cell::new(Doc::with_client_id(1));
        let mut mgr = UndoManager::new();
        node(&doc, "t", &mut NoReporter, |mut n| mgr.expand_scope(&mut n));

        // create element
        let e = node_with(&doc, ORIGIN, "t", &mut mgr, |mut f| {
            let content = Delta::with_name("test-node")
                .insert_attr("a", "100".to_string())
                .insert_attr("b", "0".to_string());
            f.insert(0, content).node_id().unwrap()
        });
        mgr.capture(());

        // change one attribute
        node_with(&doc, ORIGIN, e.clone(), &mut mgr, |mut e| {
            e.insert_attr("a", "200".to_string());
        });
        mgr.capture(());

        // change both attributes
        node_with(&doc, ORIGIN, e, &mut mgr, |mut e| {
            e.insert_attr("a", "180".to_string());
            e.insert_attr("b", "50".to_string());
        });
        mgr.capture(());

        mgr.undo(&mut NoReporter);
        mgr.undo(&mut NoReporter);
        mgr.undo(&mut NoReporter);

        mgr.redo(&mut NoReporter);
        mgr.redo(&mut NoReporter);
        mgr.redo(&mut NoReporter);

        let str = node(&doc, "t", &mut NoReporter, |f| f.format_xml().to_string());
        assert!(
            str == r#"<test-node a="180" b="50"></test-node>"#
                || str == r#"<test-node b="50" a="180"></test-node>"#
        );
    }

    #[test]
    fn undo_block_bug() {
        // https://github.com/yjs/yjs/issues/343
        let doc = Cell::new(Doc::with_options({
            let mut o = crate::doc::Options::default();
            o.client_id = ClientID::new(1);
            o.skip_gc = true;
            o
        }));
        let mut mgr = UndoManager::new();
        node(&doc, "map", &mut NoReporter, |mut n| {
            mgr.expand_scope(&mut n)
        });

        let blocks = |text: &str| Delta::new().insert_attr("text", text.to_owned());
        let text = node(&doc, "map", &mut mgr, |mut design| {
            let content = Delta::new().insert_attr("blocks", blocks("1"));
            design.insert_attr("text", content).node_id().unwrap()
        });
        mgr.capture(());
        for content in ["1", "3", "4"] {
            node(&doc, text.clone(), &mut mgr, |mut text| {
                text.insert_attr("blocks", blocks(content));
            });
            mgr.capture(());
        }
        // {"text":{"blocks":{"text":"4"}}}
        mgr.undo(&mut NoReporter); // {"text":{"blocks":{"3"}}}
        mgr.undo(&mut NoReporter); // {"text":{"blocks":{"text":"2"}}}
        mgr.undo(&mut NoReporter); // {"text":{"blocks":{"text":"1"}}}
        mgr.undo(&mut NoReporter); // {}
        mgr.redo(&mut NoReporter); // {"text":{"blocks":{"text":"1"}}}
        mgr.redo(&mut NoReporter); // {"text":{"blocks":{"text":"2"}}}
        mgr.redo(&mut NoReporter); // {"text":{"blocks":{"text":"3"}}}
        mgr.redo(&mut NoReporter); // {"text":{}}
        let actual = node(&doc, "map", &mut NoReporter, |design| {
            design.deserialize::<Design>().unwrap()
        });
        assert_eq!(actual, design("4"));
    }

    #[test]
    fn undo_delete_text_format() {
        // https://github.com/yrs/yjs/issues/392
        fn send(src: &Cell<Doc>, dst: &Cell<Doc>) {
            let update = Update::decode_v1(
                &src.acquire()
                    .transact()
                    .encode_state_as_update_v1(&StateVector::default()),
            )
            .unwrap();
            dst.acquire_mut()
                .transact_mut()
                .apply_update(update)
                .unwrap();
        }

        let doc1 = Cell::new(Doc::with_client_id(1));
        node(&doc1, "test", &mut NoReporter, |mut n| {
            n.insert_text(0, "Attack ships on fire off the shoulder of Orion.")
        }); // D1: 'Attack ships on fire off the shoulder of Orion.'
        let doc2 = Cell::new(Doc::with_client_id(2));

        send(&doc1, &doc2); // D2: 'Attack ships on fire off the shoulder of Orion.'
        let mut mgr = UndoManager::new();
        node(&doc1, "test", &mut NoReporter, |mut n| {
            mgr.expand_scope(&mut n)
        });

        let attrs = Attrs::from([("bold".into(), true.into())]);
        node(&doc1, "test", &mut mgr, |mut n| n.format(13, 7, &attrs)); // D1: 'Attack ships <b>on fire</b> off the shoulder of Orion.'

        mgr.capture(());

        send(&doc1, &doc2); // D2: 'Attack ships <b>on fire</b> off the shoulder of Orion.'

        let attrs2 = Attrs::from([("bold".into(), Any::Null)]);
        node(&doc1, "test", &mut mgr, |mut n| n.format(16, 4, &attrs2)); // D1: 'Attack ships <b>on </b>fire off the shoulder of Orion.'

        // TODO(unified-api): rich-text `diff`/`Diff`/`YChange` assertions have no NodeRef
        // equivalent yet (`to_delta` is not implemented). Original expected the delta:
        //   ["Attack ships ", <b>"on "</b>, "fire off the shoulder of Orion."]

        mgr.capture(());
        send(&doc1, &doc2); // D2: 'Attack ships <b>on </b>fire off the shoulder of Orion.'

        mgr.undo(&mut NoReporter); // D1: 'Attack ships <b>on fire</b> off the shoulder of Orion.'
        send(&doc1, &doc2); // D2: 'Attack ships <b>on fire</b> off the shoulder of Orion.'

        // TODO(unified-api): rich-text `diff`/`Diff`/`YChange` assertions have no NodeRef
        // equivalent yet. Original expected on both docs the delta:
        //   ["Attack ships ", <b>"on fire"</b>, " off the shoulder of Orion."]
    }

    #[test]
    fn special_deletion_case() {
        // https://github.com/yjs/yjs/issues/447
        const ORIGIN: &str = "undoable";
        let doc = Cell::new(Doc::with_client_id(1));
        let mut mgr = UndoManager::new();
        node(&doc, "test", &mut NoReporter, |mut n| {
            mgr.expand_scope(&mut n)
        });

        let e = node(&doc, "test", &mut mgr, |mut f| {
            let content = Delta::with_name("test")
                .insert_attr("a", "1".to_string())
                .insert_attr("b", "2".to_string());
            f.insert(0, content).node_id().unwrap()
        });
        let as_string = || {
            node(&doc, "test", &mut NoReporter, |f| {
                f.format_xml().to_string()
            })
        };

        let s = as_string();
        assert!(s == r#"<test a="1" b="2"></test>"# || s == r#"<test b="2" a="1"></test>"#);

        {
            // change attribute "b" and delete test-node within a single transaction
            let mut guard = doc.acquire_mut();
            let mut txn = guard.transact_mut_with(ORIGIN);
            txn.node_mut(e).unwrap().insert_attr("b", "3".to_string());
            txn.node_mut("test").unwrap().remove(0, 1);
            txn.commit(&mut mgr);
        }
        assert_eq!(as_string(), "");

        mgr.undo(&mut NoReporter);
        let s = as_string();
        assert!(s == r#"<test a="1" b="2"></test>"# || s == r#"<test b="2" a="1"></test>"#);
    }

    #[test]
    fn undo_in_embed() {
        let d1 = Cell::new(Doc::with_client_id(1));
        let mut mgr = UndoManager::new();
        node(&d1, "test", &mut NoReporter, |mut n| {
            mgr.expand_scope(&mut n)
        });

        let d2 = Cell::new(Doc::with_client_id(2));

        let attrs = Attrs::from([("bold".into(), true.into())]);
        let nested = node(&d1, "test", &mut mgr, |mut txt1| {
            let embed = Delta::new().insert_text("initial text");
            txt1.apply_delta(Delta::new().insert_fmt(embed, &attrs));
            txt1.get(0).unwrap().node_id().unwrap()
        });
        let nested_string = || {
            node(&d1, nested.clone(), &mut NoReporter, |nested| {
                nested.to_string()
            })
        };

        assert_eq!(nested_string(), "initial text".to_string());
        mgr.capture(());
        node(&d1, nested.clone(), &mut mgr, |mut nested| {
            let len = nested.len();
            nested.remove(0, len);
        });
        node(&d1, nested.clone(), &mut mgr, |mut nested| {
            nested.insert_text(0, "other text")
        });
        assert_eq!(nested_string(), "other text".to_string());
        mgr.undo(&mut NoReporter);
        assert_eq!(nested_string(), "initial text".to_string());

        exchange_updates(&mut [&mut *d1.acquire_mut(), &mut *d2.acquire_mut()]);

        let nested2 = node(&d2, "test", &mut NoReporter, |txt2| {
            txt2.get(0).unwrap().node_id().unwrap()
        });
        assert_eq!(
            node(&d2, nested2, &mut NoReporter, |nested2| nested2.to_string()),
            "initial text".to_string()
        );

        mgr.undo(&mut NoReporter);
        assert_eq!(node(&d1, "test", &mut NoReporter, |txt1| txt1.len()), 0);
    }

    /// `{ s1: T }` root shape shared by github issue regression tests.
    #[derive(Debug, Deserialize, PartialEq)]
    struct Root<T> {
        s1: T,
    }

    #[test]
    fn github_issue_345() {
        // https://github.com/y-crdt/y-crdt/issues/345
        let doc = Cell::new(Doc::new());
        let mut mgr = UndoManager::with_options(Options::default());
        node(&doc, "r", &mut NoReporter, |mut n| mgr.expand_scope(&mut n));
        let client_id = doc.acquire().client_id();

        let s1 = node_with(&doc, client_id, "r", &mut mgr, |mut map| {
            map.insert_attr("s1", Delta::new()).node_id().unwrap()
        });
        mgr.capture(());

        let a = node_with(&doc, client_id, s1.clone(), &mut mgr, |mut s1| {
            s1.insert_attr("a", Delta::new().insert("a1".to_string()))
                .node_id()
                .unwrap()
        });
        mgr.capture(());

        let b = node_with(&doc, client_id, s1, &mut mgr, |mut s1| {
            s1.insert_attr("b", Delta::new().insert("b1".to_string()))
                .node_id()
                .unwrap()
        });
        mgr.capture(());

        node_with(&doc, client_id, b, &mut mgr, |mut b| {
            b.insert(1, "b2".to_string())
        });
        mgr.capture(());

        for (index, value) in [(1, "a3"), (2, "a4"), (3, "a5")] {
            node_with(&doc, client_id, a.clone(), &mut mgr, |mut a| {
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
        let map_json = || {
            node(&doc, "r", &mut NoReporter, |map| {
                map.deserialize::<Root<S1>>().unwrap().s1
            })
        };

        assert_eq!(map_json(), s1(&["a1", "a3", "a4", "a5"], &["b1", "b2"]));

        mgr.undo(&mut NoReporter); // {"s1": {"a": ["a1", "a3", "a4"], "b": ["b1", "b2"]}}
        mgr.undo(&mut NoReporter); // {"s1": {"a": ["a1", "a3"], "b": ["b1", "b2"]}}
        mgr.undo(&mut NoReporter); // {"s1": {"a": ["a1"], "b": ["b1", "b2"]}}
        mgr.undo(&mut NoReporter); // {"s1": {"a": ["a1"], "b": ["b1"]}}
        assert_eq!(map_json(), s1(&["a1"], &["b1"]));

        mgr.redo(&mut NoReporter);
        assert_eq!(map_json(), s1(&["a1"], &["b1", "b2"]));

        mgr.redo(&mut NoReporter);
        assert_eq!(map_json(), s1(&["a1", "a3"], &["b1", "b2"]));

        mgr.redo(&mut NoReporter);
        assert_eq!(map_json(), s1(&["a1", "a3", "a4"], &["b1", "b2"]));

        mgr.redo(&mut NoReporter);
        assert_eq!(map_json(), s1(&["a1", "a3", "a4", "a5"], &["b1", "b2"]));
    }

    #[test]
    fn github_issue_345_part_2() {
        // https://github.com/y-crdt/y-crdt/issues/345
        let d = Cell::new(Doc::new());
        let s1 = node(&d, "r", &mut NoReporter, |mut r| {
            r.insert_attr("s1", Delta::new()).node_id().unwrap()
        });

        let mut mgr = UndoManager::with_options(Options::default());
        node(&d, "r", &mut NoReporter, |mut n| mgr.expand_scope(&mut n));
        node(&d, s1.clone(), &mut mgr, |mut s1| {
            s1.insert_attr("b1", Delta::new().insert_attr("f1", 11));
        });
        mgr.capture(());

        node(&d, s1.clone(), &mut mgr, |mut s1| s1.remove_attr("b1"));
        mgr.capture(());

        node(&d, s1, &mut mgr, |mut s1| {
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
        let r_json = || {
            node(&d, "r", &mut NoReporter, |r| {
                r.deserialize::<Root<S1>>().unwrap().s1.b1
            })
        };

        assert_eq!(r_json(), Some(B1 { f1: 20 }));

        mgr.undo(&mut NoReporter);
        assert_eq!(r_json(), None);
        assert!(mgr.can_undo(), "should be able to undo");

        mgr.undo(&mut NoReporter);
        assert_eq!(r_json(), Some(B1 { f1: 11 }));
        assert!(mgr.can_undo(), "should be able to undo to the init state");
    }

    #[test]
    fn issue_371() {
        let doc = Cell::new(Doc::with_client_id(1));

        let s1 = node(&doc, "r", &mut NoReporter, |mut r| {
            r.insert_attr("s1", Delta::new()).node_id().unwrap()
        }); // { s1: {} }
        let b1_arr = node(&doc, s1, &mut NoReporter, |mut s1| {
            s1.insert_attr("b1", Delta::new()).node_id().unwrap()
        }); // { s1: { b1: [] } }
        let el1 = node(&doc, b1_arr.clone(), &mut NoReporter, |mut b1_arr| {
            b1_arr.insert(0, Delta::new()).node_id().unwrap()
        }); // { s1: { b1: [{}] } }
        node(&doc, el1.clone(), &mut NoReporter, |mut el1| {
            el1.insert_attr("f1", 8)
        }); // { s1: { b1: [{ f1: 8 }] } }
        node(&doc, el1.clone(), &mut NoReporter, |mut el1| {
            el1.insert_attr("f2", true)
        }); // { s1: { b1: [{ f1: 8, f2: true }] } }

        let mut mgr = UndoManager::with_options(Options::default());
        node(&doc, "r", &mut NoReporter, |mut n| mgr.expand_scope(&mut n));
        {
            let mut guard = doc.acquire_mut();
            let mut txn = guard.transact_mut();
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
        node(&doc, el1.clone(), &mut mgr, |mut el1| {
            el1.insert_attr("f2", false)
        });
        mgr.capture(());

        // { s1: { b1: [{ f1: 8, f2: false }, { f1: 13, f2: true }] } }
        node(&doc, el1, &mut mgr, |mut el1| el1.insert_attr("f2", true));
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
        let r_json = || {
            node(&doc, "r", &mut NoReporter, |r| {
                r.deserialize::<Root<S1>>().unwrap().s1.b1
            })
        };

        mgr.undo(&mut NoReporter); // { s1: { b1: [{ f1: 8, f2: false }, { f1: 13, f2: false }] } }
        assert_eq!(r_json(), [el(8, Some(false)), el(13, Some(false))]);
        mgr.undo(&mut NoReporter); // { s1: { b1: [{ f1: 8, f2: false }, { f1: 13 }] } }
        assert_eq!(r_json(), [el(8, Some(false)), el(13, None)]);
        mgr.undo(&mut NoReporter); // { s1: { b1: [{ f1: 8, f2: true }] } }
        assert_eq!(r_json(), [el(8, Some(true))]);
        assert!(!mgr.undo(&mut NoReporter).is_some()); // no more changes tracked by undo manager
    }

    #[test]
    fn issue_371_2() {
        let doc = Cell::new(Doc::with_client_id(1));
        let s1 = node(&doc, "r", &mut NoReporter, |mut r| {
            r.insert_attr("s1", Delta::new()).node_id().unwrap()
        }); // { s1:{} }
        node(&doc, s1.clone(), &mut NoReporter, |mut s1| {
            s1.insert_attr("f2", "AAA".to_string())
        }); // { s1: { f2: AAA } }
        node(&doc, s1.clone(), &mut NoReporter, |mut s1| {
            s1.insert_attr("f1", false)
        }); // { s1: { f1: false, f2: AAA } }

        let mut mgr = UndoManager::with_options(Options::default());
        node(&doc, "r", &mut NoReporter, |mut n| mgr.expand_scope(&mut n));
        node(&doc, s1.clone(), &mut mgr, |mut s1| s1.remove_attr("f2")); // { s1: { f1: false } }
        mgr.capture(());

        node(&doc, s1.clone(), &mut mgr, |mut s1| {
            s1.insert_attr("f2", "C1".to_string())
        }); // { s1: { f1: false, f2: C1 } }
        mgr.capture(());

        node(&doc, s1, &mut mgr, |mut s1| {
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
        let r_json = || {
            node(&doc, "r", &mut NoReporter, |r| {
                r.deserialize::<Root<S1>>().unwrap().s1
            })
        };

        mgr.undo(&mut NoReporter); // { s1: { f1: false, f2: C1 } }
        assert_eq!(r_json(), s1(Some("C1")));
        mgr.undo(&mut NoReporter); // { s1: { f1: false } }
        assert_eq!(r_json(), s1(None));
        mgr.undo(&mut NoReporter); // { s1: { f1: false, f2: AAA } }
        assert_eq!(r_json(), s1(Some("AAA")));
        assert!(!mgr.undo(&mut NoReporter).is_some()); // no more changes tracked by undo manager
    }

    #[test]
    fn issue_380() {
        let d = Cell::new(Doc::with_client_id(1));
        let s1 = node(&d, "r", &mut NoReporter, |mut r| {
            r.insert_attr("s1", Delta::new()).node_id().unwrap()
        }); // {r:{s1:{}}
        let b1_arr = node(&d, s1, &mut NoReporter, |mut s1| {
            s1.insert_attr("b1", Delta::new()).node_id().unwrap()
        }); // {r:{s1:{b1:[]}}

        let b1_el1 = node(&d, b1_arr.clone(), &mut NoReporter, |mut b1_arr| {
            b1_arr.insert(0, Delta::new()).node_id().unwrap()
        }); // {r:{s1:{b1:[{}]}}
        let b2_arr = node(&d, b1_el1, &mut NoReporter, |mut b1_el1| {
            b1_el1.insert_attr("b2", Delta::new()).node_id().unwrap()
        }); // {r:{s1:{b1:[{b2:[]}]}}
        let b2_arr_nest = node(&d, b2_arr, &mut NoReporter, |mut b2_arr| {
            b2_arr.insert(0, Delta::new()).node_id().unwrap()
        }); // {r:{s1:{b1:[{b2:[[]]}]}}
        node(&d, b2_arr_nest.clone(), &mut NoReporter, |mut nest| {
            nest.insert(0, 232291652)
        }); // {r:{s1:{b1:[{b2:[[232291652]]}]}}
        node(&d, b2_arr_nest.clone(), &mut NoReporter, |mut nest| {
            nest.insert(1, -30)
        }); // {r:{s1:{b1:[{b2:[[232291652, -30]]}]}}

        let mut mgr = UndoManager::with_options(Options::default());
        node(&d, "r", &mut NoReporter, |mut n| mgr.expand_scope(&mut n));

        node(&d, b2_arr_nest, &mut mgr, |mut nest| {
            nest.remove(1, 1); // {r:{s1:{b1:[{b2:[[232291652]]}]}}
            nest.insert(1, -5); // {r:{s1:{b1:[{b2:[[232291652, -5]]}]}}
        });
        mgr.capture(());

        {
            let mut guard = d.acquire_mut();
            let mut txn = guard.transact_mut();

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
        let r_json = || {
            node(&d, "r", &mut NoReporter, |r| {
                r.deserialize::<Root<S1>>().unwrap().s1.b1
            })
        };

        assert_eq!(r_json(), [b2(-6), f2("C1")]);

        mgr.undo(&mut NoReporter); // {r:{s1:{b1:[{b2:[[232291652, -5]]}]}}
        assert_eq!(r_json(), [b2(-5)]);

        mgr.undo(&mut NoReporter); // {r:{s1:{b1:[{b2:[[232291652, -30]]}]}}
        assert_eq!(r_json(), [b2(-30)]);
    }

    #[test]
    fn multi_doc_undo() {
        let mut um = UndoManager::new();
        let d1 = Cell::new(Doc::new());
        let d2 = Cell::new(Doc::new());
        node(&d1, "text", &mut NoReporter, |mut n| {
            um.expand_scope(&mut n)
        });
        node(&d2, "text", &mut NoReporter, |mut n| {
            um.expand_scope(&mut n)
        });

        node(&d1, "text", &mut um, |mut n| n.insert_text(0, "abc"));
        node(&d2, "text", &mut um, |mut n| n.insert_text(0, "xyz"));

        assert_eq!(um.undo_stack().len(), 2);
        assert!(um.can_undo(), "should be undoable (1)");
        assert!(!um.can_redo(), "should not be redoable (1)");

        um.undo(&mut NoReporter);

        assert!(um.can_undo(), "should be undoable (2)");
        assert!(um.can_redo(), "should be redoable (2)");
        assert_eq!(
            d1.acquire().transact().node("text").unwrap().to_string(),
            "abc"
        );
        assert_eq!(
            d2.acquire().transact().node("text").unwrap().to_string(),
            ""
        );

        um.undo(&mut NoReporter);

        assert!(!um.can_undo(), "should not be undoable (3)");
        assert!(um.can_redo(), "should be redoable (3)");
        assert_eq!(
            d1.acquire().transact().node("text").unwrap().to_string(),
            ""
        );
        assert_eq!(
            d2.acquire().transact().node("text").unwrap().to_string(),
            ""
        );

        // shouldn't have any effect
        assert!(
            !um.undo(&mut NoReporter).is_some(),
            "undo should have no effect"
        );
        assert!(!um.can_undo(), "should not be undoable (4)");
        assert!(um.can_redo(), "should be redoable (4)");
        assert_eq!(
            d1.acquire().transact().node("text").unwrap().to_string(),
            ""
        );
        assert_eq!(
            d2.acquire().transact().node("text").unwrap().to_string(),
            ""
        );

        um.redo(&mut NoReporter);

        assert!(um.can_undo(), "should be undoable (5)");
        assert!(um.can_redo(), "should be redoable (5)");
        assert_eq!(
            d1.acquire().transact().node("text").unwrap().to_string(),
            "abc"
        );
        assert_eq!(
            d2.acquire().transact().node("text").unwrap().to_string(),
            ""
        );

        um.redo(&mut NoReporter);

        assert_eq!(um.undo_stack().len(), 2);
        assert!(um.can_undo(), "should be undoable (6)");
        assert!(!um.can_redo(), "should not be redoable (6)");
        assert_eq!(
            d1.acquire().transact().node("text").unwrap().to_string(),
            "abc"
        );
        assert_eq!(
            d2.acquire().transact().node("text").unwrap().to_string(),
            "xyz"
        );
    }

    #[test]
    fn gc_collects_kept_children_of_a_collected_type() {
        // #667, #676: clearing the redo stack un-keeps map "p" though "number", which the undo
        // stack still holds, stays kept. Collecting "p" after a remote removal freed its branch
        // and left "number" pointing into it, so encoding read freed memory.
        let d1 = Cell::new(Doc::with_client_id(1));
        let d2 = Cell::new(Doc::with_client_id(2));
        let exchange = || {
            let mut g1 = d1.acquire_mut();
            let mut g2 = d2.acquire_mut();
            exchange_updates(&mut [&mut *g1, &mut *g2]);
        };
        node(&d1, "styles", &mut NoReporter, |mut styles| {
            styles.insert_attr(
                "p",
                In::Node(Box::new(Delta::new().insert_attr("kind", "number"))),
            );
        });
        exchange();
        let number = ID::new(ClientID::new(1), 1);

        let mut mgr = UndoManager::new();
        node(&d2, "styles", &mut NoReporter, |mut n| {
            mgr.expand_scope(&mut n)
        });
        {
            let mut txn = d2.transact_owned_mut();
            let Some(Out::Node(p)) = txn.node("styles").unwrap().attr("p") else {
                panic!("expected a nested map")
            };
            txn.node_mut(p.id).unwrap().insert_attr("kind", "bullet");
            txn.commit(&mut mgr);
        }
        mgr.undo(&mut NoReporter);
        mgr.capture(());
        node(&d2, "styles", &mut mgr, |mut n| {
            n.insert_attr("q", In::Node(Box::new(Delta::new()))); // clears the redo stack
        });

        node(&d1, "styles", &mut NoReporter, |mut n| {
            n.remove_attr("p");
        });
        exchange();

        // "number" went with its map, as in Yjs.
        assert!(d2.acquire().blocks.get_item(&number).is_none());
        d2.acquire()
            .transact()
            .encode_state_as_update_v1(&StateVector::default());
        while mgr.undo(&mut NoReporter).is_some() {}
        exchange();
        assert_eq!(
            d1.acquire().transact().node("styles").unwrap().to_json(),
            d2.acquire().transact().node("styles").unwrap().to_json()
        );
    }

    // no Reporter equivalent: UndoManager no longer observes Doc::destroy
    // #[test]
    // fn multi_doc_after_destroy() {
    //     let mut um = UndoManager::new();
    //     let d1 = Cell::new(Doc::new());
    //     let d2 = Cell::new(Doc::new());
    //     node(&d1, "text", &mut NoReporter, |mut n| um.expand_scope(&mut n));
    //     node(&d2, "text", &mut NoReporter, |mut n| um.expand_scope(&mut n));
    //     node(&d1, "text", &mut NoReporter, |mut n| um.expand_scope(&mut n)); // doing this twice for test-coverage
    //
    //     node(&d1, "text", &mut um, |mut n| n.insert_text(0, "a"));
    //     node(&d2, "text", &mut um, |mut n| n.insert_text(0, "b"));
    //
    //     assert_eq!(
    //         d1.acquire().transact().node("text").unwrap().to_string(),
    //         "a"
    //     );
    //     let d2_guid = d2.acquire().guid().clone();
    //     d2.acquire_mut().destroy(None);
    //     assert!(um.docs().all(|d| *d.acquire().guid() != d2_guid));
    //
    //     um.undo(&mut NoReporter);
    //     assert_eq!(
    //         d1.acquire().transact().node("text").unwrap().to_string(),
    //         ""
    //     );
    // }

    #[test]
    fn sticky_index_inside_redone_text_keeps_its_offset() {
        use crate::Assoc;

        let doc = Cell::new(Doc::with_client_id(1));
        let mut mgr = UndoManager::new();
        node(&doc, "test", &mut NoReporter, |mut n| {
            mgr.expand_scope(&mut n)
        });
        node(&doc, "test", &mut mgr, |mut n| {
            n.insert_text(0, "hello world")
        });
        let pos = doc
            .acquire()
            .transact()
            .node("test")
            .unwrap()
            .sticky_index(5, Assoc::After)
            .unwrap();
        let text = || doc.acquire().transact().node("test").unwrap().to_string();

        mgr.undo(&mut NoReporter);
        assert_eq!(text(), "");
        mgr.redo(&mut NoReporter);
        assert_eq!(text(), "hello world");

        let offset = pos.get_offset(&doc.acquire().transact()).unwrap();
        assert_eq!(offset.index, 5, "Yjs resolves the same position to 5");
    }
}
