use crate::block::{Block, ClientID, ItemContent, ItemPtr};
use crate::block_store::BlockStore;
use crate::encoding::read::Error;
use crate::event::{SubdocsEvent, TransactionCleanupEvent, UpdateEvent};
use crate::id_set::DeleteSet;
use crate::node::{Node, NodePtr};
use crate::slice::ItemSlice;
use crate::transaction::TransactionState;
use crate::transaction::{Origin, TransactionMut};
use crate::types::{Path, PathSegment, ToJson, TypeRef};
use crate::update::PendingUpdate;
use crate::updates::decoder::{Decode, Decoder};
use crate::updates::encoder::{Encode, Encoder};
use crate::utils::OptionExt;
use crate::{Any, Subscription};
use crate::{
    ArrayRef, ID, IdSet, MapRef, NodeID, Snapshot, StateVector, TextRef, Transaction, Uuid,
    XmlFragmentRef, uuid_v4, uuid_v4_from,
};
use crate::{Observer, error};
use std::borrow::Borrow;
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::convert::TryFrom;
use std::fmt::Formatter;
use std::sync::Arc;

/// A Yrs document type. Documents are the most important units of collaborative resources management.
/// All shared collections live within a scope of their corresponding documents. All updates are
/// generated on per-document basis (rather than individual shared type). All operations on shared
/// collections happen via [Transaction](crate::Transaction), which lifetime is also bound to a document.
///
/// Document manages so-called root types, which are top-level shared types definitions (as opposed
/// to recursively nested types).
pub struct Doc {
    pub(crate) options: Options,

    /// Root types (a.k.a. top-level types). These types are defined by users at the document level,
    /// they have their own unique names and represent core shared types that expose operations
    /// which can be called concurrently by remote peers in a conflict-free manner.
    pub(crate) types: HashMap<Arc<str>, Box<Node>>,

    /// A block store of a current document. It represent all blocks (inserted or tombstoned
    /// operations) integrated - and therefore visible - into a current document.
    pub(crate) blocks: BlockStore,

    /// A pending update. It contains blocks, which are not yet integrated into `blocks`, usually
    /// because due to issues in update exchange, there were some missing blocks that need to be
    /// integrated first before the data from `pending` can be applied safely.
    pub(crate) pending: Option<PendingUpdate>,

    /// A pending delete set. Just like `pending`, it contains deleted ranges of blocks that have
    /// not been yet applied due to missing blocks that prevent `pending` update to be integrated
    /// into `blocks`.
    pub(crate) pending_ds: Option<IdSet>,

    /// Sub-documents owned by this store, keyed by their guid.
    pub(crate) subdocs: HashMap<Uuid, Doc>,

    pub(crate) events: Option<Box<DocEvents>>,

    /// Pointer to a parent block - present only if a current document is a sub-document of another
    /// document.
    pub(crate) parent: Option<ItemPtr>,

    /// Dependencies between items and weak links pointing to these items.
    pub(crate) linked_by: HashMap<ItemPtr, HashSet<NodePtr>>,
}

