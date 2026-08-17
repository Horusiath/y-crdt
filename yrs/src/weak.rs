use std::collections::hash_map::Entry;
use std::collections::{Bound, HashSet};
use std::convert::TryFrom;
use std::fmt::Formatter;
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut, RangeBounds};
use std::sync::Arc;

use thiserror::Error;

use crate::block::{ItemContent, ItemPtr};
use crate::iter::{
    AsIter, BlockIter, BlockIterator, BlockSliceIterator, IntoBlockIter, RangeIter, TxnIterator,
    Values,
};
use crate::node::{Node, NodePtr, TypeRef};
use crate::{Assoc, Doc, ID, IndexScope, NodeRef, Out, StickyIndex, Transaction, TransactionMut};

impl<T> NodeRef<T> {
    /// Returns a [LinkSource] corresponding with current [WeakRef].
    /// Returns `None` if underlying branch reference was not meant to be used as [WeakRef].
    pub fn try_source(&self) -> Option<&LinkSource> {
        let node = self.as_ref();
        if let TypeRef::WeakLink(source) = &node.type_ref {
            Some(source)
        } else {
            None
        }
    }

    /// Returns a [LinkSource] corresponding with current [WeakRef].
    ///
    /// # Panics
    ///
    /// This method panic if an underlying branch was not meant to be used as [WeakRef]. This can
    /// happen if a different shared type was forcibly cast to [WeakRef]. To avoid panic, use
    /// [WeakRef::try_source] instead.
    pub fn source(&self) -> &LinkSource {
        self.try_source()
            .expect("Defect: called WeakRef-specific method over non-WeakRef shared type")
    }

    /// Returns a block [ID] to a beginning of a quoted range.
    /// For quotes linking to a single elements this is equal to [WeakRef::end_id].
    pub fn start_id(&self) -> Option<&ID> {
        self.source().start().id()
    }

    /// Returns a block [ID] to an ending of a quoted range.
    /// For quotes linking to a single elements this is equal to [WeakRef::start_id].
    pub fn end_id(&self) -> Option<&ID> {
        self.source().end().id()
    }
}

impl<T, D> NodeRef<T>
where
    T: Deref<Target = Transaction<D>>,
    D: Deref<Target = Doc>,
{
    /// Tries to dereference a value for linked Map entry. If element didn't exist, `None` will
    /// be returned.
    ///
    /// # Example
    ///
    /// ```rust
    /// use yrs::{Doc, Map};
    ///
    /// let mut doc = Doc::new();
    /// let map = doc.get_or_insert_map("map");
    /// let mut txn = doc.transact_mut();
    ///
    /// // insert a value and the link referencing it
    /// map.insert(&mut txn, "A", "value");
    /// let link = map.link(&txn, "A").unwrap();
    /// let link = map.insert(&mut txn, "B", link);
    ///
    /// assert_eq!(link.try_deref_value(&txn), Some("value".into()));
    ///
    /// // update entry and check if link has been updated
    /// map.insert(&mut txn, "A", "other");
    /// assert_eq!(link.try_deref_value(&txn), Some("other".into()));
    /// ```
    pub fn try_deref(&self) -> Option<Out> {
        let source = self.try_source()?;
        let item = source.start().get_item(self.txn().doc());
        let last = item.to_iter().last()?;
        if last.is_deleted() {
            None
        } else {
            last.content.get_last()
        }
    }

    /// Returns an iterator over [Out]s existing in a scope of the current [WeakRef] quotation
    /// range.
    pub fn unquote(&self) -> Unquote<'_, D> {
        if let Some(source) = self.try_source() {
            source.unquote(self.txn())
        } else {
            Unquote::empty()
        }
    }
}

#[repr(transparent)]
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct LinkSource(Arc<LinkSourceInner>);

#[derive(Debug, Eq, PartialEq)]
pub struct LinkSourceInner {
    pub(crate) quote_start: StickyIndex,
    pub(crate) quote_end: StickyIndex,
}

