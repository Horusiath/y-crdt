use std::collections::hash_map::Entry;
use std::collections::{Bound, HashSet};
use std::convert::TryFrom;
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
use crate::{Assoc, Doc, ID, In, IndexScope, Out, StickyIndex, Transaction, TransactionMut};

/// Weak link reference represents a reference to a single element or consecutive range of elements
/// stored in another collection in the same document.
///
/// The same element may be linked by many [WeakRef]s, however the ownership still belongs to
/// a collection, where referenced elements were originally inserted in. For this reason removing
/// [WeakRef] doesn't affect linked elements. [WeakRef] can also be outdated when the linked
/// reference has been removed.
///
/// In order to create a [WeakRef], a preliminary [WeakPrelim] element must be obtained first. This
/// can be done via either:
///
/// - [Map::link] to pick a reference to key-value entry of map. As entry is being updated, so will
/// be the referenced value.
/// - [Array::quote] to take a reference to a consecutive range of array's elements. Any elements
/// inserted in an originally quoted range will later on appear when [WeakRef::unquote] is called.
/// - [Text::quote] to take a reference to a slice of text. When materialized, quoted slice will
/// contain any changes that happened within the quoted slice. It will also contain formatting
/// information about the quotation.
///
/// [WeakPrelim] can be used like any preliminary type (ie. inserted into array, map or as embedded
/// value in text), producing [WeakRef] in a result. [WeakRef] can be also cloned and converted back
/// into [WeakPrelim], allowing to reference the same element(s) in many different places.
///
/// [WeakRef] can also be observed on via [WeakRef::observe]/[WeakRef::observe_deep]. These enable
/// to react to changes which happen in other parts of the document tree.
///
/// # Example
///
/// ```rust
/// use yrs::{Array, Doc, Map, Quotable, Assoc};
///
/// let mut doc = Doc::new();
/// let array = doc.get_or_insert_array("array");
/// let map = doc.get_or_insert_map("map");
/// let mut txn = doc.transact_mut();
///
/// // insert values
/// array.insert_range(&mut txn, 0, ["A", "B", "C", "D"]);
///
/// // link the reference for value in another collection
/// let link = array.quote(&txn, 1..=2).unwrap(); // [B, C]
/// let link = map.insert(&mut txn, "key", link);
///
/// // evaluate quoted range
/// let values: Vec<_> = link.unquote(&txn).map(|v| v.to_string(&txn)).collect();
/// assert_eq!(values, vec!["B".to_string(), "C".to_string()]);
///
/// // update quoted range
/// array.insert(&mut txn, 2, "E"); // [A, B, E, C, D]
///
/// // evaluate quoted range (updated)
/// let values: Vec<_> = link.unquote(&txn).map(|v| v.to_string(&txn)).collect();
/// assert_eq!(values, vec!["B".to_string(), "E".to_string(), "C".to_string()]);
/// ```
#[repr(transparent)]
#[derive(Debug, Clone)]
pub struct WeakRef<P>(P);

