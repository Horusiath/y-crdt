use crate::block::{BlockRange, Item, ItemContent, ItemPtr};
use crate::cell::{Acquire, AcquireMut};
use crate::formatter::{AttrValue, AttributeValue, DiffFormatter};
use crate::lib0::{Decode, Encode, Encoder, EncoderV1};
use crate::meta::{ContentIds, ContentMap};
use crate::node::NodePtr;
use crate::slice::{BlockSlice, ItemSlice};
use crate::{
    Any, Cell, Doc, DocHandle, ID, IdSet, OffsetKind, Origin, Transaction, TransactionMut, Update,
};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

/// Session used to track changes between `prev` document and `next` suggestion document.
///
/// Changes made to `next` render as suggestions through [Suggestion::formatter], until they
/// are accepted (moved into `prev`) or rejected (undone in `next`). Changes made to `prev` always
/// flow into `next`. With suggestion mode off, local changes to `next` flow into `prev` as well.
///
/// The session lives until [Suggestion::destroy] is called, it's dropped, or either doc is
/// destroyed.
///
/// # Panics
///
/// Committing a transaction on one doc while holding a transaction on the other one can result in
/// deadlock or panic.
pub struct Suggestion<A: AttributeValue = AttrValue> {
    inner: Arc<Inner<A>>,
}

struct Inner<A: AttributeValue> {
    prev: Cell<Doc>,
    next: Cell<Doc>,
    diff: Cell<DiffFormatter<A>>,
    suggestion_mode: AtomicBool,
    suggestion_origins: Cell<Option<Vec<Origin>>>,
    /// Tags every update the session applies, and keys its observers.
    origin: Origin,
}

impl<A: AttributeValue> Suggestion<A> {
    /// Starts a session rendering the changes `next` made since `prev`. Deleted content that
    /// `next` no longer holds is restored from `prev`.
    ///
    /// `next` should have [crate::Options::cleanup_formatting] off, otherwise suggested formatting
    /// may be cleaned up. `prev` and `next` must be distinct docs.
    pub fn new(prev: Cell<Doc>, next: Cell<Doc>, attributions: Option<ContentMap<A>>) -> Self {
        let diff = {
            let (p, n) = (prev.acquire(), next.acquire());
            assert_ne!(p.guid(), n.guid(), "prev and next must be distinct docs");
            let mut diff = DiffFormatter::new(&ContentIds::doc_diff(&p, &n), attributions);
            diff.restore_content(&p.transact());
            diff
        };
        let inner = Arc::new_cyclic(|weak: &Weak<Inner<A>>| Inner {
            prev,
            next,
            diff: Cell::new(diff),
            suggestion_mode: AtomicBool::new(true),
            suggestion_origins: Cell::new(None),
            origin: Origin::from(weak.as_ptr() as usize),
        });
        Inner::register(&inner);
        Suggestion { inner }
    }

