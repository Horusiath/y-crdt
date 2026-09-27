use crate::block::{ID, ItemContent, ItemPtr, split_str};
use crate::cursor::Cursor;
use crate::delta::{AttrOp, Op};
use crate::id_set::DeleteSet;
use crate::iter::TxnIterator;
use crate::node::{Attrs, DeepObservable, Node, NodePtr, Observable, TypePtr, TypeRef};
use crate::slice::BlockSlice;
use crate::{Any, Delta, Doc, IdSet, In, NodeID, OffsetKind, Out, Transaction};
use serde::de::Visitor;
use smallvec::SmallVec;
use std::collections::{Bound, HashMap, HashSet};
use std::fmt::{Display, Formatter};
use std::ops::{Deref, DerefMut, Range, RangeBounds};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct NodeRef<T> {
    ptr: NodePtr,
    txn: T,
}

impl<T> AsRef<Node> for NodeRef<T> {
    fn as_ref(&self) -> &Node {
        &*self.ptr
    }
}

impl<T> DeepObservable for NodeRef<T> {}
impl<T> Observable for NodeRef<T> {}

impl<T> NodeRef<T> {
    pub(crate) fn new(ptr: NodePtr, txn: T) -> Self {
        NodeRef { ptr, txn }
    }

    pub fn id(&self) -> NodeID {
        self.ptr.id()
    }
}

// read operations
impl<T, D> NodeRef<T>
where
    T: Deref<Target = Transaction<D>>,
    D: Deref<Target = Doc>,
{
    pub fn txn(&self) -> &Transaction<D> {
        &self.txn
    }

    fn seek(&self, index: u32) -> Option<Cursor> {
        Cursor::at(self.ptr, self.txn.doc.offset_kind(), index)
    }

    /// The parent type, or `None` for root-level types.
    pub fn parent(&self) -> Option<NodeID> {
        let item = self.ptr.item?;
        let parent = item.parent.as_node()?;
        Some(parent.id())
    }

    /// The element name. `None` for text/array/map types, `Some` for XML elements.
    pub fn name(&self) -> Option<&Arc<str>> {
        self.ptr.name.as_ref()
    }

    /// Number of countable (list/text) elements in this type.
    pub fn len(&self) -> u32 {
        self.ptr.content_len
    }

    /// Number of non-deleted key-value attribute pairs.
    pub fn attr_len(&self) -> u32 {
        let mut count = 0;
        for value in self.ptr.map.values() {
            if !value.is_deleted() {
                count += 1;
            }
        }
        count
    }

    /// Returns the element at the given index.
    pub fn get(&self, index: u32) -> Option<Out> {
        let mut cursor = self.seek(index)?;
        cursor.get()
    }

    /// Returns a portion of children within the given range.
    pub fn range<R: RangeBounds<u32>>(&self, range: R) -> Vec<Out> {
        let start = match range.start_bound() {
            Bound::Included(v) => *v,
            Bound::Excluded(v) => *v + 1,
            Bound::Unbounded => 0,
        };
        let end = match range.end_bound() {
            Bound::Included(v) => *v,
            Bound::Excluded(v) => *v - 1,
            Bound::Unbounded => self.ptr.content_len - 1,
        };

        debug_assert!(start <= end);
        let mut remaining = end - start;
        let mut res = Vec::with_capacity(remaining as usize);

        if let Some(mut cursor) = self.seek(start) {
            while remaining != 0
                && let Some(value) = cursor.next()
            {
                remaining -= 1;
                res.push(value);
            }
        };
        res
    }

    /// Iterator over children values.
    pub fn iter(&self) -> impl Iterator<Item = Out> + use<T, D> {
        self.seek(0).unwrap()
    }

    /// Returns the value of the named attribute, or `None` if absent or deleted.
    pub fn attr(&self, name: &str) -> Option<Out> {
        let item = self.ptr.map.get(name).copied()?;
        if item.is_deleted() {
            None
        } else {
            item.content.get_last()
        }
    }

    /// Returns whether the named attribute exists and is not deleted.
    pub fn contains_attr(&self, name: &str) -> bool {
        match self.ptr.map.get(name) {
            Some(item) => !item.is_deleted(),
            None => false,
        }
    }

    /// Returns all attribute key-value pairs.
    pub fn attrs(&self) -> impl Iterator<Item = (&str, Out)> + '_ {
        self.ptr.map.iter().filter_map(|(name, item)| {
            if item.is_deleted() {
                None
            } else if let Some(value) = item.content.get_last() {
                Some((name.as_ref(), value))
            } else {
                None
            }
        })
    }

    /// Iterator over non-deleted attribute keys.
    pub fn attr_keys(&self) -> impl Iterator<Item = &str> + '_ {
        self.ptr.map.iter().filter_map(|(name, item)| {
            if item.is_deleted() {
                None
            } else {
                Some(name.as_ref())
            }
        })
    }

    /// Iterator over non-deleted attribute values.
    pub fn attr_values(&self) -> impl Iterator<Item = Out> + '_ {
        self.ptr.map.values().filter_map(|item| {
            if item.is_deleted() {
                None
            } else {
                item.content.get_last()
            }
        })
    }

    /// Converts this type to a JSON-compatible representation.
    pub fn to_json(&self) -> Any {
        todo!()
    }

    /// Returns a delta representation of this type's contents.
    pub fn delta(&self, options: &DeltaOptions) -> Delta {
        let items = match (&options.items_to_render, &options.deleted_items) {
            (None, None) => None,
            (Some(inserted), None) => Some(inserted.clone()),
            (None, Some(deleted)) => Some(deleted.clone()),
            (Some(inserted), Some(deleted)) => {
                // either inserted or deleted but not both at the same time
                let mut items = inserted.diff(deleted);
                items.merge_with(deleted.diff(inserted));
                Some(items)
            }
        };
        let modified = if let Some(items) = &items
            && options.deep
        {
            Some(self.compute_modified(items))
        } else {
            None
        };
        let render = NodeFormatter {
            encoding: self.txn.doc.options.offset_kind,
            items,
            deleted: options.deleted_items.as_ref(),
            modified,
            retain_inserts: options.retain_inserts,
            deep: options.deep,
            link_depth: options.link_depth,
        };
        render.node(self.ptr)
    }

    /// Maps every node touched by `items` onto a set of
    /// keys changed within it (a `None` key marks a change made to node's children).
    fn compute_modified(&self, items: &IdSet) -> Modified {
        let mut modified = Modified::new();
        let mut blocks = items.blocks();
        while let Some(block) = blocks.next(&*self.txn) {
            if let BlockSlice::Item(slice) = block {
                let mut current = Some(slice.ptr);
                while let Some(item) = current {
                    let parent = item.parent.as_node().unwrap();
                    let keys = modified.entry(*parent).or_default();
                    if !keys.insert(item.parent_sub.clone()) {
                        break; // has already been marked as modified
                    }
                    current = parent.item;
                }
            }
        }
        modified
    }
}

