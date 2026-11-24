use crate::block::{ID, Item, ItemContent, ItemPtr};
use crate::formatter::{DefaultFormatter, DeltaFormatter};
use crate::gc::GCCollector;
use crate::id_set::DeleteSet;
use crate::iter::TxnIterator;
use crate::lib0::{Encode, Encoder, EncoderV1, EncoderV2};
use crate::node::{Node, NodePtr, TypePtr, TypeRef};
use crate::node_ref::get_delta;
use crate::reporter::{NoReporter, Reporter};
use crate::slice::BlockSlice;
use crate::undo::{Scope, UndoStack};
use crate::update::Update;
use crate::utils::OptionExt;
use crate::{
    Any, Delta, Doc, IdSet, In, NodeID, NodeRef, Out, Snapshot, StateVector, Uuid,
    merge_updates_v1, merge_updates_v2,
};
use smallvec::SmallVec;
use std::cell::OnceCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Formatter;
use std::hash::Hash;
use std::ops::{Deref, DerefMut};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::OnceLock;
// ReadTxn and WriteTxn traits have been replaced by generic impl blocks on Transaction<D>.
// Use Transaction<&Doc> for read-only access and Transaction<&mut Doc> for read-write access.

fn merge_pending_v1(update: Vec<u8>, store: &Doc) -> Vec<u8> {
    let mut merge = VecDeque::new();
    if let Some(pending) = store.pending.as_ref() {
        merge.push_back(pending.update.encode_v1());
    }
    if let Some(pending_ds) = store.pending_ds.as_ref() {
        let mut u = Update::new();
        u.delete_set = pending_ds.clone();
        merge.push_back(u.encode_v1());
    }
    if merge.is_empty() {
        update
    } else {
        merge.push_front(update);
        merge_updates_v1(merge).unwrap()
    }
}

fn merge_pending_v2(update: Vec<u8>, store: &Doc) -> Vec<u8> {
    let mut merge = VecDeque::new();
    if let Some(pending) = store.pending.as_ref() {
        merge.push_back(pending.update.encode_v2());
    }
    if let Some(pending_ds) = store.pending_ds.as_ref() {
        let mut u = Update::new();
        u.delete_set = pending_ds.clone();
        merge.push_back(u.encode_v2());
    }
    if merge.is_empty() {
        update
    } else {
        merge.push_front(update);
        merge_updates_v2(merge).unwrap()
    }
}

/// A handle used by a [Transaction] to access its [Doc].
///
/// Handles which also implement [DerefMut] (`&mut Doc`, [crate::OwnedMut]) give read-write
/// transactions, the others (`&Doc`, [crate::OwnedRef]) give read-only ones.
pub trait DocHandle: Deref<Target = Doc> {
    /// Returns a mutable reference to a document, if current handle allows writes.
    /// Used to commit read-write transactions when they are dropped.
    fn try_doc_mut(&mut self) -> Option<&mut Doc>;
}

impl DocHandle for &Doc {
    #[inline]
    fn try_doc_mut(&mut self) -> Option<&mut Doc> {
        None
    }
}

impl DocHandle for &mut Doc {
    #[inline]
    fn try_doc_mut(&mut self) -> Option<&mut Doc> {
        Some(self)
    }
}

impl DocHandle for crate::cell::OwnedRef<Doc> {
    #[inline]
    fn try_doc_mut(&mut self) -> Option<&mut Doc> {
        None
    }
}

impl DocHandle for crate::cell::OwnedMut<Doc> {
    #[inline]
    fn try_doc_mut(&mut self) -> Option<&mut Doc> {
        Some(self)
    }
}

#[cfg(feature = "sync")]
impl DocHandle for crate::cell::sync::OwnedRef<Doc> {
    #[inline]
    fn try_doc_mut(&mut self) -> Option<&mut Doc> {
        None
    }
}

#[cfg(feature = "sync")]
impl DocHandle for crate::cell::sync::OwnedMut<Doc> {
    #[inline]
    fn try_doc_mut(&mut self) -> Option<&mut Doc> {
        Some(self)
    }
}

/// A transaction provides a controlled scope for reading and writing to a [Doc].
///
/// - `Transaction<&Doc>` is a lightweight **read-only** transaction.
/// - `Transaction<&mut Doc>` (aka [`TransactionMut`]) is a **read-write** transaction that
///   tracks changes and auto-commits when dropped.
/// - `Transaction<OwnedRef<Doc>>`/`Transaction<OwnedMut<Doc>>` are their equivalents, which own
///   a reference to a [crate::Cell] containing the document (see [crate::Cell::transact_owned_mut]).
///
/// Read-write transactions store information about all changes performed in their scope.
/// These are used during [`Transaction::commit`] to optimize metadata and report changes.
/// For performance, batch as many updates as possible in a single transaction.
///
/// Rollbacks are not supported. Use [UndoManager] to undo operations.
#[repr(C)]
pub struct Transaction<D: DocHandle> {
    pub(crate) doc: D,
    pub(crate) state: Option<Box<TransactionState>>,
}

/// Backward-compatible alias for a read-write transaction.
pub type TransactionMut<'doc> = Transaction<&'doc mut Doc>;

/// Mutable state accumulated during a read-write transaction. Tracks inserts, deletes,
/// changed types, and other metadata needed for commit.
pub struct TransactionState {
    /// State vector of a current transaction at the moment of its creation.
    before_state: OnceCell<StateVector>,
    /// Current state vector of a transaction, which includes all performed updates.
    after_state: OnceCell<StateVector>,
    /// ID's of the blocks to be merged.
    pub(crate) merge_blocks: Vec<ID>,
    /// Describes the set of deleted items by ids.
    pub(crate) delete_set: IdSet,
    /// Describes the set of inserted items by ids.
    pub(crate) insert_set: IdSet,
    pub(crate) cleanups: IdSet,
    /// All types that were directly modified (property added or child inserted/deleted).
    /// New types are not included in this Set.
    pub(crate) changed: HashMap<NodePtr, HashSet<Option<Arc<str>>>>,
    pub(crate) changed_parent_types: Vec<NodePtr>,
    pub(crate) subdocs: Option<Box<Subdocs>>,
    pub(crate) origin: Option<Origin>,
    pub(crate) local: bool,
    pub(crate) attributed_deletes: Option<IdSet>,
    /// Set by [crate::undo::UndoManager::undo]/[crate::undo::UndoManager::redo], so that
    /// [crate::undo::UndoManager::track] ignores this transaction.
    pub(crate) undo_redo: bool,
    committed: bool,
    needs_cleanup: bool,
}