/// Generates `observe_*`, `observe_*_with`, and `unobserve_*` methods on [`Doc`] for a given
/// event. Each event group produces methods that take `&mut self` for exclusive access.
macro_rules! define_doc_observer {
    (
        $(#[doc = $doc:literal])*
        $observe:ident, $observe_with:ident, $unobserve:ident,
        $field:ident, $($bound:tt)+
    ) => {
        $(#[doc = $doc])*
        #[cfg(feature = "sync")]
        pub fn $observe<F>(&mut self, f: F) -> Subscription
        where
            F: $($bound)+ + Send + Sync + 'static,
        {
            let events = self.events.get_or_init();
            events.$field.subscribe(Box::new(f))
        }

        $(#[doc = $doc])*
        #[cfg(not(feature = "sync"))]
        pub fn $observe<F>(&mut self, f: F) -> Subscription
        where
            F: $($bound)+ + 'static,
        {
            let events = self.events.get_or_init();
            events.$field.subscribe(Box::new(f))
        }

        #[cfg(feature = "sync")]
        pub fn $observe_with<K, F>(&mut self, key: K, f: F)
        where
            K: Into<Origin>,
            F: $($bound)+ + Send + Sync + 'static,
        {
            let events = self.events.get_or_init();
            events.$field.subscribe_with(key.into(), Box::new(f));
        }

        #[cfg(not(feature = "sync"))]
        pub fn $observe_with<K, F>(&mut self, key: K, f: F)
        where
            K: Into<Origin>,
            F: $($bound)+ + 'static,
        {
            let events = self.events.get_or_init();
            events.$field.subscribe_with(key.into(), Box::new(f));
        }

        pub fn $unobserve<K>(&mut self, key: K) -> bool
        where
            K: Into<Origin>,
        {
            let events = self.events.get_or_init();
            events.$field.unsubscribe(&key.into())
        }
    };
}

impl Doc {
    /// Creates a new document with a randomized client identifier.
    pub fn new() -> Self {
        Self::with_options(Options::default())
    }

    /// Creates a new document with a specified `client_id`. It's up to a caller to guarantee that
    /// this identifier is unique across all communicating replicas of that document.
    pub fn with_client_id(client_id: u64) -> Self {
        Self::with_options(Options::with_client_id(ClientID::new(client_id)))
    }

    /// Creates a new document with a configured set of [Options].
    pub fn with_options(options: Options) -> Self {
        Doc {
            options,
            types: HashMap::default(),
            blocks: BlockStore::default(),
            subdocs: HashMap::default(),
            linked_by: HashMap::default(),
            events: None,
            pending: None,
            pending_ds: None,
            parent: None,
        }
    }

    pub(crate) fn new_subdoc(parent: ItemPtr, options: Options) -> Self {
        Doc {
            options,
            types: HashMap::default(),
            blocks: BlockStore::default(),
            subdocs: HashMap::default(),
            linked_by: HashMap::default(),
            events: None,
            pending: None,
            pending_ds: None,
            parent: Some(parent),
        }
    }

    /// Creates a lightweight read-only transaction.
    pub fn transact(&self) -> Transaction<&Doc> {
        Transaction {
            doc: self,
            state: None,
        }
    }

    /// Creates a read-write capable transaction.
    pub fn transact_mut(&mut self) -> TransactionMut<'_> {
        Transaction {
            doc: self,
            state: None,
        }
    }

    /// Creates a read-write capable transaction with an `origin` classifier attached.
    pub fn transact_mut_with<T: Into<Origin>>(&mut self, origin: T) -> TransactionMut<'_> {
        Transaction {
            doc: self,
            state: Some(Box::new(TransactionState::new(Some(origin.into())))),
        }
    }

    /// A unique client identifier, that's also a unique identifier of current document replica
    /// and it's subdocuments.
    ///
    /// Default: randomly generated.
    pub fn client_id(&self) -> ClientID {
        self.options.client_id
    }

    /// A globally unique identifier, that's also a unique identifier of current document replica,
    /// and unlike [Doc::client_id] it's not shared with its subdocuments.
    ///
    /// Default: randomly generated UUID v4.
    pub fn guid(&self) -> &Uuid {
        &self.options.guid
    }

    /// Returns a unique collection identifier, if defined.
    ///
    /// Default: `None`.
    pub fn collection_id(&self) -> Option<&Arc<str>> {
        self.options.collection_id.as_ref()
    }

    /// Informs if current document is skipping garbage collection on deleted collections
    /// on transaction commit.
    ///
    /// Default: `false`.
    pub fn skip_gc(&self) -> bool {
        self.options.skip_gc
    }

    /// If current document is subdocument, it will automatically for a document to load.
    ///
    /// Default: `false`.
    pub fn auto_load(&self) -> bool {
        self.options.auto_load
    }

    /// Whether the document should be synced by the provider now.
    /// This is toggled to true when you call [Doc::load]
    ///
    /// Default value: `true`.
    pub fn should_load(&self) -> bool {
        self.options.should_load
    }

    /// Returns encoding used to count offsets and lengths in text operations.
    pub fn offset_kind(&self) -> OffsetKind {
        self.options.offset_kind
    }

    define_doc_observer!(
        /// Subscribe callback function for any changes performed within transaction scope. These
        /// changes are encoded using lib0 v1 encoding and can be decoded using [Update::decode_v1]
        /// if necessary or passed to remote peers right away. This callback is triggered on
        /// function commit.
        observe_update_v1, observe_update_v1_with, unobserve_update_v1,
        update_v1_events, FnMut(&Transaction<&Doc>, &UpdateEvent)
    );

    define_doc_observer!(
        /// Subscribe callback function for any changes performed within transaction scope. These
        /// changes are encoded using lib0 v2 encoding and can be decoded using [Update::decode_v2]
        /// if necessary or passed to remote peers right away. This callback is triggered on
        /// function commit.
        observe_update_v2, observe_update_v2_with, unobserve_update_v2,
        update_v2_events, FnMut(&Transaction<&Doc>, &UpdateEvent)
    );

    define_doc_observer!(
        /// Subscribe callback function to updates on the `Doc`. The callback will receive state
        /// updates and deletions when a document transaction is committed.
        observe_transaction_cleanup, observe_transaction_cleanup_with, unobserve_transaction_cleanup,
        transaction_cleanup_events, FnMut(&Transaction<&Doc>, &TransactionCleanupEvent)
    );

    define_doc_observer!(
        observe_after_transaction,
        observe_after_transaction_with,
        unobserve_after_transaction,
        after_transaction_events,
        FnMut(&mut TransactionMut)
    );

    define_doc_observer!(
        /// Subscribe a callback that fires after the transaction body completes but before
        /// type-level observers are triggered. This is used by attribution managers to update
        /// their internal state before any observer reads attribution data.
        observe_before_observer_calls, observe_before_observer_calls_with, unobserve_before_observer_calls,
        before_observer_calls_events, FnMut(&Transaction<&Doc>)
    );

    define_doc_observer!(
        /// Subscribe callback function, that will be called whenever a subdocuments inserted in
        /// this [Doc] will request a load.
        observe_subdocs, observe_subdocs_with, unobserve_subdocs,
        subdocs_events, FnMut(&Transaction<&Doc>, &SubdocsEvent)
    );

    define_doc_observer!(
        /// Subscribe callback function, that will be called whenever a [Doc::destroy] has been
        /// called.
        observe_destroy, observe_destroy_with, unobserve_destroy,
        destroy_events, FnMut(&Transaction<&Doc>, &Doc)
    );

    /// Sends a load request to a parent document. Works only if current document is a sub-document
    /// of a document.
    pub fn load(&mut self, parent_txn: &mut TransactionMut) {
        let was_loaded = self.options.should_load;
        self.options.should_load = true;
        if !was_loaded && self.is_subdoc() {
            let guid = self.options.guid.clone();
            parent_txn.subdocs_mut().loaded.insert(guid);
        }
    }

    /// Starts destroy procedure for a current document, triggering an "destroy" callback and
    /// invalidating all event callback subscriptions.
    pub fn destroy(&mut self, parent_txn: Option<&mut TransactionMut<'_>>) {
        // Recursively destroy subdocs
        let subdoc_guids: Vec<_> = self.subdocs.keys().cloned().collect();
        for guid in subdoc_guids {
            if let Some(subdoc) = self.subdocs.get_mut(&guid) {
                subdoc.destroy(None);
            }
        }
        if let Some(parent_txn) = parent_txn {
            if let Some(mut item) = self.parent.take() {
                let parent_ref = item.clone();
                let is_deleted = item.is_deleted();
                if let Some(opts) = item.content.as_subdoc_options() {
                    let mut options = opts.clone();
                    options.should_load = false;
                    let guid = options.guid.clone();
                    let new_doc = Doc::new_subdoc(parent_ref, options.clone());
                    parent_txn.doc.subdocs.insert(guid.clone(), new_doc);
                    // Update ItemContent with new options
                    item.content = ItemContent::Doc(None, options);
                    if !is_deleted {
                        parent_txn.subdocs_mut().added.insert(guid.clone());
                    }
                    parent_txn.subdocs_mut().removed.insert(guid);
                }
            }
        }
        // cleanup events
        if let Some(mut events) = self.events.take() {
            let doc_ptr = self as *const Doc;
            let txn: TransactionMut = Transaction {
                doc: self,
                state: None,
            };
            unsafe {
                let doc_ref = &*doc_ptr;
                let txn_ref = txn.as_readonly();
                events.destroy_events.trigger(|cb| cb(txn_ref, doc_ref));
            }
        }
    }

    /// If current document has been inserted as a sub-document, returns a reference to a parent
    /// document, which contains it.
    pub fn parent_doc(&self) -> Option<Uuid> {
        self.transact().parent_doc()
    }

    pub fn node_id(&self) -> Option<NodeID> {
        self.transact().node_id()
    }

    /// Returns a reference to the document's [Options].
    pub fn options(&self) -> &Options {
        &self.options
    }

    /// If there are any missing updates, this method will return a pending update which contains
    /// updates waiting for their predecessors to arrive in order to be integrated.
    pub fn pending_update(&self) -> Option<&PendingUpdate> {
        self.pending.as_ref()
    }

    /// Returns a mutable reference to the pending update if it exists.
    pub fn pending_update_mut(&mut self) -> Option<&mut PendingUpdate> {
        self.pending.as_mut()
    }

    /// If there are some delete updates waiting for missing updates to arrive in order to be
    /// applied, this method will return them.
    pub fn pending_ds(&self) -> Option<&IdSet> {
        self.pending_ds.as_ref()
    }

    /// Returns a mutable reference to the pending delete set if it exists.
    pub fn pending_ds_mut(&mut self) -> Option<&mut IdSet> {
        self.pending_ds.as_mut()
    }

    pub fn is_subdoc(&self) -> bool {
        self.parent.is_some()
    }

    /// Get the latest clock sequence number observed and integrated into a current store client.
    /// This is exclusive value meaning it describes a clock value of the beginning of the next
    /// block that's about to be inserted. You cannot use that clock value to find any existing
    /// block content.
    pub fn get_local_state(&self) -> u32 {
        self.blocks.get_clock(&self.options.client_id)
    }

    /// Returns a branch reference to a complex type identified by its pointer. Returns `None` if
    /// no such type could be found or was ever defined.
    pub(crate) fn get_type<K: Borrow<str>>(&self, key: K) -> Option<NodePtr> {
        let ptr = NodePtr::from(self.types.get(key.borrow())?);
        Some(ptr)
    }

    /// Returns a branch reference to a complex type identified by its pointer. Returns `None` if
    /// no such type could be found or was ever defined.
    pub(crate) fn get_or_create_type<K: Into<Arc<str>>>(
        &mut self,
        key: K,
        type_ref: TypeRef,
    ) -> NodePtr {
        let key = key.into();
        match self.types.entry(key.clone()) {
            Entry::Occupied(e) => {
                let mut branch = NodePtr::from(e.get());
                branch.repair_type_ref(type_ref);
                branch
            }
            Entry::Vacant(e) => {
                let mut branch = Node::new(type_ref);
                let mut branch_ref = NodePtr::from(&mut branch);
                branch_ref.name = Some(key);
                e.insert(branch);
                branch_ref
            }
        }
    }

    /// Encodes all changes from current transaction block store up to a given `snapshot`.
    /// This enables to encode state of a document at some specific point in the past.
    pub fn encode_state_from_snapshot<E: Encoder>(
        &self,
        snapshot: &Snapshot,
        encoder: &mut E,
    ) -> Result<(), error::Error> {
        if !self.options.skip_gc {
            return Err(error::Error::Gc);
        }
        self.write_blocks_to(&snapshot.state_map, encoder);
        snapshot.delete_set.encode(encoder);

        Ok(())
    }

    pub(crate) fn write_blocks_to<E: Encoder>(&self, sv: &StateVector, encoder: &mut E) {
        let local_sv = self.blocks.get_state_vector();
        let mut diff = Vec::with_capacity(sv.len());
        for (&client_id, &clock) in sv.iter() {
            if local_sv.contains_client(&client_id) {
                diff.push((client_id, clock.min(local_sv.get(&client_id))));
            }
        }
        // Write items with higher client ids first
        // This heavily improves the conflict algorithm.
        diff.sort_by(|a, b| b.0.cmp(&a.0));

        encoder.write_var(diff.len());
        for (client, clock) in diff {
            let blocks = self.blocks.get_client(&client).unwrap();
            let clock = clock.min(blocks.clock() + 1);
            let last_idx = blocks.find_index(clock - 1).unwrap();
            // write # encoded structs
            encoder.write_var(last_idx + 1);
            encoder.write_client(client);
            encoder.write_var(0);
            for i in 0..last_idx {
                let block = blocks[i].as_slice();
                block.encode(encoder);
            }
            let last_block = &blocks[last_idx];
            // write first struct with an offset
            let mut slice = last_block.as_slice();
            slice.trim_end(slice.clock_end() - (clock - 1));
            slice.encode(encoder);
        }
    }

    /// Compute a diff to sync with another client.
    ///
    /// This is the most efficient method to sync with another client by only
    /// syncing the differences.
    ///
    /// The sync protocol in Yrs/js is:
    /// * Send StateVector to the other client.
    /// * The other client comutes a minimal diff to sync by using the StateVector.
    pub fn encode_diff<E: Encoder>(&self, sv: &StateVector, encoder: &mut E) {
        //TODO: this could be actually 2 steps:
        // 1. create Diff of block store and remote state vector (it can have lifetime of bock store)
        // 2. make Diff implement Encode trait and encode it
        // this way we can add some extra utility method on top of Diff (like introspection) without need of decoding it.
        self.write_blocks_from(sv, encoder);
        let delete_set = IdSet::from_store(&self.blocks);
        delete_set.encode(encoder);
    }

    pub(crate) fn write_blocks_from<E: Encoder>(&self, sv: &StateVector, encoder: &mut E) {
        let local_sv = self.blocks.get_state_vector();
        let mut diff = Self::diff_state_vectors(&local_sv, sv);

        // Write items with higher client ids first
        // This heavily improves the conflict algorithm.
        diff.sort_by(|a, b| b.0.cmp(&a.0));

        encoder.write_var(diff.len());
        for (client, clock) in diff {
            let blocks = self.blocks.get_client(&client).unwrap();
            let clock = clock.max(
                blocks
                    .get(0)
                    .map(|i| i.as_ref().clock_start())
                    .unwrap_or_default(),
            ); // make sure the first id exists
            let start = blocks.find_index(clock).unwrap();
            // write # encoded structs
            encoder.write_var(blocks.len() - start);
            encoder.write_client(client);
            encoder.write_var(clock);
            let first_block = blocks.get(start).unwrap().as_ref();
            // write first struct with an offset
            let offset = clock - first_block.clock_start();
            let mut slice = first_block.as_slice();
            slice.trim_start(offset);
            slice.encode(encoder);
            for i in (start + 1)..blocks.len() {
                let block = &blocks[i];
                block.as_slice().encode(encoder);
            }
        }
    }

    fn diff_state_vectors(local_sv: &StateVector, remote_sv: &StateVector) -> Vec<(ClientID, u32)> {
        let mut diff = Vec::new();
        for (client, &remote_clock) in remote_sv.iter() {
            let local_clock = local_sv.get(client);
            if local_clock > remote_clock {
                diff.push((*client, remote_clock));
            }
        }
        for (client, _) in local_sv.iter() {
            if !remote_sv.contains_client(client) {
                diff.push((*client, 0));
            }
        }
        diff
    }

    pub fn get_type_from_path(&self, path: &Path) -> Option<NodePtr> {
        let mut i = path.iter();
        if let Some(PathSegment::Key(root_name)) = i.next() {
            let mut current = self.get_type(root_name.clone())?;
            while let Some(segment) = i.next() {
                match segment {
                    PathSegment::Key(key) => {
                        let child = current.map.get(key)?;
                        if let ItemContent::Node(child_branch) = &child.content {
                            current = NodePtr::from(child_branch.as_ref());
                        } else {
                            return None;
                        }
                    }
                    PathSegment::Index(index) => {
                        if let Some((ItemContent::Node(child_branch), _)) = current.get_at(*index) {
                            current = child_branch.into();
                        } else {
                            return None;
                        }
                    }
                }
            }
            Some(current)
        } else {
            None
        }
    }

    /// Consumes current block slice view, materializing it into actual block representation equivalent,
    /// splitting underlying block along [ItemSlice::start]/[ItemSlice::end] offsets.
    ///
    /// Returns a block created this way, that represents the boundaries that current [ItemSlice]
    /// was representing.
    pub(crate) fn materialize(&mut self, mut slice: ItemSlice) -> ItemPtr {
        let id = slice.id().clone();
        let blocks = self.blocks.get_client_mut(&id.client).unwrap();
        let mut links = None;
        let item = &*slice.ptr;
        if item.info.is_linked() {
            links = self.linked_by.get(&slice.ptr).cloned();
        }

        let mut index = None;
        let mut ptr = if slice.adjacent_left() {
            slice.ptr
        } else {
            let mut i = blocks.find_index(id.clock).unwrap();
            if let Some(new) = slice.ptr.splice(slice.start, OffsetKind::Utf16) {
                if let Some(source) = links.clone() {
                    let dest = self
                        .linked_by
                        .entry(ItemPtr::from(new.as_ref()))
                        .or_default();
                    dest.extend(source);
                }
                blocks.insert(i + 1, Block::Item(new));
                i += 1;
                //todo: txn merge blocks insert?
                index = Some(i);
            }
            let ptr = blocks[i].as_item().unwrap();
            slice = ItemSlice::new(ptr, 0, slice.end - slice.start);
            ptr
        };

        if !slice.adjacent_right() {
            // split block on the right side
            let i = if let Some(i) = index {
                i
            } else {
                let last_id = slice.last_id();
                blocks.find_index(last_id.clock).unwrap()
            };
            let new = ptr.splice(slice.len(), OffsetKind::Utf16).unwrap();
            if let Some(source) = links {
                let dest = self
                    .linked_by
                    .entry(ItemPtr::from(new.as_ref()))
                    .or_default();
                dest.extend(source);
            }
            blocks.insert(i + 1, Block::Item(new));
            //todo: txn merge blocks insert?
        }

        ptr
    }

    /// Returns a collection of sub documents linked within the structures of this document store.
    pub fn subdocs(&self) -> impl Iterator<Item = &Doc> {
        self.subdocs.values()
    }

    /// Returns a collection of sub documents linked within the structures of this document store.
    pub fn subdocs_mut(&mut self) -> impl Iterator<Item = &mut Doc> {
        self.subdocs.values_mut()
    }

    /// Returns a collection of globally unique identifiers of sub documents linked within
    /// the structures of this document store.
    pub fn subdoc_guids(&self) -> impl Iterator<Item = &Uuid> {
        self.subdocs.keys()
    }

    /// Returns a mutable reference to a subdoc by guid.
    pub(crate) fn subdoc_mut(&mut self, guid: &Uuid) -> Option<&mut Doc> {
        self.subdocs.get_mut(guid)
    }

    /// Returns a reference to a subdoc by guid.
    pub(crate) fn subdoc(&self, guid: &Uuid) -> Option<&Doc> {
        self.subdocs.get(guid)
    }

    pub(crate) fn follow_redone(&self, id: &ID) -> Option<ItemSlice> {
        let mut next_id = Some(*id);
        let mut slice = None;
        while let Some(next) = next_id.as_mut() {
            slice = self.blocks.get_item_clean_start(next);
            if let Some(slice) = &slice {
                next_id = slice.ptr.redone;
            } else {
                break;
            }
        }
        slice
    }
}

impl PartialEq for Doc {
    fn eq(&self, other: &Self) -> bool {
        self.options.guid == other.options.guid
    }
}

impl std::fmt::Debug for Doc {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Doc(id: {}, guid: {})",
            self.options.client_id, self.options.guid
        )
    }
}