impl<P: AsRef<Node>> WeakRef<P> {
    /// Returns a [LinkSource] corresponding with current [WeakRef].
    /// Returns `None` if underlying branch reference was not meant to be used as [WeakRef].
    pub fn try_source(&self) -> Option<&Arc<LinkSource>> {
        let branch = self.as_ref();
        if let TypeRef::WeakLink(source) = &branch.type_ref {
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
    /// happen if a different shared type was forcibly casted to [WeakRef]. To avoid panic, use
    /// [WeakRef::try_source] instead.
    pub fn source(&self) -> &Arc<LinkSource> {
        self.try_source()
            .expect("Defect: called WeakRef-specific method over non-WeakRef shared type")
    }

    /// Returns a block [ID] to a beginning of a quoted range.
    /// For quotes linking to a single elements this is equal to [WeakRef::end_id].
    pub fn start_id(&self) -> Option<&ID> {
        self.source().quote_start.id()
    }

    /// Returns a block [ID] to an ending of a quoted range.
    /// For quotes linking to a single elements this is equal to [WeakRef::start_id].
    pub fn end_id(&self) -> Option<&ID> {
        self.source().quote_end.id()
    }
}

impl<P: From<NodePtr>> From<NodePtr> for WeakRef<P> {
    fn from(inner: NodePtr) -> Self {
        WeakRef(P::from(inner))
    }
}

impl<P: TryFrom<ItemPtr>> TryFrom<ItemPtr> for WeakRef<P> {
    type Error = P::Error;

    fn try_from(value: ItemPtr) -> Result<Self, Self::Error> {
        match P::try_from(value) {
            Ok(p) => Ok(WeakRef(p)),
            Err(e) => Err(e),
        }
    }
}

impl<P: From<NodePtr>> TryFrom<Out> for WeakRef<P> {
    type Error = Out;

    fn try_from(value: Out) -> Result<Self, Self::Error> {
        match value {
            Out::YWeakLink(value) => Ok(WeakRef(P::from(value.0))),
            other => Err(other),
        }
    }
}

impl<P> WeakRef<P>
where
    P: SharedRef + Map,
{
    /// Tries to dereference a value for linked [Map] entry, performing automatic conversion if
    /// possible. If conversion was not possible or element didn't exist, an error case will be
    /// returned.
    ///
    /// Use [WeakRef::try_deref_value] if conversion is not possible or desired at the current moment.
    pub fn try_deref<D, V>(&self, txn: &Transaction<D>) -> Result<V, Option<V::Error>>
    where
        D: Deref<Target = Doc>,
        V: TryFrom<Out>,
    {
        if let Some(value) = self.try_deref_value(txn) {
            match V::try_from(value) {
                Ok(value) => Ok(value),
                Err(value) => Err(Some(value)),
            }
        } else {
            Err(None)
        }
    }

    /// Tries to dereference a value for linked [Map] entry. If element didn't exist, `None` will
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
    pub fn try_deref_value<D: Deref<Target = Doc>>(&self, txn: &Transaction<D>) -> Option<Out> {
        let source = self.try_source()?;
        let item = source.quote_start.get_item(txn.doc());
        let last = item.to_iter().last()?;
        if last.is_deleted() {
            None
        } else {
            last.content.get_last()
        }
    }
}

impl<P> WeakRef<P>
where
    P: SharedRef + Array,
{
    /// Returns an iterator over [Out]s existing in a scope of the current [WeakRef] quotation
    /// range.
    pub fn unquote<'a, D: Deref<Target = Doc>>(&self, txn: &'a Transaction<D>) -> Unquote<'a, D> {
        if let Some(source) = self.try_source() {
            source.unquote(txn)
        } else {
            Unquote::empty()
        }
    }
}

impl<V> AsPrelim for WeakRef<V>
where
    V: AsRef<Node> + TryFrom<ItemPtr>,
{
    type Prelim = WeakPrelim<V>;

    fn as_prelim<D: Deref<Target = Doc>>(&self, _txn: &Transaction<D>) -> Self::Prelim {
        let source = self.try_source().unwrap();
        WeakPrelim::with_source(source.clone())
    }
}

/// A preliminary type for [WeakRef]. Once inserted into document it can be used as a weak reference
/// link to another value living inside of the document store.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct WeakPrelim<P> {
    source: Arc<LinkSource>,
    _marker: PhantomData<P>,
}

impl<P> WeakPrelim<P> {
    pub(crate) fn new(start: StickyIndex, end: StickyIndex) -> Self {
        let source = Arc::new(LinkSource::new(start, end));
        WeakPrelim {
            source,
            _marker: PhantomData::default(),
        }
    }
    pub(crate) fn with_source(source: Arc<LinkSource>) -> Self {
        WeakPrelim {
            source,
            _marker: PhantomData::default(),
        }
    }

    pub fn into_inner(&self) -> WeakPrelim<NodePtr> {
        WeakPrelim {
            source: self.source.clone(),
            _marker: PhantomData::default(),
        }
    }

    pub fn source(&self) -> &Arc<LinkSource> {
        &self.source
    }
}

impl<P> WeakPrelim<P>
where
    P: SharedRef + Array,
{
    /// Returns an iterator over [Out]s existing in a scope of the current [WeakPrelim] quotation
    /// range.
    pub fn unquote<'a, D: Deref<Target = Doc>>(&self, txn: &'a Transaction<D>) -> Unquote<'a, D> {
        self.source.unquote(txn)
    }
}