impl TransactionState {
    pub fn new(origin: Option<Origin>) -> Self {
        TransactionState {
            before_state: OnceCell::new(),
            after_state: OnceCell::new(),
            merge_blocks: Vec::default(),
            delete_set: IdSet::new(),
            insert_set: IdSet::new(),
            cleanups: IdSet::new(),
            changed: HashMap::default(),
            changed_parent_types: Vec::default(),
            subdocs: None,
            origin,
            local: true,
            attributed_deletes: None,
            undo_redo: false,
            committed: false,
            needs_cleanup: false,
        }
    }
}

impl Default for TransactionState {
    fn default() -> Self {
        Self::new(None)
    }
}

impl TransactionState {
    /// State vector of a `doc` at the moment when current transaction was created.
    pub(crate) fn before_state(&self, doc: &Doc) -> &StateVector {
        self.before_state.get_or_init(|| {
            let mut sv = doc.blocks.get_state_vector();
            for (client, ranges) in self.insert_set.iter() {
                if let Some(clock) = ranges.clock_start() {
                    sv.set_min(*client, clock);
                }
            }
            sv
        })
    }

    /// State vector of a `doc` including all updates performed by current transaction.
    pub(crate) fn after_state(&self, doc: &Doc) -> &StateVector {
        self.after_state.get_or_init(|| {
            let mut sv = doc.blocks.get_state_vector();
            for (client, ranges) in self.insert_set.iter() {
                if let Some(clock) = ranges.clock_end() {
                    sv.set_max(*client, clock);
                }
            }
            sv
        })
    }
}

/// Ensures that the transaction state is initialized, creating it on demand if needed.
#[inline]
pub(crate) fn ensure_state(state: &mut Option<Box<TransactionState>>) -> &mut TransactionState {
    state.get_or_insert_with(new_state)
}

/// Kept out of line, so that [ensure_state] callers don't inline the allocation path.
#[cold]
#[inline(never)]
fn new_state() -> Box<TransactionState> {
    Box::new(TransactionState::default())
}

/// Read methods — available on all transactions.
impl<D: DocHandle> Transaction<D> {
    /// Returns a reference to the [Doc] that this transaction operates on.
    #[inline]
    pub fn doc(&self) -> &Doc {
        &self.doc
    }

    /// Returns state vector describing current state of the updates.
    pub fn state_vector(&self) -> StateVector {
        self.doc().blocks.get_state_vector()
    }

    /// Returns a snapshot which describes a current state of updates and removals made within
    /// the corresponding document.
    pub fn snapshot(&self) -> Snapshot {
        let store = self.doc();
        let blocks = &store.blocks;
        let sv = blocks.get_state_vector();
        let ds = IdSet::tombstones_from(blocks);
        Snapshot::new(sv, ds)
    }

    /// Encodes all changes from current transaction block store up to a given `snapshot`.
    pub fn encode_state_from_snapshot<E: Encoder>(
        &self,
        snapshot: &Snapshot,
        encoder: &mut E,
    ) -> crate::Result<()> {
        self.doc().encode_state_from_snapshot(snapshot, encoder)
    }

    /// Encodes the difference between remote peer state given its `state_vector` and the state
    /// of a current local peer.
    pub fn encode_diff<E: Encoder>(&self, state_vector: &StateVector, encoder: &mut E) {
        self.doc().encode_diff(state_vector, encoder)
    }

    pub fn encode_diff_v1(&self, state_vector: &StateVector) -> Vec<u8> {
        let mut encoder = EncoderV1::new();
        self.encode_diff(state_vector, &mut encoder);
        encoder.to_vec()
    }

    pub fn encode_diff_v2(&self, state_vector: &StateVector) -> Vec<u8> {
        let mut encoder = EncoderV2::new();
        self.encode_diff(state_vector, &mut encoder);
        encoder.to_vec()
    }

    pub fn encode_state_as_update<E: Encoder>(&self, sv: &StateVector, encoder: &mut E) {
        let store = self.doc();
        store.write_blocks_from(sv, encoder);
        let ds = IdSet::tombstones_from(&store.blocks);
        ds.encode(encoder);
    }

    pub fn encode_state_as_update_v1(&self, sv: &StateVector) -> Vec<u8> {
        let mut encoder = EncoderV1::new();
        self.encode_state_as_update(sv, &mut encoder);
        merge_pending_v1(encoder.to_vec(), self.doc())
    }

    pub fn encode_state_as_update_v2(&self, sv: &StateVector) -> Vec<u8> {
        let mut encoder = EncoderV2::new();
        self.encode_state_as_update(sv, &mut encoder);
        merge_pending_v2(encoder.to_vec(), self.doc())
    }