impl std::fmt::Display for Doc {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Doc(id: {}, guid: {})",
            self.options.client_id, self.options.guid
        )
    }
}

impl Encode for Doc {
    /// Encodes the document state to a binary format.
    ///
    /// Document updates are idempotent and commutative. Caveats:
    /// * It doesn't matter in which order document updates are applied.
    /// * As long as all clients receive the same document updates, all clients
    ///   end up with the same content.
    /// * Even if an update contains known information, the unknown information
    ///   is extracted and integrated into the document structure.
    fn encode<E: Encoder>(&self, encoder: &mut E) {
        self.encode_diff(&StateVector::default(), encoder)
    }
}

impl Default for Doc {
    fn default() -> Self {
        Doc::new()
    }
}

impl ToJson for Doc {
    fn to_json<D: std::ops::Deref<Target = Doc>>(&self, txn: &Transaction<D>) -> Any {
        let mut m = HashMap::new();
        for (key, value) in txn.root_refs() {
            m.insert(key.to_string(), value.to_json(txn));
        }
        Any::from(m)
    }
}

macro_rules! define_event_type {
    ($name:ident ($($args:tt)*)) => {
        #[cfg(feature = "sync")]
        pub type $name = Box<dyn FnMut($($args)*) + Send + Sync + 'static>;
        #[cfg(not(feature = "sync"))]
        pub type $name = Box<dyn FnMut($($args)*) + 'static>;
    };
}

define_event_type!(TransactionCleanupFn(&Transaction<&Doc>, &TransactionCleanupEvent));
define_event_type!(AfterTransactionFn(&mut TransactionMut));
define_event_type!(UpdateFn(&Transaction<&Doc>, &UpdateEvent));
define_event_type!(SubdocsFn(&Transaction<&Doc>, &SubdocsEvent));
define_event_type!(DestroyFn(&Transaction<&Doc>, &Doc));
define_event_type!(BeforeObserverCallsFn(&Transaction<&Doc>));

#[derive(Default)]
pub struct DocEvents {
    /// Handles subscriptions for the transaction cleanup event. Events are called with the
    /// newest updates once they are committed and compacted.
    pub transaction_cleanup_events: Observer<TransactionCleanupFn>,

    /// Handles subscriptions for the `afterTransactionCleanup` event. Events are called with the
    /// newest updates once they are committed and compacted.
    pub after_transaction_events: Observer<AfterTransactionFn>,

    /// A subscription handler. It contains all callbacks with registered by user functions that
    /// are supposed to be called, once a new update arrives.
    pub update_v1_events: Observer<UpdateFn>,

    /// A subscription handler. It contains all callbacks with registered by user functions that
    /// are supposed to be called, once a new update arrives.
    pub update_v2_events: Observer<UpdateFn>,

    /// Handles subscriptions for subdocs events.
    pub subdocs_events: Observer<SubdocsFn>,

    pub destroy_events: Observer<DestroyFn>,

    /// Handles subscriptions for the `beforeObserverCalls` event. Callbacks are called after
    /// the transaction body completes but before type-level observers are triggered.
    pub before_observer_calls_events: Observer<BeforeObserverCallsFn>,
}

impl DocEvents {
    pub fn emit_update_v1(&mut self, txn: &TransactionMut) {
        if self.update_v1_events.has_subscribers() {
            if !txn.delete_set().is_empty() || txn.after_state() != txn.before_state() {
                let update = UpdateEvent::new_v1(txn);
                let txn = txn.as_readonly();
                self.update_v1_events
                    .trigger(|callback| callback(txn, &update));
            }
        }
    }

    pub fn emit_update_v2(&mut self, txn: &TransactionMut) {
        if self.update_v2_events.has_subscribers() {
            if !txn.delete_set().is_empty() || txn.after_state() != txn.before_state() {
                let update = UpdateEvent::new_v2(txn);
                let txn = txn.as_readonly();
                self.update_v2_events.trigger(|fun| fun(txn, &update));
            }
        }
    }

    pub fn emit_after_transaction(&mut self, txn: &mut TransactionMut) {
        self.after_transaction_events.trigger(|fun| fun(txn));
    }

    pub fn emit_transaction_cleanup(&mut self, txn: &TransactionMut) {
        if self.transaction_cleanup_events.has_subscribers() {
            let event = TransactionCleanupEvent::new(txn);
            let txn = txn.as_readonly();
            self.transaction_cleanup_events
                .trigger(|fun| fun(txn, &event));
        }
    }

    pub fn emit_before_observer_calls(&mut self, txn: &TransactionMut) {
        let txn = txn.as_readonly();
        self.before_observer_calls_events.trigger(|fun| fun(txn));
    }
}

/// Configuration options of [Doc] instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// Globally unique client identifier. This value must be unique across all active collaborating
    /// peers, otherwise a update collisions will happen, causing document store state to be corrupted.
    ///
    /// Default value: randomly generated.
    pub client_id: ClientID,
    /// A globally unique identifier for this document.
    ///
    /// Default value: randomly generated UUID v4.
    pub guid: Uuid,
    /// Associate this document with a collection. This only plays a role if your provider has
    /// a concept of collection.
    ///
    /// Default value: `None`.
    pub collection_id: Option<Arc<str>>,
    /// How to we count offsets and lengths used in text operations.
    ///
    /// Default value: [OffsetKind::Bytes].
    pub offset_kind: OffsetKind,
    /// Determines if transactions commits should try to perform GC-ing of deleted items.
    ///
    /// Default value: `false`.
    pub skip_gc: bool,
    /// If a subdocument, automatically load document. If this is a subdocument, remote peers will
    /// load the document as well automatically.
    ///
    /// Default value: `false`.
    pub auto_load: bool,
    /// Whether the document should be synced by the provider now.
    /// This is toggled to true when you call ydoc.load().
    ///
    /// Default value: `true`.
    pub should_load: bool,

    /// Whenever we receive an update that might remove piece of text, it might turn out that it was
    /// surrounded by the formatting attributes, that now are effectively dead and unrenderable, but
    /// still are considered alive blocks.
    ///
    /// This flag orders cleanup of dangling formatting attributes.
    pub cleanup_formatting: bool,
}

impl Options {
    pub fn with_client_id(client_id: ClientID) -> Self {
        Options {
            client_id,
            guid: uuid_v4(),
            collection_id: None,
            offset_kind: OffsetKind::Bytes,
            skip_gc: false,
            auto_load: false,
            should_load: true,
            cleanup_formatting: true,
        }
    }

    pub fn with_guid_and_client_id(guid: Uuid, client_id: ClientID) -> Self {
        Options {
            client_id,
            guid,
            collection_id: None,
            offset_kind: OffsetKind::Bytes,
            skip_gc: false,
            auto_load: false,
            should_load: true,
            cleanup_formatting: false,
        }
    }

    fn as_any(&self) -> Any {
        let mut m = HashMap::new();
        m.insert("gc".to_owned(), (!self.skip_gc).into());
        if let Some(collection_id) = self.collection_id.as_ref() {
            m.insert("collectionId".to_owned(), collection_id.clone().into());
        }
        let encoding = match self.offset_kind {
            OffsetKind::Bytes => 1,
            OffsetKind::Utf16 => 0, // 0 for compatibility with Yjs, which doesn't have this option
        };
        m.insert("encoding".to_owned(), Any::BigInt(encoding));
        m.insert("autoLoad".to_owned(), self.auto_load.into());
        m.insert("shouldLoad".to_owned(), self.should_load.into());
        Any::from(m)
    }
}

impl Default for Options {
    fn default() -> Self {
        let client_id = ClientID::random();
        let mut rng = fastrand::Rng::new();
        let uuid = uuid_v4_from(rng.u128(..));
        Self::with_guid_and_client_id(uuid, client_id)
    }
}

impl Encode for Options {
    fn encode<E: Encoder>(&self, encoder: &mut E) {
        let guid = self.guid.to_string();
        encoder.write_string(&guid);
        encoder.write_any(&self.as_any())
    }
}

impl Decode for Options {
    fn decode<D: Decoder>(decoder: &mut D) -> Result<Self, Error> {
        let mut options = Options::default();
        options.should_load = false; // for decoding shouldLoad is false by default
        let guid = decoder.read_string()?;
        options.guid = guid.into();

        if let Any::Map(opts) = decoder.read_any()? {
            for (k, v) in opts.iter() {
                match (k.as_str(), v) {
                    ("gc", Any::Bool(gc)) => options.skip_gc = !*gc,
                    ("autoLoad", Any::Bool(auto_load)) => options.auto_load = *auto_load,
                    ("collectionId", Any::String(cid)) => options.collection_id = Some(cid.clone()),
                    ("encoding", Any::BigInt(1)) => options.offset_kind = OffsetKind::Bytes,
                    ("encoding", _) => options.offset_kind = OffsetKind::Utf16,
                    _ => { /* do nothing */ }
                }
            }
        }

        Ok(options)
    }
}

/// Determines how string length and offsets of [Text]/[XmlText] are being determined.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OffsetKind {
    /// Compute editable strings length and offset using UTF-8 byte count.
    Bytes,
    /// Compute editable strings length and offset using UTF-16 chars count.
    Utf16,
}