    /// Runs `f` with the current diff formatter (ex. `node.formatted(fmt).delta(..)`). `f` must not
    /// commit a transaction on either doc.
    pub fn formatter(&self) -> crate::Ref<'_, DiffFormatter<A>> {
        self.inner.diff.acquire()
    }

    /// With suggestion mode on (default), local changes to `next` stay there as suggestions.
    pub fn set_suggestion_mode(&self, on: bool) {
        self.inner.suggestion_mode.store(on, Ordering::Release);
    }

    pub fn suggestion_mode(&self) -> bool {
        self.inner.suggestion_mode.load(Ordering::Acquire)
    }

    /// Limits the origins of `next` transactions which flow into `prev` with suggestion mode off.
    /// `None` (default) allows any origin.
    pub fn set_suggestion_origins(&self, origins: Option<Vec<Origin>>) {
        *self.inner.suggestion_origins.acquire_mut() = origins;
    }

    /// Accepts all suggestions.
    pub fn accept_all_changes(&self) {
        let inner = &*self.inner;
        inner.apply(&inner.prev, inner.prev_upgrade());
    }

    /// Rejects all suggestions.
    pub fn reject_all_changes(&self) {
        let update = self.inner.prev_upgrade();
        let mut prev = self.inner.prev.acquire_mut();
        // untagged: the revert must flow back into `next`
        let mut txn = prev.transact_mut();
        if txn.apply_update(update).is_ok() {
            let (inserts, deletes) = (txn.insert_set().clone(), txn.delete_set().clone());
            txn.revert(&inserts, &deletes);
        }
    }

    /// Accepts the suggestions between `start` and `end` (or till the end of their node if
    /// `None`). Suggested formatting is accepted till all formats opened in the range are closed.
    pub fn accept_changes(&self, start: ID, end: Option<ID>) {
        let inner = &*self.inner;
        let update = {
            let next = inner.next.acquire();
            let ids = inner
                .diff
                .acquire()
                .collect_suggested_changes(&next, start, end, true);
            let mut encoder = EncoderV1::new();
            next.write_selected(&ids.inserts, &mut encoder);
            ids.deletes.encode(&mut encoder);
            encoder.to_vec()
        };
        if let Ok(update) = Update::decode_v1(&update) {
            inner.apply(&inner.prev, update);
        }
    }

    /// Rejects the suggestions between `start` and `end` (or till the end of their node if
    /// `None`).
    pub fn reject_changes(&self, start: ID, end: Option<ID>) {
        {
            let mut next = self.inner.next.acquire_mut();
            let mut txn = next.transact_mut();
            let ids = {
                let diff = self.inner.diff.acquire();
                let ids = diff.collect_suggested_changes(txn.doc(), start, end, false);
                diff.refill(txn.doc_mut(), &ids.deletes);
                ids
            };
            txn.revert(&ids.inserts, &ids.deletes);
        }
        self.accept_changes(start, end);
    }

    /// Ends the session, unregistering its observers.
    pub fn destroy(self) {
        // see: Drop
    }
}

impl<A: AttributeValue> Drop for Suggestion<A> {
    fn drop(&mut self) {
        self.inner.unregister();
    }
}

impl<A: AttributeValue> Inner<A> {
    fn register(this: &Arc<Self>) {
        let origin = &this.origin;
        {
            let mut next = this.next.acquire_mut();
            let w = Arc::downgrade(this);
            next.observe_before_observer_calls_with(origin.clone(), move |txn| {
                if let Some(s) = w.upgrade() {
                    s.on_next_change(txn);
                }
            });
            let w = Arc::downgrade(this);
            next.observe_update_v1_with(origin.clone(), move |txn, e| {
                if let Some(s) = w.upgrade() {
                    s.on_next_update(txn, &e.update);
                }
            });
            let w = Arc::downgrade(this);
            next.observe_after_transaction_with(origin.clone(), move |txn| {
                if let Some(s) = w.upgrade() {
                    s.on_next_commit(txn);
                }
            });
            let w = Arc::downgrade(this);
            next.observe_destroy_with(origin.clone(), move |_, _| {
                if let Some(s) = w.upgrade() {
                    s.unregister();
                }
            });
        }
        let mut prev = this.prev.acquire_mut();
        let w = Arc::downgrade(this);
        prev.observe_before_observer_calls_with(origin.clone(), move |txn| {
            if let Some(s) = w.upgrade() {
                s.on_prev_change(txn);
            }
        });
        let w = Arc::downgrade(this);
        prev.observe_update_v1_with(origin.clone(), move |txn, e| {
            if let Some(s) = w.upgrade() {
                s.on_prev_update(txn, &e.update);
            }
        });
        let w = Arc::downgrade(this);
        prev.observe_destroy_with(origin.clone(), move |_, _| {
            if let Some(s) = w.upgrade() {
                s.unregister();
            }
        });
    }

    fn unregister(&self) {
        for cell in [&self.prev, &self.next] {
            if let Some(mut doc) = cell.try_acquire_mut() {
                doc.unobserve_before_observer_calls(self.origin.clone());
                doc.unobserve_update_v1(self.origin.clone());
                doc.unobserve_after_transaction(self.origin.clone());
                doc.unobserve_destroy(self.origin.clone());
            }
        }
    }