    /// Returns an iterator over top level (root) shared types available in current [Doc].
    pub fn root_refs(&self) -> RootRefs<'_> {
        let store = self.doc();
        RootRefs(store.types.iter())
    }

    /// Returns a collection of globally unique identifiers of sub documents linked within
    /// the structures of this document store.
    pub fn subdoc_guids(&self) -> impl Iterator<Item = &crate::Uuid> {
        let store = self.doc();
        store.subdoc_guids()
    }

    /// Returns a collection of sub documents linked within the structures of this document store.
    pub fn subdocs(&self) -> impl Iterator<Item = &Doc> {
        let store = self.doc();
        store.subdocs()
    }

    pub fn subdoc(&self, guid: &Uuid) -> Option<&Doc> {
        self.doc().subdocs.get(guid)
    }

    #[inline]
    pub fn node<N: Into<NodeID>>(&self, id: N) -> Option<NodeRef<&Self>> {
        let ptr = self.doc.node(id.into())?;
        Some(NodeRef::new(ptr, self))
    }

    /// If current document has been inserted as a sub-document, returns the guid of its parent
    /// document.
    pub fn parent_doc(&self) -> Option<crate::Uuid> {
        if let Some(item) = self.doc().parent.as_deref() {
            if let ItemContent::Doc(parent_guid, _) = &item.content {
                return parent_guid.clone();
            }
        }
        None
    }

    /// If current document has been inserted as a sub-document, returns its [NodeID].
    pub fn node_id(&self) -> Option<NodeID> {
        if let Some(item) = self.doc().parent {
            Some(NodeID::Nested(item.id))
        } else {
            None
        }
    }

    /// Returns `true` if current document has any pending updates that are not yet
    /// integrated into the document.
    pub fn has_missing_updates(&self) -> bool {
        let store = self.doc();
        store.pending.is_some() || store.pending_ds.is_some()
    }

    /// Data about insertions performed in the scope of current transaction.
    pub fn insert_set(&self) -> &IdSet {
        static EMPTY: OnceLock<IdSet> = OnceLock::new();
        self.state
            .as_ref()
            .map_or_else(|| EMPTY.get_or_init(IdSet::new), |s| &s.insert_set)
    }

    /// Data about deletions performed in the scope of current transaction.
    pub fn delete_set(&self) -> &IdSet {
        static EMPTY: OnceLock<IdSet> = OnceLock::new();
        self.state
            .as_ref()
            .map_or_else(|| EMPTY.get_or_init(IdSet::new), |s| &s.delete_set)
    }

    /// Checks if this transaction inserted or deleted anything.
    pub fn has_changes(&self) -> bool {
        if let Some(s) = &self.state {
            !s.insert_set.is_empty() || !s.delete_set.is_empty()
        } else {
            false
        }
    }

    /// Returns `false` if this transaction applies a remote update.
    pub fn is_local(&self) -> bool {
        self.state.as_ref().map_or(true, |s| s.local)
    }

    /// Deletes performed over content which was already deleted, but still rendered by
    /// a formatter (see [crate::DeltaFormatter]).
    pub fn attributed_deletes(&self) -> Option<&IdSet> {
        let state = self.state.as_ref()?;
        state.attributed_deletes.as_ref()
    }

    /// Returns origin of the transaction if any was defined.
    pub fn origin(&self) -> Option<&Origin> {
        self.state.as_ref().and_then(|s| s.origin.as_ref())
    }

    /// Returns a list of root level types changed in a scope of the current transaction.
    pub fn changed_parent_types(&self) -> &[NodePtr] {
        self.state.as_ref().map_or(&[], |s| &s.changed_parent_types)
    }

    /// Corresponding document's state vector at the moment when current transaction was created.
    pub fn before_state(&self) -> &StateVector {
        self.state
            .as_ref()
            .expect("transaction state not initialized")
            .before_state(&self.doc)
    }

    /// State vector of the transaction after [Transaction::commit] has been called.
    pub fn after_state(&self) -> &StateVector {
        self.state
            .as_ref()
            .expect("transaction state not initialized")
            .after_state(&self.doc)
    }

    /// Checks if item with a given `id` has been added to a block store within this transaction.
    pub fn has_added(&self, id: &ID) -> bool {
        match &self.state {
            None => false,
            Some(state) => state.insert_set.contains(id),
        }
    }

    /// Checks if item with a given `id` has been deleted within this transaction.
    pub fn has_deleted(&self, id: &ID) -> bool {
        match &self.state {
            None => false,
            Some(state) => state.delete_set.contains(id),
        }
    }

    /// Encodes changes made within the scope of the current transaction using lib0 v1 lib0.
    pub fn encode_update_v1(&self) -> Vec<u8> {
        let mut encoder = EncoderV1::new();
        self.encode_update(&mut encoder);
        encoder.to_vec()
    }

    /// Encodes changes made within the scope of the current transaction using lib0 v2 lib0.
    pub fn encode_update_v2(&self) -> Vec<u8> {
        let mut encoder = EncoderV2::new();
        self.encode_update(&mut encoder);
        encoder.to_vec()
    }

    /// Encodes changes made within the scope of the current transaction.
    pub fn encode_update<E: Encoder>(&self, encoder: &mut E) {
        let store = &*self.doc;
        store.write_blocks_from(self.before_state(), encoder);
        self.delete_set().encode(encoder);
    }

    /// Nodes directly modified by this transaction, with their changed keys (`None` = children).
    pub fn modified(&self) -> impl Iterator<Item = (NodeRef<&Self>, &HashSet<Option<Arc<str>>>)> {
        let changed = self.state.as_deref().map(|s| s.changed.iter());
        changed
            .into_iter()
            .flatten()
            .map(move |(&node, keys)| (NodeRef::new(node, self), keys))
    }

    /// Nodes directly modified by this transaction which are `root` or nested in it, weak links
    /// included. Unordered.
    pub fn modified_within<N: Into<NodeID>>(
        &self,
        root: N,
    ) -> impl Iterator<Item = NodeRef<&Self>> {
        let root = self.doc.node(root.into());
        let changed = root.and(self.state.as_deref()).map(|s| s.changed.keys());
        changed.into_iter().flatten().filter_map(move |&node| {
            let mut found = false;
            let links = &self.doc.linked_by;
            for_each_ancestor(links, node, &mut HashSet::new(), &mut |n| {
                found |= Some(n) == root
            });
            found.then(|| NodeRef::new(node, self))
        })
    }

    /// Keys changed within a `node`, if it was directly modified by this transaction.
    fn changed_keys(&self, node: NodeID) -> Option<&HashSet<Option<Arc<str>>>> {
        let state = self.state.as_deref()?;
        if state.changed.is_empty() {
            return None;
        }
        state.changed.get(&self.doc.node(node)?)
    }

    /// Checks if a `node` was modified by this transaction. With `deep` set, modifications of
    /// nodes nested in it (weak links included) count as well.
    pub fn is_modified<N: Into<NodeID>>(&self, node: N, deep: bool) -> bool {
        if !deep {
            self.changed_keys(node.into()).is_some()
        } else {
            let parents = self.changed_parent_types();
            !parents.is_empty()
                && self
                    .doc
                    .node(node.into())
                    .is_some_and(|n| parents.contains(&n))
        }
    }

    /// Attribute keys of a `node` changed by this transaction.
    pub fn keys_changed<N: Into<NodeID>>(&self, node: N) -> impl Iterator<Item = &Arc<str>> {
        self.changed_keys(node.into())
            .into_iter()
            .flatten()
            .flatten()
    }

    /// Checks if children of a `node` were changed by this transaction.
    pub fn children_changed<N: Into<NodeID>>(&self, node: N) -> bool {
        self.changed_keys(node.into())
            .is_some_and(|keys| keys.contains(&None))
    }

    /// Changes made to a `node` by this transaction, including nested nodes if `deep` is set.
    pub fn delta<N: Into<NodeID>>(&self, node: N, deep: bool) -> Delta<Out> {
        self.delta_fmt(node, deep, &DefaultFormatter)
    }

    /// Like [Transaction::delta], formatted via custom formatter `f`.
    pub fn delta_fmt<N: Into<NodeID>, F: DeltaFormatter>(
        &self,
        node: N,
        deep: bool,
        f: &F,
    ) -> Delta<Out> {
        let node = node.into();
        let (true, Some(state), Some(ptr)) = (
            self.is_modified(node.clone(), deep),
            self.state.as_deref(),
            self.doc.node(node),
        ) else {
            return Delta::default();
        };
        // cleanup deletes only redundant format markers, which render as no change
        get_delta(&self.doc, state, ptr, deep, f)
    }
}

