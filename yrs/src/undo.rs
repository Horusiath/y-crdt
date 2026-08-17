use crate::block::ItemPtr;
use crate::cell::{Acquire, AcquireMut};
use crate::id_set::DeleteSet;
use crate::iter::TxnIterator;
use crate::node::{Node, NodePtr};
use crate::slice::BlockSlice;
use crate::sync::Clock;
use crate::transaction::Origin;
use crate::{Cell, Doc, ID, IdSet, NodeID, Observer, Transaction, TransactionMut, Uuid};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt::Formatter;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use std::sync::atomic::{AtomicPtr, Ordering};

macro_rules! define_undo_observer {
    (
        $(#[doc = $doc:literal])*
        $observe:ident, $observe_with:ident, $unobserve:ident,
        $field:ident, $($bound:tt)+
    ) => {
        $(#[doc = $doc])*
        #[cfg(feature = "sync")]
        pub fn $observe<F>(&mut self, f: F) -> crate::Subscription
        where
            F: $($bound)+ + Send + Sync + 'static,
        {
            self.inner_mut().$field.subscribe(Box::new(f))
        }

        $(#[doc = $doc])*
        #[cfg(not(feature = "sync"))]
        pub fn $observe<F>(&mut self, f: F) -> crate::Subscription
        where
            F: $($bound)+ + 'static,
        {
            self.inner_mut().$field.subscribe(Box::new(f))
        }

        #[cfg(feature = "sync")]
        pub fn $observe_with<K, F>(&mut self, key: K, f: F)
        where
            K: Into<Origin>,
            F: $($bound)+ + Send + Sync + 'static,
        {
            self.inner_mut().$field.subscribe_with(key.into(), Box::new(f))
        }

        #[cfg(not(feature = "sync"))]
        pub fn $observe_with<K, F>(&mut self, key: K, f: F)
        where
            K: Into<Origin>,
            F: $($bound)+ + 'static,
        {
            self.inner_mut().$field.subscribe_with(key.into(), Box::new(f))
        }

        pub fn $unobserve<K>(&mut self, key: K) -> bool
        where
            K: Into<Origin>,
        {
            self.inner_mut().$field.unsubscribe(&key.into())
        }
    };
}

/// Undo manager is a structure used to perform undo/redo operations over the associated shared
/// type(s).
///
/// Undo-/redo-able actions (a.k.a. [StackItem]s) are not equivalent to [TransactionMut]
/// unit of work, but rather a series of updates batched within specified time intervals
/// (see: [Options::capture_timeout_millis]) and their corresponding origins
/// (see: [Doc::transact_mut_with] and [UndoManager::include_origin]).
///
/// Individual stack item boundaries can be also specified explicitly by calling [UndoManager::reset],
/// which denotes the end of the batch.
///
/// In order to revert an operation, call [UndoManager::undo], then [UndoManager::redo] to bring it
/// back.
///
/// Users can also subscribe to change notifications observed by undo manager:
/// - [UndoManager::observe_item_added], which is fired every time a new [StackItem] is created.
/// - [UndoManager::observe_item_updated], which is fired every time when an existing [StackItem]
///    had been extended due to new document changes arriving before capture timeout for that stack
///    item finished.
/// - [UndoManager::observe_item_popped], which is fired whenever [StackItem] is being from undo
///    manager as a result of calling either [UndoManager::undo] or [UndoManager::redo] method.
pub struct UndoManager<M> {
    state: Arc<Inner<M>>,
}

#[cfg(feature = "sync")]
type UndoFn<M> = Box<dyn FnMut(&TransactionMut, &mut Event<M>) + Send + Sync + 'static>;

#[cfg(not(feature = "sync"))]
type UndoFn<M> = Box<dyn FnMut(&TransactionMut, &mut Event<M>) + 'static>;

#[cfg(feature = "sync")]
type ClearedFn = Box<dyn FnMut(&StackClearedEvent) + Send + Sync + 'static>;

#[cfg(not(feature = "sync"))]
type ClearedFn = Box<dyn FnMut(&StackClearedEvent) + 'static>;

#[cfg(feature = "sync")]
pub trait Meta: Default + Send + Sync {}
#[cfg(feature = "sync")]
impl<M> Meta for M where M: Default + Send + Sync {}

#[cfg(not(feature = "sync"))]
pub trait Meta: Default {}
#[cfg(not(feature = "sync"))]
impl<M> Meta for M where M: Default {}

struct Inner<M> {
    docs: HashMap<Arc<str>, Cell<Doc>>,
    scope: HashSet<NodePtr>,
    options: Options<M>,
    undo_stack: UndoStack<M>,
    redo_stack: UndoStack<M>,
    undoing: bool,
    redoing: bool,
    last_change: u64,
    observer_added: Observer<UndoFn<M>>,
    observer_updated: Observer<UndoFn<M>>,
    observer_popped: Observer<UndoFn<M>>,
    observer_cleared: Observer<ClearedFn>,
}

impl<M> UndoManager<M>
where
    M: Meta + 'static,
{
    /// Creates a new instance of the [UndoManager] working in a `scope` of a particular shared
    /// type and document. While it's possible for undo manager to observe multiple shared types
    /// (see: [UndoManager::expand_scope]), it can only work with a single document at the same time.
    #[cfg(not(target_family = "wasm"))]
    pub fn new() -> Self {
        Self::with_options(Options::default())
    }

    /// Creates a new instance of the [UndoManager] working in a context of a given document, but
    /// without any pre-initialize scope. While it's possible for undo manager to observe multiple
    /// shared types (see: [UndoManager::expand_scope]), it can only work with a single document
    /// at the same time.
    pub fn with_options(mut options: Options<M>) -> Self {
        let undo_stack = UndoStack(std::mem::take(&mut options.init_undo_stack));
        let redo_stack = UndoStack(std::mem::take(&mut options.init_redo_stack));
        let state = Arc::new(Inner {
            scope: HashSet::new(),
            options,
            undo_stack,
            redo_stack,
            undoing: false,
            redoing: false,
            last_change: 0,
            observer_added: Observer::new(),
            observer_updated: Observer::new(),
            observer_popped: Observer::new(),
            observer_cleared: Observer::new(),
            docs: HashMap::new(),
        });

        UndoManager { state }
    }

    /// Extends a list of shared types tracked by current undo manager by a given `scope`.
    /// Returns `true` if UndoManager scope was successfully extended.
    /// Returns `false` if a given `scope` didn't exist within provided `doc`.
    pub fn expand_scope(&mut self, doc: &Cell<Doc>, scope: NodeID) -> bool {
        let origin = Origin::from(Arc::as_ptr(&self.state) as usize);
        let inner_mut = Arc::get_mut(&mut self.state).unwrap();
        let ptr1 = AtomicPtr::new(inner_mut as *mut Inner<M>);
        let ptr2 = AtomicPtr::new(inner_mut as *mut Inner<M>);
        let (node_ptr, guid) = {
            let doc_ref = doc.acquire();
            let ptr = match doc_ref.node(scope) {
                Some(ptr) => ptr,
                None => return false,
            };
            let guid = doc_ref.guid().clone();
            (ptr, guid)
        };

        if !inner_mut.docs.contains_key(&guid) {
            inner_mut.options.tracked_origins.insert(origin.clone());

            {
                let mut doc_mut = doc.acquire_mut();
                doc_mut.observe_destroy_with(origin.clone(), move |txn, _| {
                    let ptr = ptr1.load(Ordering::Acquire);
                    let inner = unsafe { ptr.as_mut().unwrap() };
                    Self::handle_destroy(txn, inner)
                });

                doc_mut.observe_after_transaction_with(origin, move |txn| {
                    let ptr = ptr2.load(Ordering::Acquire);
                    let inner = unsafe { ptr.as_mut().unwrap() };
                    Self::handle_after_transaction(inner, txn);
                });
            }

            inner_mut.docs.insert(guid, doc.clone());
        }
        let inner = Arc::get_mut(&mut self.state).unwrap();
        inner.scope.insert(node_ptr);
        true
    }

    pub fn docs(&self) -> impl Iterator<Item = &Cell<Doc>> {
        self.state.docs.values()
    }

    fn inner_mut(&mut self) -> &mut Inner<M> {
        Arc::get_mut(&mut self.state).unwrap()
    }

    fn should_skip(inner: &Inner<M>, txn: &TransactionMut) -> bool {
        if let Some(capture_transaction) = &inner.options.capture_transaction {
            if !capture_transaction(txn) {
                return true;
            }
        }
        !inner
            .scope
            .iter()
            .any(|parent| txn.changed_parent_types().contains(parent))
            || !txn
                .origin()
                .map(|o| inner.options.tracked_origins.contains(o))
                .unwrap_or(inner.options.tracked_origins.len() == 1) // tracked origins contain only undo manager itself
    }

    fn handle_after_transaction(inner: &mut Inner<M>, txn: &mut TransactionMut) {
        if Self::should_skip(inner, txn) {
            return;
        }
        let target = txn.doc().guid().clone();
        let undoing = inner.undoing;
        let redoing = inner.redoing;
        if undoing {
            inner.last_change = 0; // next undo should not be appended to last stack item
        } else if !redoing {
            // neither undoing nor redoing: delete redoStack
            let scope = &inner.scope;
            inner.redo_stack.0.retain_mut(|stack_item| {
                if stack_item.doc != target {
                    true // retain stack items from other docs
                } else {
                    let mut deleted = stack_item.deletions.blocks();
                    while let Some(slice) = deleted.next(txn) {
                        if let Some(item) = slice.as_item() {
                            if scope.iter().any(|b| b.is_parent_of(Some(item))) {
                                item.keep(false);
                            }
                        }
                    }
                    false
                }
            });
        }

        let insertions = txn.insert_set().clone();
        let now = inner.options.timestamp.now();
        let stack = if undoing {
            &mut inner.redo_stack
        } else {
            &mut inner.undo_stack
        };
        // Does current change and the last one belong to the same doc?
        let same_doc = match stack.last() {
            None => false,
            Some(item) => item.doc == target,
        };
        let extend = !undoing
            && !redoing
            && same_doc
            && inner.last_change > 0
            && now - inner.last_change < inner.options.capture_timeout_millis;

        // should we extend the last stack item or create a new one?
        if extend {
            // append change to last stack op
            let last_op = stack.last_mut().unwrap(); // always true - we checked if stack is empty above
            last_op.deletions.merge_with(txn.delete_set().clone());
            last_op.insertions.merge_with(insertions);
        } else {
            // create a new stack op
            let doc = txn.doc().guid().clone();
            let item = StackItem::new(doc, txn.delete_set().clone(), insertions);
            stack.push(item);
        }

        if !undoing && !redoing {
            inner.last_change = now;
        }
        // make sure that deleted structs are not gc'd
        let ds = txn.delete_set().clone();
        let mut deleted = ds.blocks();
        while let Some(slice) = deleted.next(txn) {
            if let Some(item) = slice.as_item() {
                if inner.scope.iter().any(|b| b.is_parent_of(Some(item))) {
                    item.keep(true);
                }
            }
        }

        let last_op = stack.last_mut().unwrap();
        let meta = std::mem::take(&mut last_op.meta);
        let mut event = if undoing {
            Event::redo(
                meta,
                txn.origin().cloned(),
                txn.changed_parent_types().to_vec(),
            )
        } else {
            Event::undo(
                meta,
                txn.origin().cloned(),
                txn.changed_parent_types().to_vec(),
            )
        };
        if !extend {
            if inner.observer_added.has_subscribers() {
                inner.observer_added.trigger(|fun| fun(txn, &mut event));
            }
        } else {
            if inner.observer_updated.has_subscribers() {
                inner.observer_updated.trigger(|fun| fun(txn, &mut event));
            }
        }
        last_op.meta = event.meta;
    }

    fn handle_destroy(txn: &Transaction<&Doc>, inner: &mut Inner<M>) {
        let origin = Origin::from(inner as *mut Inner<M> as usize);
        // Just remove from tracked origins. The observer subscriptions will be cleaned up
        // when the Observer itself is dropped (the doc is being destroyed).
        inner.options.tracked_origins.remove(&origin);
        let doc_id = txn.doc().guid().clone();
        inner.docs.remove(&doc_id);
        inner
            .undo_stack
            .0
            .retain_mut(|stack_item| stack_item.doc != doc_id);
        inner
            .redo_stack
            .0
            .retain_mut(|stack_item| stack_item.doc != doc_id);
    }

    define_undo_observer!(
        /// Registers a callback to be called every time a new [StackItem] is created.
        observe_item_added, observe_item_added_with, unobserve_item_added,
        observer_added, FnMut(&TransactionMut, &mut Event<M>)
    );

    define_undo_observer!(
        /// Registers a callback to be called every time an existing [StackItem] is extended.
        observe_item_updated, observe_item_updated_with, unobserve_item_updated,
        observer_updated, FnMut(&TransactionMut, &mut Event<M>)
    );

    define_undo_observer!(
        /// Registers a callback to be called every time a [StackItem] is popped
        /// via [UndoManager::undo] or [UndoManager::redo].
        observe_item_popped, observe_item_popped_with, unobserve_item_popped,
        observer_popped, FnMut(&TransactionMut, &mut Event<M>)
    );

    define_undo_observer!(
        /// Registers a callback to be called every time undo/redo stacks are cleared.
        /// The callback receives two booleans: `(undo_stack_cleared, redo_stack_cleared)`.
        observe_stack_cleared, observe_stack_cleared_with, unobserve_stack_cleared,
        observer_cleared, FnMut(&StackClearedEvent)
    );

    /// Extends a list of origins tracked by current undo manager by given `origin`. Origin markers
    /// can be assigned to updates executing in a scope of a particular transaction
    /// (see: [Doc::transact_mut_with]).
    pub fn include_origin<O>(&mut self, origin: O)
    where
        O: Into<Origin>,
    {
        let inner = Arc::get_mut(&mut self.state).unwrap();
        inner.options.tracked_origins.insert(origin.into());
    }

    /// Removes an `origin` from the list of origins tracked by a current undo manager.
    pub fn exclude_origin<O>(&mut self, origin: O)
    where
        O: Into<Origin>,
    {
        let inner = Arc::get_mut(&mut self.state).unwrap();
        inner.options.tracked_origins.remove(&origin.into());
    }

    /// Clears all [StackItem]s stored within current UndoManager undo AND redo stacks, effectively
    /// resetting its state.
    ///
    /// # Deadlocks
    ///
    /// In order to perform its function, this method must guarantee that underlying document store
    /// is not being modified by another running `TransactionMut`. It does so by acquiring
    /// a read-only transaction itself. If transaction couldn't be acquired (because another
    /// read-write transaction is in progress), it will hold current thread until, acquisition is
    /// available.
    pub fn clear_all(&mut self) {
        self.clear_internal(true, true)
    }

    fn clear_internal(&mut self, clear_undo: bool, clear_redo: bool) {
        let inner = Arc::get_mut(&mut self.state).unwrap();

        let undo_cleared =
            clear_undo && Self::clear_stack(&inner.scope, &inner.docs, &mut inner.undo_stack);
        let redo_cleared =
            clear_redo && Self::clear_stack(&inner.scope, &inner.docs, &mut inner.redo_stack);

        if undo_cleared || redo_cleared {
            let e = StackClearedEvent::new(undo_cleared, redo_cleared);
            inner.observer_cleared.trigger(|f| f(&e));
        }
    }

    /// Clears all [StackItem]s stored within current UndoManager undo stack alone, leaving redo
    /// stack untouched.
    ///
    /// # Deadlocks
    ///
    /// In order to perform its function, this method must guarantee that underlying document store
    /// is not being modified by another running `TransactionMut`. It does so by acquiring
    /// a read-only transaction itself. If transaction couldn't be acquired (because another
    /// read-write transaction is in progress), it will hold current thread until, acquisition is
    /// available.
    pub fn clear_undo(&mut self) {
        self.clear_internal(true, false)
    }

    /// Clears all [StackItem]s stored within current UndoManager redo stack alone, leaving undo
    /// stack untouched.
    ///
    /// # Deadlocks
    ///
    /// In order to perform its function, this method must guarantee that underlying document store
    /// is not being modified by another running `TransactionMut`. It does so by acquiring
    /// a read-only transaction itself. If transaction couldn't be acquired (because another
    /// read-write transaction is in progress), it will hold current thread until, acquisition is
    /// available.
    pub fn clear_redo(&mut self) {
        self.clear_internal(false, true)
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

    pub fn as_origin(&self) -> Origin {
        let mgr_ptr: *const Inner<M> = &*self.state;
        Origin::from(mgr_ptr as usize)
    }

    /// [UndoManager] merges undo stack items if they were created withing the time gap smaller than
    /// [Options::capture_timeout_millis]. You can call this method so that the next stack item won't be
    /// merged.
    ///
    /// Example:
    /// ```rust,ignore
    /// use yrs::{Doc, GetString, Text, UndoManager};
    /// let mut doc = Doc::new();
    ///
    /// // without UndoManager::stop
    /// let txt = doc.get_or_insert_text("no-stop");
    /// let mut mgr = UndoManager::new();
    /// mgr.expand_scope(&mut doc, &txt);
    /// txt.insert(&mut doc.transact_mut(), 0, "a");
    /// txt.insert(&mut doc.transact_mut(), 1, "b");
    /// mgr.undo_blocking();
    /// txt.get_string(&doc.transact()); // => "" (note that 'ab' was removed)
    ///
    /// // with UndoManager::stop
    /// let txt = doc.get_or_insert_text("with-stop");
    /// let mut mgr = UndoManager::new();
    /// mgr.expand_scope(&mut doc, &txt);
    /// txt.insert(&mut doc.transact_mut(), 0, "a");
    /// mgr.reset();
    /// txt.insert(&mut doc.transact_mut(), 1, "b");
    /// mgr.undo_blocking();
    /// txt.get_string(&doc.transact()); // => "a" (note that only 'b' was removed)
    /// ```
    pub fn reset(&mut self) {
        let inner = Arc::get_mut(&mut self.state).unwrap();
        inner.last_change = 0;
    }

    /// Are there any undo steps available?
    pub fn can_undo(&self) -> bool {
        !self.state.undo_stack.is_empty()
    }

    /// Returns a list of [StackItem]s stored within current undo manager responsible for performing
    /// potential undo operations.
    pub fn undo_stack(&self) -> &[StackItem<M>] {
        &self.state.undo_stack.0
    }

    /// Returns a list of [StackItem]s stored within current undo manager responsible for performing
    /// potential redo operations.
    pub fn redo_stack(&self) -> &[StackItem<M>] {
        &self.state.redo_stack.0
    }

    /// Undo last action tracked by current undo manager. Actions (a.k.a. [StackItem]s) are groups
    /// of updates performed in a given time range - they also can be separated explicitly by
    /// calling [UndoManager::reset].
    ///
    /// Successful execution returns a boolean value telling if an undo call has performed any changes.
    ///
    /// # Deadlocks
    ///
    /// This method requires exclusive access to underlying document store. This means that
    /// no other transaction on that same document can be active while calling this method.
    /// Otherwise, it may cause a deadlock.
    ///
    /// See also: [UndoManager::try_undo] and [UndoManager::undo_blocking].
    pub async fn undo(&mut self) -> bool {
        let inner = self.inner_mut();
        Self::pop(inner, true).await
    }

    /// Undo last action tracked by current undo manager. Actions (a.k.a. [StackItem]s) are groups
    /// of updates performed in a given time range - they also can be separated explicitly by
    /// calling [UndoManager::reset].
    ///
    /// Successful execution returns a boolean value telling if an undo call has performed any changes.
    ///
    /// # Deadlocks
    ///
    /// This method requires exclusive access to underlying document store. This means that
    /// no other transaction on that same document can be active while calling this method.
    /// Otherwise, it may cause a deadlock.
    ///
    /// See also: [UndoManager::try_undo] and [UndoManager::undo].
    pub fn undo_blocking(&mut self) -> bool {
        let inner = self.inner_mut();
        Self::pop_blocking(inner, true)
    }

    /// Are there any redo steps available?
    pub fn can_redo(&self) -> bool {
        !self.state.redo_stack.is_empty()
    }

    /// Redo'es last action previously undo'ed by current undo manager. Actions
    /// (a.k.a. [StackItem]s) are groups of updates performed in a given time range - they also can
    /// be separated explicitly by calling [UndoManager::reset].
    ///
    /// Successful execution returns a boolean value telling if an undo call has performed any changes.
    ///
    /// # Deadlocks
    ///
    /// This method requires exclusive access to underlying document store. This means that
    /// no other transaction on that same document can be active while calling this method.
    /// Otherwise, it may cause a deadlock.
    ///
    /// See also: [UndoManager::try_redo] and [UndoManager::redo_blocking].
    pub async fn redo(&mut self) -> bool {
        let inner = self.inner_mut();
        Self::pop(inner, false).await
    }

    /// Redo'es last action previously undo'ed by current undo manager. Actions
    /// (a.k.a. [StackItem]s) are groups of updates performed in a given time range - they also can
    /// be separated explicitly by calling [UndoManager::reset].
    ///
    /// Successful execution returns a boolean value telling if an undo call has performed any changes.
    ///
    /// # Deadlocks
    ///
    /// This method requires exclusive access to underlying document store. This means that
    /// no other transaction on that same document can be active while calling this method.
    /// Otherwise, it may cause a deadlock.
    ///
    /// See also: [UndoManager::try_redo] and [UndoManager::redo_blocking].
    pub fn redo_blocking(&mut self) -> bool {
        let inner = self.inner_mut();
        Self::pop_blocking(inner, false)
    }

    //TODO: async undo/redo needs redesign - AsyncTransact has been removed
    // and Doc is no longer Clone/Send/Sync.
    async fn pop(state: &mut Inner<M>, undoing: bool) -> bool {
        // Fall back to blocking implementation since async transactions
        // are no longer available.
        Self::pop_blocking(state, undoing)
    }

    fn pop_blocking(state: &mut Inner<M>, undoing: bool) -> bool {
        state.undoing = undoing;
        state.redoing = !undoing;
        let origin = Origin::from(state as *mut Inner<M> as usize);
        let (stack, other) = if undoing {
            (&mut state.undo_stack, &state.redo_stack)
        } else {
            (&mut state.redo_stack, &state.undo_stack)
        };
        let mut changed = false;
        while let Some(item) = stack.pop() {
            if let Some(cell) = state.docs.get(&item.doc) {
                let mut doc = cell.acquire_mut();
                let txn = doc.transact_mut_with(origin.clone());

                if Self::try_process(
                    item,
                    txn,
                    stack,
                    other,
                    &state.scope,
                    &mut state.observer_popped,
                    undoing,
                    origin.clone(),
                ) {
                    changed = true;
                    break;
                }
            }
        }
        state.undoing = false;
        state.redoing = false;
        changed
    }

    fn try_process(
        item: StackItem<M>,
        mut txn: TransactionMut,
        stack: &mut UndoStack<M>,
        other: &UndoStack<M>,
        scope: &HashSet<NodePtr>,
        observer_popped: &mut Observer<UndoFn<M>>,
        undoing: bool,
        origin: Origin,
    ) -> bool {
        let mut to_redo = HashSet::<ItemPtr>::new();
        let mut to_delete = Vec::<ItemPtr>::new();
        let mut change_performed = false;

        let deleted: Vec<_> = item.insertions.blocks().collect(&txn);
        for slice in deleted {
            if let BlockSlice::Item(slice) = slice {
                let mut item = txn.doc.materialize(slice);
                if item.redone.is_some() {
                    let slice = match txn.doc.follow_redone(item.id()) {
                        Some(slice) => slice,
                        None => return false,
                    };
                    item = txn.doc.materialize(slice);
                }

                if !item.is_deleted() && scope.iter().any(|b| b.is_parent_of(Some(item))) {
                    to_delete.push(item);
                }
            }
        }

        let mut deleted = item.deletions.blocks();
        while let Some(slice) = deleted.next(&txn) {
            if let BlockSlice::Item(slice) = slice {
                let ptr = txn.doc.materialize(slice);
                if scope.iter().any(|b| b.is_parent_of(Some(ptr)))
                    && !item.insertions.contains(ptr.id())
                // Never redo structs in stackItem.insertions because they were created and deleted in the same capture interval.
                {
                    to_redo.insert(ptr);
                }
            }
        }

        for &ptr in to_redo.iter() {
            let mut ptr = ptr;
            change_performed |= ptr
                .redo(&mut txn, &to_redo, &item.insertions, stack, other)
                .is_some();
        }

        // We want to delete in reverse order so that children are deleted before
        // parents, so we have more information available when items are filtered.
        for &item in to_delete.iter().rev() {
            // if self.options.delete_filter(item) {
            txn.delete(item);
            change_performed = true;
        }

        txn.commit();
        if change_performed {
            txn.commit();
            let mut e = if undoing {
                Event::undo(item.meta, Some(origin), txn.changed_parent_types().to_vec())
            } else {
                Event::redo(item.meta, Some(origin), txn.changed_parent_types().to_vec())
            };
            if observer_popped.has_subscribers() {
                observer_popped.trigger(|fun| fun(&txn, &mut e));
            }
            true
        } else {
            false
        }
    }
}

impl<M: std::fmt::Debug> std::fmt::Debug for UndoManager<M> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let mut s = f.debug_struct("UndoManager");
        let state = &self.state;
        s.field("scope", &state.scope);
        s.field("tracked_origins", &state.options.tracked_origins);
        if !state.undo_stack.is_empty() {
            s.field("undo", &state.undo_stack);
        }
        if !state.redo_stack.is_empty() {
            s.field("redo", &state.redo_stack);
        }
        s.finish()
    }
}

impl<M> Drop for UndoManager<M> {
    fn drop(&mut self) {
        let origin = Origin::from(Arc::as_ptr(&self.state) as usize);
        if let Some(state) = Arc::get_mut(&mut self.state) {
            for cell in state.docs.values() {
                let mut doc = cell.acquire_mut();
                doc.unobserve_destroy(origin.clone());
                doc.unobserve_after_transaction(origin.clone());
            }
        }
    }
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
    /// Undo-/redo-able updates are grouped together in time-constrained snapshots. This field
    /// determines the period of time, every snapshot will be automatically made in.
    pub capture_timeout_millis: u64,

    /// List of origins tracked by corresponding [UndoManager].
    /// If provided, it will track only updates made within transactions of specific origin.
    /// If not provided, it will track only updates made within transaction with no origin defined.
    pub tracked_origins: HashSet<Origin>,

    /// Custom logic decider, that along with [tracked_origins] can be used to determine if
    /// transaction changes should be captured or not.
    pub capture_transaction: Option<CaptureTransactionFn>,

    /// Custom clock function, that can be used to generate timestamps used by
    /// [Options::capture_timeout_millis].
    pub timestamp: Arc<dyn Clock>,

    /// Initial undo stack that can be pre-filled with some operations that can be undone.
    pub init_undo_stack: Vec<StackItem<M>>,

    /// Initial redo stack that can be pre-filled with some operations that can be redone.
    pub init_redo_stack: Vec<StackItem<M>>,
}

pub type CaptureTransactionFn = Arc<dyn Fn(&TransactionMut) -> bool + Send + Sync + 'static>;

#[cfg(not(target_family = "wasm"))]
impl<M> Default for Options<M> {
    fn default() -> Self {
        Options {
            capture_timeout_millis: 500,
            tracked_origins: HashSet::new(),
            capture_transaction: None,
            timestamp: Arc::new(crate::sync::time::SystemClock),
            init_undo_stack: Vec::new(),
            init_redo_stack: Vec::new(),
        }
    }
}

/// A unit of work for the [UndoManager]. It contains a compressed information about all updates and
/// deletions tracked by a corresponding undo manager. Whenever an [UndoManger::undo] or
/// [UndoManager::redo] methods are called a last [StackItem] is being used to modify a state of
/// the document.
///
/// Stack items are stored internally by undo manager and created automatically whenever a new
/// update from tracked shared type and transaction of tracked origin has been committed within
/// a threshold specified by [Options::capture_timeout_millis] time window since the previous stack
/// item creation. They can also be created explicitly by calling [UndoManager::reset], which marks
/// the end of the last stack item batch.
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

#[derive(Debug, Clone, Serialize)]
pub struct StackClearedEvent {
    pub undo_stack_cleared: bool,
    pub redo_stack_cleared: bool,
}

impl StackClearedEvent {
    pub fn new(undo_stack_cleared: bool, redo_stack_cleared: bool) -> Self {
        Self {
            undo_stack_cleared,
            redo_stack_cleared,
        }
    }
}

#[derive(Debug)]
pub struct Event<M> {
    meta: M,
    origin: Option<Origin>,
    kind: EventKind,
    changed_parent_types: Vec<NodePtr>,
}

impl<M> Event<M> {
    fn undo(meta: M, origin: Option<Origin>, changed_parent_types: Vec<NodePtr>) -> Self {
        Event {
            meta,
            origin,
            changed_parent_types,
            kind: EventKind::Undo,
        }
    }

    fn redo(meta: M, origin: Option<Origin>, changed_parent_types: Vec<NodePtr>) -> Self {
        Event {
            meta,
            origin,
            changed_parent_types,
            kind: EventKind::Redo,
        }
    }

    pub fn meta(&self) -> &M {
        &self.meta
    }

    pub fn meta_mut(&mut self) -> &mut M {
        &mut self.meta
    }

    /// Checks if given shared collection has changed in the scope of currently notified update.
    pub fn has_changed<T: AsRef<Node>>(&self, target: &T) -> bool {
        let ptr = NodePtr::from(target.as_ref());
        self.changed_parent_types.contains(&ptr)
    }

    /// Returns a transaction origin related to this update notification.
    pub fn origin(&self) -> Option<&Origin> {
        self.origin.as_ref()
    }

    /// Returns an enum informing if current update is result of undo or redo operation.
    pub fn kind(&self) -> EventKind {
        self.kind
    }

    /// Returns info about all changed shared collections.
    pub fn changed_parent_types(&self) -> &[NodePtr] {
        &self.changed_parent_types
    }
}

/// Enum which informs if correlated [Event] was produced as a result of either undo or redo
/// operation over [UndoManager].
#[repr(u8)]
#[derive(Debug, Clone, Copy, Ord, PartialOrd, Eq, PartialEq)]
pub enum EventKind {
    /// Referenced event was result of [UndoManager::undo] operation.
    Undo,
    /// Referenced event was result of [UndoManager::redo] operation.
    Redo,
}

#[cfg(test)]
mod test {
    use std::collections::HashMap;
    use std::convert::TryInto;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::block::ClientID;
    use crate::cell::{Acquire, AcquireMut};

    use crate::node::{Attrs, NodePtr};
    use crate::test_utils::exchange_updates;
    use crate::undo::{Options, StackItem};
    use crate::updates::decoder::Decode;
    use crate::{
        Any, Cell, Delta, DeltaOptions, Doc, In, NodeID, NodeRef, Origin, Out, StateVector,
        TransactionMut, UndoManager, Update, any,
    };

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
        mgr.expand_scope(&d1, "test".into());

        let d2 = Cell::new(Doc::with_client_id(2));

        // items that are added & deleted in the same transaction won't be undo
        d1.acquire_mut()
            .transact_mut()
            .node_mut("test")
            .unwrap()
            .insert_text(0, "test");
        d1.acquire_mut()
            .transact_mut()
            .node_mut("test")
            .unwrap()
            .remove(0, 4);
        mgr.undo_blocking();
        assert_eq!(
            d1.acquire().transact().node("test").unwrap().to_string(),
            ""
        );

        // follow redone items
        d1.acquire_mut()
            .transact_mut()
            .node_mut("test")
            .unwrap()
            .insert_text(0, "a");
        mgr.reset();
        d1.acquire_mut()
            .transact_mut()
            .node_mut("test")
            .unwrap()
            .remove(0, 1);
        mgr.reset();
        mgr.undo_blocking();
        assert_eq!(
            d1.acquire().transact().node("test").unwrap().to_string(),
            "a"
        );
        mgr.undo_blocking();
        assert_eq!(
            d1.acquire().transact().node("test").unwrap().to_string(),
            ""
        );

        d1.acquire_mut()
            .transact_mut()
            .node_mut("test")
            .unwrap()
            .insert_text(0, "abc");
        d2.acquire_mut()
            .transact_mut()
            .node_mut("test")
            .unwrap()
            .insert_text(0, "xyz");

        {
            let mut g1 = d1.acquire_mut();
            let mut g2 = d2.acquire_mut();
            exchange_updates(&mut [&mut *g1, &mut *g2]);
        }
        mgr.undo_blocking();
        assert_eq!(
            d1.acquire().transact().node("test").unwrap().to_string(),
            "xyz"
        );
        mgr.redo_blocking();
        assert_eq!(
            d1.acquire().transact().node("test").unwrap().to_string(),
            "abcxyz"
        );

        {
            let mut g1 = d1.acquire_mut();
            let mut g2 = d2.acquire_mut();
            exchange_updates(&mut [&mut *g1, &mut *g2]);
        }

        d2.acquire_mut()
            .transact_mut()
            .node_mut("test")
            .unwrap()
            .remove(0, 1);

        {
            let mut g1 = d1.acquire_mut();
            let mut g2 = d2.acquire_mut();
            exchange_updates(&mut [&mut *g1, &mut *g2]);
        }

        mgr.undo_blocking();
        assert_eq!(
            d1.acquire().transact().node("test").unwrap().to_string(),
            "xyz"
        );
        mgr.redo_blocking();
        {
            let mut d1 = d1.acquire_mut();
            let mut t1 = d1.transact_mut();
            let mut txt1 = t1.node_mut("test").unwrap();
            assert_eq!(txt1.to_string(), "bcxyz");

            let bold = Attrs::from([("bold".into(), true.into())]);
            txt1.format(1, 3, bold.clone());
            let diff = txt1.to_delta(&DeltaOptions::default());
            assert_eq!(
                diff,
                vec![
                    Delta::out().insert_text("b"),
                    Delta::out().insert_text_with("cxy", bold),
                    Delta::out().insert_text("z"),
                ]
            );
        }
        mgr.undo_blocking();
        mgr.redo_blocking();
    }

    #[test]
    fn double_undo() {
        let doc = Cell::new(Doc::with_client_id(1));
        doc.acquire_mut()
            .transact_mut()
            .node_mut("test")
            .unwrap()
            .insert_text(0, "1221");

        let mut mgr = UndoManager::new();
        mgr.expand_scope(&doc, "test".into());
        doc.acquire_mut()
            .transact_mut()
            .node_mut("test")
            .unwrap()
            .insert_text(2, "3");
        doc.acquire_mut()
            .transact_mut()
            .node_mut("test")
            .unwrap()
            .insert_text(3, "3");

        mgr.undo_blocking();
        mgr.undo_blocking();

        doc.acquire_mut()
            .transact_mut()
            .node_mut("test")
            .unwrap()
            .insert_text(2, "3");
        assert_eq!(
            doc.acquire().transact().node("test").unwrap().to_string(),
            "12321"
        );
    }

    #[test]
    fn undo_map() {
        let d1 = Cell::new(Doc::with_client_id(1));

        d1.acquire_mut()
            .transact_mut()
            .node_mut("test")
            .unwrap()
            .insert_attr("a", 0);
        let mut mgr = UndoManager::new();
        mgr.expand_scope(&d1, "test".into());
        d1.acquire_mut()
            .transact_mut()
            .node_mut("test")
            .unwrap()
            .insert_attr("a", 1);
        mgr.undo_blocking();
        assert_eq!(
            d1.acquire()
                .transact()
                .node("test")
                .unwrap()
                .attr("a")
                .unwrap(),
            0.into()
        );
        mgr.redo_blocking();
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
        d1.acquire_mut()
            .transact_mut()
            .node_mut("test")
            .unwrap()
            .insert_attr("b", "initial".to_string());
        mgr.reset();
        d1.acquire_mut()
            .transact_mut()
            .node_mut("test")
            .unwrap()
            .insert_attr("b", "val1".to_string());
        d1.acquire_mut()
            .transact_mut()
            .node_mut("test")
            .unwrap()
            .insert_attr("b", "val2".to_string());
        mgr.reset();
        mgr.undo_blocking();
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

    fn node<F, T>(cell: &Cell<Doc>, node: impl Into<NodeID>, f: F) -> T
    where
        F: FnOnce(NodeRef<&mut TransactionMut<'_>>) -> T,
    {
        let mut doc = cell.acquire_mut();
        let mut txn = doc.transact_mut();
        let node = txn.node_mut(node.into()).unwrap();
        f(node)
    }

    /// Same as [node], but the underlying transaction is tagged with a given `origin`.
    fn node_with<F, T>(
        cell: &Cell<Doc>,
        origin: impl Into<Origin>,
        node: impl Into<NodeID>,
        f: F,
    ) -> T
    where
        F: FnOnce(NodeRef<&mut TransactionMut<'_>>) -> T,
    {
        let mut doc = cell.acquire_mut();
        let mut txn = doc.transact_mut_with(origin);
        let node = txn.node_mut(node.into()).unwrap();
        f(node)
    }

    #[test]
    // TODO(unified-api): nested MapPrelim/ArrayPrelim + Array::insert_range + Out::cast have no NodeRef equivalent yet
    fn undo_array() {
        let array = NodeID::root("test");
        let d1 = Cell::new(Doc::with_client_id(1));

        let d2 = Cell::new(Doc::with_client_id(2));

        let mut mgr = UndoManager::new();
        mgr.expand_scope(&d1, array.clone());
        node(&d1, "test", |mut array| array.insert_range(0, [1, 2, 3]));
        node(&d2, "test", |mut array| array.insert_range(0, [4, 5, 6]));

        exchange_updates(&mut [&mut *d1.acquire_mut(), &mut *d2.acquire_mut()]);

        node(&d1, "test", |mut array| {
            assert_eq!(array.to_json(), vec![1, 2, 3, 4, 5, 6].into())
        });

        mgr.undo_blocking();

        node(&d1, "test", |mut array| {
            assert_eq!(array.to_json(), vec![4, 5, 6].into())
        });

        mgr.redo_blocking();

        node(&d1, "test", |mut array| {
            assert_eq!(array.to_json(), vec![1, 2, 3, 4, 5, 6].into())
        });

        exchange_updates(&mut [&mut *d1.acquire_mut(), &mut *d2.acquire_mut()]);

        node(&d2, "test", |mut array| {
            array.remove(0, 1); // user2 deletes [1]
        });

        exchange_updates(&mut [&mut *d1.acquire_mut(), &mut *d2.acquire_mut()]);

        mgr.undo_blocking();
        node(&d2, "test", |mut array| {
            assert_eq!(array.to_json(), vec![4, 5, 6].into());
        });

        mgr.redo_blocking();

        node(&d1, "test", |mut array| {
            assert_eq!(array.to_json(), vec![2, 3, 4, 5, 6].into());
            array.remove(0, 5);
        });

        // test nested structure
        let map = node(&d1, "test", |mut array| {
            let Out::Node(map) = array.insert(0, Delta::new()) else {
                unreachable!()
            };
            assert_eq!(array.to_json(), Any::from_json(r#"[{}]"#).unwrap());
            map
        });

        mgr.reset();

        node(&d1, map.clone(), |mut map| {
            map.insert_attr("a", 1);
        });
        node(&d1, "test", |mut array| {
            assert_eq!(array.to_json(), Any::from_json(r#"[{"a":1}]"#).unwrap());
        });

        mgr.undo_blocking();

        node(&d1, "test", |mut array| {
            let actual = array.to_json();
            let expected = Any::from_json(r#"[{}]"#).unwrap();
            assert_eq!(actual, expected);
        });

        mgr.undo_blocking();

        node(&d1, "test", |mut array| {
            assert_eq!(array.to_json(), vec![2, 3, 4, 5, 6].into());
        });

        mgr.redo_blocking();

        node(&d1, "test", |mut array| {
            let actual = array.to_json();
            let expected = Any::from_json(r#"[{}]"#).unwrap();
            assert_eq!(actual, expected);
        });

        mgr.redo_blocking();
        node(&d1, "test", |array| {
            let actual = array.to_json();
            let expected = Any::from_json(r#"[{"a":1}]"#).unwrap();
            assert_eq!(actual, expected);
        });

        exchange_updates(&mut [&mut *d1.acquire_mut(), &mut *d2.acquire_mut()]);

        let map2 = node(&d2, "test", |array| {
            array.get(0).unwrap().node_id().unwrap()
        });
        node(&d2, map2, |mut map| {
            map.insert_attr("b", 2);
        });

        exchange_updates(&mut [&mut *d1.acquire_mut(), &mut *d2.acquire_mut()]);

        node(&d1, "test", |array| {
            let expected = Any::from_json(r#"[{"a":1,"b":2}]"#).unwrap();
            assert_eq!(array.to_json(), expected);
        });

        mgr.undo_blocking();
        node(&d1, "test", |array| {
            let expected = Any::from_json(r#"[{"b":2}]"#).unwrap();
            assert_eq!(array.to_json(), expected);
        });

        mgr.undo_blocking();
        node(&d1, "test", |array| {
            assert_eq!(array.to_json(), vec![2, 3, 4, 5, 6].into());
        });

        mgr.redo_blocking();
        node(&d1, "test", |array| {
            let expected = Any::from_json(r#"[{"b":2}]"#).unwrap();
            assert_eq!(array.to_json(), expected);
        });

        mgr.redo_blocking();
        node(&d1, "test", |array| {
            let expected = Any::from_json(r#"[{"a":1,"b":2}]"#).unwrap();
            assert_eq!(array.to_json(), expected);
        });
    }

    #[test]
    fn undo_xml() {
        let d1 = Cell::new(Doc::with_client_id(1));
        let xml1 = node(&d1, "xml", |mut frag| {
            frag.insert(0, Delta::with_name("undefined"))
                .node_id()
                .unwrap()
        });

        let mut mgr = UndoManager::new();
        mgr.expand_scope(&d1, xml1.clone());
        let child = node(&d1, xml1.clone(), |mut xml| {
            xml.insert(0, Delta::with_name("p")).node_id().unwrap()
        });
        let text_child = node(&d1, child, |mut child| {
            child
                .insert(0, Delta::new().insert_text("content"))
                .node_id()
                .unwrap()
        });

        let as_string = || node(&d1, xml1.clone(), |xml| xml.to_string());

        assert_eq!(as_string(), "<undefined><p>content</p></undefined>");
        // format textchild and revert that change
        mgr.reset();
        node(&d1, text_child, |mut text| {
            text.format(3, 4, Attrs::from([("bold".into(), any!({}))]))
        });
        assert_eq!(
            as_string(),
            "<undefined><p>con<bold>tent</bold></p></undefined>"
        );
        mgr.undo_blocking();
        assert_eq!(as_string(), "<undefined><p>content</p></undefined>");
        mgr.redo_blocking();
        assert_eq!(
            as_string(),
            "<undefined><p>con<bold>tent</bold></p></undefined>"
        );
        node(&d1, xml1.clone(), |mut xml| xml.remove(0, 1));
        assert_eq!(as_string(), "<undefined></undefined>");
        mgr.undo_blocking();
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
        mgr.expand_scope(&doc, "test".into());

        let result = Arc::new(AtomicUsize::new(0));
        let counter = AtomicUsize::new(1);

        let txt_clone = txt.clone();
        let _sub1 = mgr.observe_item_added(move |_, e| {
            assert!(e.has_changed(&txt_clone));
            let c = counter.fetch_add(1, Ordering::SeqCst);
            let e = e.meta_mut().entry("test".to_string()).or_default();
            *e = c;
        });

        let txt_clone = txt.clone();
        let result_clone = result.clone();
        let _sub2 = mgr.observe_item_popped(move |_, e| {
            assert!(e.has_changed(&txt_clone));
            if let Some(&v) = e.meta_mut().get("test") {
                result_clone.store(v, Ordering::Relaxed);
            }
        });

        doc.acquire_mut()
            .transact_mut()
            .node_mut("test")
            .unwrap()
            .insert_text(0, "abc");
        mgr.undo_blocking();
        assert_eq!(result.load(Ordering::SeqCst), 1);
        mgr.redo_blocking();
        assert_eq!(result.load(Ordering::SeqCst), 2);
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
            m1.expand_scope(&d1, "test".into());

            let _sub1 = m1.observe_item_added(move |_, e| {
                e.meta_mut().entry("test".to_string()).or_default();
            });

            d1.acquire_mut()
                .transact_mut()
                .node_mut("test")
                .unwrap()
                .insert_text(0, "c");
            m1.reset();
            d1.acquire_mut()
                .transact_mut()
                .node_mut("test")
                .unwrap()
                .insert_text(0, "b");
            m1.reset();
            d1.acquire_mut()
                .transact_mut()
                .node_mut("test")
                .unwrap()
                .insert_text(0, "a");

            m1.undo_blocking();
            assert_eq!(
                d1.acquire().transact().node("test").unwrap().to_string(),
                "bc"
            );
            m1.redo_blocking();
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
        m2.expand_scope(&d2, "test".into());

        m2.undo_blocking();
        assert_eq!(
            d2.acquire().transact().node("test").unwrap().to_string(),
            "bc"
        );
        m2.redo_blocking();
        assert_eq!(
            d2.acquire().transact().node("test").unwrap().to_string(),
            "abc"
        );
    }

    #[test]
    fn undo_until_change_performed() {
        let d1 = Cell::new(Doc::with_client_id(1));
        let d2 = Cell::new(Doc::with_client_id(2));

        let map1a = node(&d1, "array", |mut arr| {
            arr.push_back(Delta::new().insert_attr("hello", "world".to_string()))
                .node_id()
                .unwrap()
        });
        let map1b = node(&d1, "array", |mut arr| {
            arr.push_back(Delta::new().insert_attr("key", "value".to_string()))
                .node_id()
                .unwrap()
        });

        exchange_updates(&mut [&mut *d1.acquire_mut(), &mut *d2.acquire_mut()]);

        let mut mgr1 = UndoManager::new();
        mgr1.expand_scope(&d1, "array".into());
        let d1_client_id = d1.acquire().client_id();
        mgr1.include_origin(d1_client_id);

        let mut mgr2 = UndoManager::new();
        mgr2.expand_scope(&d2, "array".into());
        let d2_client_id = d2.acquire().client_id();
        mgr2.include_origin(d2_client_id);

        node_with(&d1, d1_client_id, map1b.clone(), |mut map| {
            map.insert_attr("key", "value modified".to_string());
        });

        exchange_updates(&mut [&mut *d1.acquire_mut(), &mut *d2.acquire_mut()]);
        mgr1.reset();

        node_with(&d1, d1_client_id, map1a, |mut map| {
            map.insert_attr("hello", "world modified".to_string());
        });

        exchange_updates(&mut [&mut *d1.acquire_mut(), &mut *d2.acquire_mut()]);

        node_with(&d2, d2_client_id, "array", |mut arr| arr.remove(0, 1));

        exchange_updates(&mut [&mut *d1.acquire_mut(), &mut *d2.acquire_mut()]);
        mgr2.undo_blocking();

        exchange_updates(&mut [&mut *d1.acquire_mut(), &mut *d2.acquire_mut()]);
        mgr1.undo_blocking();
        exchange_updates(&mut [&mut *d1.acquire_mut(), &mut *d2.acquire_mut()]);

        node(&d1, map1b, |map| {
            assert_eq!(map.attr("key"), Some("value".into()));
        });
    }

    #[test]
    fn nested_undo() {
        // This issue has been reported in https://github.com/yjs/yjs/issues/317
        let doc = Cell::new(Doc::with_options(crate::doc::Options {
            skip_gc: true,
            client_id: ClientID::new(1),
            ..crate::doc::Options::default()
        }));
        let mut mgr = UndoManager::with_options({
            let mut o = Options::default();
            o.capture_timeout_millis = 0;
            o
        });
        mgr.expand_scope(&doc, "map".into());

        let blocks = |text: &str| Delta::new().insert_attr("text", text.to_owned());
        let text = node(&doc, "map", |mut design| {
            let content = Delta::new().insert_attr("blocks", blocks("Type something"));
            design.insert_attr("text", content).node_id().unwrap()
        });

        node(&doc, text.clone(), |mut text| {
            text.insert_attr("blocks", blocks("Something"));
        });

        node(&doc, text, |mut text| {
            text.insert_attr("blocks", blocks("Something else"));
        });

        let design_json = || node(&doc, "map", |design| design.to_json());

        assert_eq!(
            design_json(),
            Any::from_json(r#"{ "text": { "blocks": { "text": "Something else" } } }"#).unwrap()
        );
        mgr.undo_blocking();
        assert_eq!(
            design_json(),
            Any::from_json(r#"{ "text": { "blocks": { "text": "Something" } } }"#).unwrap()
        );
        mgr.undo_blocking();
        assert_eq!(
            design_json(),
            Any::from_json(r#"{ "text": { "blocks": { "text": "Type something" } } }"#).unwrap()
        );
        mgr.undo_blocking();
        assert_eq!(design_json(), Any::from_json(r#"{}"#).unwrap());
        mgr.redo_blocking();
        assert_eq!(
            design_json(),
            Any::from_json(r#"{ "text": { "blocks": { "text": "Type something" } } }"#).unwrap()
        );
        mgr.redo_blocking();
        assert_eq!(
            design_json(),
            Any::from_json(r#"{ "text": { "blocks": { "text": "Something" } } }"#).unwrap()
        );
        mgr.redo_blocking();
        assert_eq!(
            design_json(),
            Any::from_json(r#"{ "text": { "blocks": { "text": "Something else" } } }"#).unwrap()
        );
    }

    #[test]
    fn consecutive_redo_bug() {
        // https://github.com/yjs/yjs/issues/355
        let doc = Cell::new(Doc::with_client_id(1));
        let mut mgr = UndoManager::new();
        mgr.expand_scope(&doc, "root".into());

        let point = node(&doc, "root", |mut root| {
            let content = Delta::new().insert_attr("x", 0).insert_attr("y", 0);
            root.insert_attr("a", content).node_id().unwrap()
        });
        mgr.reset();

        let mut set_point = |x: i32, y: i32| {
            node(&doc, point.clone(), |mut point| point.insert_attr("x", x));
            node(&doc, point.clone(), |mut point| point.insert_attr("y", y));
        };

        set_point(100, 100);
        mgr.reset();
        set_point(200, 200);
        mgr.reset();
        set_point(300, 300);
        mgr.reset();

        let point_json = || node(&doc, point.clone(), |point| point.to_json());

        assert_eq!(
            point_json(),
            Any::from_json(r#"{"x":300,"y":300}"#).unwrap()
        );

        mgr.undo_blocking(); // x=200, y=200
        assert_eq!(
            point_json(),
            Any::from_json(r#"{"x":200,"y":200}"#).unwrap()
        );

        mgr.undo_blocking(); // x=100, y=100
        assert_eq!(
            point_json(),
            Any::from_json(r#"{"x":100,"y":100}"#).unwrap()
        );

        mgr.undo_blocking(); // x=0, y=0
        assert_eq!(point_json(), Any::from_json(r#"{"x":0,"y":0}"#).unwrap());

        mgr.undo_blocking(); // null
        node(&doc, "root", |root| assert_eq!(root.attr("a"), None));

        mgr.redo_blocking(); // x=0, y=0
        assert_eq!(point_json(), Any::from_json(r#"{"x":0,"y":0}"#).unwrap());

        mgr.redo_blocking(); // x=100, y=100
        assert_eq!(
            point_json(),
            Any::from_json(r#"{"x":100,"y":100}"#).unwrap()
        );

        mgr.redo_blocking(); // x=200, y=200
        assert_eq!(
            point_json(),
            Any::from_json(r#"{"x":200,"y":200}"#).unwrap()
        );

        mgr.redo_blocking(); // x=300, y=300
        assert_eq!(
            point_json(),
            Any::from_json(r#"{"x":300,"y":300}"#).unwrap()
        );
    }

    #[test]
    fn undo_xml_bug() {
        // https://github.com/yjs/yjs/issues/304
        const ORIGIN: &str = "origin";
        let doc = Cell::new(Doc::with_client_id(1));
        let mut mgr = UndoManager::with_options({
            let mut o = Options::default();
            o.capture_timeout_millis = 0;
            o
        });
        mgr.expand_scope(&doc, "t".into());
        mgr.include_origin(ORIGIN);

        // create element
        let e = node_with(&doc, ORIGIN, "t", |mut f| {
            let content = Delta::with_name("test-node")
                .insert_attr("a", "100".to_string())
                .insert_attr("b", "0".to_string());
            f.insert(0, content).node_id().unwrap()
        });

        // change one attribute
        node_with(&doc, ORIGIN, e.clone(), |mut e| {
            e.insert_attr("a", "200".to_string());
        });

        // change both attributes
        node_with(&doc, ORIGIN, e, |mut e| {
            e.insert_attr("a", "180".to_string());
            e.insert_attr("b", "50".to_string());
        });

        mgr.undo_blocking();
        mgr.undo_blocking();
        mgr.undo_blocking();

        mgr.redo_blocking();
        mgr.redo_blocking();
        mgr.redo_blocking();

        let str = node(&doc, "t", |f| f.to_string());
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
        let mut mgr = UndoManager::with_options({
            let mut o = Options::default();
            o.capture_timeout_millis = 0;
            o
        });
        mgr.expand_scope(&doc, "map".into());

        let blocks = |text: &str| Delta::new().insert_attr("text", text.to_owned());
        let text = node(&doc, "map", |mut design| {
            let content = Delta::new().insert_attr("blocks", blocks("1"));
            design.insert_attr("text", content).node_id().unwrap()
        });
        for content in ["1", "3", "4"] {
            node(&doc, text.clone(), |mut text| {
                text.insert_attr("blocks", blocks(content));
            });
        }
        // {"text":{"blocks":{"text":"4"}}}
        mgr.undo_blocking(); // {"text":{"blocks":{"3"}}}
        mgr.undo_blocking(); // {"text":{"blocks":{"text":"2"}}}
        mgr.undo_blocking(); // {"text":{"blocks":{"text":"1"}}}
        mgr.undo_blocking(); // {}
        mgr.redo_blocking(); // {"text":{"blocks":{"text":"1"}}}
        mgr.redo_blocking(); // {"text":{"blocks":{"text":"2"}}}
        mgr.redo_blocking(); // {"text":{"blocks":{"text":"3"}}}
        mgr.redo_blocking(); // {"text":{}}
        let actual = node(&doc, "map", |design| design.to_json());
        assert_eq!(
            actual,
            Any::from_json(r#"{"text":{"blocks":{"text":"4"}}}"#).unwrap()
        );
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
        doc1.acquire_mut()
            .transact_mut()
            .node_mut("test")
            .unwrap()
            .insert_text(0, "Attack ships on fire off the shoulder of Orion."); // D1: 'Attack ships on fire off the shoulder of Orion.'
        let doc2 = Cell::new(Doc::with_client_id(2));

        send(&doc1, &doc2); // D2: 'Attack ships on fire off the shoulder of Orion.'
        let mut mgr = UndoManager::new();
        mgr.expand_scope(&doc1, "test".into());

        let attrs = Attrs::from([("bold".into(), true.into())]);
        doc1.acquire_mut()
            .transact_mut()
            .node_mut("test")
            .unwrap()
            .format(13, 7, attrs.clone()); // D1: 'Attack ships <b>on fire</b> off the shoulder of Orion.'

        mgr.reset();

        send(&doc1, &doc2); // D2: 'Attack ships <b>on fire</b> off the shoulder of Orion.'

        let attrs2 = Attrs::from([("bold".into(), Any::Null)]);
        doc1.acquire_mut()
            .transact_mut()
            .node_mut("test")
            .unwrap()
            .format(16, 4, attrs2.clone()); // D1: 'Attack ships <b>on </b>fire off the shoulder of Orion.'

        // TODO(unified-api): rich-text `diff`/`Diff`/`YChange` assertions have no NodeRef
        // equivalent yet (`to_delta` is not implemented). Original expected the delta:
        //   ["Attack ships ", <b>"on "</b>, "fire off the shoulder of Orion."]

        mgr.reset();
        send(&doc1, &doc2); // D2: 'Attack ships <b>on </b>fire off the shoulder of Orion.'

        mgr.undo_blocking(); // D1: 'Attack ships <b>on fire</b> off the shoulder of Orion.'
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
        mgr.expand_scope(&doc, "test".into());
        mgr.include_origin(ORIGIN);

        let e = node(&doc, "test", |mut f| {
            let content = Delta::with_name("test")
                .insert_attr("a", "1".to_string())
                .insert_attr("b", "2".to_string());
            f.insert(0, content).node_id().unwrap()
        });
        let as_string = || node(&doc, "test", |f| f.to_string());

        let s = as_string();
        assert!(s == r#"<test a="1" b="2"></test>"# || s == r#"<test b="2" a="1"></test>"#);

        {
            // change attribute "b" and delete test-node within a single transaction
            let mut guard = doc.acquire_mut();
            let mut txn = guard.transact_mut_with(ORIGIN);
            txn.node_mut(e).unwrap().insert_attr("b", "3".to_string());
            txn.node_mut("test").unwrap().remove(0, 1);
        }
        assert_eq!(as_string(), "");

        mgr.undo_blocking();
        let s = as_string();
        assert!(s == r#"<test a="1" b="2"></test>"# || s == r#"<test b="2" a="1"></test>"#);
    }

    #[test]
    fn undo_in_embed() {
        let d1 = Cell::new(Doc::with_client_id(1));
        let mut mgr = UndoManager::new();
        mgr.expand_scope(&d1, "test".into());

        let d2 = Cell::new(Doc::with_client_id(2));

        let attrs = Attrs::from([("bold".into(), true.into())]);
        let nested = node(&d1, "test", |mut txt1| {
            let embed = Delta::new().insert_text("initial text");
            txt1.apply_delta([Delta::new().insert_with(embed, attrs)]);
            txt1.get(0).unwrap().node_id().unwrap()
        });
        let nested_string = || node(&d1, nested.clone(), |nested| nested.to_string());

        assert_eq!(nested_string(), "initial text".to_string());
        mgr.reset();
        node(&d1, nested.clone(), |mut nested| {
            let len = nested.len();
            nested.remove(0, len);
        });
        node(&d1, nested.clone(), |mut nested| {
            nested.insert_text(0, "other text")
        });
        assert_eq!(nested_string(), "other text".to_string());
        mgr.undo_blocking();
        assert_eq!(nested_string(), "initial text".to_string());

        exchange_updates(&mut [&mut *d1.acquire_mut(), &mut *d2.acquire_mut()]);

        let nested2 = node(&d2, "test", |txt2| txt2.get(0).unwrap().node_id().unwrap());
        assert_eq!(
            node(&d2, nested2, |nested2| nested2.to_string()),
            "initial text".to_string()
        );

        mgr.undo_blocking();
        assert_eq!(node(&d1, nested, |nested| nested.len()), 0);
    }

    #[test]
    fn github_issue_345() {
        // https://github.com/y-crdt/y-crdt/issues/345
        let doc = Cell::new(Doc::new());
        let mut mgr = UndoManager::with_options(Options::default());
        mgr.expand_scope(&doc, "r".into());
        let client_id = doc.acquire().client_id();
        mgr.include_origin(client_id);

        let s1 = node_with(&doc, client_id, "r", |mut map| {
            map.insert_attr("s1", Delta::new()).node_id().unwrap()
        });
        mgr.reset();

        let a = node_with(&doc, client_id, s1.clone(), |mut s1| {
            s1.insert_attr("a", Delta::new().insert("a1".to_string()))
                .node_id()
                .unwrap()
        });
        mgr.reset();

        let b = node_with(&doc, client_id, s1, |mut s1| {
            s1.insert_attr("b", Delta::new().insert("b1".to_string()))
                .node_id()
                .unwrap()
        });
        mgr.reset();

        node_with(&doc, client_id, b, |mut b| b.insert(1, "b2".to_string()));
        mgr.reset();

        for (index, value) in [(1, "a3"), (2, "a4"), (3, "a5")] {
            node_with(&doc, client_id, a.clone(), |mut a| {
                a.insert(index, value.to_string())
            });
            mgr.reset();
        }

        let map_json = || node(&doc, "r", |map| map.to_json());

        assert_eq!(
            map_json(),
            any!({"s1": {"a": ["a1", "a3", "a4", "a5"], "b": ["b1", "b2"]}})
        );

        mgr.undo_blocking(); // {"s1": {"a": ["a1", "a3", "a4"], "b": ["b1", "b2"]}}
        mgr.undo_blocking(); // {"s1": {"a": ["a1", "a3"], "b": ["b1", "b2"]}}
        mgr.undo_blocking(); // {"s1": {"a": ["a1"], "b": ["b1", "b2"]}}
        mgr.undo_blocking(); // {"s1": {"a": ["a1"], "b": ["b1"]}}
        assert_eq!(map_json(), any!({"s1": {"a": ["a1"], "b": ["b1"]}}));

        mgr.redo_blocking();
        assert_eq!(map_json(), any!({"s1": {"b": ["b1", "b2"], "a": ["a1"]}}));

        mgr.redo_blocking();
        assert_eq!(
            map_json(),
            any!({"s1": {"a": ["a1", "a3"], "b": ["b1", "b2"]}})
        );

        mgr.redo_blocking();
        assert_eq!(
            map_json(),
            any!({"s1": {"a": ["a1", "a3", "a4"], "b": ["b1", "b2"]}})
        );

        mgr.redo_blocking();
        assert_eq!(
            map_json(),
            any!({"s1": {"a": ["a1", "a3", "a4", "a5"], "b": ["b1", "b2"]}})
        );
    }

    #[test]
    fn github_issue_345_part_2() {
        // https://github.com/y-crdt/y-crdt/issues/345
        let d = Cell::new(Doc::new());
        let s1 = node(&d, "r", |mut r| {
            r.insert_attr("s1", Delta::new()).node_id().unwrap()
        });

        let mut mgr = UndoManager::with_options(Options::default());
        mgr.expand_scope(&d, "r".into());
        node(&d, s1.clone(), |mut s1| {
            s1.insert_attr("b1", Delta::new().insert_attr("f1", 11));
        });
        mgr.reset();

        node(&d, s1.clone(), |mut s1| s1.remove_attr("b1"));
        mgr.reset();

        node(&d, s1, |mut s1| {
            s1.insert_attr("b1", Delta::new().insert_attr("f1", 20));
        });
        mgr.reset();

        let r_json = || node(&d, "r", |r| r.to_json());

        assert_eq!(r_json(), any!({"s1": {"b1": {"f1": 20}}}));

        mgr.undo_blocking();
        assert_eq!(r_json(), any!({"s1": {}}));
        assert!(mgr.can_undo(), "should be able to undo");

        mgr.undo_blocking();
        assert_eq!(r_json(), any!({"s1": {"b1": {"f1": 11}}}));
        assert!(mgr.can_undo(), "should be able to undo to the init state");
    }

    #[test]
    fn issue_371() {
        let doc = Cell::new(Doc::with_client_id(1));

        let s1 = node(&doc, "r", |mut r| {
            r.insert_attr("s1", Delta::new()).node_id().unwrap()
        }); // { s1: {} }
        let b1_arr = node(&doc, s1, |mut s1| {
            s1.insert_attr("b1", Delta::new()).node_id().unwrap()
        }); // { s1: { b1: [] } }
        let el1 = node(&doc, b1_arr.clone(), |mut b1_arr| {
            b1_arr.insert(0, Delta::new()).node_id().unwrap()
        }); // { s1: { b1: [{}] } }
        node(&doc, el1.clone(), |mut el1| el1.insert_attr("f1", 8)); // { s1: { b1: [{ f1: 8 }] } }
        node(&doc, el1.clone(), |mut el1| el1.insert_attr("f2", true)); // { s1: { b1: [{ f1: 8, f2: true }] } }

        let mut mgr = UndoManager::with_options(Options::default());
        mgr.expand_scope(&doc, "r".into());
        {
            let mut guard = doc.acquire_mut();
            let mut txn = guard.transact_mut();
            // { s1: { b1: [{ f1: 8, f2: false }, { f1: 8, f2: true }] } }
            let el0 = Delta::new().insert_attr("f1", 8).insert_attr("f2", false);
            txn.node_mut(b1_arr).unwrap().insert(0, el0);

            let mut el1 = txn.node_mut(el1.clone()).unwrap();
            el1.insert_attr("f1", 13); // { s1: { b1: [{ f1: 8, f2: false }, { f1: 13, f2: true }] } }
            el1.remove_attr("f2"); // { s1: { b1: [{ f1: 8, f2: false }, { f1: 13 }] } }
        }
        mgr.reset();

        // { s1: { b1: [{ f1: 8, f2: false }, { f1: 13, f2: false }] } }
        node(&doc, el1.clone(), |mut el1| el1.insert_attr("f2", false));
        mgr.reset();

        // { s1: { b1: [{ f1: 8, f2: false }, { f1: 13, f2: true }] } }
        node(&doc, el1, |mut el1| el1.insert_attr("f2", true));
        mgr.reset();

        let r_json = || node(&doc, "r", |r| r.to_json());

        mgr.undo_blocking(); // { s1: { b1: [{ f1: 8, f2: false }, { f1: 13, f2: false }] } }
        assert_eq!(
            r_json(),
            any!({ "s1": { "b1": [{ "f1": 8, "f2": false }, { "f1": 13, "f2": false }] } })
        );
        mgr.undo_blocking(); // { s1: { b1: [{ f1: 8, f2: false }, { f1: 13 }] } }
        assert_eq!(
            r_json(),
            any!({ "s1": { "b1": [{ "f1": 8, "f2": false }, { "f1": 13 }] } })
        );
        mgr.undo_blocking(); // { s1: { b1: [{ f1: 8, f2: true }] } }
        assert_eq!(
            r_json(),
            any!({ "s1": { "b1": [{ "f1": 8, "f2": true }] } })
        );
        assert!(!mgr.undo_blocking()); // no more changes tracked by undo manager
    }

    #[test]
    fn issue_371_2() {
        let doc = Cell::new(Doc::with_client_id(1));
        let s1 = node(&doc, "r", |mut r| {
            r.insert_attr("s1", Delta::new()).node_id().unwrap()
        }); // { s1:{} }
        node(&doc, s1.clone(), |mut s1| {
            s1.insert_attr("f2", "AAA".to_string())
        }); // { s1: { f2: AAA } }
        node(&doc, s1.clone(), |mut s1| s1.insert_attr("f1", false)); // { s1: { f1: false, f2: AAA } }

        let mut mgr = UndoManager::with_options(Options::default());
        mgr.expand_scope(&doc, "r".into());
        node(&doc, s1.clone(), |mut s1| s1.remove_attr("f2")); // { s1: { f1: false } }
        mgr.reset();

        node(&doc, s1.clone(), |mut s1| {
            s1.insert_attr("f2", "C1".to_string())
        }); // { s1: { f1: false, f2: C1 } }
        mgr.reset();

        node(&doc, s1, |mut s1| s1.insert_attr("f2", "C2".to_string())); // { s1: { f1: false, f2: C2 } }
        mgr.reset();

        let r_json = || node(&doc, "r", |r| r.to_json());

        mgr.undo_blocking(); // { s1: { f1: false, f2: C1 } }
        assert_eq!(r_json(), any!({ "s1": { "f1": false, "f2": "C1" } }));
        mgr.undo_blocking(); // { s1: { f1: false } }
        assert_eq!(r_json(), any!({ "s1": { "f1": false } }));
        mgr.undo_blocking(); // { s1: { f1: false, f2: AAA } }
        assert_eq!(r_json(), any!({ "s1": { "f1": false, "f2": "AAA" } }));
        assert!(!mgr.undo_blocking()); // no more changes tracked by undo manager
    }

    #[test]
    fn issue_380() {
        let d = Cell::new(Doc::with_client_id(1));
        let s1 = node(&d, "r", |mut r| {
            r.insert_attr("s1", Delta::new()).node_id().unwrap()
        }); // {r:{s1:{}}
        let b1_arr = node(&d, s1, |mut s1| {
            s1.insert_attr("b1", Delta::new()).node_id().unwrap()
        }); // {r:{s1:{b1:[]}}

        let b1_el1 = node(&d, b1_arr.clone(), |mut b1_arr| {
            b1_arr.insert(0, Delta::new()).node_id().unwrap()
        }); // {r:{s1:{b1:[{}]}}
        let b2_arr = node(&d, b1_el1, |mut b1_el1| {
            b1_el1.insert_attr("b2", Delta::new()).node_id().unwrap()
        }); // {r:{s1:{b1:[{b2:[]}]}}
        let b2_arr_nest = node(&d, b2_arr, |mut b2_arr| {
            b2_arr.insert(0, Delta::new()).node_id().unwrap()
        }); // {r:{s1:{b1:[{b2:[[]]}]}}
        node(&d, b2_arr_nest.clone(), |mut nest| {
            nest.insert(0, 232291652)
        }); // {r:{s1:{b1:[{b2:[[232291652]]}]}}
        node(&d, b2_arr_nest.clone(), |mut nest| nest.insert(1, -30)); // {r:{s1:{b1:[{b2:[[232291652, -30]]}]}}

        let mut mgr = UndoManager::with_options(Options::default());
        mgr.expand_scope(&d, "r".into());

        node(&d, b2_arr_nest, |mut nest| {
            nest.remove(1, 1); // {r:{s1:{b1:[{b2:[[232291652]]}]}}
            nest.insert(1, -5); // {r:{s1:{b1:[{b2:[[232291652, -5]]}]}}
        });
        mgr.reset();

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
        }
        mgr.reset();

        let r_json = || node(&d, "r", |r| r.to_json());

        assert_eq!(
            r_json(),
            any!({"s1":{"b1":[{"b2":[[232291652, -6]]}, {"f2":"C1"}]}})
        );

        mgr.undo_blocking(); // {r:{s1:{b1:[{b2:[[232291652, -5]]}]}}
        assert_eq!(r_json(), any!({"s1":{"b1":[{"b2":[[232291652, -5]]}]}}));

        mgr.undo_blocking(); // {r:{s1:{b1:[{b2:[[232291652, -30]]}]}}
        assert_eq!(r_json(), any!({"s1":{"b1":[{"b2":[[232291652, -30]]}]}}));
    }

    #[test]
    fn multi_doc_undo() {
        let mut um = UndoManager::new();
        let d1 = Cell::new(Doc::new());
        let d2 = Cell::new(Doc::new());
        um.expand_scope(&d1, "text".into());
        um.expand_scope(&d2, "text".into());

        d1.acquire_mut()
            .transact_mut()
            .node_mut("text")
            .unwrap()
            .insert_text(0, "abc");
        d2.acquire_mut()
            .transact_mut()
            .node_mut("text")
            .unwrap()
            .insert_text(0, "xyz");

        assert_eq!(um.undo_stack().len(), 2);
        assert!(um.can_undo(), "should be undoable (1)");
        assert!(!um.can_redo(), "should not be redoable (1)");

        um.undo_blocking();

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

        um.undo_blocking();

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
        assert!(!um.undo_blocking(), "undo should have no effect");
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

        um.redo_blocking();

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

        um.redo_blocking();

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
    fn multi_doc_after_destroy() {
        let mut um = UndoManager::with_options({
            let mut o = Options::default();
            o.capture_timeout_millis = 0;
            o
        });
        let d1 = Cell::new(Doc::new());
        let d2 = Cell::new(Doc::new());
        um.expand_scope(&d1, "text".into());
        um.expand_scope(&d2, "text".into());
        um.expand_scope(&d1, "text".into()); // doing this twice for test-coverage

        d1.acquire_mut()
            .transact_mut()
            .node_mut("text")
            .unwrap()
            .insert_text(0, "a");
        d2.acquire_mut()
            .transact_mut()
            .node_mut("text")
            .unwrap()
            .insert_text(0, "b");

        assert_eq!(
            d1.acquire().transact().node("text").unwrap().to_string(),
            "a"
        );
        let d2_guid = d2.acquire().guid().clone();
        d2.acquire_mut().destroy(None);
        assert!(um.docs().all(|d| *d.acquire().guid() != d2_guid));

        um.undo_blocking();
        assert_eq!(
            d1.acquire().transact().node("text").unwrap().to_string(),
            ""
        );
    }
}