    fn on_next_change(&self, txn: &Transaction<&Doc>) {
        if txn.origin() == Some(&self.origin) || !has_changes(txn) {
            return;
        }
        // change made in `next`: unless forwarded from `prev`, it's a suggestion
        let mut diff = self.diff.acquire_mut();
        diff.extend(&ContentIds::from_transaction(txn));
        diff.restore_content(txn); // deleted content is gc'd later in the commit
    }

    fn on_prev_change(&self, txn: &Transaction<&Doc>) {
        if has_changes(txn) {
            let mut diff = self.diff.acquire_mut();
            // remove `prev`'s changes from diff formatter - they are a new baseline
            diff.retract(&ContentIds::from_transaction(txn));
        }
    }

    fn on_prev_update(&self, txn: &Transaction<&Doc>, update: &[u8]) {
        if txn.origin() != Some(&self.origin)
            && let Ok(update) = Update::decode_v1(update)
        {
            // sync `prev` changes to `next` doc
            self.apply(&self.next, update);
        }
    }

    fn on_next_update(&self, txn: &Transaction<&Doc>, update: &[u8]) {
        if self.forwards(txn)
            && let Ok(update) = Update::decode_v1(update)
        {
            // sync `next` changes to `prev` doc
            self.apply(&self.prev, update);
        }
    }

    fn on_next_commit(&self, txn: &mut TransactionMut) {
        if self.forwards(txn)
            && let Some(deletes) = txn.attributed_deletes()
        {
            // deletes of content already deleted in `next`, but still rendered, delete it in `prev`
            let update = Update {
                blocks: Default::default(),
                delete_set: deletes.clone(),
            };
            self.apply(&self.prev, update);
        }
    }

    /// Check if a `next` transaction applies to `prev`.
    fn forwards<D: DocHandle>(&self, txn: &Transaction<D>) -> bool {
        if self.suggestion_mode.load(Ordering::Acquire) || !txn.is_local() {
            return false;
        }
        let origin = txn.origin();
        if origin == Some(&self.origin) {
            return false;
        }
        match (&*self.suggestion_origins.acquire(), origin) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(origins), Some(origin)) => origins.contains(origin),
        }
    }

    /// Returns an update that contains changes from `next`, that `prev` is missing.
    fn prev_upgrade(&self) -> Update {
        let sv = self.prev.acquire().transact().state_vector();
        let update = self.next.acquire().transact().encode_diff_v1(&sv);
        Update::decode_v1(&update).unwrap_or_default()
    }

    /// Applies an `update` to a doc, tagged with the session origin.
    fn apply(&self, cell: &Cell<Doc>, update: Update) {
        let mut doc = cell.acquire_mut();
        let mut txn = doc.transact_mut_with(self.origin.clone());
        // updates encoded from a doc integrate in any other doc
        let _ = txn.apply_update(update);
    }
}

#[inline]
fn has_changes<D: DocHandle>(txn: &Transaction<D>) -> bool {
    !txn.insert_set().is_empty() || !txn.delete_set().is_empty()
}