/// Transmute helper: downgrade a shared reference to a mutable transaction into a read-only view.
impl<'doc> Transaction<&'doc mut Doc> {
    /// Returns a read-only view of this mutable transaction.
    ///
    /// # Safety
    /// This uses `transmute` internally. It is safe because:
    /// - `Transaction<&mut Doc>` and `Transaction<&Doc>` have identical layout (`#[repr(C)]`).
    /// - A shared reference (`&self`) only permits read access.
    #[inline(always)]
    pub fn as_readonly(&self) -> &Transaction<&'doc Doc> {
        unsafe { std::mem::transmute(self) }
    }
}

/// Write methods — available on read-write transactions (see [DocHandle]).
impl<D: DocHandle + DerefMut> Transaction<D> {
    /// Splits current transaction onto its document and state, initializing the latter if needed.
    #[inline]
    pub(crate) fn split_mut(&mut self) -> (&mut Doc, &mut TransactionState) {
        (&mut *self.doc, ensure_state(&mut self.state))
    }

    #[inline]
    pub fn doc_mut(&mut self) -> &mut Doc {
        &mut self.doc
    }

    pub fn subdocs_mut(&mut self) -> &mut Subdocs {
        ensure_state(&mut self.state).subdocs.get_or_init()
    }

    pub fn node_mut<N: Into<NodeID>>(&mut self, id: N) -> Option<NodeRef<&mut Self>> {
        let node = match id.into() {
            NodeID::Root(name) => self.doc.get_or_create_type(name, TypeRef::Undefined),
            NodeID::Nested(id) => {
                let mut item = self.doc.blocks.get_item(&id)?;
                if item.is_deleted() {
                    return None;
                }
                if let ItemContent::Node(node) = &mut item.content {
                    NodePtr::from(&*node)
                } else {
                    return None;
                }
            }
        };
        Some(NodeRef::new(node, self))
    }

    /// Prunes pending updates from the current document and returns them.
    pub fn prune_pending(&mut self) -> Option<Update> {
        let mut merge = Vec::with_capacity(2);
        let store = self.doc_mut();
        if let Some(pending) = store.pending.take() {
            merge.push(pending.update);
        }
        if let Some(pending_ds) = store.pending_ds.take() {
            let mut u = Update::new();
            u.delete_set = pending_ds.clone();
            merge.push(u);
        }
        if merge.is_empty() {
            None
        } else {
            Some(Update::merge_updates(merge))
        }
    }

    /// Applies a deserialized [Update] contents into a document owning current transaction. Update
    /// payload can be generated by methods such as [TransactionMut::encode_diff] or written to
    /// [Reporter::encoder] by a commit. Updates are allowed to contain
    /// duplicate blocks (already presen in current document store) - these will be ignored.
    ///
    /// # Pending updates
    ///
    /// Remote update integration requires that all to-be-integrated blocks must have their direct
    /// predecessors already in place. Out of order updates from the same peer will be stashed
    /// internally and their integration will be postponed until missing blocks arrive first.
    pub fn apply_update(&mut self, update: Update) -> crate::Result<()> {
        let (doc, state) = self.split_mut();
        state.apply_update(doc, update)
    }

    /// Undoes a change described by the ids it inserted and deleted.
    /// Returns `true` if anything changed.
    pub fn revert(&mut self, inserts: &IdSet, deletes: &IdSet) -> bool {
        let (doc, state) = self.split_mut();
        let unused = UndoStack::default();
        crate::undo::revert::<()>(doc, state, inserts, deletes, &Scope::Doc, &unused, &unused)
    }

    /// Perform garbage collection of deleted blocks, even if a document was created with `skip_gc`
    /// option.
    ///
    /// If `delete_set` is provided, it will be used to limit the scope of garbage collection
    /// to only those blocks that are present in the delete set. If `delete_set` is `None`, all
    /// deleted blocks will be considered for garbage collection.
    pub fn gc(&mut self, delete_set: Option<&IdSet>) {
        let (doc, state) = self.split_mut();
        GCCollector::collect_all(doc, state, delete_set);
    }

    /// Commits current transaction. This step involves cleaning up and optimizing changes performed
    /// during lifetime of a transaction. Such changes include squashing delete sets data,
    /// squashing blocks that have been appended one after another to preserve memory and reporting
    /// the outcome.
    ///
    /// This step is performed automatically when a transaction is about to be dropped (its life
    /// scope comes to an end). Outcome of the commit is passed to a `reporter`. Commit consumes
    /// the transaction, so it cannot be committed twice.
    pub fn commit<R: Reporter>(mut self, reporter: &mut R) {
        // commit runs on a `TransactionMut` view of this transaction. Its state is cleared
        // by commit, so neither the view nor `self` commits again when dropped.
        let mut view = Transaction {
            doc: &mut *self.doc,
            state: self.state.take(),
        };
        view.commit_view(reporter);
    }
}