impl LinkSource {
    pub fn new(start: StickyIndex, end: StickyIndex) -> Self {
        LinkSource(Arc::new(LinkSourceInner {
            quote_start: start,
            quote_end: end,
        }))
    }

    pub fn start(&self) -> &StickyIndex {
        &self.0.quote_start
    }

    pub fn end(&self) -> &StickyIndex {
        &self.0.quote_end
    }

    #[inline]
    pub fn is_single(&self) -> bool {
        match (self.start().scope(), self.end().scope()) {
            (IndexScope::Relative(x), IndexScope::Relative(y)) => x == y,
            _ => false,
        }
    }

    /// Remove reference to current weak link from all items it quotes.
    pub(crate) fn unlink_all(&self, txn: &mut TransactionMut, branch_ptr: NodePtr) {
        let item = self.start().get_item(txn.doc());
        let mut i = item.to_iter();
        while let Some(item) = Iterator::next(&mut i) {
            if item.info.is_linked() {
                txn.unlink(item, branch_ptr);
            }
        }
    }

    pub(crate) fn unquote<'a, D: Deref<Target = Doc>>(
        &self,
        txn: &'a Transaction<D>,
    ) -> Unquote<'a, D> {
        let mut current = self.start().get_item(txn.doc());
        if let Some(ptr) = &mut current {
            if Self::try_right_most(ptr) {
                current = Some(*ptr);
            }
        }
        if let Some(item) = current.as_deref() {
            let parent = *item.parent.as_node().unwrap();
            Unquote::new(txn, parent, self.start().clone(), self.end().clone())
        } else {
            Unquote::empty()
        }
    }

    /// If provided ref is pointing to map type which has been updated, we may want to invalidate
    /// current pointer to point to its right most neighbor.
    fn try_right_most(item: &mut ItemPtr) -> bool {
        if item.parent_sub.is_some() {
            // for map types go to the most recent one
            if let Some(curr_block) = item.right.to_iter().last() {
                *item = curr_block;
                return true;
            }
        }
        false
    }

    pub(crate) fn materialize(&self, txn: &mut TransactionMut, inner_ref: NodePtr) {
        let curr = if let Some(ptr) = self.start().get_item(txn.doc()) {
            ptr
        } else {
            // referenced element has already been GCed
            return;
        };
        if curr.parent_sub.is_some() {
            // for maps, advance to most recent item
            if let Some(mut last) = Some(curr).to_iter().last() {
                last.info.set_linked();
                let linked_by = txn.doc.linked_by.entry(last).or_default();
                linked_by.insert(inner_ref);
            }
        } else {
            let mut first = true;
            let from = self.start().clone();
            let to = self.end().clone();
            let mut i = Some(curr).to_iter().within_range(from, to);
            while let Some(slice) = i.next() {
                let mut item = if !slice.adjacent() {
                    txn.doc.materialize(slice)
                } else {
                    slice.ptr
                };
                if first {
                    first = false;
                }
                item.info.set_linked();
                let linked_by = txn.doc.linked_by.entry(item).or_default();
                linked_by.insert(inner_ref);
            }
        }
    }

    pub(crate) fn quoted_string<D: Deref<Target = Doc>>(
        &self,
        txn: &Transaction<D>,
        f: &mut Formatter<'_>,
    ) -> std::fmt::Result {
        let mut curr = self.start().get_item(txn.doc());
        let end = self.end().id();
        while let Some(item) = curr.as_deref() {
            if let Some(end) = end {
                if self.end().assoc == Assoc::Before && &item.id == end {
                    // right side is open (last item excluded)
                    break;
                }
            }
            if !item.is_deleted() {
                if let ItemContent::String(s) = &item.content {
                    f.write_str(s.as_str())?;
                }
            }
            if let Some(end) = end {
                if self.end().assoc == Assoc::After && &item.last_id() == end {
                    // right side is closed (last item included)
                    break;
                }
            }
            curr = item.right;
        }
        Ok(())
    }
}

/// Iterator over non-deleted items, bounded by the given ID range.
pub struct Unquote<'a, D>(Option<AsIter<'a, D, Values<RangeIter<BlockIter>>>>);

