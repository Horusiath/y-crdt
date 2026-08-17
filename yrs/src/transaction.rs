use crate::block::{ID, Item, ItemContent, ItemPosition, ItemPtr};
use crate::doc::DocEvents;
use crate::error::{Error, UpdateError};
use crate::event::{Event, SubdocsEvent};
use crate::gc::GCCollector;
use crate::id_set::DeleteSet;
use crate::iter::TxnIterator;
use crate::node::{Node, NodePtr, TypePtr, TypeRef};
use crate::slice::BlockSlice;
use crate::update::Update;
use crate::updates::encoder::{Encode, Encoder, EncoderV1, EncoderV2};
use crate::utils::OptionExt;
use crate::{
    Any, Doc, IdSet, In, NodeID, NodeRef, Out, Snapshot, StateVector, Uuid, merge_updates_v1,
    merge_updates_v2,
};
use smallvec::SmallVec;
use std::cell::OnceCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Formatter;
use std::hash::Hash;
use std::ops::Deref;
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

/// A transaction provides a controlled scope for reading and writing to a [Doc].
///
/// - `Transaction<&Doc>` is a lightweight **read-only** transaction.
/// - `Transaction<&mut Doc>` (aka [`TransactionMut`]) is a **read-write** transaction that
///   tracks changes and auto-commits when dropped.
///
/// Read-write transactions store information about all changes performed in their scope.
/// These are used during [`Transaction::commit`] to optimize metadata, trigger event callbacks, etc.
/// For performance, batch as many updates as possible in a single transaction.
///
/// Rollbacks are not supported. Use [UndoManager] to undo operations.
#[repr(C)]
pub struct Transaction<D> {
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
    pub(crate) changed: HashMap<TypePtr, HashSet<Option<Arc<str>>>>,
    pub(crate) changed_parent_types: Vec<NodePtr>,
    pub(crate) subdocs: Option<Box<Subdocs>>,
    pub(crate) origin: Option<Origin>,
    pub(crate) local: bool,
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

/// Ensures that the transaction state is initialized, creating it on demand if needed.
#[inline]
pub(crate) fn ensure_state(state: &mut Option<Box<TransactionState>>) -> &mut TransactionState {
    state.get_or_insert_with(|| Box::new(TransactionState::default()))
}

/// Read methods — available on both `Transaction<&Doc>` and `Transaction<&mut Doc>`.
impl<D: Deref<Target = Doc>> Transaction<D> {
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
        let ds = IdSet::from_store(blocks);
        Snapshot::new(sv, ds)
    }