impl<A: AttributeValue> DiffFormatter<A> {
    /// Ids of the suggestions in `doc` between `start` and `end` (or till the end of their node if
    /// `None`) - a port of Yjs `collectSuggestedChanges`. The walk starts at the first rendered
    /// unattributed content left of `start`, and goes on past `end` until all formats opened by
    /// collected markers are closed. With `all`, it also collects unattributed deletes, their
    /// inserts, and the subtrees of collected nodes, which the receiving doc filters.
    pub(crate) fn collect_suggested_changes(
        &self,
        doc: &Doc,
        start: ID,
        end: Option<ID>,
        all: bool,
    ) -> ContentIds {
        let mut ids = ContentIds::default();
        let Some(mut curr) = doc.blocks.get_item(&start) else {
            return ids;
        };
        let end = end.and_then(|end| doc.blocks.get_item(&end));
        // walk left to the first rendered unattributed content
        let mut item = Some(curr);
        while let Some(left) = curr.left {
            curr = left;
            if let ItemContent::Format(_, value) = &curr.content
                && matches!(**value, Any::Null | Any::Undefined)
            {
                item = curr.right;
                break;
            }
            item = Some(curr);
            if !curr.is_deleted() {
                let range = curr.range();
                let (mut unattributed, mut tail) = (false, None);
                for (r, attrs) in self.inserts().slice(&range) {
                    if attrs.is_none() {
                        (unattributed, tail) = (true, None);
                    } else if tail.is_none() {
                        tail = Some(r.start);
                    }
                }
                if unattributed {
                    if let Some(tail) = tail {
                        let end = range.clock + range.len;
                        ids.inserts.insert(ID::new(range.client, tail), end - tail);
                        if all && let Some(node) = curr.content.as_node() {
                            ids.insert_subtree(node);
                        }
                    }
                    item = curr.right;
                    break;
                }
            }
        }
        let mut open_formats = HashSet::new();
        let mut found_end = false;
        'items: while let Some(curr) = item {
            item = curr.right;
            found_end |= Some(curr) == end;
            let range = curr.range();
            let id = |start: u32| ID::new(range.client, start);
            if curr.is_deleted() {
                for (r, attrs) in self.deletes().slice(&range) {
                    if attrs.is_some() || all {
                        ids.deletes.insert(id(r.start), r.end - r.start);
                        if all {
                            // inserted and deleted content may be missing in the receiving doc
                            ids.inserts.insert(id(r.start), r.end - r.start);
                        }
                    }
                }
                continue;
            }
            if let ItemContent::Format(key, value) = &curr.content {
                if matches!(**value, Any::Null | Any::Undefined) {
                    open_formats.remove(key);
                } else {
                    open_formats.insert(key.clone());
                }
            }
            for (r, attrs) in self.inserts().slice(&range) {
                if attrs.is_some() {
                    ids.inserts.insert(id(r.start), r.end - r.start);
                    if all && let Some(node) = curr.content.as_node() {
                        ids.insert_subtree(node);
                    }
                } else if found_end && open_formats.is_empty() {
                    break 'items;
                }
            }
        }
        ids
    }

    /// Puts the restored content back into the gc'd items of `deletes`, as undo re-creates
    /// items from their content (Yjs refills them in `collectSuggestedChanges`).
    fn refill(&self, doc: &mut Doc, deletes: &IdSet) {
        let gcd: Vec<_> = deletes
            .iter_blocks(&doc.blocks)
            .filter_map(|slice| match slice {
                BlockSlice::Item(s) if matches!(s.ptr.content, ItemContent::Deleted(_)) => {
                    Some(s.range())
                }
                _ => None,
            })
            .collect();
        for range in gcd {
            self.restored_in(&range, |clock, content| {
                let Some(item) = doc.blocks.get_item(&ID::new(range.client, clock)) else {
                    return;
                };
                let start = clock - item.id.clock;
                let end = start + content.len(OffsetKind::Utf16) - 1;
                let mut item = doc.materialize(ItemSlice::new(item, start, end));
                if content.is_countable() {
                    item.info.set_countable();
                }
                item.content = content;
            });
        }
    }
}

impl ContentIds {
    fn include(&mut self, item: &Item) {
        self.inserts.insert(item.id, item.len);
        if item.is_deleted() {
            self.deletes.insert(item.id, item.len);
        } else if let Some(child) = item.content.as_node() {
            self.insert_subtree(child);
        }
    }

    /// Insert all item ranges from `node` into current [ContentIds]. All visited items land in
    /// [ContentIds::inserts], while deleted ones also fill [ContentIds::deletes].
    fn insert_subtree(&mut self, node: NodePtr) {
        // for children: from head right-wise
        let mut curr = node.start;
        while let Some(item) = curr {
            self.include(&*item);
            curr = item.right;
        }

        // for attributes: from each entry left-wise
        for &item in node.map.values() {
            let mut curr = Some(item);
            while let Some(item) = curr {
                self.include(&*item);
                curr = item.left;
            }
        }
    }
}