impl<'a, D: Deref<Target = Doc>> Unquote<'a, D> {
    fn new(txn: &'a Transaction<D>, parent: NodePtr, from: StickyIndex, to: StickyIndex) -> Self {
        let iter = BlockIter::new(parent.start).within_range(from, to).values();
        Unquote(Some(AsIter::new(iter, txn)))
    }

    fn empty() -> Self {
        Unquote(None)
    }
}

impl<'a, D: Deref<Target = Doc>> Iterator for Unquote<'a, D> {
    type Item = Out;

    fn next(&mut self) -> Option<Self::Item> {
        let iter = self.0.as_mut()?;
        iter.next()
    }
}

/// Trait which defines a capability to quote a range of elements from implementing collection
/// and referencing them later in other collections.
pub trait Quotable: AsRef<Node> + Sized {
    /// Returns [WeakPrelim] to a given range of elements, if it's in a boundaries of a current
    /// quotable collection.
    ///
    /// Quoted ranges inclusivity define behavior of quote in face of concurrent inserts that might
    /// have happen, example:
    /// - Inclusive range (eg. `1..=2`) means, that any concurrent inserts that happen between
    ///   indexes 2 and 3 will **not** be part of the quoted range.
    /// - Exclusive range (eg. `1..3`) theoretically being similar to an upper one, will behave
    ///   differently as for concurrent inserts on 2nd and 3rd index boundary, these inserts will be
    ///   counted as a part of quoted range.
    ///
    /// # Errors
    ///
    /// This method may return an [QuoteError::OutOfBounds] if passed range params span beyond
    /// the boundaries of a current collection ie. `0..yarray.len()` will error, as the upper index
    /// refers to position that's not present in current collection - even though the position
    /// itself is not included in range it still has to exists as a point of reference.
    ///
    /// Currently this method doesn't support unbounded ranges (ie. `..n`, `n..`). Passing such
    /// range will cause [QuoteError::UnboundedRange] error.
    ///
    /// # Example
    /// ```
    /// use yrs::{Doc, Array, Assoc, Quotable};
    /// let mut doc = Doc::new();
    /// let array = doc.get_or_insert_array("array");
    /// array.insert_range(&mut doc.transact_mut(), 0, [1,2,3,4]);
    /// // quote elements 2 and 3
    /// let prelim = array.quote(&doc.transact(), 1..3).unwrap();
    /// let quote = array.insert(&mut doc.transact_mut(), 0, prelim);
    /// // retrieve quoted values
    /// let quoted: Vec<_> = quote.unquote(&doc.transact()).collect();
    /// assert_eq!(quoted, vec![2.into(), 3.into()]);
    /// ```
    fn quote<D, R>(&self, txn: &Transaction<D>, range: R) -> Result<LinkSource, QuoteError>
    where
        D: Deref<Target = Doc>,
        R: RangeBounds<u32>,
    {
        let this = NodePtr::from(self.as_ref());
        let start = match range.start_bound() {
            Bound::Included(&i) => Some((i, Assoc::Before)),
            Bound::Excluded(&i) => Some((i, Assoc::After)),
            Bound::Unbounded => None,
        };
        let end = match range.end_bound() {
            Bound::Included(&i) => Some((i, Assoc::After)),
            Bound::Excluded(&i) => Some((i, Assoc::Before)),
            Bound::Unbounded => None,
        };
        let encoding = txn.doc().options.offset_kind;
        let mut start_index = 0;
        let mut remaining = start_index;
        let mut curr = None;
        let mut i = this.start.to_iter();

        let start = if let Some((start_i, assoc_start)) = start {
            start_index = start_i;
            remaining = start_index;
            // figure out the first ID
            curr = i.next();
            while let Some(item) = curr.as_deref() {
                if remaining == 0 {
                    break;
                }
                if !item.is_deleted() && item.is_countable() {
                    let len = item.content_len(encoding);
                    if remaining < len {
                        break;
                    }
                    remaining -= len;
                }
                curr = i.next();
            }
            let start_id = if let Some(item) = curr.as_deref() {
                let mut id = item.id.clone();
                id.clock += if let ItemContent::String(s) = &item.content {
                    s.block_offset(remaining, encoding)
                } else {
                    remaining
                };
                id
            } else {
                return Err(QuoteError::OutOfBounds);
            };
            StickyIndex::new(IndexScope::Relative(start_id), assoc_start)
        } else {
            curr = i.next();
            StickyIndex::new(IndexScope::Absolute(this.id()), Assoc::Before)
        };

        let end = if let Some((end_index, assoc_end)) = end {
            // figure out the last ID
            remaining = end_index - start_index + remaining;
            while let Some(item) = curr.as_deref() {
                if !item.is_deleted() && item.is_countable() {
                    let len = item.content_len(encoding);
                    if remaining < len {
                        break;
                    }
                    remaining -= len;
                }
                curr = i.next();
            }
            let end_id = if let Some(item) = curr.as_deref() {
                let mut id = item.id.clone();
                id.clock += if let ItemContent::String(s) = &item.content {
                    s.block_offset(remaining, encoding)
                } else {
                    remaining
                };
                id
            } else {
                return Err(QuoteError::OutOfBounds);
            };
            StickyIndex::new(IndexScope::Relative(end_id), assoc_end)
        } else {
            StickyIndex::new(IndexScope::Absolute(this.id()), Assoc::After)
        };

        let source = LinkSource::new(start, end);
        Ok(source)
    }
}