/// Commit path of read-write transactions. Committed transaction is represented as
/// `Transaction<&mut Doc>` view.
impl<'doc> Transaction<&'doc mut Doc> {
    fn commit_view<R: Reporter>(&mut self, reporter: &mut R) {
        let state = match self.state.as_mut() {
            None => return,
            Some(state) if state.committed => return,
            Some(state) => state,
        };
        state.committed = true;

        if !state.changed.is_empty() {
            Self::collect_changes(state, &self.doc);
        }

        if state.needs_cleanup && self.doc.options.cleanup_formatting {
            let state = self.state.as_deref_mut().unwrap();
            state.cleanup_fmt(self.doc);
        }

        // report the transaction before GC, while its deleted content still renders
        {
            let tx = self.as_readonly();
            reporter.fill(tx);
            reporter.committing(tx);
        }

        // 5. try GC delete set
        if !self.doc.options.skip_gc {
            let state = self.state.as_deref_mut().unwrap();
            GCCollector::collect(self.doc, state);
        }

        {
            let state = self.state.as_mut().unwrap();

            // 6. try merge delete set
            state.delete_set.try_squash_with(self.doc);

            // 7. on all affected store.clients props, try to merge
            for (client, ids) in state.insert_set.iter() {
                if let Some(first_clock) = ids.clock_start() {
                    let blocks =
                        unsafe { self.doc.blocks.get_client_mut(client).unwrap_unchecked() };
                    // we iterate from right to left so we can safely remove entries
                    let first_change_pos =
                        blocks.find_index(first_clock).unwrap_or_default().max(1);
                    let mut i = blocks.len() - 1;
                    while i >= first_change_pos {
                        i = i.saturating_sub(1 + blocks.squash_left(i));
                    }
                }
            }

            // 8. get merge_structs and try to merge to left
            for id in state.merge_blocks.iter() {
                if let Some(blocks) = self.doc.blocks.get_client_mut(&id.client) {
                    if let Some(replaced_pos) = blocks.find_index(id.clock) {
                        if replaced_pos + 1 < blocks.len() {
                            blocks.squash_left(replaced_pos + 1);
                        } else if replaced_pos > 0 {
                            blocks.squash_left(replaced_pos);
                        }
                    }
                }
            }
        }

        reporter.encode(self.as_readonly());

        // 10. add and remove subdocs
        if let Some(subdocs) = self.state.as_mut().unwrap().subdocs.take() {
            self.handle_subdoc_events(subdocs);
        }

        reporter.committed(self.as_readonly());
        self.state = None;
    }

    /// Fills `changed_parent_types` with every changed node and its ancestors.
    fn collect_changes(state: &mut TransactionState, doc: &Doc) {
        let mut visited = HashSet::new();
        for &node in state.changed.keys() {
            state.needs_cleanup |= node.has_formatting && !state.local;
            visited.clear();
            for_each_ancestor(&doc.linked_by, node, &mut visited, &mut |n| {
                state.changed_parent_types.push(n)
            });
        }
    }

    fn handle_subdoc_events(&mut self, subdocs: Box<Subdocs>) {
        let client_id = self.doc.options.client_id;
        let collection_id = self.doc.options.collection_id.clone();
        for guid in subdocs.added.iter() {
            if let Some(subdoc) = self.doc.subdocs.get_mut(guid) {
                subdoc.options.client_id = client_id;
                if subdoc.options.collection_id.is_none() {
                    subdoc.options.collection_id = collection_id.clone();
                }
            }
        }
        let replaced: HashSet<_> = subdocs
            .removed
            .intersection(&subdocs.added)
            .cloned()
            .collect();

        for guid in subdocs.removed.iter() {
            if !replaced.contains(guid) {
                self.doc.subdocs.remove(guid);
            }
        }

        let removed = subdocs.removed;

        for guid in removed.iter() {
            if replaced.contains(guid) {
                continue;
            }
            if let Some(mut subdoc) = self.doc.subdocs.remove(guid) {
                subdoc.destroy(Some(self));
            }
        }
    }
}

/// Calls `f` with `node` and its ancestors, following weak links to their holders.
/// `visited` dedups link holders.
pub(crate) fn for_each_ancestor(
    links: &HashMap<ItemPtr, HashSet<NodePtr>>,
    node: NodePtr,
    visited: &mut HashSet<NodePtr>,
    f: &mut impl FnMut(NodePtr),
) {
    let mut current = node;
    loop {
        f(current);
        let Some(item) = current.item else { break };

        #[cfg(feature = "weak")]
        if item.info.is_linked() {
            if let Some(linked_by) = links.get(&item) {
                for &link in linked_by.iter() {
                    if visited.insert(link) {
                        for_each_ancestor(links, link, visited, f);
                    }
                }
            }
        }

        match item.parent {
            TypePtr::Node(parent) => current = parent,
            _ => break,
        }
    }
}