impl<T, D> Display for NodeRef<T>
where
    T: Deref<Target = Transaction<D>>,
    D: Deref<Target = Doc>,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let mut curr = self.ptr.start;
        while let Some(ptr) = curr {
            if !ptr.is_deleted() {
                match &ptr.content {
                    ItemContent::String(str) => {
                        write!(f, "{}", str)?;
                    }
                    _ => { /* do nothing */ }
                }
            }
            curr = ptr.right;
        }
        Ok(())
    }
}

// write operations
impl<'doc, T> NodeRef<T>
where
    T: DerefMut<Target = Transaction<&'doc mut Doc>>,
{
    pub fn txn_mut(&mut self) -> &mut Transaction<&'doc mut Doc> {
        &mut *self.txn
    }

    /// Inserts a single value at the given index.
    ///
    /// # Panics
    ///
    /// Panics if `index` is greater than the current length of this type's children.
    pub fn insert(&mut self, index: u32, value: impl Into<In>) -> Out {
        let mut cursor = self.seek(index).unwrap();
        let item = cursor.insert(&mut self.txn, value.into(), None).unwrap();
        item.content.get_last().unwrap()
    }

    /// Inserts multiple values one after another, starting at the given index.
    ///
    /// # Panics
    ///
    /// Panics if `index` is greater than the current length of this type's children.
    pub fn insert_range(&mut self, index: u32, values: impl IntoIterator<Item = impl Into<In>>) {
        let delta = Delta::new()
            .retain(index)
            .insert_range(values.into_iter().map(Into::into));
        self.apply_delta(delta)
    }

    /// Inserts a string of text at the given index.
    pub fn insert_text(&mut self, index: u32, text: &str) {
        self.apply_delta(Delta::new().retain(index).insert_text(text));
    }

    /// Inserts a string of text at the given index with formatting attributes.
    pub fn insert_text_fmt(&mut self, index: u32, text: &str, attrs: &Attrs) {
        self.apply_delta(Delta::new().retain(index).insert_text_fmt(text, &attrs))
    }

    /// Applies formatting attributes to a range starting at `index` for `len` elements.
    pub fn format(&mut self, index: u32, len: u32, attrs: &Attrs) {
        self.apply_delta(Delta::new().retain(index).retain_fmt(len, attrs))
    }

    /// Appends a string of text to the end of this type's children.
    pub fn push_text(&mut self, text: &str) {
        let index = self.ptr.content_len;
        self.insert_text(index, text);
    }

    /// Prepends content to the beginning of this type's children.
    #[inline]
    pub fn push_front(&mut self, content: impl Into<In>) -> Out {
        self.insert(0, content)
    }

    /// Appends content to the end of this type's children.
    #[inline]
    pub fn push_back(&mut self, content: impl Into<In>) -> Out {
        self.insert(self.ptr.content_len, content)
    }

    /// Removes `len` elements starting at `index`.
    pub fn remove(&mut self, index: u32, len: u32) {
        if let Some(mut cursor) = Cursor::at(self.ptr, self.txn.doc.offset_kind(), index) {
            cursor.remove(&mut self.txn, len);
        }
    }

    /// Removes all element's matching the given `predicate`.
    /// Returns a number of elements removed.
    pub fn remove_filter<F>(&mut self, mut predicate: F) -> u32
    where
        F: FnMut(Out) -> bool,
    {
        let mut removed = 0;
        let mut index = 0;
        while index < self.ptr.content_len {
            if self.get(index).is_some_and(&mut predicate) {
                self.remove(index, 1);
                removed += 1;
                // elements after `index` shifted left, so don't advance
            } else {
                index += 1;
            }
        }
        removed
    }

    /// Inserts or updates an attribute value.
    pub fn insert_attr(&mut self, name: impl Into<Arc<str>>, value: impl Into<In>) -> Out {
        let item = self
            .txn
            .create_map_item(self.ptr, name.into(), value.into())
            .unwrap();
        item.content.get_last().unwrap()
    }

    /// Removes an attribute by name.
    pub fn remove_attr(&mut self, name: &str) -> Option<Out> {
        let ptr = self.ptr;
        ptr.remove(self.txn_mut(), name)
    }

    /// Removes all attributes.
    pub fn clear_attrs(&mut self) {
        for &item in self.ptr.map.values() {
            self.txn.delete(item);
        }
    }

    /// Applies a sequence of delta operations to this type.
    pub fn apply_delta(&mut self, delta: Delta<In>) {
        if self.ptr.item.map(|item| item.is_deleted()).unwrap_or(false) {
            return; // current node is already deleted
        }

        if !delta.children.is_empty() {
            self.apply_ops(delta.children);
        }

        if !delta.attrs.is_empty() {
            self.apply_attr_ops(delta.attrs)
        }
    }

    fn apply_ops(&mut self, ops: Vec<Op<In>>) {
        let mut cursor = Cursor::new(self.ptr, self.txn.doc.offset_kind());
        for op in ops {
            match op {
                Op::Retain { len, format: None } => {
                    let n = cursor.forward(len);
                    assert_eq!(n, len)
                }
                Op::Retain {
                    len,
                    format: Some(attrs),
                } => {
                    let n = cursor.format(&mut self.txn, len, &attrs);
                    assert_eq!(n, len)
                }
                Op::InsertText { text, format } => {
                    cursor.insert_text(&mut self.txn, &text, format.as_deref());
                }
                Op::Insert { items, format } => {
                    cursor.insert_range(&mut self.txn, items, format.as_deref());
                }
                Op::Remove { len, .. } => {
                    let n = cursor.remove(&mut self.txn, len);
                    assert_eq!(n, len)
                }
                Op::Modify { delta, format } => {
                    cursor.skip_hidden();
                    let item = cursor.right().unwrap();
                    //assert_matches!(item.content, ItemContent::Node { .. });
                    if item.is_deleted() { todo!() } else { todo!() }
                }
            }
        }
    }

    fn apply_attr_ops(&mut self, attrs: HashMap<Arc<str>, AttrOp<In>>) {
        for (key, op) in attrs {
            match op {
                AttrOp::Update { value, .. } => {
                    self.txn.create_map_item(self.ptr, key, value).unwrap();
                }
                AttrOp::Remove { .. } => {
                    self.ptr.remove(&mut self.txn, &key);
                }
                AttrOp::Modify { delta } => match self.ptr.map.get(&key).copied() {
                    Some(item) => {
                        let node = item.as_node().unwrap();
                        NodeRef::new(node, self.txn.deref_mut()).apply_delta(*delta)
                    }
                    None => {
                        self.txn
                            .create_map_item(self.ptr, key, In::Node(delta))
                            .unwrap();
                    }
                },
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct DeltaOptions {
    /// If `true`, retain formatter inserts with attributions.
    pub retain_inserts: bool,
    /// If `true`, retain formatter+attributed deletes only. Yrs has no attribution renderer yet,
    /// therefore deletes are never attributed and this option has no effect (see: [NodeRef::delta]).
    pub retain_deletes: bool,
    /// Render child types as delta.
    pub deep: bool,
    /// When weak links are used, how deep should we follow them.
    /// This is also to prevent infinite recursion from happening.
    /// Default: 0 (don't jump through links).
    pub link_depth: u32,
    pub items_to_render: Option<IdSet>,
    /// Used for computing `prev` in attributes.
    pub deleted_items: Option<IdSet>,
}

impl Default for DeltaOptions {
    fn default() -> Self {
        DeltaOptions {
            retain_inserts: false,
            retain_deletes: false,
            link_depth: 0,
            deep: false,
            items_to_render: None,
            deleted_items: None,
        }
    }
}

/// Nodes that should be formatter as modified children, mapped onto the keys changed within them.
/// A `None` key marks a change made to node's children.
type Modified = HashMap<NodePtr, HashSet<Option<Arc<str>>>>;

/// Renders a [Node] as a [Delta] - a port of Yjs `YType.toDelta`.
struct NodeFormatter<'a> {
    encoding: OffsetKind,
    /// If `None` renders the full node state.
    /// Otherwise, will render only changes marked on [IdSet].
    items: Option<IdSet>,
    /// Ids deleted by the change being formatter. Used to resolve previous values of attributes.
    deleted: Option<&'a IdSet>,
    modified: Option<Modified>,
    /// If `true`, formatter inserts are retained instead of being formatter as inserts.
    retain_inserts: bool,
    /// Render child nodes as delta.
    deep: bool,
    link_depth: u32,
}

impl<'a> NodeFormatter<'a> {
    fn node(&self, node: NodePtr) -> Delta {
        let attrs_to_render = self.modified.as_ref().and_then(|m| m.get(&node));
        let render_children =
            self.modified.is_none() || attrs_to_render.map_or(true, |keys| keys.contains(&None));
        let mut delta = match &node.type_ref {
            TypeRef::XmlElement(name) => Delta::with_name(name.clone()),
            #[cfg(feature = "weak")]
            TypeRef::WeakLink(source) if self.link_depth == 0 => Delta {
                link: Some(source.clone()),
                ..Delta::default()
            },
            _ => Delta::default(),
        };
        self.attrs(&mut delta, node, attrs_to_render);
        if render_children {
            self.children(&mut delta, node);
        }
        delta
    }

    /// Port of Yjs `typeMapGetDelta`. When `keys` are given, only these attributes are formatter.
    fn attrs(&self, d: &mut Delta, node: NodePtr, keys: Option<&HashSet<Option<Arc<str>>>>) {
        match keys {
            None => {
                for (key, item) in node.map.iter() {
                    self.attr(d, key, *item);
                }
            }
            Some(keys) => {
                for key in keys.iter().flatten() {
                    if let Some(item) = node.map.get(key) {
                        self.attr(d, key, *item);
                    }
                }
            }
        }
    }

    fn attr(&self, d: &mut Delta, key: &Arc<str>, item: ItemPtr) {
        let child = item.content.as_node();
        let op = if item.is_deleted() {
            // Hard-deleted attribute within a change render: emit the `deleteAttr` op so consumers (the
            // `YEvent` delta, RDT bindings, the maintained `delta` cache) can apply the removal. In
            // full-state mode (`itemsToRender == null`) the attribute is simply omitted (above renders
            // run with `render === false` for such items, so nothing was emitted before either).
            if let Some(items) = &self.items
                && items.contains(&item.last_id())
            {
                AttrOp::remove(item.content.get_last())
            } else {
                return;
            }
        } else if let Some(child) = child.filter(|child| self.is_modified(*child)) {
            AttrOp::modify(self.node(child))
        } else {
            let value = match child {
                Some(child) => self.node_value(child),
                None => match item.content.get_last() {
                    Some(value) => value,
                    None => return,
                },
            };
            AttrOp::update(value, self.prev_attr(item))
        };
        d.attrs.insert(key.clone(), op);
    }

    fn prev_attr(&self, item: ItemPtr) -> Option<Out> {
        let deleted = self.deleted?;
        let left = item.left?;
        if deleted.contains(&left.last_id()) {
            left.content.get_last()
        } else {
            None
        }
    }

    fn node_value(&self, node: NodePtr) -> Out {
        if !self.deep {
            return Out::node(node.id());
        }
        let delta = self.node(node);
        if delta == Delta::default() {
            Out::node(node.id()) // there's nothing to render within a child node
        } else {
            Out::node_with_delta(node.id(), delta)
        }
    }

    fn is_modified(&self, node: NodePtr) -> bool {
        match &self.modified {
            Some(modified) => modified.contains_key(&node),
            None => false,
        }
    }

    fn children(&self, d: &mut Delta, node: NodePtr) {
        let mut current_formats = Attrs::new(); // saves all current formats for insert
        let mut changed_formats = Attrs::new(); // saves changed formats for retain
        let mut previous_formats = Attrs::new(); // the value before changes
        let mut curr = node.start;
        while let Some(item) = curr {
            curr = item.right;
            let content = &item.content;
            if let ItemContent::Format(key, value) = content {
                // format markers always flow through the shared state machine
                self.format(
                    item,
                    key,
                    value,
                    &mut current_formats,
                    &mut changed_formats,
                    &mut previous_formats,
                );
            } else if item.is_deleted() {
                // plain deleted content is invisible; in a change render, the ranges deleted by
                // this change emit `delete` ops - position-only, the content itself is not needed
                if let Some(items) = &self.items {
                    for (range, exists) in slice(items, &item.id, item.len()) {
                        if exists {
                            let count = match content {
                                ItemContent::String(_) | ItemContent::Deleted(_) => {
                                    range.end - range.start
                                }
                                _ => 1,
                            };
                            d.remove_with(count);
                        }
                    }
                }
            } else if let Some(items) = &self.items {
                // change render on alive plain content: inserted ranges become inserts, everything
                // else retains (position-only). `retainInserts` (attribution-heal renders) retains
                // previously inserted content while clearing its former attribution.
                let modified = content.as_node().filter(|child| self.is_modified(*child));
                for (range, exists) in slice(items, &item.id, item.len()) {
                    if !exists {
                        if let Some(child) = modified {
                            d.modify_with(self.node(child));
                        } else {
                            // mirror the piece-wise op sizes of the delete branch
                            let count = match content {
                                ItemContent::String(_) | ItemContent::Deleted(_) => {
                                    range.end - range.start
                                }
                                _ => 1,
                            };
                            d.retain_fmt_with(count, &changed_formats);
                        }
                    } else if self.retain_inserts {
                        if let Some(child) = modified {
                            d.modify_with(self.node(child));
                        } else {
                            d.retain_fmt_with(range.end - range.start, &changed_formats);
                        }
                    } else {
                        let offset = range.start - item.id.clock;
                        let len = range.end - range.start;
                        self.push_content(d, content, offset, len, &current_formats);
                    }
                }
            } else if self.retain_inserts {
                // attribution-overlay render (e.g. `toDelta({ renderer, retainInserts: true })`):
                // existing content is retained, clearing any formerly cached own-attribution
                d.retain_fmt_with(content.len(self.encoding), &changed_formats);
            } else {
                // full render: a plain insert of the whole item
                self.push_content(d, content, 0, item.len(), &current_formats);
            }
        }
    }

    /// Emits insert ops for a `len` elements long slice of `content`, starting at `offset`.
    fn push_content(
        &self,
        d: &mut Delta,
        content: &ItemContent,
        offset: u32,
        len: u32,
        format: &Attrs,
    ) {
        match content {
            ItemContent::Node(node) => {
                d.insert_fmt_with(self.node_value(NodePtr::from(node)), format)
            }
            ItemContent::String(str) => {
                // clock offsets are always UTF-16 based
                let (_, str) = split_str(str, offset as usize, OffsetKind::Utf16);
                let (str, _) = split_str(str, len as usize, OffsetKind::Utf16);
                d.insert_text_fmt_with(str, format);
            }
            _ => {
                let mut buf = vec![Out::default(); len as usize];
                let read = content.read(offset as usize, &mut buf);
                d.insert_range_fmt_with(buf.into_iter().take(read), format)
            }
        }
    }

    /// Format state machine: everything that comes after a format marker is formatted by it.
    fn format(
        &self,
        item: ItemPtr,
        key: &Arc<str>,
        value: &Any,
        current: &mut Attrs,
        changed: &mut Attrs,
        previous: &mut Attrs,
    ) {
        let deleted = item.is_deleted();
        let render = match &self.items {
            None => !deleted,                        // full render
            Some(items) => items.contains(&item.id), // change render
        };
        if render {
            if deleted {
                // `previousFormats` tracks the value governing the current walk position in the
                // *consuming cache state*, so the retain diff below stays a plain cache → new-state
                // comparison. Contributors: markers deleted by this change in event renders (their
                // value governed the cache until now; heal-rendered deletes are PRE-deleted markers
                // whose removal the cache already rendered — they must not contribute), alive
                // markers re-rendered by heal renders (`retainInserts` — their value was in the
                // cache), and retained alive markers (the branch below). A gated update here is not
                // enough: a marker deleted by the change whose value coincides with the walk's
                // current value would silently drop out of the cache-reference tracking, and a later
                // marker would be misread as a restore-to-previous, swallowing a needed format diff.
                if self.items.is_some() && !self.retain_inserts {
                    previous.insert(key.clone(), value.clone());
                }
            } else {
                if self.retain_inserts {
                    previous.insert(key.clone(), value.clone());
                }
                update_format(current, key, value);
            }
            // the retain diff for following spans is exactly cache (previousFormats) → new
            // state (currentFormats), recomputed per key at every rendered marker
            if current.get(key).unwrap_or(&Any::Null) == previous.get(key).unwrap_or(&Any::Null) {
                changed.remove(key);
            } else {
                let value = current.get(key).cloned().unwrap_or(Any::Null);
                changed.insert(key.clone(), value);
            }
        } else if !deleted {
            // an alive marker retained by this change: it doesn't change the formatting of the
            // content that follows it, it only describes it
            update_format(current, key, value);
            changed.remove(key);
            previous.insert(key.clone(), value.clone());
        }
    }
}

/// Splits an id range of `[clock..clock+len)` onto slices, marking which of them exist in a given
/// `set` - a port of Yjs `IdSet.slice`.
fn slice(set: &IdSet, id: &ID, len: u32) -> SmallVec<[(Range<u32>, bool); 1]> {
    let mut res = SmallVec::new();
    let end = id.clock + len;
    if let Some(ranges) = set.get(&id.client)
        && let Some(index) = ranges.find_start(id.clock)
    {
        let mut prev_end = id.clock;
        for (range, _) in &ranges.as_slice()[index..] {
            if range.start >= end {
                break;
            }
            let start = range.start.max(id.clock);
            let stop = range.end.min(end);
            if prev_end < start {
                res.push((prev_end..start, false));
            }
            res.push((start..stop, true));
            prev_end = stop;
        }
        if !res.is_empty() && prev_end < end {
            res.push((prev_end..end, false));
        }
    }
    if res.is_empty() {
        res.push((id.clock..end, false));
    }
    res
}

/// Sets a format `value` under a given `key`, where a `null` value removes the format.
fn update_format(formats: &mut Attrs, key: &Arc<str>, value: &Any) {
    if let Any::Null = value {
        formats.remove(key);
    } else {
        formats.insert(key.clone(), value.clone());
    }
}