#[cfg(test)]
mod test {
    use crate::block::{Block, BlockRange, ClientID, ItemContent};
    use crate::error::Error;
    use crate::test_utils::{Blocks, exchange_updates};
    use crate::transaction::TransactionMut;
    use crate::update::Update;
    use crate::updates::decoder::Decode;
    use crate::updates::encoder::{Encode, Encoder, EncoderV1};
    use crate::{
        Any, Doc, ID, IdSet, OffsetKind, Options, Snapshot, StateVector, Subscription, Transaction,
        TransactionCleanupEvent, UpdateEvent, Uuid, any, uuid_v4,
    };
    use arc_swap::ArcSwapOption;
    use assert_matches2::assert_matches;
    use std::collections::BTreeSet;
    use std::iter::FromIterator;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};

    /// Load a subdoc by guid during a transaction.
    fn load_subdoc(txn: &mut TransactionMut, guid: &Uuid) {
        if let Some(mut subdoc) = txn.doc.subdocs.remove(guid) {
            subdoc.load(txn);
            txn.doc.subdocs.insert(guid.clone(), subdoc);
        }
    }

    /// Destroy a subdoc by guid during a transaction.
    fn destroy_subdoc(txn: &mut TransactionMut, guid: &Uuid) {
        if let Some(mut subdoc) = txn.doc.subdocs.remove(guid) {
            subdoc.destroy(Some(txn));
        }
    }

    #[test]
    fn apply_update_basic_v1() {
        /* Result of calling following code:
        ```javascript
        const doc = new Y.Doc()
        const ytext = doc.getText('type')
        doc.transact(function () {
            for (let i = 0; i < 3; i++) {
                ytext.insert(0, (i % 10).toString())
            }
        })
        const update = Y.encodeStateAsUpdate(doc)
        ```
         */
        let update = &[
            1, 3, 227, 214, 245, 198, 5, 0, 4, 1, 4, 116, 121, 112, 101, 1, 48, 68, 227, 214, 245,
            198, 5, 0, 1, 49, 68, 227, 214, 245, 198, 5, 1, 1, 50, 0,
        ];
        let mut doc = Doc::new();
        let mut txn = doc.transact_mut();
        txn.apply_update(Update::decode_v1(update).unwrap())
            .unwrap();

        let txt = txn.node("type").unwrap();
        let actual = txt.to_string();
        assert_eq!(actual, "210".to_owned());
    }

    #[test]
    fn apply_update_basic_v2() {
        /* Result of calling following code:
        ```javascript
        const doc = new Y.Doc()
        const ytext = doc.getText('type')
        doc.transact(function () {
            for (let i = 0; i < 3; i++) {
                ytext.insert(0, (i % 10).toString())
            }
        })
        const update = Y.encodeStateAsUpdateV2(doc)
        ```
         */
        let update = &[
            0, 0, 6, 195, 187, 207, 162, 7, 1, 0, 2, 0, 2, 3, 4, 0, 68, 11, 7, 116, 121, 112, 101,
            48, 49, 50, 4, 65, 1, 1, 1, 0, 0, 1, 3, 0, 0,
        ];
        let mut doc = Doc::new();
        let mut txn = doc.transact_mut();
        txn.apply_update(Update::decode_v2(update).unwrap())
            .unwrap();

        let txt = txn.node("type").unwrap();
        let actual = txt.to_string();
        assert_eq!(actual, "210".to_owned());
    }

    #[test]
    fn encode_basic() {
        let mut doc = Doc::with_client_id(1490905955);
        let mut t = doc.transact_mut();
        let mut txt = t.node_mut("type").unwrap();
        txt.insert(0, "0");
        txt.insert(0, "1");
        txt.insert(0, "2");

        let encoded = t.encode_state_as_update_v1(&StateVector::default());
        let expected = &[
            1, 3, 227, 214, 245, 198, 5, 0, 4, 1, 4, 116, 121, 112, 101, 1, 48, 68, 227, 214, 245,
            198, 5, 0, 1, 49, 68, 227, 214, 245, 198, 5, 1, 1, 50, 0,
        ];
        assert_eq!(encoded.as_slice(), expected);
    }

    #[test]
    fn integrate() {
        // create new document at A and add some initial text to it
        let mut d1 = Doc::new();
        let mut t1 = d1.transact_mut();
        let mut txt = t1.node_mut("test").unwrap();
        // Question: why YText.insert uses positions of blocks instead of actual cursor positions
        // in text as seen by user?
        txt.insert(0, "hello");
        txt.insert(5, " ");
        txt.insert(6, "world");

        assert_eq!(txt.get_string(&t1), "hello world".to_string());

        // create document at B
        let mut d2 = Doc::new();
        let mut t2 = d2.transact_mut();
        let sv = t2.state_vector().encode_v1();

        // create an update A->B based on B's state vector
        let mut encoder = EncoderV1::new();
        t1.encode_diff(
            &StateVector::decode_v1(sv.as_slice()).unwrap(),
            &mut encoder,
        );
        let binary = encoder.to_vec();

        // decode an update incoming from A and integrate it at B
        let update = Update::decode_v1(binary.as_slice()).unwrap();
        let pending = update.integrate(&mut t2).unwrap();

        assert!(pending.0.is_none());
        assert!(pending.1.is_none());

        // check if B sees the same thing that A does
        let txt = t2.node("test").unwrap();
        assert_eq!(txt.get_string(&t1), "hello world".to_string());
    }

    #[test]
    fn on_update() {
        let counter = Arc::new(AtomicU32::new(0));
        let mut doc = Doc::new();
        let mut doc2 = Doc::new();
        let c = counter.clone();
        let sub = doc2.observe_update_v1(move |_, e| {
            let u = Update::decode_v1(&e.update).unwrap();
            let blocks = Blocks::new(&u.blocks);
            for block in blocks {
                c.fetch_add(block.len(), Ordering::SeqCst);
            }
        });
        let mut txn = doc.transact_mut();
        {
            let mut txt = txn.node_mut("test").unwrap();
            txt.insert(0, "abc");
            let mut txn2 = doc2.transact_mut();
            let sv = txn2.state_vector().encode_v1();
            let u = txn.encode_diff_v1(&StateVector::decode_v1(sv.as_slice()).unwrap());
            txn2.apply_update(Update::decode_v1(u.as_slice()).unwrap())
                .unwrap();
        }
        assert_eq!(counter.load(Ordering::SeqCst), 3); // update has been propagated

        drop(sub);

        {
            let mut txt = txn.node_mut("test").unwrap();
            txt.insert(3, "de");
            let mut txn2 = doc2.transact_mut();
            let sv = txn2.state_vector().encode_v1();
            let u = txn.encode_diff_v1(&StateVector::decode_v1(sv.as_slice()).unwrap());
            txn2.apply_update(Update::decode_v1(u.as_slice()).unwrap())
                .unwrap();
        }
        assert_eq!(counter.load(Ordering::SeqCst), 3); // since subscription has been dropped, update was not propagated
    }

    #[test]
    #[cfg(feature = "small-client")]
    fn pending_update_integration() {
        let mut doc = Doc::new();

        let updates = [
            vec![
                1, 2, 242, 196, 218, 129, 3, 0, 40, 1, 5, 115, 116, 97, 116, 101, 5, 100, 105, 114,
                116, 121, 1, 121, 40, 1, 7, 99, 111, 110, 116, 101, 120, 116, 4, 112, 97, 116, 104,
                1, 119, 13, 117, 110, 116, 105, 116, 108, 101, 100, 52, 46, 116, 120, 116, 0,
            ],
            vec![
                1, 1, 242, 196, 218, 129, 3, 2, 40, 1, 7, 99, 111, 110, 116, 101, 120, 116, 13,
                108, 97, 115, 116, 95, 109, 111, 100, 105, 102, 105, 101, 100, 1, 119, 27, 50, 48,
                50, 50, 45, 48, 52, 45, 49, 51, 84, 49, 48, 58, 49, 48, 58, 53, 55, 46, 48, 55, 51,
                54, 50, 51, 90, 0,
            ],
            vec![
                1, 2, 242, 196, 218, 129, 3, 3, 4, 1, 6, 115, 111, 117, 114, 99, 101, 1, 97, 168,
                242, 196, 218, 129, 3, 0, 1, 120, 0,
            ],
            vec![
                1, 1, 242, 196, 218, 129, 3, 4, 168, 242, 196, 218, 129, 3, 0, 1, 120, 1, 242, 196,
                218, 129, 3, 1, 0, 1,
            ],
            vec![
                1, 1, 152, 182, 129, 244, 193, 193, 227, 4, 0, 168, 242, 196, 218, 129, 3, 4, 1,
                121, 1, 242, 196, 218, 129, 3, 2, 0, 1, 4, 1,
            ],
            vec![
                1, 2, 242, 196, 218, 129, 3, 5, 132, 242, 196, 218, 129, 3, 3, 1, 98, 168, 152,
                190, 167, 244, 1, 0, 1, 120, 0,
            ],
            vec![
                1, 1, 242, 196, 218, 129, 3, 6, 168, 152, 190, 167, 244, 1, 0, 1, 120, 1, 152, 190,
                167, 244, 1, 1, 0, 1,
            ],
            vec![
                1, 1, 242, 196, 218, 129, 3, 7, 132, 242, 196, 218, 129, 3, 5, 1, 99, 0,
            ],
            vec![
                1, 1, 242, 196, 218, 129, 3, 8, 132, 242, 196, 218, 129, 3, 7, 1, 100, 0,
            ],
        ];

        for u in updates {
            let mut txn = doc.transact_mut();
            let u = Update::decode_v1(u.as_slice()).unwrap();
            println!("integrate pending update: {u:#?}");
            txn.apply_update(u).unwrap();
        }
        let txn = doc.transact();
        let txt = txn.node("source").unwrap();
        assert_eq!(txt.to_string(), "abcd".to_string());
    }

    #[test]
    fn ypy_issue_32() {
        let mut d1 = Doc::with_client_id(1971027812);
        let mut t1 = d1.transact_mut();
        let mut source_1 = t1.node_mut("source").unwrap();
        source_1.push_text("a");

        let updates = [
            vec![
                1, 2, 201, 210, 153, 56, 0, 40, 1, 5, 115, 116, 97, 116, 101, 5, 100, 105, 114,
                116, 121, 1, 121, 40, 1, 7, 99, 111, 110, 116, 101, 120, 116, 4, 112, 97, 116, 104,
                1, 119, 13, 117, 110, 116, 105, 116, 108, 101, 100, 52, 46, 116, 120, 116, 0,
            ],
            vec![
                1, 1, 201, 210, 153, 56, 2, 168, 201, 210, 153, 56, 0, 1, 120, 1, 201, 210, 153,
                56, 1, 0, 1,
            ],
            vec![
                1, 1, 201, 210, 153, 56, 3, 40, 1, 7, 99, 111, 110, 116, 101, 120, 116, 13, 108,
                97, 115, 116, 95, 109, 111, 100, 105, 102, 105, 101, 100, 1, 119, 27, 50, 48, 50,
                50, 45, 48, 52, 45, 49, 54, 84, 49, 52, 58, 48, 51, 58, 53, 51, 46, 57, 51, 48, 52,
                54, 56, 90, 0,
            ],
            vec![
                1, 1, 201, 210, 153, 56, 4, 168, 201, 210, 153, 56, 2, 1, 121, 1, 201, 210, 153,
                56, 1, 2, 1,
            ],
        ];
        for u in updates {
            let u = Update::decode_v1(&u).unwrap();
            d1.transact_mut().apply_update(u).unwrap();
        }

        assert_eq!("a", source_1.get_string(&d1.transact()));

        let mut d2 = Doc::new();
        let state_2 = d2.transact().state_vector().encode_v1();
        let update = d1
            .transact()
            .encode_state_as_update_v1(&StateVector::decode_v1(&state_2).unwrap());
        let update = Update::decode_v1(&update).unwrap();
        d2.transact_mut().apply_update(update).unwrap();

        let source_2 = d2.transact().node("source").unwrap().to_string();
        assert_eq!("a", source_2);

        let update = Update::decode_v1(&[
            1, 2, 201, 210, 153, 56, 5, 132, 228, 254, 237, 171, 7, 0, 1, 98, 168, 201, 210, 153,
            56, 4, 1, 120, 0,
        ])
        .unwrap();
        d1.transact_mut().apply_update(update).unwrap();
        assert_eq!("ab", source_1.get_string(&d1.transact()));

        let mut d3 = Doc::new();
        let state_3 = d3.transact().state_vector().encode_v1();
        let state_3 = StateVector::decode_v1(&state_3).unwrap();
        let update = d1.transact().encode_state_as_update_v1(&state_3);
        let update = Update::decode_v1(&update).unwrap();
        d3.transact_mut().apply_update(update).unwrap();

        let source_3 = d3.transact().node("source").unwrap().to_string();
        assert_eq!("ab", source_3);
    }

    #[test]
    fn observe_transaction_cleanup() {
        // Setup
        let mut doc = Doc::new();
        let text = doc.get_or_insert_text("test");
        let before_state = Arc::new(ArcSwapOption::default());
        let after_state = Arc::new(ArcSwapOption::default());
        let delete_set = Arc::new(ArcSwapOption::default());
        // Create interior mutable references for the callback.
        let before_ref = before_state.clone();
        let after_ref = after_state.clone();
        let delete_ref = delete_set.clone();
        // Subscribe callback

        let sub: Subscription = doc.observe_transaction_cleanup(
            move |_: &Transaction<&Doc>, event: &TransactionCleanupEvent| {
                before_ref.store(Some(event.before_state.clone().into()));
                after_ref.store(Some(event.after_state.clone().into()));
                delete_ref.store(Some(event.delete_set.clone().into()));
            },
        );

        {
            let mut txn = doc.transact_mut();

            // Update the document
            text.insert(&mut txn, 0, "abc");
            text.remove_range(&mut txn, 1, 2);
            txn.commit();

            // Compare values
            assert_eq!(
                before_state.swap(None),
                Some(Arc::new(txn.before_state().clone()))
            );
            assert_eq!(
                after_state.swap(None),
                Some(Arc::new(txn.after_state().clone()))
            );
            assert_eq!(
                delete_set.swap(None),
                Some(Arc::new(txn.delete_set().clone()))
            );
        }

        // Ensure that the subscription is successfully dropped.
        drop(sub);
        let mut txn = doc.transact_mut();
        text.insert(&mut txn, 0, "should not update");
        txn.commit();
        assert_ne!(
            after_state.swap(None),
            Some(Arc::new(txn.after_state().clone()))
        );
    }

    #[test]
    fn partially_duplicated_update() {
        let mut d1 = Doc::with_client_id(1);
        let txt1 = d1.get_or_insert_text("text");
        txt1.insert(&mut d1.transact_mut(), 0, "hello");
        let u = d1
            .transact()
            .encode_state_as_update_v1(&StateVector::default());

        let mut d2 = Doc::with_client_id(2);
        let txt2 = d2.get_or_insert_text("text");
        d2.transact_mut()
            .apply_update(Update::decode_v1(&u).unwrap())
            .unwrap();

        txt1.insert(&mut d1.transact_mut(), 5, "world");
        let u = d1
            .transact()
            .encode_state_as_update_v1(&StateVector::default());
        d2.transact_mut()
            .apply_update(Update::decode_v1(&u).unwrap())
            .unwrap();

        assert_eq!(
            txt1.get_string(&d1.transact()),
            txt2.get_string(&d2.transact())
        );
    }

    #[test]
    fn incremental_observe_update() {
        const INPUT: &'static str = "hello";

        let mut d1 = Doc::with_client_id(1);
        let txt1 = d1.get_or_insert_text("text");
        let acc = Arc::new(Mutex::new(String::new()));

        let a = acc.clone();
        let _sub = d1.observe_update_v1(move |_: &Transaction<&Doc>, e: &UpdateEvent| {
            let u = Update::decode_v1(&e.update).unwrap();
            for block in u.blocks.into_blocks(false) {
                if let Block::Item(item) = block {
                    if let ItemContent::String(s) = &item.content {
                        // each character is appended in individual transaction 1-by-1,
                        // therefore each update should contain a single string with only
                        // one element
                        let mut aref = a.lock().unwrap();
                        aref.push_str(s.as_str());
                    } else {
                        panic!("unexpected content type")
                    }
                }
            }
        });

        for c in INPUT.chars() {
            // append characters 1-by-1 (1 transactions per character)
            txt1.push(&mut d1.transact_mut(), &c.to_string());
        }

        assert_eq!(acc.lock().unwrap().as_str(), INPUT);

        // test incremental deletes
        let acc = Arc::new(Mutex::new(vec![]));
        let a = acc.clone();
        let _sub = d1.observe_update_v1(move |_: &Transaction<&Doc>, e: &UpdateEvent| {
            let u = Update::decode_v1(&e.update).unwrap();
            for (&client_id, range) in u.delete_set.iter() {
                if client_id == ClientID::new(1) {
                    let mut aref = a.lock().unwrap();
                    for r in range.iter() {
                        aref.push(r.clone());
                    }
                }
            }
        });

        for _ in 0..INPUT.len() as u32 {
            txt1.remove_range(&mut d1.transact_mut(), 0, 1);
        }

        let expected = vec![(0..1), (1..2), (2..3), (3..4), (4..5)];
        assert_eq!(&*acc.lock().unwrap(), &expected);
    }

    #[test]
    fn ycrdt_issue_174() {
        let mut doc = Doc::new();
        let bin = &[
            0, 0, 11, 176, 133, 128, 149, 31, 205, 190, 199, 196, 21, 7, 3, 0, 3, 5, 0, 17, 168, 1,
            8, 0, 40, 0, 8, 0, 40, 0, 8, 0, 40, 0, 33, 1, 39, 110, 91, 49, 49, 49, 114, 111, 111,
            116, 105, 51, 50, 114, 111, 111, 116, 115, 116, 114, 105, 110, 103, 114, 111, 111, 116,
            97, 95, 108, 105, 115, 116, 114, 111, 111, 116, 97, 95, 109, 97, 112, 114, 111, 111,
            116, 105, 51, 50, 95, 108, 105, 115, 116, 114, 111, 111, 116, 105, 51, 50, 95, 109, 97,
            112, 114, 111, 111, 116, 115, 116, 114, 105, 110, 103, 95, 108, 105, 115, 116, 114,
            111, 111, 116, 115, 116, 114, 105, 110, 103, 95, 109, 97, 112, 65, 1, 4, 3, 4, 6, 4, 6,
            4, 5, 4, 8, 4, 7, 4, 11, 4, 10, 3, 0, 5, 1, 6, 0, 1, 0, 1, 0, 1, 2, 65, 8, 2, 8, 0,
            125, 2, 119, 5, 119, 111, 114, 108, 100, 118, 2, 1, 98, 119, 1, 97, 1, 97, 125, 1, 118,
            2, 1, 98, 119, 1, 98, 1, 97, 125, 2, 125, 1, 125, 2, 119, 1, 97, 119, 1, 98, 8, 0, 1,
            141, 223, 163, 226, 10, 1, 0, 1,
        ];
        let update = Update::decode_v2(bin).unwrap();
        doc.transact_mut().apply_update(update).unwrap();

        let root = doc.get_or_insert_map("root");
        let actual = root.to_json(&doc.transact());
        let expected = Any::from_json(
            r#"{
              "string": "world",
              "a_list": [{"b": "a", "a": 1}],
              "i32_map": {"1": 2},
              "a_map": {
                "1": {"a": 2, "b": "b"}
              },
              "string_list": ["a"],
              "i32": 2,
              "string_map": {"1": "b"},
              "i32_list": [1]
            }"#,
        )
        .unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn snapshots_splitting_text() {
        let mut options = Options::with_client_id(ClientID::new(1));
        options.skip_gc = true;

        let mut d1 = Doc::with_options(options);
        let txt1 = d1.get_or_insert_text("text");
        txt1.insert(&mut d1.transact_mut(), 0, "hello");
        let snapshot = d1.transact_mut().snapshot();
        txt1.insert(&mut d1.transact_mut(), 5, "_world");

        let mut encoder = EncoderV1::new();
        d1.transact_mut()
            .encode_state_from_snapshot(&snapshot, &mut encoder)
            .unwrap();
        let update = Update::decode_v1(&encoder.to_vec()).unwrap();

        let mut d2 = Doc::with_client_id(2);
        let txt2 = d2.get_or_insert_text("text");
        d2.transact_mut().apply_update(update).unwrap();

        assert_eq!(txt2.get_string(&d2.transact()), "hello".to_string());
    }

    #[test]
    fn snapshot_non_splitting_text() {
        let mut options = Options::default();
        options.skip_gc = true;

        let mut doc = Doc::with_options(options.clone().into());
        let txt = doc.get_or_insert_text("name");

        let mut txn = doc.transact_mut();
        txt.insert(&mut txn, 0, "Lucas");
        drop(txn);

        let txn = doc.transact();
        let snapshot = txn.snapshot();

        let mut encoder = EncoderV1::new();
        txn.encode_state_from_snapshot(&snapshot, &mut encoder)
            .unwrap();
        let state_diff = encoder.to_vec();

        let mut remote_doc = Doc::with_options(options);
        let remote_txt = remote_doc.get_or_insert_text("name");
        let mut txn = remote_doc.transact_mut();
        let update = Update::decode_v1(&state_diff).unwrap();
        txn.apply_update(update).unwrap();

        let actual = remote_txt.get_string(&txn);

        assert_eq!(actual, "Lucas");
    }

    #[test]
    fn yrb_issue_45() {
        let diffs: Vec<Vec<u8>> = vec![
            vec![
                1, 3, 197, 134, 244, 186, 10, 0, 7, 1, 7, 100, 101, 102, 97, 117, 108, 116, 3, 9,
                112, 97, 114, 97, 103, 114, 97, 112, 104, 7, 0, 197, 134, 244, 186, 10, 0, 6, 4, 0,
                197, 134, 244, 186, 10, 1, 1, 115, 0,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 3, 132, 197, 134, 244, 186, 10, 2, 3, 227, 129, 149,
                1, 197, 134, 244, 186, 10, 1, 2, 1,
            ],
            vec![
                1, 4, 197, 134, 244, 186, 10, 0, 7, 1, 7, 100, 101, 102, 97, 117, 108, 116, 3, 9,
                112, 97, 114, 97, 103, 114, 97, 112, 104, 7, 0, 197, 134, 244, 186, 10, 0, 6, 1, 0,
                197, 134, 244, 186, 10, 1, 1, 132, 197, 134, 244, 186, 10, 2, 3, 227, 129, 149, 1,
                197, 134, 244, 186, 10, 1, 2, 1,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 4, 132, 197, 134, 244, 186, 10, 3, 1, 120, 0,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 5, 132, 197, 134, 244, 186, 10, 4, 3, 227, 129, 129,
                1, 197, 134, 244, 186, 10, 1, 4, 1,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 6, 132, 197, 134, 244, 186, 10, 5, 1, 107, 0,
            ],
            vec![
                1, 2, 197, 134, 244, 186, 10, 4, 129, 197, 134, 244, 186, 10, 3, 1, 132, 197, 134,
                244, 186, 10, 4, 3, 227, 129, 129, 1, 197, 134, 244, 186, 10, 1, 4, 1,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 7, 132, 197, 134, 244, 186, 10, 6, 3, 227, 129, 147,
                1, 197, 134, 244, 186, 10, 1, 6, 1,
            ],
            vec![
                1, 2, 197, 134, 244, 186, 10, 6, 129, 197, 134, 244, 186, 10, 5, 1, 132, 197, 134,
                244, 186, 10, 6, 3, 227, 129, 147, 1, 197, 134, 244, 186, 10, 1, 6, 1,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 8, 132, 197, 134, 244, 186, 10, 7, 1, 114, 0,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 9, 132, 197, 134, 244, 186, 10, 8, 3, 227, 130, 140,
                1, 197, 134, 244, 186, 10, 1, 8, 1,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 8, 132, 197, 134, 244, 186, 10, 7, 1, 114, 0,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 10, 132, 197, 134, 244, 186, 10, 9, 1, 107, 0,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 11, 132, 197, 134, 244, 186, 10, 10, 3, 227, 129,
                139, 1, 197, 134, 244, 186, 10, 1, 10, 1,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 12, 132, 197, 134, 244, 186, 10, 11, 1, 114, 0,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 13, 132, 197, 134, 244, 186, 10, 12, 3, 227, 130,
                137, 1, 197, 134, 244, 186, 10, 1, 12, 1,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 9, 132, 197, 134, 244, 186, 10, 8, 3, 227, 130, 140,
                1, 197, 134, 244, 186, 10, 1, 8, 1,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 10, 132, 197, 134, 244, 186, 10, 9, 1, 107, 0,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 11, 132, 197, 134, 244, 186, 10, 10, 3, 227, 129,
                139, 1, 197, 134, 244, 186, 10, 1, 10, 1,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 12, 132, 197, 134, 244, 186, 10, 11, 1, 114, 0,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 14, 132, 197, 134, 244, 186, 10, 13, 1, 98, 0,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 16, 132, 197, 134, 244, 186, 10, 15, 1, 103, 0,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 15, 132, 197, 134, 244, 186, 10, 14, 3, 227, 129,
                176, 1, 197, 134, 244, 186, 10, 1, 14, 1,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 17, 132, 197, 134, 244, 186, 10, 16, 3, 227, 129,
                144, 1, 197, 134, 244, 186, 10, 1, 16, 1,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 17, 132, 197, 134, 244, 186, 10, 16, 3, 227, 129,
                144, 1, 197, 134, 244, 186, 10, 1, 16, 1,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 18, 132, 197, 134, 244, 186, 10, 17, 6, 227, 131,
                144, 227, 130, 176, 1, 197, 134, 244, 186, 10, 2, 15, 1, 17, 1,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 20, 132, 197, 134, 244, 186, 10, 19, 1, 103, 0,
            ],
            vec![
                1, 3, 197, 134, 244, 186, 10, 13, 132, 197, 134, 244, 186, 10, 12, 3, 227, 130,
                137, 129, 197, 134, 244, 186, 10, 13, 1, 132, 197, 134, 244, 186, 10, 14, 4, 227,
                129, 176, 103, 1, 197, 134, 244, 186, 10, 2, 12, 1, 14, 1,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 21, 132, 197, 134, 244, 186, 10, 20, 3, 227, 129,
                140, 1, 197, 134, 244, 186, 10, 1, 20, 1,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 23, 132, 197, 134, 244, 186, 10, 22, 3, 227, 129,
                170, 1, 197, 134, 244, 186, 10, 1, 22, 1,
            ],
            vec![
                1, 3, 197, 134, 244, 186, 10, 18, 132, 197, 134, 244, 186, 10, 17, 6, 227, 131,
                144, 227, 130, 176, 129, 197, 134, 244, 186, 10, 19, 1, 132, 197, 134, 244, 186,
                10, 20, 3, 227, 129, 140, 1, 197, 134, 244, 186, 10, 3, 15, 1, 17, 1, 20, 1,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 24, 132, 197, 134, 244, 186, 10, 23, 3, 227, 129,
                132, 0,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 22, 132, 197, 134, 244, 186, 10, 21, 1, 110, 0,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 26, 132, 197, 134, 244, 186, 10, 25, 3, 227, 129,
                139, 1, 197, 134, 244, 186, 10, 1, 25, 1,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 25, 132, 197, 134, 244, 186, 10, 24, 1, 107, 0,
            ],
            vec![
                1, 4, 197, 134, 244, 186, 10, 22, 129, 197, 134, 244, 186, 10, 21, 1, 132, 197,
                134, 244, 186, 10, 22, 6, 227, 129, 170, 227, 129, 132, 129, 197, 134, 244, 186,
                10, 24, 1, 132, 197, 134, 244, 186, 10, 25, 3, 227, 129, 139, 1, 197, 134, 244,
                186, 10, 2, 22, 1, 25, 1,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 27, 132, 197, 134, 244, 186, 10, 26, 1, 100, 0,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 28, 132, 197, 134, 244, 186, 10, 27, 3, 227, 129,
                169, 1, 197, 134, 244, 186, 10, 1, 27, 1,
            ],
            vec![
                1, 2, 197, 134, 244, 186, 10, 27, 129, 197, 134, 244, 186, 10, 26, 1, 132, 197,
                134, 244, 186, 10, 27, 3, 227, 129, 169, 1, 197, 134, 244, 186, 10, 1, 27, 1,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 29, 132, 197, 134, 244, 186, 10, 28, 3, 227, 129,
                134, 0,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 30, 132, 197, 134, 244, 186, 10, 29, 1, 107, 0,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 29, 132, 197, 134, 244, 186, 10, 28, 3, 227, 129,
                134, 0,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 31, 132, 197, 134, 244, 186, 10, 30, 3, 227, 129,
                139, 1, 197, 134, 244, 186, 10, 1, 30, 1,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 30, 132, 197, 134, 244, 186, 10, 29, 1, 107, 0,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 31, 132, 197, 134, 244, 186, 10, 30, 3, 227, 129,
                139, 1, 197, 134, 244, 186, 10, 1, 30, 1,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 32, 135, 197, 134, 244, 186, 10, 0, 3, 9, 112, 97,
                114, 97, 103, 114, 97, 112, 104, 0,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 32, 135, 197, 134, 244, 186, 10, 0, 3, 9, 112, 97,
                114, 97, 103, 114, 97, 112, 104, 0,
            ],
            vec![
                1, 2, 197, 134, 244, 186, 10, 33, 7, 0, 197, 134, 244, 186, 10, 32, 6, 4, 0, 197,
                134, 244, 186, 10, 33, 1, 107, 0,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 35, 132, 197, 134, 244, 186, 10, 34, 3, 227, 129,
                139, 1, 197, 134, 244, 186, 10, 1, 34, 1,
            ],
            vec![
                1, 1, 197, 134, 244, 186, 10, 36, 132, 197, 134, 244, 186, 10, 35, 1, 107, 0,
            ],
        ];

        let mut doc = Doc::new();
        let mut txn = doc.transact_mut();
        for diff in diffs {
            let u = Update::decode_v1(diff.as_slice()).unwrap();
            txn.apply_update(u).unwrap();
        }
    }

    #[test]
    fn root_refs() {
        let mut doc = Doc::new();
        {
            let _txt = doc.get_or_insert_text("text");
            let _array = doc.get_or_insert_array("array");
            let _map = doc.get_or_insert_map("map");
            let _xml_elem = doc.get_or_insert_xml_fragment("xml_elem");
        }

        let txn = doc.transact();
        for (key, value) in txn.root_refs() {
            match key {
                "text" => assert!(value.cast::<TextRef>().is_ok()),
                "array" => assert!(value.cast::<ArrayRef>().is_ok()),
                "map" => assert!(value.cast::<MapRef>().is_ok()),
                "xml_elem" => assert!(value.cast::<XmlFragmentRef>().is_ok()),
                "xml_text" => assert!(value.cast::<XmlTextRef>().is_ok()),
                other => panic!("unrecognized root type: '{}'", other),
            }
        }
    }

    #[test]
    fn integrate_block_with_parent_gc() {
        let mut d1 = Doc::with_client_id(1);
        let mut d2 = Doc::with_client_id(2);
        let mut d3 = Doc::with_client_id(3);

        {
            let root = d1.get_or_insert_array("array");
            let mut txn = d1.transact_mut();
            root.push_back(&mut txn, ArrayPrelim::from(["A"]));
        }

        exchange_updates(&mut [&mut d1, &mut d2, &mut d3]);

        {
            let root = d2.get_or_insert_array("array");
            let mut t2 = d2.transact_mut();
            root.remove(&mut t2, 0);
            d1.transact_mut()
                .apply_update(Update::decode_v1(&t2.encode_update_v1()).unwrap())
                .unwrap();
        }

        {
            let root = d3.get_or_insert_array("array");
            let mut t3 = d3.transact_mut();
            let a3 = root.get(&t3, 0).unwrap().cast::<ArrayRef>().unwrap();
            a3.push_back(&mut t3, "B");
            // D1 got update which already removed a3, but this must not cause panic
            d1.transact_mut()
                .apply_update(Update::decode_v1(&t3.encode_update_v1()).unwrap())
                .unwrap();
        }

        exchange_updates(&mut [&mut d1, &mut d2, &mut d3]);

        let r1 = d1.get_or_insert_array("array").to_json(&d1.transact());
        let r2 = d2.get_or_insert_array("array").to_json(&d2.transact());
        let r3 = d3.get_or_insert_array("array").to_json(&d3.transact());

        assert_eq!(r1, r2);
        assert_eq!(r2, r3);
        assert_eq!(r3, r1);
    }

    #[test]
    fn subdoc() {
        let mut doc = Doc::with_client_id(1);
        let event = Arc::new(ArcSwapOption::default());
        let event_c = event.clone();
        let _sub = doc.observe_subdocs(move |_, e| {
            let added = e.added().cloned().collect();
            let removed = e.removed().cloned().collect();
            let loaded = e.loaded().cloned().collect();
            event_c.store(Some(Arc::new((added, removed, loaded))));
        });
        let subdocs = doc.get_or_insert_map("mysubdocs");
        let uuid_a: Uuid = "A".into();
        let doc_a = Doc::with_options({
            let mut o = Options::default();
            o.guid = uuid_a.clone();
            o
        });
        {
            let mut txn = doc.transact_mut();
            subdocs.insert(&mut txn, "a", doc_a);
            load_subdoc(&mut txn, &uuid_a);
        }

        let actual = event.swap(None);
        assert_eq!(
            actual,
            Some((vec![uuid_a.clone()], vec![], vec![uuid_a.clone()]).into())
        );

        {
            let mut txn = doc.transact_mut();
            load_subdoc(&mut txn, &uuid_a);
        }
        let actual = event.swap(None);
        assert_eq!(actual, None);

        {
            let mut txn = doc.transact_mut();
            destroy_subdoc(&mut txn, &uuid_a);
        }
        let actual = event.swap(None);
        assert_eq!(
            actual,
            Some(Arc::new((
                vec![uuid_a.clone()],
                vec![uuid_a.clone()],
                vec![]
            )))
        );

        {
            let mut txn = doc.transact_mut();
            load_subdoc(&mut txn, &uuid_a);
        }
        let actual = event.swap(None);
        assert_eq!(
            actual,
            Some(Arc::new((vec![], vec![], vec![uuid_a.clone()])))
        );

        let doc_b = Doc::with_options({
            let mut o = Options::default();
            o.guid = uuid_a.clone();
            o.should_load = false;
            o
        });
        subdocs.insert(&mut doc.transact_mut(), "b", doc_b);
        let actual = event.swap(None);
        assert_eq!(
            actual,
            Some(Arc::new((vec![uuid_a.clone()], vec![], vec![])))
        );

        {
            let mut txn = doc.transact_mut();
            load_subdoc(&mut txn, &uuid_a);
        }
        let actual = event.swap(None);
        assert_eq!(
            actual,
            Some(Arc::new((vec![], vec![], vec![uuid_a.clone()])))
        );

        let uuid_c: Uuid = "C".into();
        let doc_c = Doc::with_options({
            let mut o = Options::default();
            o.guid = uuid_c.clone();
            o
        });
        {
            let mut txn = doc.transact_mut();
            subdocs.insert(&mut txn, "c", doc_c);
            load_subdoc(&mut txn, &uuid_c);
        }
        let actual = event.swap(None);
        assert_eq!(
            actual,
            Some(Arc::new((
                vec![uuid_c.clone()],
                vec![],
                vec![uuid_c.clone()]
            )))
        );

        let guids: BTreeSet<_> = doc.transact().subdoc_guids().cloned().collect();
        assert_eq!(guids, BTreeSet::from([uuid_a.clone(), uuid_c.clone()]));

        let data = doc
            .transact()
            .encode_state_as_update_v1(&StateVector::default());

        let mut doc2 = Doc::new();
        let event = Arc::new(ArcSwapOption::default());
        let event_c = event.clone();
        let _sub = doc2.observe_subdocs(move |_, e| {
            let added: Vec<_> = e.added().cloned().collect();
            let removed: Vec<_> = e.removed().cloned().collect();
            let loaded: Vec<_> = e.loaded().cloned().collect();
            event_c.store(Some(Arc::new((added, removed, loaded))));
        });
        let update = Update::decode_v1(&data).unwrap();
        doc2.transact_mut().apply_update(update).unwrap();
        let mut actual = event.swap(None).unwrap();
        Arc::get_mut(&mut actual).unwrap().0.sort();
        // In the new architecture, subdocs index is HashMap<Uuid, ItemPtr> and
        // added is HashSet<Uuid>, so duplicate guids collapse to a single entry.
        assert_eq!(
            actual,
            Arc::new((vec![uuid_a.clone(), uuid_c.clone()], vec![], vec![]))
        );

        {
            let mut txn = doc2.transact_mut();
            load_subdoc(&mut txn, &uuid_a);
        }
        let actual = event.swap(None);
        assert_eq!(
            actual,
            Some(Arc::new((vec![], vec![], vec![uuid_a.clone()])))
        );

        let guids: BTreeSet<_> = doc2.transact().subdoc_guids().cloned().collect();
        assert_eq!(guids, BTreeSet::from([uuid_a.clone(), uuid_c.clone()]));
        {
            let subdocs_map = doc2.transact().get_map("mysubdocs").unwrap();
            let mut txn = doc2.transact_mut();
            subdocs_map.remove(&mut txn, "a");
        }

        let actual = event.swap(None);
        assert_eq!(
            actual,
            Some(Arc::new((vec![], vec![uuid_a.clone()], vec![])))
        );

        let mut guids: Vec<_> = doc2.transact().subdoc_guids().cloned().collect();
        guids.sort();
        // In the new architecture, subdocs index is HashMap<Uuid, ItemPtr>.
        // Removing map entry "a" removes guid "A" from the index entirely,
        // even though "b" also has guid "A", because the index only stores
        // one ItemPtr per guid.
        assert_eq!(guids, vec![uuid_c.clone()]);
    }

    #[test]
    fn subdoc_load_edge_cases() {
        let mut doc = Doc::with_client_id(1);
        let array = doc.get_or_insert_array("test");
        let subdoc_1 = Doc::new();
        let uuid_1 = subdoc_1.guid().clone();

        let event = Arc::new(ArcSwapOption::default());
        let event_c = event.clone();
        let _sub = doc.observe_subdocs(move |_, e| {
            let added = e.added().cloned().collect();
            let removed = e.removed().cloned().collect();
            let loaded = e.loaded().cloned().collect();

            event_c.store(Some(Arc::new((added, removed, loaded))));
        });
        {
            let mut txn = doc.transact_mut();
            array.insert(&mut txn, 0, subdoc_1);
            let subdoc_ref = txn.doc.subdoc(&uuid_1).unwrap();
            assert!(subdoc_ref.should_load());
            assert!(!subdoc_ref.auto_load());
        }
        let last_event = event.swap(None);
        assert_eq!(
            last_event,
            Some((vec![uuid_1.clone()], vec![], vec![uuid_1.clone()]).into())
        );

        // destroy and check whether lastEvent adds it again to added (it shouldn't)
        {
            let mut txn = doc.transact_mut();
            destroy_subdoc(&mut txn, &uuid_1);
        }
        // After destroy, a new subdoc is created for the same item.
        // Get the uuid of the replacement subdoc.
        let uuid_2 = {
            let txn = doc.transact();
            let out = array.get(&txn, 0).unwrap();
            match out {
                crate::Out::Doc(uuid) => uuid,
                _ => panic!("expected YDoc"),
            }
        };

        let last_event = event.swap(None);
        assert_eq!(
            last_event,
            Some((vec![uuid_2.clone()], vec![uuid_2.clone()], vec![]).into())
        );

        // load
        {
            let mut txn = doc.transact_mut();
            load_subdoc(&mut txn, &uuid_2);
        }
        let last_event = event.swap(None);
        assert_eq!(
            last_event,
            Some(Arc::new((vec![], vec![], vec![uuid_2.clone()])))
        );

        // apply from remote
        let mut doc2 = Doc::with_client_id(2);
        let event_c = event.clone();
        let _sub = doc2.observe_subdocs(move |_, e| {
            let added = e.added().cloned().collect();
            let removed = e.removed().cloned().collect();
            let loaded = e.loaded().cloned().collect();

            event_c.store(Some(Arc::new((added, removed, loaded))));
        });
        let u = Update::decode_v1(
            &doc.transact()
                .encode_state_as_update_v1(&StateVector::default()),
        );
        doc2.transact_mut().apply_update(u.unwrap()).unwrap();
        let uuid_3 = {
            let array = doc2.get_or_insert_array("test");
            let txn = doc2.transact();
            match array.get(&txn, 0).unwrap() {
                crate::Out::Doc(uuid) => uuid,
                _ => panic!("expected YDoc"),
            }
        };
        {
            let subdoc_ref = doc2.subdoc(&uuid_3).unwrap();
            assert!(!subdoc_ref.should_load());
            assert!(!subdoc_ref.auto_load());
        }
        let last_event = event.swap(None);
        assert_eq!(
            last_event,
            Some(Arc::new((vec![uuid_3.clone()], vec![], vec![])))
        );

        // load
        {
            let mut txn = doc2.transact_mut();
            load_subdoc(&mut txn, &uuid_3);
            assert!(txn.doc.subdoc(&uuid_3).unwrap().should_load());
        }
        let last_event = event.swap(None);
        assert_eq!(
            last_event,
            Some(Arc::new((vec![], vec![], vec![uuid_3.clone()])))
        );
    }

    #[test]
    fn subdoc_auto_load_edge_cases() {
        let mut doc = Doc::with_client_id(1);
        let array = doc.get_or_insert_array("test");
        let subdoc_1 = Doc::with_options({
            let mut o = Options::default();
            o.auto_load = true;
            o
        });
        let uuid_1 = subdoc_1.guid().clone();

        let event = Arc::new(ArcSwapOption::default());
        let event_c = event.clone();
        let _sub = doc.observe_subdocs(move |_, e| {
            let added = e.added().cloned().collect();
            let removed = e.removed().cloned().collect();
            let loaded = e.loaded().cloned().collect();

            event_c.store(Some(Arc::new((added, removed, loaded))));
        });

        {
            let mut txn = doc.transact_mut();
            array.insert(&mut txn, 0, subdoc_1);
        }
        {
            let subdoc_ref = doc.subdoc(&uuid_1).unwrap();
            assert!(subdoc_ref.should_load());
            assert!(subdoc_ref.auto_load());
        }

        let last_event = event.swap(None);
        assert_eq!(
            last_event,
            Some(Arc::new((
                vec![uuid_1.clone()],
                vec![],
                vec![uuid_1.clone()]
            )))
        );

        // destroy and check whether lastEvent adds it again to added (it shouldn't)
        {
            let mut txn = doc.transact_mut();
            destroy_subdoc(&mut txn, &uuid_1);
        }

        let uuid_2 = {
            let txn = doc.transact();
            match array.get(&txn, 0).unwrap() {
                crate::Out::Doc(uuid) => uuid,
                _ => panic!("expected YDoc"),
            }
        };

        let last_event = event.swap(None);
        assert_eq!(
            last_event,
            Some(Arc::new((
                vec![uuid_2.clone()],
                vec![uuid_2.clone()],
                vec![]
            )))
        );

        {
            let mut txn = doc.transact_mut();
            load_subdoc(&mut txn, &uuid_2);
        }
        let last_event = event.swap(None);
        assert_eq!(
            last_event,
            Some(Arc::new((vec![], vec![], vec![uuid_2.clone()])))
        );

        // apply from remote
        let mut doc2 = Doc::with_client_id(2);
        let event_c = event.clone();
        let _sub = doc2.observe_subdocs(move |_, e| {
            let added = e.added().cloned().collect();
            let removed = e.removed().cloned().collect();
            let loaded = e.loaded().cloned().collect();

            event_c.store(Some(Arc::new((added, removed, loaded))));
        });
        let u = Update::decode_v1(
            &doc.transact()
                .encode_state_as_update_v1(&StateVector::default()),
        );
        doc2.transact_mut().apply_update(u.unwrap()).unwrap();
        let uuid_3 = {
            let array = doc2.get_or_insert_array("test");
            let txn = doc2.transact();
            match array.get(&txn, 0).unwrap() {
                crate::Out::Doc(uuid) => uuid,
                _ => panic!("expected YDoc"),
            }
        };
        {
            let subdoc_ref = doc2.subdoc(&uuid_3).unwrap();
            assert!(subdoc_ref.should_load());
            assert!(subdoc_ref.auto_load());
        }
        let last_event = event.swap(None);
        assert_eq!(
            last_event,
            Some(Arc::new((
                vec![uuid_3.clone()],
                vec![],
                vec![uuid_3.clone()]
            )))
        );
    }

    #[test]
    fn to_json() {
        let mut doc = Doc::new();
        let mut txn = doc.transact_mut();
        let text = txn.get_or_insert_text("text");
        let array = txn.get_or_insert_array("array");
        let map = txn.get_or_insert_map("map");
        let xml_fragment = txn.get_or_insert_xml_fragment("xml-fragment");
        let xml_element = xml_fragment.insert(&mut txn, 0, XmlElementPrelim::empty("xml-element"));
        let xml_text = xml_fragment.insert(&mut txn, 0, XmlTextPrelim::new(""));

        text.push(&mut txn, "hello");
        xml_text.push(&mut txn, "world");
        xml_fragment.insert(&mut txn, 0, XmlElementPrelim::empty("div"));
        xml_element.insert(&mut txn, 0, XmlElementPrelim::empty("body"));
        array.insert_range(&mut txn, 0, [1, 2, 3]);
        map.insert(&mut txn, "key1", "value1");

        // sub documents cannot use their parent's transaction
        let mut sub_doc = Doc::new();
        let sub_text = sub_doc.get_or_insert_text("sub-text");
        let sub_guid = sub_doc.guid().clone();
        let _sub_uuid = map.insert(&mut txn, "sub-doc", sub_doc);
        {
            let sub_doc = txn.doc.subdocs.get_mut(&sub_guid).unwrap();
            let mut sub_txn = sub_doc.transact_mut();
            sub_text.push(&mut sub_txn, "sample");
        }

        drop(txn);

        let txn = doc.transact();
        let actual = doc.to_json(&txn);
        let expected = any!({
            "text": "hello",
            "array": [1,2,3],
            "map": {
                "key1": "value1",
                "sub-doc": {
                    "guid": sub_guid.as_ref()
                }
            },
            "xml-fragment": "<div></div>world<xml-element><body></body></xml-element>",
        });
        assert_eq!(actual, expected);
    }

    #[test]
    fn apply_snapshot_updates() {
        let update = {
            let mut doc = Doc::with_options(Options {
                client_id: ClientID::new(1),
                skip_gc: true,
                offset_kind: OffsetKind::Utf16,
                ..Options::default()
            });
            let txt = doc.get_or_insert_text("test");
            let mut txn = doc.transact_mut();
            txt.insert(&mut txn, 0, "hello");

            let snap = txn.snapshot();

            txt.insert(&mut txn, 5, " world");

            let mut encoder = EncoderV1::new();
            txn.encode_state_from_snapshot(&snap, &mut encoder).unwrap();
            encoder.to_vec()
        };

        let mut doc = Doc::with_client_id(1);
        let txt = doc.get_or_insert_text("test");
        let mut txn = doc.transact_mut();
        txn.apply_update(Update::decode_v1(&update).unwrap())
            .unwrap();
        let str = txt.get_string(&txn);
        assert_eq!(&str, "hello");
    }

    #[test]
    fn out_of_order_updates() {
        let updates = Arc::new(Mutex::new(vec![]));

        let mut d1 = Doc::new();
        let _sub = {
            let updates = updates.clone();
            d1.observe_update_v1(move |_, e| {
                let mut u = updates.lock().unwrap();
                u.push(Update::decode_v1(&e.update).unwrap());
            })
        };

        let map = d1.get_or_insert_map("map");
        map.insert(&mut d1.transact_mut(), "a", 1); // U1: 'a' => 1
        map.insert(&mut d1.transact_mut(), "a", 1.1); // U2: 'a' => 1.1
        map.insert(&mut d1.transact_mut(), "b", 2); // U3: 'b' => 2

        assert_eq!(map.to_json(&d1.transact()), any!({"a": 1.1, "b": 2}));

        let mut d2 = Doc::new();
        let map = d2.get_or_insert_map("map");

        {
            let mut updates = updates.lock().unwrap();
            let u3 = updates.pop().unwrap(); // 'b' => 2
            let u2 = updates.pop().unwrap(); // 'a' => 1.1
            let u1 = updates.pop().unwrap(); // 'a' => 1
            let mut txn = d2.transact_mut();

            txn.apply_update(u1).unwrap(); // apply: 'a' => 1
            assert_eq!(map.to_json(&txn), any!({"a": 1}));

            txn.apply_update(u3).unwrap(); // apply: 'b' => 2 (it's ok, we insert a skip for u2)
            assert_eq!(map.to_json(&txn), any!({"a": 1, "b": 2}));

            txn.apply_update(u2).unwrap(); // apply: 'a' => 1.1
            assert_eq!(map.to_json(&txn), any!({"a": 1.1, "b": 2}));
        }
    }

    #[test]
    fn encoding_buffer_overflow_errors() {
        assert_matches!(
            Update::decode_v1(&vec![
                0xe4, 0x9c, 0x10, 0x00, 0x05, 0xff, 0xff, 0x05, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
                0x01, 0x00, 0x00, 0x00, 0xed, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00, 0xfe, 0xb8, 0xc2,
                0xe9, 0xad, 0x87, 0xd9, 0x12, 0x00, 0x00, 0x01, 0x01, 0xff, 0xed, 0xf6,
            ]),
            Err(crate::encoding::read::Error::EndOfBuffer(_))
        );

        assert_matches!(
            Update::decode_v2(&vec![
                0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x01, 0x02, 0x00, 0x00,
                0x16, 0x02, 0x00, 0x00, 0x01, 0xfd, 0xff, 0xff, 0xff, 0xff, 0x7f, 0x00, 0x00,
            ]),
            Err(crate::encoding::read::Error::EndOfBuffer(_))
        );
        assert_matches!(
            Update::decode_v2(&vec![
                0xe4, 0x95, 0x00, 0x00, 0x01, 0x18, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x01,
                0x00, 0x00, 0xed, 0x01, 0xbe, 0x82, 0xe3, 0xc3, 0x1c, 0x01, 0x02, 0xe4, 0x95, 0x00,
                0x00, 0x01, 0x18, 0x00, 0x00, 0x01, 0x18, 0x00, 0x00, 0x01, 0x00, 0x01, 0xed, 0x00,
            ]),
            Err(crate::encoding::read::Error::InvalidVarInt)
        );
        assert_matches!(
            Update::decode_v2(&vec![
                0x8f, 0x01, 0x80, 0x00, 0x00, 0x00, 0x01, 0xaa, 0x01, 0x00, 0x01, 0x02, 0x00, 0x00,
                0x16, 0x02, 0x00, 0xe5, 0xc4, 0x43, 0x14, 0xe7, 0xa6, 0x8b, 0x93, 0xae, 0xb5, 0xfd,
                0x5d, 0xe8, 0x26, 0x9a, 0x8a, 0x59, 0x00, 0x31, 0xd5, 0x0f, 0x12, 0x01, 0x30, 0x00,
                0x00, 0x00,
            ]),
            Err(crate::encoding::read::Error::EndOfBuffer(_))
        );
        assert_matches!(
            Update::decode_v2(&vec![
                0x00, 0x01, 0x23, 0x00, 0x00, 0x00, 0x01, 0x02, 0x81, 0x00, 0x00, 0x10, 0x00, 0xc7,
                0xdc, 0x00, 0xc4, 0x7a, 0x80, 0x00, 0x41, 0xab, 0xea, 0xd6, 0x00, 0x01, 0x00, 0x00,
                0x01, 0x00, 0x00, 0x84, 0x00, 0x00, 0x10, 0xff, 0xc7, 0xdc, 0xff, 0x00, 0x00, 0x00,
            ]),
            Err(crate::encoding::read::Error::EndOfBuffer(_))
        );
    }

    #[test]
    fn observe_after_transaction() {
        let mut d1 = Doc::with_client_id(1);
        let txt1 = d1.get_or_insert_text("text");

        let e = Arc::new(ArcSwapOption::default());
        let e_copy = e.clone();
        d1.observe_after_transaction_with("key", move |txn| {
            e_copy.swap(Some(Arc::new((
                txn.before_state().clone(),
                txn.after_state().clone(),
                txn.delete_set().clone(),
            ))));
        });

        txt1.insert(&mut d1.transact_mut(), 0, "hello world");
        let actual = e.swap(None);
        assert_eq!(
            actual,
            Some(Arc::new((
                StateVector::from_iter([(ClientID::new(1), 0)]),
                StateVector::from_iter([(ClientID::new(1), 11)]),
                IdSet::default()
            )))
        );

        txt1.remove_range(&mut d1.transact_mut(), 2, 7);
        let actual = e.swap(None);
        assert_eq!(
            actual,
            Some(Arc::new((
                StateVector::from_iter([(ClientID::new(1), 11)]),
                StateVector::from_iter([(ClientID::new(1), 11)]),
                {
                    let mut ds = IdSet::new();
                    ds.insert(ID::new(ClientID::new(1), 2), 7);
                    ds
                }
            )))
        );

        d1.unobserve_after_transaction("key");

        txt1.insert(&mut d1.transact_mut(), 4, " the door");
        let actual = e.swap(None);
        assert!(actual.is_none());
    }

    fn init_test_data<const N: usize>(txn: &mut TransactionMut, data: [&str; N]) -> TextRef {
        let map = txn.get_or_insert_map("map");
        let txt = map.insert(txn, "text", TextPrelim::default());
        for ch in data {
            txt.insert(txn, 0, ch);
        }
        txt
    }

    #[test]
    fn force_gc() {
        let mut doc = Doc::with_options(Options {
            client_id: ClientID::new(1),
            skip_gc: true,
            ..Default::default()
        });
        let map = doc.get_or_insert_map("map");

        {
            // create some initial data
            let mut txn = doc.transact_mut();
            init_test_data(&mut txn, ["c", "b", "a"]);

            // drop nested type
            map.remove(&mut txn, "text");
        }

        // verify that skip_gc works and we have access to an original text content
        {
            let txn = doc.transact();
            let mut i = 1;
            for c in ["c", "b", "a"] {
                let block = txn
                    .doc()
                    .blocks
                    .get_block(&ID::new(ClientID::new(1), i))
                    .unwrap()
                    .as_item()
                    .unwrap();
                assert!(block.is_deleted(), "`abc` should be marked as deleted");
                assert_eq!(&block.content, &ItemContent::String(c.into()));
                i += 1;
            }
        }

        // force GC and check if original content is hard deleted
        doc.transact_mut().gc(None);

        let txn = doc.transact();
        let block = txn
            .doc()
            .blocks
            .get_block(&ID::new(ClientID::new(1), 1))
            .unwrap()
            .as_ref();
        assert_eq!(block.len(), 3, "GCed blocks should be squashed");
        assert!(block.is_deleted(), "`abc` should be deleted");
        assert_matches!(&block, &Block::GC(_));
    }

    #[test]
    fn force_gc_with_delete_set() {
        let mut doc = Doc::with_options(Options {
            client_id: ClientID::new(1),
            skip_gc: true,
            ..Default::default()
        });
        let m0 = doc.get_or_insert_map("map");
        let s1 = {
            let mut tx = doc.transact_mut();
            let t1 = init_test_data(&mut tx, ["c", "b", "a"]); // <1#1..3>
            assert_eq!(t1.get_string(&tx), "abc");
            tx.snapshot()
        };

        let s2 = {
            let mut tx = doc.transact_mut();
            let t2 = init_test_data(&mut tx, ["f", "e", "d"]); // <1#5..7>
            assert_eq!(t2.get_string(&tx), "def");
            tx.snapshot()
        };

        let s3 = {
            let mut tx = doc.transact_mut();
            let t3 = init_test_data(&mut tx, ["i", "h", "g"]); // <1#9..11>
            assert_eq!(t3.get_string(&tx), "ghi");
            tx.snapshot()
        };

        // restore data to s1
        {
            let doc_restored = restore_from_snapshot(&doc, &s1).unwrap();
            let txn = doc_restored.transact();
            let m0_restored = txn.get_map("map").unwrap();
            let txt = m0_restored
                .get(&txn, "text")
                .unwrap()
                .cast::<TextRef>()
                .unwrap();
            assert_eq!(txt.get_string(&txn), "abc");
        }

        // verify that blocks 'abc' are not GCed and available
        {
            let txn = doc.transact();
            let mut i = 1;
            for c in ["c", "b", "a"] {
                let block = txn
                    .doc()
                    .blocks
                    .get_block(&ID::new(ClientID::new(1), i))
                    .unwrap()
                    .as_item()
                    .unwrap();
                assert!(block.is_deleted(), "`abc` should be marked as deleted");
                assert_eq!(&block.content, &ItemContent::String(c.into()));
                i += 1;
            }
        }

        // garbage collect anything below s2
        doc.transact_mut().gc(Some(&s2.delete_set));

        // verify that we GC 'abc' blocks and compressed them
        let txn = doc.transact();
        let block = txn
            .doc()
            .blocks
            .get_block(&ID::new(ClientID::new(1), 1))
            .unwrap()
            .as_ref();
        assert_eq!(
            block,
            &Block::GC(BlockRange::new(ID::new(ClientID::new(1), 1), 3)),
            "block should be GCed & compressed"
        );

        // try to restore data to s1 again
        let doc_restored = restore_from_snapshot(&doc, &s1).unwrap();
        let txn = doc_restored.transact();
        let m0_restored = txn.get_map("map").unwrap();
        let txt = m0_restored.get(&txn, "text");
        assert!(
            txt.is_none(),
            "we restored snapshot s1, but it's content should be already GCed"
        );

        // verify that blocks from s2 are still accessible
        {
            let doc_restored = restore_from_snapshot(&doc, &s2).unwrap();
            let txn = doc_restored.transact();
            let m0_restored = txn.get_map("map").unwrap();
            let txt = m0_restored
                .get(&txn, "text")
                .unwrap()
                .cast::<TextRef>()
                .unwrap();
            assert_eq!(txt.get_string(&txn), "def");
        }
    }

    fn restore_from_snapshot(doc: &Doc, snapshot: &Snapshot) -> Result<Doc, Error> {
        let mut encoder = EncoderV1::new();
        doc.transact()
            .encode_state_from_snapshot(&snapshot, &mut encoder)?;
        let mut doc = Doc::new();
        doc.transact_mut()
            .apply_update(Update::decode_v1(&encoder.to_vec()).unwrap())
            .unwrap();
        Ok(doc)
    }

    #[test]
    fn uuid_generation() {
        let guid = uuid_v4();
        let uuid = uuid::Uuid::parse_str(&guid).unwrap();
        assert_eq!(&*uuid.to_string(), &*guid);
    }

    #[test]
    fn pending_delete_out_of_order() {
        // Test for bug fix: pending deletes should be recorded when the target client
        // doesn't exist in the block store yet
        let mut doc = Doc::new();

        let (upd1, upd2) = {
            let mut doc2 = Doc::new();
            let mut tx = doc2.transact_mut();
            let text = tx.get_or_insert_text("example");
            text.insert(&mut tx, 0, "foo");
            let upd1 = tx.encode_update_v2();
            drop(tx);

            let mut tx = doc2.transact_mut();
            let text = tx.get_or_insert_text("example");
            text.remove_range(&mut tx, 0, 1);
            let upd2 = tx.encode_update_v2();
            assert_eq!(text.get_string(&tx), "oo");
            drop(tx);

            (upd1, upd2)
        };

        // Apply delete BEFORE insert (out of order)
        let mut tx = doc.transact_mut();
        tx.apply_update(Update::decode_v2(&upd2).unwrap()).unwrap();

        // Delete should be pending since the blocks don't exist yet
        assert!(tx.has_missing_updates(), "Delete should be pending");

        // Apply insert
        tx.apply_update(Update::decode_v2(&upd1).unwrap()).unwrap();

        // After insert arrives, pending delete should be auto-applied
        let text = tx.get_or_insert_text("example");
        assert_eq!(
            text.get_string(&tx),
            "oo",
            "Pending delete should have been applied"
        );
    }
}