    /// Encodes all changes from current transaction block store up to a given `snapshot`.
    pub fn encode_state_from_snapshot<E: Encoder>(
        &self,
        snapshot: &Snapshot,
        encoder: &mut E,
    ) -> Result<(), Error> {
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
        let ds = IdSet::from_store(&store.blocks);
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
    pub fn root_refs(&self) -> RootRefs {
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
        let state = self
            .state
            .as_ref()
            .expect("transaction state not initialized");
        state.before_state.get_or_init(|| {
            let mut sv = self.doc.blocks.get_state_vector();
            for (client, ranges) in state.insert_set.iter() {
                if let Some(clock) = ranges.clock_start() {
                    sv.set_min(*client, clock);
                }
            }
            sv
        })
    }

    /// State vector of the transaction after [Transaction::commit] has been called.
    pub fn after_state(&self) -> &StateVector {
        let state = self
            .state
            .as_ref()
            .expect("transaction state not initialized");
        state.after_state.get_or_init(|| {
            let mut sv = self.doc.blocks.get_state_vector();
            for (client, ranges) in state.insert_set.iter() {
                if let Some(clock) = ranges.clock_end() {
                    sv.set_max(*client, clock);
                }
            }
            sv
        })
    }

    /// Checks if item with a given `id` has been added to a block store within this transaction.
    pub(crate) fn has_added(&self, id: &ID) -> bool {
        match &self.state {
            None => false,
            Some(state) => state.insert_set.contains(id),
        }
    }

    /// Checks if item with a given `id` has been deleted within this transaction.
    pub(crate) fn has_deleted(&self, id: &ID) -> bool {
        match &self.state {
            None => false,
            Some(state) => state.delete_set.contains(id),
        }
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

/// Write methods — only available on `Transaction<&mut Doc>` (aka `TransactionMut`).
impl<'doc> Transaction<&'doc mut Doc> {
    #[inline]
    pub fn doc_mut(&mut self) -> &mut Doc {
        self.doc
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

    pub fn events(&self) -> Option<&DocEvents> {
        self.doc.events.as_deref()
    }

    pub fn events_mut(&mut self) -> &mut DocEvents {
        self.doc.events.get_or_init()
    }

    /// Encodes changes made within the scope of the current transaction using lib0 v1 encoding.
    ///
    /// Document updates are idempotent and commutative. Caveats:
    /// * It doesn't matter in which order document updates are applied.
    /// * As long as all clients receive the same document updates, all clients
    ///   end up with the same content.
    /// * Even if an update contains known information, the unknown information
    ///   is extracted and integrated into the document structure.
    pub fn encode_update_v1(&self) -> Vec<u8> {
        let mut encoder = EncoderV1::new();
        self.encode_update(&mut encoder);
        encoder.to_vec()
    }

    /// Encodes changes made within the scope of the current transaction using lib0 v2 encoding.
    ///
    /// Document updates are idempotent and commutative. Caveats:
    /// * It doesn't matter in which order document updates are applied.
    /// * As long as all clients receive the same document updates, all clients
    ///   end up with the same content.
    /// * Even if an update contains known information, the unknown information
    ///   is extracted and integrated into the document structure.
    pub fn encode_update_v2(&self) -> Vec<u8> {
        let mut encoder = EncoderV2::new();
        self.encode_update(&mut encoder);
        encoder.to_vec()
    }

    /// Encodes changes made within the scope of the current transaction.
    ///
    /// Document updates are idempotent and commutative. Caveats:
    /// * It doesn't matter in which order document updates are applied.
    /// * As long as all clients receive the same document updates, all clients
    ///   end up with the same content.
    /// * Even if an update contains known information, the unknown information
    ///   is extracted and integrated into the document structure.
    pub fn encode_update<E: Encoder>(&self, encoder: &mut E) {
        let store = &*self.doc;
        store.write_blocks_from(self.before_state(), encoder);
        self.delete_set().encode(encoder);
    }

    /// Applies given `id_set` onto current transaction to run multi-range deletion.
    /// Returns a remaining of original ID set, that couldn't be applied.
    pub(crate) fn apply_delete(&mut self, ds: &IdSet) -> Option<IdSet> {
        let mut unapplied = IdSet::new();
        for (client, ranges) in ds.iter() {
            if let Some(mut blocks) = self.doc.blocks.get_client_mut(client) {
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
                                    if let Some(split) = self
                                        .doc
                                        .blocks
                                        .split_block_inner(item, clock - item.id.clock)
                                    {
                                        index += 1;
                                        ensure_state(&mut self.state)
                                            .merge_blocks
                                            .push(*split.id());
                                    }
                                    blocks = self.doc.blocks.get_client_mut(client).unwrap();
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
                                                if let Some(split) =
                                                    self.doc.blocks.split_block_inner(
                                                        item,
                                                        clock_end - item.id.clock,
                                                    )
                                                {
                                                    if item.info.is_linked() {
                                                        if let Some(links) =
                                                            self.doc.linked_by.get(&item).cloned()
                                                        {
                                                            self.doc.linked_by.insert(split, links);
                                                        }
                                                    }

                                                    ensure_state(&mut self.state)
                                                        .merge_blocks
                                                        .push(*split.id());
                                                }
                                            }
                                            self.delete(item);
                                            blocks =
                                                self.doc.blocks.get_client_mut(client).unwrap();
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
    pub(crate) fn delete(&mut self, mut item: ItemPtr) -> bool {
        let mut recurse = Vec::new();
        let mut result = false;

        if !item.is_deleted() {
            if item.parent_sub.is_none() && item.is_countable() {
                if let TypePtr::Node(mut parent) = item.parent {
                    parent.block_len -= item.len();
                    parent.content_len -= item.content_len(self.doc.options.offset_kind);
                }
            }

            item.mark_as_deleted();
            ensure_state(&mut self.state)
                .delete_set
                .insert(item.id.clone(), item.len());
            if let Some(parent) = item.parent.as_node() {
                self.add_changed_type(*parent, item.parent_sub.clone());
            } else {
                // parent has been GC'ed
            }

            match &mut item.content {
                ItemContent::Doc(_, opts) => {
                    let subdocs = ensure_state(&mut self.state).subdocs.get_or_init();
                    let guid = opts.guid.clone();
                    if !subdocs.added.remove(&guid) {
                        subdocs.removed.insert(guid);
                    }
                }
                ItemContent::Node(inner) => {
                    let branch_ptr = NodePtr::from(inner);
                    #[cfg(feature = "weak")]
                    if let TypeRef::WeakLink(source) = &branch_ptr.type_ref {
                        source.unlink_all(self, branch_ptr);
                    }
                    let mut ptr = branch_ptr.start;
                    ensure_state(&mut self.state)
                        .changed
                        .remove(&TypePtr::Node(branch_ptr));

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
                if let Some(linked_by) = self.doc.linked_by.remove(&item) {
                    for link in linked_by {
                        self.add_changed_type(link, item.parent_sub.clone());
                    }
                }
            }
            result = true;
        }

        for &ptr in recurse.iter() {
            let id = *ptr.id();
            if !self.delete(ptr) {
                // Whis will be gc'd later and we want to merge it if possible
                // We try to merge all deleted items after each transaction,
                // but we have no knowledge about that this needs to be merged
                // since it is not in transaction.ds. Hence we add it to transaction._mergeStructs
                ensure_state(&mut self.state).merge_blocks.push(id);
            }
        }

        result
    }

    /// Applies a deserialized [Update] contents into a document owning current transaction. Update
    /// payload can be generated by methods such as [TransactionMut::encode_diff] or passed to
    /// [Doc::observe_update_v1]/[Doc::observe_update_v2] callbacks. Updates are allowed to contain
    /// duplicate blocks (already presen in current document store) - these will be ignored.
    ///
    /// # Pending updates
    ///
    /// Remote update integration requires that all to-be-integrated blocks must have their direct
    /// predecessors already in place. Out of order updates from the same peer will be stashed
    /// internally and their integration will be postponed until missing blocks arrive first.
    pub fn apply_update(&mut self, mut update: Update) -> Result<(), UpdateError> {
        // force that transaction.local is set to non-local
        ensure_state(&mut self.state).local = false;

        // 1. Trim the incoming update with the blocks that we already have
        let known_state = self.doc.blocks.known_state(&update.blocks);
        update.blocks.exclude(&known_state);

        // 2. Integrate incoming update
        let (remaining, remaining_ds) = update.integrate(self)?;

        // 3. Check if we have pending updates to integrate
        let mut retry = false;
        {
            let store = &mut *self.doc;
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
        if let Some(pending_ds) = self.doc.pending_ds.take() {
            let ds2 = self.apply_delete(&pending_ds);
            let ds = match (remaining_ds, ds2) {
                (Some(mut a), Some(b)) => {
                    a.delete_set.merge_with(b);
                    Some(a.delete_set)
                }
                (Some(x), _) => Some(x.delete_set),
                (_, Some(x)) => Some(x),
                _ => None,
            };
            self.doc.pending_ds = ds;
        } else {
            self.doc.pending_ds = remaining_ds.map(|update| update.delete_set);
        }

        // 5. check if we should reapply pending data
        if retry {
            if let Some(pending) = self.doc.pending.take() {
                let ds = self.doc.pending_ds.take().unwrap_or_default();
                let mut ds_update = Update::new();
                ds_update.delete_set = ds;
                self.apply_update(pending.update)?;
                self.apply_update(ds_update)?;
            }
        }

        Ok(())
    }

    pub(crate) fn create_item(
        &mut self,
        pos: &ItemPosition,
        value: In,
        parent_sub: Option<Arc<str>>,
    ) -> Option<ItemPtr> {
        let (left, right, origin, id) = {
            let left = pos.left;
            let right = pos.right;
            let origin = if let Some(item) = pos.left.as_deref() {
                Some(item.last_id())
            } else {
                None
            };
            let client_id = self.doc.options.client_id;
            let id = ID::new(client_id, self.doc.get_local_state());

            (left, right, origin, id)
        };
        // preliminary node contents are integrated only after the item wrapping them has been
        // integrated itself - otherwise nested content would have no parent to attach to
        let mut remainder = None;
        let mut content = match value {
            In::Any(value) => ItemContent::Any(vec![value]),
            In::Node(node) => {
                let type_ref = match &node.name {
                    Some(name) => TypeRef::XmlElement(name.clone()),
                    None => TypeRef::Undefined,
                };
                let inner = Node::new(node.name.clone(), type_ref);
                remainder = Some(node);
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
        let block = Item::new(
            id,
            left,
            origin,
            right,
            right.map(|r| r.id().clone()),
            pos.parent.clone(),
            parent_sub,
            content,
        )?;
        let block_ptr = self.integrate_item(block, 0);

        if let Some(remainder) = remainder {
            let mut node = NodeRef::new(inner_ref.unwrap(), &mut *self);
            node.apply_delta([remainder]);
        }

        block_ptr
    }

    fn call_type_observers(
        changed_parent_types: &mut Vec<NodePtr>,
        all_links: &HashMap<ItemPtr, HashSet<NodePtr>>,
        branch: NodePtr,
        changed_parents: &mut HashMap<NodePtr, Vec<usize>>,
        event_cache: &Vec<Event>,
        visited: &mut HashSet<NodePtr>,
    ) {
        let mut current = branch;
        loop {
            changed_parent_types.push(current);
            if current.deep_observers.has_subscribers() {
                let entries = changed_parents.entry(current).or_default();
                entries.push(event_cache.len() - 1);
            }

            if let Some(item) = current.item {
                if item.info.is_linked() {
                    if let Some(linked_by) = all_links.get(&item) {
                        for &link in linked_by.iter() {
                            if visited.insert(link) {
                                Self::call_type_observers(
                                    changed_parent_types,
                                    all_links,
                                    link,
                                    changed_parents,
                                    event_cache,
                                    visited,
                                )
                            }
                        }
                    }
                }
                if let TypePtr::Node(parent) = item.parent {
                    current = parent;
                    continue;
                }
            }

            break;
        }
    }

    fn call_observers(&mut self) {
        let mut changed_parents: HashMap<NodePtr, Vec<usize>> = HashMap::new();
        let mut event_cache = Vec::new();

        // Take changed out to avoid holding a mutable borrow on self.state during iteration
        let state = self.state.as_mut().unwrap();
        let changed = std::mem::take(&mut state.changed);
        let local = state.local;
        for (ptr, subs) in changed.iter() {
            if let TypePtr::Node(branch) = ptr {
                if branch.has_formatting && !local {
                    self.state.as_mut().unwrap().needs_cleanup = true;
                }
                let mut branch = *branch;
                // SAFETY: same as as_readonly() — identical layout, shared ref only
                let txn: &Transaction<&Doc> = unsafe { std::mem::transmute(&*self) };
                if let Some(e) = branch.trigger(txn, subs.clone()) {
                    event_cache.push(e);
                    let state = self.state.as_mut().unwrap();
                    Self::call_type_observers(
                        &mut state.changed_parent_types,
                        &self.doc.linked_by,
                        branch,
                        &mut changed_parents,
                        &event_cache,
                        &mut HashSet::default(),
                    );
                }
            }
        }

        // deep observe events
        for (&branch, events) in changed_parents.iter() {
            for &i in events.iter() {
                event_cache[i].set_current_target(branch);
            }

            // sort events by path length so that top-level events are fired first.
            // We don't need to check for events.length
            // because we know it has at least one element
            let mut sorted: Vec<&Event> = events.iter().map(|&i| &event_cache[i]).collect();
            sorted.sort_by_key(|e| e.path().len());

            let mut branch = branch;
            // SAFETY: same as as_readonly() — identical layout, shared ref only
            let txn: &Transaction<&Doc> = unsafe { std::mem::transmute(&*self) };
            branch.trigger_deep(txn, &sorted);
        }
    }

    /// Commits current transaction. This step involves cleaning up and optimizing changes performed
    /// during lifetime of a transaction. Such changes include squashing delete sets data,
    /// squashing blocks that have been appended one after another to preserve memory and triggering
    /// events.
    ///
    /// This step is performed automatically when a transaction is about to be dropped (its life
    /// scope comes to an end).
    pub fn commit(&mut self) {
        match self.state.as_ref() {
            None => return,
            Some(state) if state.committed => return,
            _ => {}
        }
        self.state.as_mut().unwrap().committed = true;

        // 2. emit 'beforeObserverCalls'
        if let Some(mut events) = self.doc.events.take() {
            events.emit_before_observer_calls(self);
            self.doc.events = Some(events);
        }
        // 3. for each change observed by the transaction call type observers
        if !self.state.as_ref().unwrap().changed.is_empty() {
            self.call_observers();
        }

        if self.state.as_ref().unwrap().needs_cleanup && self.doc.options.cleanup_formatting {
            self.cleanup_fmt();
        }

        // 4. emit 'afterTransaction'
        if let Some(mut events) = self.doc.events.take() {
            events.emit_after_transaction(self);
            self.doc.events = Some(events);
        }

        // 5. try GC delete set
        if !self.doc.options.skip_gc {
            GCCollector::collect(self);
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

        // 9. emit 'afterTransactionCleanup', 'update', 'updateV2'
        if let Some(mut events) = self.doc.events.take() {
            events.emit_transaction_cleanup(self);
            events.emit_update_v1(self);
            events.emit_update_v2(self);
            self.doc.events = Some(events);
        }

        // 10. add and remove subdocs
        if let Some(subdocs) = self.state.as_mut().unwrap().subdocs.take() {
            self.handle_subdoc_events(subdocs);
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

        let removed = if let Some(mut events) = self.doc.events.take() {
            let removed = if events.subdocs_events.has_subscribers() {
                let e = SubdocsEvent::new(subdocs);
                let txn = self.as_readonly();
                events.subdocs_events.trigger(|cb| cb(txn, &e));
                e.removed
            } else {
                subdocs.removed
            };
            self.doc.events = Some(events);
            removed
        } else {
            subdocs.removed
        };

        for guid in removed.iter() {
            if replaced.contains(guid) {
                continue;
            }
            if let Some(mut subdoc) = self.doc.subdocs.remove(guid) {
                subdoc.destroy(Some(self));
            }
        }
    }

    fn cleanup_fmt(&mut self) {
        let mut needs_cleanup = HashSet::new();
        let state = self.state.as_ref().unwrap();

        // check if another formatting item was inserted
        for item in state.insert_set.iter_blocks(&self.doc.blocks) {
            if let Some(item) = item.as_item() {
                if !item.is_deleted() {
                    if let ItemContent::Format(_, _) = &item.content {
                        needs_cleanup.insert(*item.parent.as_node().unwrap());
                    }
                }
            }
        }

        // cleanup in a new transaction
        let cleanup = state
            .delete_set
            .iter_blocks(&self.doc.blocks)
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
            self.cleanup_fmt_gap_contextless(item);
        }

        // If a formatting item was inserted, we simply clean the whole type.
        // We need to compute currentAttributes for the current position anyway.
        for text_ref in needs_cleanup {
            self.cleanup_text_fmt(text_ref);
        }
    }

    fn cleanup_text_fmt(&mut self, text_ref: NodePtr) -> usize {
        if !self.doc.options.cleanup_formatting {
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
                        res += self.cleanup_fmt_gap(start, end, &start_attrs, &mut current_attrs);
                        start_attrs = current_attrs.clone();
                        start = end;
                    }
                }
            }
            end = endp.right;
        }
        res
    }

    fn cleanup_fmt_gap_contextless(&mut self, mut item: ItemPtr) {
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
                        self.delete(item);
                        self.state
                            .as_mut()
                            .unwrap()
                            .cleanups
                            .insert(item.id, item.len);
                    }
                }
            }
            itemo = item.left;
        }
    }

    fn cleanup_fmt_gap(
        &mut self,
        mut start: Option<ItemPtr>,
        curr: Option<ItemPtr>,
        start_attrs: &HashMap<Arc<str>, Box<Any>>,
        curr_attrs: &mut HashMap<Arc<str>, Box<Any>>,
    ) -> usize {
        if !self.doc.options.cleanup_formatting {
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
                        self.delete(startp);
                        self.state
                            .as_mut()
                            .unwrap()
                            .cleanups
                            .insert(startp.id, startp.len);
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

    /// Perform garbage collection of deleted blocks, even if a document was created with `skip_gc`
    /// option.
    ///
    /// If `delete_set` is provided, it will be used to limit the scope of garbage collection
    /// to only those blocks that are present in the delete set. If `delete_set` is `None`, all
    /// deleted blocks will be considered for garbage collection.
    pub fn gc(&mut self, delete_set: Option<&IdSet>) {
        GCCollector::collect_all(self, delete_set);
    }

    pub(crate) fn add_changed_type(&mut self, parent: NodePtr, parent_sub: Option<Arc<str>>) {
        let trigger = if let Some(ptr) = parent.item {
            (ptr.id().clock < self.before_state().get(&ptr.id().client)) && !ptr.is_deleted()
        } else {
            true
        };
        if trigger {
            let e = ensure_state(&mut self.state)
                .changed
                .entry(parent.into())
                .or_default();
            e.insert(parent_sub.clone());
        }
    }

    pub(crate) fn split_by_snapshot(&mut self, snapshot: &Snapshot) {
        let mut merge_blocks: Vec<ID> = Vec::new();
        let blocks = &mut self.doc.blocks;
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

        ensure_state(&mut self.state)
            .merge_blocks
            .append(&mut merge_blocks);
        let mut deleted = snapshot.delete_set.blocks();
        while let Some(slice) = deleted.next(self) {
            if let BlockSlice::Item(slice) = slice {
                //TODO: we technically don't need to physically split underlying item in two
                // if we were to use block slices all the way down.

                // split the blocks by delete set
                let ptr = self.doc.materialize(slice);
                ensure_state(&mut self.state).merge_blocks.push(ptr.id);
            }
        }
    }

    #[cfg(feature = "weak")]
    pub(crate) fn unlink(&mut self, mut source: ItemPtr, link: NodePtr) {
        let all_links = &mut self.doc.linked_by;
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
                ensure_state(&mut self.state).merge_blocks.push(source.id);
            }
        }
    }
}

impl<D> Drop for Transaction<D> {
    fn drop(&mut self) {
        match self.state.as_ref() {
            None => return,
            Some(s) if s.committed => return,
            _ => {}
        }
        // SAFETY: Only Transaction<&mut Doc> can have state that is not None and not committed.
        // Transaction<&Doc> is always constructed with state = None.
        // Both variants have identical layout due to #[repr(C)] and pointer-sized D.
        let this: &mut Transaction<&mut Doc> = unsafe { std::mem::transmute(self) };
        this.commit();
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

#[derive(Default)]
pub struct Subdocs {
    pub(crate) added: HashSet<crate::Uuid>,
    pub(crate) removed: HashSet<crate::Uuid>,
    pub(crate) loaded: HashSet<crate::Uuid>,
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