impl<P> WeakPrelim<P>
where
    P: SharedRef + Map,
{
    pub fn try_deref_raw<D: Deref<Target = Doc>>(&self, txn: &Transaction<D>) -> Option<Out> {
        self.source.unquote(txn).next()
    }

    pub fn try_deref<D, V>(&self, txn: &Transaction<D>) -> Result<V, Option<V::Error>>
    where
        D: Deref<Target = Doc>,
        V: TryFrom<Out>,
    {
        if let Some(value) = self.try_deref_raw(txn) {
            match V::try_from(value) {
                Ok(value) => Ok(value),
                Err(value) => Err(Some(value)),
            }
        } else {
            Err(None)
        }
    }
}

impl<P: AsRef<Node>> From<WeakRef<P>> for WeakPrelim<P> {
    fn from(value: WeakRef<P>) -> Self {
        let branch = value.0.as_ref();
        if let TypeRef::WeakLink(source) = &branch.type_ref {
            WeakPrelim {
                source: source.clone(),
                _marker: PhantomData::default(),
            }
        } else {
            panic!("Defect: WeakRef's underlying branch is not matching expected weak ref.")
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct LinkSource {
    pub(crate) quote_start: StickyIndex,
    pub(crate) quote_end: StickyIndex,
}

impl LinkSource {
    pub fn new(start: StickyIndex, end: StickyIndex) -> Self {
        LinkSource {
            quote_start: start,
            quote_end: end,
        }
    }

    #[inline]
    pub fn is_single(&self) -> bool {
        match (self.quote_start.scope(), self.quote_end.scope()) {
            (IndexScope::Relative(x), IndexScope::Relative(y)) => x == y,
            _ => false,
        }
    }

    /// Remove reference to current weak link from all items it quotes.
    pub(crate) fn unlink_all(&self, txn: &mut TransactionMut, branch_ptr: NodePtr) {
        let item = self.quote_start.get_item(txn.doc());
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
        let mut current = self.quote_start.get_item(txn.doc());
        if let Some(ptr) = &mut current {
            if Self::try_right_most(ptr) {
                current = Some(*ptr);
            }
        }
        if let Some(item) = current.as_deref() {
            let parent = *item.parent.as_node().unwrap();
            Unquote::new(
                txn,
                parent,
                self.quote_start.clone(),
                self.quote_end.clone(),
            )
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
        let curr = if let Some(ptr) = self.quote_start.get_item(txn.doc()) {
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
            let from = self.quote_start.clone();
            let to = self.quote_end.clone();
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

    pub fn to_string<D: Deref<Target = Doc>>(&self, txn: &Transaction<D>) -> String {
        let mut result = String::new();
        let mut curr = self.quote_start.get_item(txn.doc());
        let end = self.quote_end.id();
        while let Some(item) = curr.as_deref() {
            if let Some(end) = end {
                if self.quote_end.assoc == Assoc::Before && &item.id == end {
                    // right side is open (last item excluded)
                    break;
                }
            }
            if !item.is_deleted() {
                if let ItemContent::String(s) = &item.content {
                    result.push_str(s.as_str());
                }
            }
            if let Some(end) = end {
                if self.quote_end.assoc == Assoc::After && &item.last_id() == end {
                    // right side is closed (last item included)
                    break;
                }
            }
            curr = item.right;
        }
        result
    }

    pub fn to_xml_string<D: Deref<Target = Doc>>(&self, txn: &Transaction<D>) -> String {
        let curr = self.quote_start.get_item(txn.doc());
        if let Some(item) = curr.as_deref() {
            if let Some(branch) = item.parent.as_node() {
                return XmlTextRef::get_string_fragment(
                    branch.start,
                    Some(&self.quote_start),
                    Some(&self.quote_end),
                );
            }
        }
        String::new()
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
    fn quote<D, R>(&self, txn: &Transaction<D>, range: R) -> Result<WeakPrelim<Self>, QuoteError>
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
        Ok(WeakPrelim::with_source(Arc::new(source)))
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
                        if source.quote_end.assoc == Assoc::Before {
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
                        if source.quote_start.assoc == Assoc::After {
                            let start_id = source.quote_start.id().cloned();
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