impl TransactionState {
    /// Applies given `id_set` onto current transaction to run multi-range deletion.
    /// Returns a remaining of original ID set, that couldn't be applied.
    pub(crate) fn apply_delete(&mut self, doc: &mut Doc, ds: &IdSet) -> Option<IdSet> {
        let mut unapplied = IdSet::new();
        for (client, ranges) in ds.iter() {
            if let Some(mut blocks) = doc.blocks.get_client_mut(client) {
                let state = blocks.clock();

                for range in ranges.iter() {
                    let clock = range.start;
                    let clock_end = range.end;

                    if clock < state {
                        if state < clock_end {
                            unapplied.insert(ID::new(*client, state), clock_end - state);
                        }
                        // We can ignore the case of GC and Delete structs, because we are going to skip them
                        if let Some(mut index) = blocks.find_index(clock) {
                            // We can ignore the case of GC and Delete structs, because we are going to skip them
                            let mut block = unsafe { blocks.get(index).unwrap_unchecked() };
                            let block = block.as_mut();
                            // split the first item if necessary
                            if !block.is_deleted() && block.clock_start() < clock {
                                if let Some(item) = block.as_item() {
                                    if let Some(split) =
                                        doc.blocks.split_block_inner(item, clock - item.id.clock)
                                    {
                                        index += 1;
                                        self.merge_blocks.push(*split.id());
                                    }
                                    blocks = doc.blocks.get_client_mut(client).unwrap();
                                }
                            }

                            while index < blocks.len() {
                                let mut block = unsafe { blocks.get(index).unwrap_unchecked() };
                                let block = block.as_mut();
                                index += 1;
                                if block.clock_start() < clock_end {
                                    if !block.is_deleted() {
                                        if let Some(item) = block.as_item() {
                                            if item.id.clock + item.len() > clock_end {
                                                if let Some(split) = doc.blocks.split_block_inner(
                                                    item,
                                                    clock_end - item.id.clock,
                                                ) {
                                                    if item.info.is_linked() {
                                                        if let Some(links) =
                                                            doc.linked_by.get(&item).cloned()
                                                        {
                                                            doc.linked_by.insert(split, links);
                                                        }
                                                    }

                                                    self.merge_blocks.push(*split.id());
                                                }
                                            }
                                            self.delete(doc, item);
                                            blocks = doc.blocks.get_client_mut(client).unwrap();
                                            // just to make the borrow checker happy
                                        } else {
                                            // is a Skip - add range to unappliedDS
                                            let clock = block.clock_start().max(clock);
                                            let len = block.len().min(clock_end - clock);
                                            unapplied.insert(ID::new(*client, clock), len);
                                        }
                                    }
                                } else {
                                    break;
                                }
                            }
                        }
                    } else {
                        unapplied.insert(ID::new(*client, clock), clock_end - clock);
                    }
                }
            } else {
                // Client doesn't exist in block store yet, so all deletes for this client
                // cannot be applied and should be marked as unapplied (pending)
                for range in ranges.iter() {
                    unapplied.insert(ID::new(*client, range.start), range.end - range.start);
                }
            }
        }

        if unapplied.is_empty() {
            None
        } else {
            Some(unapplied)
        }
    }
    /// Delete item under given pointer.
    /// Returns true if block was successfully deleted, false if it was already deleted in the past.
    pub(crate) fn delete(&mut self, doc: &mut Doc, mut item: ItemPtr) -> bool {
        let mut recurse = Vec::new();
        let mut result = false;

        if !item.is_deleted() {
            if item.parent_sub.is_none() && item.is_countable() {
                if let TypePtr::Node(mut parent) = item.parent {
                    parent.block_len -= item.len();
                    parent.content_len -= item.content_len(doc.options.offset_kind);
                }
            }

            item.mark_as_deleted();
            self.delete_set.insert(item.id.clone(), item.len());
            if let Some(parent) = item.parent.as_node() {
                self.add_changed_type(doc, *parent, item.parent_sub.clone());
            } else {
                // parent has been GC'ed
            }

            match &mut item.content {
                ItemContent::Doc(_, opts) => {
                    let subdocs = self.subdocs.get_or_init();
                    let guid = opts.guid.clone();
                    if !subdocs.added.remove(&guid) {
                        subdocs.removed.insert(guid);
                    }
                }
                ItemContent::Node(inner) => {
                    let branch_ptr = NodePtr::from(inner);
                    #[cfg(feature = "weak")]
                    if let TypeRef::WeakLink(source) = &branch_ptr.type_ref {
                        source.unlink_all(doc, self, branch_ptr);
                    }
                    let mut ptr = branch_ptr.start;
                    self.changed.remove(&branch_ptr);

                    while let Some(item) = ptr.as_deref() {
                        if !item.is_deleted() {
                            recurse.push(ptr.unwrap());
                        }

                        ptr = item.right.clone();
                    }

                    for ptr in branch_ptr.map.values() {
                        recurse.push(ptr.clone());
                    }
                }
                _ => { /* nothing to do for other content types */ }
            }
            if item.info.is_linked() {
                // notify links that current element has been removed
                if let Some(linked_by) = doc.linked_by.remove(&item) {
                    for link in linked_by {
                        self.add_changed_type(doc, link, item.parent_sub.clone());
                    }
                }
            }
            result = true;
        }

        for &ptr in recurse.iter() {
            let id = *ptr.id();
            if !self.delete(doc, ptr) {
                // Whis will be gc'd later and we want to merge it if possible
                // We try to merge all deleted items after each transaction,
                // but we have no knowledge about that this needs to be merged
                // since it is not in transaction.ds. Hence we add it to transaction._mergeStructs
                self.merge_blocks.push(id);
            }
        }

        result
    }
    /// Applies a deserialized [Update] contents into a document owning current transaction. Update
    /// payload can be generated by methods such as [TransactionMut::encode_diff] or written to
    /// [Reporter::encoder] by a commit. Updates are allowed to contain
    /// duplicate blocks (already presen in current document store) - these will be ignored.
    ///
    /// # Pending updates
    ///
    /// Remote update integration requires that all to-be-integrated blocks must have their direct
    /// predecessors already in place. Out of order updates from the same peer will be stashed
    /// internally and their integration will be postponed until missing blocks arrive first.
    pub(crate) fn apply_update(&mut self, doc: &mut Doc, mut update: Update) -> crate::Result<()> {
        // force that transaction.local is set to non-local
        self.local = false;

        // 1. Trim the incoming update with the blocks that we already have
        let known_state = doc.blocks.known_state(&update.blocks);
        update.blocks.exclude(&known_state);

        // we need to check early since update clients are consumed during `update.integrate`
        let mut retry = match &doc.pending {
            Some(pending) => pending
                .missing
                .iter()
                .any(|(client, _)| update.blocks.clients.contains_key(client)),
            None => false,
        };

        // 2. Integrate incoming update
        let (remaining, remaining_ds) = update.integrate(doc, self)?;

        // 3. Check if we have pending updates to integrate
        {
            let store = &mut *doc;
            store.pending = if let Some(mut pending) = store.pending.take() {
                // check if we can apply something
                for (client, &clock) in pending.missing.iter() {
                    if clock < store.blocks.get_clock(client) {
                        retry = true;
                        break;
                    }
                }

                if let Some(remaining) = remaining {
                    // merge restStructs into store.pending
                    for (&client, &clock) in remaining.missing.iter() {
                        pending.missing.set_min(client, clock);
                    }
                    pending.update = Update::merge_updates(vec![pending.update, remaining.update]);
                }
                Some(pending)
            } else {
                remaining
            };
        }

        // 4. Check if we have pending delete set to apply
        if let Some(pending_ds) = doc.pending_ds.take() {
            let ds2 = self.apply_delete(doc, &pending_ds);
            let ds = match (remaining_ds, ds2) {
                (Some(mut a), Some(b)) => {
                    a.delete_set.merge_with(b);
                    Some(a.delete_set)
                }
                (Some(x), _) => Some(x.delete_set),
                (_, Some(x)) => Some(x),
                _ => None,
            };
            doc.pending_ds = ds;
        } else {
            doc.pending_ds = remaining_ds.map(|update| update.delete_set);
        }

        // 5. check if we should reapply pending data
        if retry {
            if let Some(pending) = doc.pending.take() {
                let ds = doc.pending_ds.take().unwrap_or_default();
                let mut ds_update = Update::new();
                ds_update.delete_set = ds;
                self.apply_update(doc, pending.update)?;
                self.apply_update(doc, ds_update)?;
            }
        }

        Ok(())
    }
    pub(crate) fn create_map_item(
        &mut self,
        doc: &mut Doc,
        parent: NodePtr,
        key: Arc<str>,
        value: In,
    ) -> Option<ItemPtr> {
        let left = parent.map.get(&key).copied();
        let mut delta = None;
        let mut content = match value {
            In::Any(value) => ItemContent::Any(vec![value]),
            In::Node(node) => {
                let (inner, remainder) = Node::from_delta(*node);
                delta = remainder;
                ItemContent::Node(inner)
            }
            In::Doc(doc) => {
                let options = doc.options.clone();
                ItemContent::Doc(None, options)
            }
        };
        let inner_ref = if let ItemContent::Node(inner_ref) = &mut content {
            Some(NodePtr::from(inner_ref))
        } else {
            None
        };
        let id = ID::new(doc.client_id(), doc.get_local_state());
        let origin = left.as_ref().map(|i| i.last_id());
        let block = Item::new(
            id,
            left,
            origin,
            None,
            None,
            TypePtr::Node(parent),
            Some(key),
            content,
        )?;
        let block_ptr = self.integrate_item(doc, block, 0);

        if let Some(remainder) = delta {
            crate::node_ref::apply_delta(
                doc,
                self,
                inner_ref.unwrap(),
                remainder,
                &crate::DefaultFormatter,
                &mut crate::Delta::new(),
            );
        }

        block_ptr
    }
}