/// Error that may appear in result of [Quotable::quote] method call.
#[derive(Debug, Error)]
pub enum QuoteError {
    /// Range lower or upper indexes passed to [Quotable::quote] were beyond scope of quoted
    /// collection.
    ///
    /// Remember: even though range itself may not include index (ie. `1..n`), that index still
    /// needs to point to existing value within quoted collection (`n < ytype.len()`) as a point
    /// of reference.
    #[error("Quoted range spans beyond the bounds of current collection")]
    OutOfBounds,
}

pub(crate) fn join_linked_range(mut block: ItemPtr, txn: &mut TransactionMut) {
    let block_copy = block.clone();
    let item = block.deref_mut();
    // this item may exists within a quoted range
    item.info.set_linked();
    // we checked if left and right exists before this method call
    let left = item.left.unwrap();
    let right = item.right.unwrap();
    let all_links = &mut txn.doc.linked_by;
    let left_links = all_links.get(&left);
    let right_links = all_links.get(&right);
    let mut common = HashSet::new();
    if let Some(llinks) = left_links {
        for link in llinks.iter() {
            match right_links {
                Some(rlinks) if rlinks.contains(link) => {
                    // new item existing in a quoted range in between two elements
                    common.insert(*link);
                }
                _ => {
                    if let TypeRef::WeakLink(source) = &link.type_ref {
                        if source.end().assoc == Assoc::Before {
                            // We're at the right edge of quoted range - right neighbor is not included
                            // but the left one is. Since quotation is open on the right side, we need to
                            // include current item.
                            common.insert(*link);
                        }
                    }
                }
            }
        }
    }
    if let Some(rlinks) = right_links {
        for link in rlinks.iter() {
            match left_links {
                Some(llinks) if llinks.contains(link) => {
                    /* already visited by previous if-loop */
                }
                _ => {
                    if let TypeRef::WeakLink(source) = &link.type_ref {
                        if source.start().assoc == Assoc::After {
                            let start_id = source.start().id().cloned();
                            let prev_id = item.left.map(|i| i.last_id());
                            if start_id == prev_id {
                                // even though current boundary if left-side exclusive, current item
                                // has been inserted on the right of it, therefore it's within range
                                common.insert(*link);
                            }
                        }
                    }
                }
            }
        }
    }
    if !common.is_empty() {
        match all_links.entry(block) {
            Entry::Occupied(mut e) => {
                let links = e.get_mut();
                for link in common {
                    links.insert(link);
                }
            }
            Entry::Vacant(e) => {
                e.insert(common);
            }
        }
    }
}