impl TransactionState {
    fn cleanup_fmt(&mut self, doc: &mut Doc) {
        let mut needs_cleanup = HashSet::new();
        // check if another formatting item was inserted
        for item in self.insert_set.iter_blocks(&doc.blocks) {
            if let Some(item) = item.as_item() {
                if !item.is_deleted() {
                    if let ItemContent::Format(_, _) = &item.content {
                        needs_cleanup.insert(*item.parent.as_node().unwrap());
                    }
                }
            }
        }

        // cleanup in a new transaction
        let cleanup = self
            .delete_set
            .iter_blocks(&doc.blocks)
            .filter_map(|slice| {
                let item = slice.as_item()?;
                let parent = item.parent.as_node()?;
                if parent.has_formatting && !needs_cleanup.contains(&parent) {
                    if let ItemContent::Format(_, _) = &item.content {
                        needs_cleanup.insert(*parent);
                    } else {
                        return Some(item);
                    }
                }
                None
            })
            .collect::<Vec<_>>();
        for item in cleanup {
            // If no formatting attribute was inserted or deleted, we can make due with contextless
            // formatting cleanups.
            // Contextless: it is not necessary to compute currentAttributes for the affected position.
            self.cleanup_fmt_gap_contextless(doc, item);
        }

        // If a formatting item was inserted, we simply clean the whole type.
        // We need to compute currentAttributes for the current position anyway.
        for text_ref in needs_cleanup {
            self.cleanup_text_fmt(doc, text_ref);
        }
    }
    fn cleanup_text_fmt(&mut self, doc: &mut Doc, text_ref: NodePtr) -> usize {
        if !doc.options.cleanup_formatting {
            return 0;
        }
        let mut res = 0;
        let mut start = text_ref.start;
        let mut end = text_ref.start;
        let mut start_attrs = HashMap::new();
        let mut current_attrs = HashMap::new();
        while let Some(endp) = end {
            if !endp.is_deleted() {
                match &endp.content {
                    ItemContent::Format(key, value) if &**value == &Any::Null => {
                        current_attrs.remove(key);
                    }
                    ItemContent::Format(key, value) => {
                        current_attrs.insert(key.clone(), value.clone());
                    }
                    _ => {
                        res +=
                            self.cleanup_fmt_gap(doc, start, end, &start_attrs, &mut current_attrs);
                        start_attrs = current_attrs.clone();
                        start = end;
                    }
                }
            }
            end = endp.right;
        }
        res
    }
    fn cleanup_fmt_gap_contextless(&mut self, doc: &mut Doc, mut item: ItemPtr) {
        // iterate until item.right is null or content
        while let Some(right) = item.right {
            if !right.is_deleted() && right.is_countable() {
                break; // we hit non-deleted non-format item
            }
            item = right;
        }
        let mut attrs = HashSet::new();
        // iterate back until a content item is found
        let mut itemo = Some(item);
        while let Some(item) = itemo {
            if !item.is_deleted() && item.is_countable() {
                break; // we hit non-deleted non-format item
            }
            if let ItemContent::Format(key, _) = &item.content {
                if !item.is_deleted() {
                    if !attrs.insert(key.clone()) {
                        self.delete(doc, item);
                        self.cleanups.insert(item.id, item.len);
                    }
                }
            }
            itemo = item.left;
        }
    }
    fn cleanup_fmt_gap(
        &mut self,
        doc: &mut Doc,
        mut start: Option<ItemPtr>,
        curr: Option<ItemPtr>,
        start_attrs: &HashMap<Arc<str>, Box<Any>>,
        curr_attrs: &mut HashMap<Arc<str>, Box<Any>>,
    ) -> usize {
        if !doc.options.cleanup_formatting {
            return 0;
        }
        let mut end = start;
        let mut end_fmts = HashMap::new();
        while let Some(endp) = end {
            if endp.is_countable() && !endp.is_deleted() {
                break;
            }

            if !endp.is_deleted() {
                if let ItemContent::Format(key, _) = &endp.content {
                    end_fmts.insert(key.clone(), endp);
                }
            }
            end = endp.right;
        }

        let mut cleanups = 0;
        let mut reached_curr = false;
        while start != end {
            if curr == start {
                reached_curr = true;
            }
            let startp = start.unwrap();
            if !startp.is_deleted() {
                if let ItemContent::Format(key, attr) = &startp.content {
                    let value = &**attr;
                    let start_attr_value = start_attrs.get(key).map_or(&Any::Null, |a| &**a);
                    if end_fmts.get(key) != Some(&startp) || start_attr_value == value {
                        // Either this format is overwritten or it is not necessary because the attribute already existed.
                        self.delete(doc, startp);
                        self.cleanups.insert(startp.id, startp.len);
                        cleanups += 1;

                        if !reached_curr
                            && curr_attrs.get(key).map_or(&Any::Null, |a| &**a) == value
                            && start_attr_value != value
                        {
                            if value == &Any::Null {
                                curr_attrs.remove(key);
                            } else {
                                curr_attrs.insert(key.clone(), Box::new(start_attr_value.clone()));
                            }
                        }
                    }
                    if !reached_curr && !startp.is_deleted() {
                        if value == &Any::Null {
                            curr_attrs.remove(key);
                        } else {
                            curr_attrs.insert(key.clone(), attr.clone());
                        }
                    }
                }
            }
            start = startp.right;
        }

        cleanups
    }
    pub(crate) fn add_changed_type(
        &mut self,
        _doc: &Doc,
        parent: NodePtr,
        parent_sub: Option<Arc<str>>,
    ) {
        let trigger = if let Some(ptr) = parent.item {
            !self.insert_set.contains(ptr.id()) && !ptr.is_deleted()
        } else {
            true
        };
        if trigger {
            let e = self.changed.entry(parent).or_default();
            e.insert(parent_sub.clone());
        }
    }
    pub(crate) fn split_by_snapshot(&mut self, doc: &mut Doc, snapshot: &Snapshot) {
        let mut merge_blocks: Vec<ID> = Vec::new();
        let blocks = &mut doc.blocks;
        for (&client, &clock) in snapshot.state_map.iter() {
            if let Some(ptr) = blocks.get_item(&ID::new(client, clock)) {
                let ptr_clock = ptr.id.clock;
                if ptr_clock < clock {
                    if let Some(right) = blocks.split_block_inner(ptr, clock - ptr_clock) {
                        merge_blocks.push(*right.id());
                    }
                }
            }
        }

        self.merge_blocks.append(&mut merge_blocks);
        let mut deleted = snapshot.delete_set.blocks();
        loop {
            let Some(slice) = deleted.next(&doc.transact()) else {
                break;
            };
            if let BlockSlice::Item(slice) = slice {
                //TODO: we technically don't need to physically split underlying item in two
                // if we were to use block slices all the way down.

                // split the blocks by delete set
                let ptr = doc.materialize(slice); // without loop this would fail on borrow checker
                self.merge_blocks.push(ptr.id);
            }
        }
    }
    #[cfg(feature = "weak")]
    pub(crate) fn unlink(&mut self, doc: &mut Doc, mut source: ItemPtr, link: NodePtr) {
        let all_links = &mut doc.linked_by;
        let prune = if let Some(linked_by) = all_links.get_mut(&source) {
            linked_by.remove(&link) && linked_by.is_empty()
        } else {
            false
        };
        if prune {
            all_links.remove(&source);
            source.info.clear_linked();
            if source.is_countable() {
                // since linked property is blocking items from merging,
                // it may turn out that source item can be merged now
                self.merge_blocks.push(source.id);
            }
        }
    }
}

impl<D: DocHandle> Drop for Transaction<D> {
    #[inline]
    fn drop(&mut self) {
        if let Some(state) = self.state.as_deref()
            && !state.committed
            && !std::thread::panicking()
        // drop during panics should not trigger commit
        {
            self.commit_internal();
        }
    }
}

impl<D: DocHandle> Transaction<D> {
    /// Kept out of line, so that dropping a transaction without pending changes stays cheap.
    #[inline(never)]
    fn commit_internal(&mut self) {
        if std::thread::panicking() {
            return; // committing during unwind would double-panic on unimplemented paths
        }
        if let Some(doc) = self.doc.try_doc_mut() {
            let mut txn = Transaction {
                doc,
                state: self.state.take(),
            };
            txn.commit_view(&mut NoReporter);
        }
    }
}

/// Iterator struct used to traverse over all of the root level types defined in a corresponding [Doc].
pub struct RootRefs<'doc>(std::collections::hash_map::Iter<'doc, Arc<str>, Box<Node>>);

impl<'doc> Iterator for RootRefs<'doc> {
    type Item = (&'doc str, Out);

    fn next(&mut self) -> Option<Self::Item> {
        let (key, branch) = self.0.next()?;
        let key = key.as_ref();
        let ptr = NodePtr::from(branch);
        Some((key, ptr.into()))
    }
}

/// Guids of subdocuments added, removed and loaded within a transaction.
#[derive(Default, Debug, Clone, serde::Serialize)]
pub struct Subdocs {
    pub(crate) added: HashSet<crate::Uuid>,
    pub(crate) removed: HashSet<crate::Uuid>,
    pub(crate) loaded: HashSet<crate::Uuid>,
}

impl Subdocs {
    /// Guids of subdocuments added to the document.
    pub fn added(&self) -> &HashSet<crate::Uuid> {
        &self.added
    }

    /// Guids of subdocuments removed from the document.
    pub fn removed(&self) -> &HashSet<crate::Uuid> {
        &self.removed
    }

    /// Guids of subdocuments which requested to be loaded.
    pub fn loaded(&self) -> &HashSet<crate::Uuid> {
        &self.loaded
    }
}

/// A binary marker that can be assigned to a read-write transaction upon creation via
/// [Transact::try_transact_mut_with]/[Transact::transact_mut_with]. It can be used to classify
/// transaction updates within a specific context, which exists for the duration of a transaction
/// (it's **not persisted** in the document store itself), i.e. *you can use unique document client
/// identifiers to differentiate updates incoming from remote nodes from those performed locally*.
#[repr(transparent)]
#[derive(Clone, Default, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct Origin(SmallVec<[u8; std::mem::size_of::<usize>()]>);

impl AsRef<[u8]> for Origin {
    fn as_ref(&self) -> &[u8] {
        self.0.as_ref()
    }
}

impl<'a, T> From<Pin<&'a T>> for Origin {
    fn from(p: Pin<&T>) -> Self {
        let ptr = Pin::get_ref(p) as *const T as usize;
        Origin(SmallVec::from_const(ptr.to_be_bytes()))
    }
}

impl<'a> From<&'a [u8]> for Origin {
    fn from(slice: &'a [u8]) -> Self {
        Origin(SmallVec::from_slice(slice))
    }
}

impl<'a> From<&'a str> for Origin {
    fn from(v: &'a str) -> Self {
        Origin(SmallVec::from_slice(v.as_ref()))
    }
}

impl From<String> for Origin {
    fn from(v: String) -> Self {
        Origin(SmallVec::from(Vec::from(v)))
    }
}

impl From<crate::block::ClientID> for Origin {
    fn from(v: crate::block::ClientID) -> Origin {
        Origin(SmallVec::from_slice(&v.get().to_be_bytes()))
    }
}

impl std::fmt::Debug for Origin {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

impl std::fmt::Display for Origin {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "Origin(")?;
        for b in self.0.iter() {
            write!(f, "{:02x?}", b)?;
        }
        write!(f, ")")
    }
}

macro_rules! impl_origin {
    ($t:ty) => {
        impl From<$t> for Origin {
            fn from(v: $t) -> Origin {
                Origin(SmallVec::from_slice(&v.to_be_bytes()))
            }
        }
    };
}

impl_origin!(u8);
impl_origin!(u16);
impl_origin!(u32);
impl_origin!(u64);
impl_origin!(u128);
impl_origin!(usize);
impl_origin!(i8);
impl_origin!(i16);
impl_origin!(i32);
impl_origin!(i64);
impl_origin!(i128);
impl_origin!(isize);
