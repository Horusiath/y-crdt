use crate::block::{ID, Item, ItemContent, ItemPosition, ItemPtr};
use crate::delta::{AttrOp, Op};
use crate::event::Event;
use crate::node::{Attrs, DeepObservable, Node, NodePtr, Observable, TypePtr};
use crate::transaction::TransactionMut;
use crate::{Any, Delta, Doc, IdSet, In, NodeID, OffsetKind, Out, Transaction};
use std::collections::{Bound, HashMap};
use std::fmt::{Display, Formatter};
use std::ops::{Deref, DerefMut, RangeBounds};
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
impl<T> Observable for NodeRef<T> {
    type Event = Event;
}

impl<T> NodeRef<T> {
    pub(crate) fn new(ptr: NodePtr, txn: T) -> Self {
        NodeRef { ptr, txn }
    }

    pub fn id(&self) -> NodeID {
        self.ptr.id()
    }
}

// read operations
impl<'txn, T, D> NodeRef<T>
where
    T: Deref<Target = Transaction<D>>,
    D: Deref<Target = Doc>,
{
    fn seek(&self, index: u32) -> Cursor {
        //TODO: search markers / skip list indexing
        let mut cursor = Cursor::new(self.ptr.start, self.txn.doc.options.offset_kind);
        cursor.forward(index);
        cursor
    }

    pub fn txn(&self) -> &'txn Transaction<D> {
        self.txn
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
        let mut cursor = self.seek(index);
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

        let mut cursor = self.seek(start);
        while remaining != 0
            && let Some(value) = cursor.next()
        {
            remaining -= 1;
            res.push(value);
        }
        res
    }

    /// Iterator over children values.
    pub fn iter(&self) -> impl Iterator<Item = Out> + '_ {
        self.seek(0)
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
    pub fn to_delta(&self, options: &DeltaOptions) -> Vec<Delta> {
        todo!()
    }
}

impl<'txn, T, D> Display for NodeRef<T>
where
    T: Deref<Target = Transaction<D>>,
    D: Deref<Target = Doc>,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        todo!()
    }
}

// write operations
impl<'txn, 'doc, T> NodeRef<T>
where
    T: DerefMut<Target = Transaction<&'doc mut Doc>>,
{
    pub fn txn_mut(&mut self) -> &mut Transaction<&mut Doc> {
        &mut *self.txn
    }

    /// Inserts a single value at the given index into the list (sequence) component of this type.
    ///
    /// Returns the integrated value or `None` if the inserted content was empty.
    fn insert_item(&mut self, index: u32, content: In) -> Option<ItemPtr> {
        let this = self.ptr;
        if let Some(mut pos) = find_position(this, &mut *self.txn, index) {
            // skip over deleted blocks, just like Yjs does
            while let Some(right) = pos.right.as_ref() {
                if right.is_deleted() {
                    pos.forward();
                } else {
                    break;
                }
            }
            self.txn.create_item(&pos, content, None)
        } else {
            panic!("The type or the position doesn't exist!");
        }
    }

    /// Inserts a single value at the given index.
    pub fn insert(&mut self, index: u32, content: impl Into<In>) {
        self.insert_item(index, content.into());
    }

    /// Inserts a string of text at the given index.
    pub fn insert_text(&mut self, index: u32, text: &str) {
        if text.is_empty() {
            return;
        }
        self.insert_item(index, In::Any(Any::String(text.into())));
    }

    /// Inserts a string of text at the given index with formatting attributes.
    pub fn insert_text_with(&mut self, index: u32, text: &str, attrs: Attrs) {
        if text.is_empty() {
            return;
        }
        let this = self.ptr;
        if let Some(mut pos) = find_position(this, &mut *self.txn, index) {
            let value = In::Any(Any::String(text.into()));
            insert(this, &mut *self.txn, &mut pos, value, attrs);
        } else {
            panic!("The type or the position doesn't exist!");
        }
    }

    /// Applies formatting attributes to a range starting at `index` for `len` elements.
    pub fn format(&mut self, index: u32, len: u32, attrs: Attrs) {
        let this = self.ptr;
        if let Some(mut pos) = find_position(this, &mut *self.txn, index) {
            insert_format(this, &mut *self.txn, &mut pos, len, attrs);
        } else {
            panic!("Index {} is outside of the range.", index);
        }
    }

    /// Appends a string of text to the end of this type's children.
    pub fn push_text(&mut self, text: &str) {
        let index = self.ptr.content_len;
        self.insert_text(index, text);
    }

    /// Prepends content to the beginning of this type's children.
    pub fn push_front(&mut self, content: impl Into<In>) -> Out {
        let ptr = self
            .insert_item(0, content.into())
            .expect("cannot insert empty value");
        ptr.content.get_last().unwrap()
    }

    /// Appends content to the end of this type's children.
    pub fn push_back(&mut self, content: impl Into<In>) -> Out {
        let index = self.ptr.content_len;
        let ptr = self
            .insert_item(index, content.into())
            .expect("cannot insert empty value");
        ptr.content.get_last().unwrap()
    }

    /// Removes `len` elements starting at `index`.
    pub fn remove(&mut self, index: u32, len: u32) {
        let this = self.ptr;
        if let Some(mut pos) = find_position(this, &mut *self.txn, index) {
            remove(&mut *self.txn, &mut pos, len)
        } else {
            panic!("The type or the position doesn't exist!");
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
            match self.get(index) {
                Some(value) if predicate(value) => {
                    self.remove(index, 1);
                    removed += 1;
                    // elements after `index` shifted left, so don't advance
                }
                _ => index += 1,
            }
        }
        removed
    }

    /// Inserts or updates an attribute value.
    pub fn insert_attr(&mut self, name: impl Into<Arc<str>>, value: impl Into<In>) -> Out {
        let mut name = name.into();
        let pos = {
            let inner = self.as_ref();
            let left = match inner.map.get_key_value(&name) {
                None => None,
                Some((existing, left)) => {
                    name = existing.clone(); // reuse existing string instead of keeping new one allocated
                    Some(*left)
                }
            };
            ItemPosition {
                parent: NodePtr::from(inner).into(),
                left,
                right: None,
                index: 0,
                current_attrs: None,
            }
        };

        let ptr = self.txn.create_item(&pos, value, Some(name)).unwrap();
        ptr.content.get_last().unwrap()
    }

    /// Removes an attribute by name.
    pub fn remove_attr(&mut self, name: &str) {
        if let Some(&item) = self.ptr.map.get(name) {
            self.txn.delete(item);
        }
    }

    /// Removes all attributes.
    pub fn clear_attrs(&mut self) {
        for &item in self.ptr.map.values() {
            self.txn.delete(item);
        }
    }

    /// Applies a sequence of delta operations to this type.
    pub fn apply_delta(&mut self, deltas: impl IntoIterator<Item = Delta<In>>) {
        if self.ptr.item.map(|item| item.is_deleted()).unwrap_or(false) {
            return; // current node is already deleted
        }

        for delta in deltas {
            if !delta.children.is_empty() {
                let mut cursor = Cursor::new(self.ptr.start, self.txn.doc.options.offset_kind);
                for op in delta.children {
                    match op {
                        Op::InsertText { text, format } => {}
                        Op::Insert { items, format } => {}
                        Op::Remove { len, prev } => {}
                        Op::Retain { len, format } => {}
                        Op::Modify { delta, format } => {}
                    }
                }
            }

            for (key, op) in delta.attrs {
                match op {
                    AttrOp::Update { value, prev } => {}
                    AttrOp::Remove { prev } => {}
                    AttrOp::Modify { delta } => {}
                }
            }
        }
    }
}

pub struct Cursor {
    curr: Option<ItemPtr>,
    index: u32,
    offset: u32,
    encoding: OffsetKind,
}

impl Cursor {
    fn new(start: Option<ItemPtr>, encoding: OffsetKind) -> Self {
        Cursor {
            curr: start,
            index: 0,
            offset: 0,
            encoding,
        }
    }

    fn forward(&mut self, by: u32) -> u32 {
        let mut remaining = by + self.offset;
        self.offset = 0;
        while remaining != 0
            && let Some(item) = self.curr
        {
            if !item.is_deleted() && item.is_countable() {
                let len = item.content_len(self.encoding);
                if remaining < len {
                    self.offset = remaining;
                    return by;
                }
                remaining -= len;
                self.index += len;
                self.curr = item.right;
            }
        }
        by - remaining
    }

    fn get(&self) -> Option<Out> {
        let item = self.curr?;
        match &item.content {
            ItemContent::Any(values) => values.get(self.offset as usize).map(Out::from),
            ItemContent::String(slice) => {
                Some(Out::Any(slice[self.offset as usize].to_string().into()))
            }
            ItemContent::JSON(values) => values.get(self.offset as usize).map(Out::from),
            ItemContent::Binary(bin) if self.offset == 0 => Some(Out::Any(Any::Buffer(bin.into()))),
            ItemContent::Doc(_, options) if self.offset == 0 => Some(Out::Doc(options.guid)),
            ItemContent::Embed(value) if self.offset == 0 => Some(Out::Any(value.clone())),
            ItemContent::Node(node) if self.offset == 0 => Some(Out::Node(node.id())),
            _ => None,
        }
    }
}

impl Iterator for Cursor {
    type Item = Out;

    fn next(&mut self) -> Option<Self::Item> {
        let value = self.get()?;
        // try to move forward by 1
        self.index += 1;
        while let Some(item) = self.curr {
            if !item.is_deleted() && item.is_countable() {
                let len = item.content_len(self.encoding);
                if self.offset < len - 1 {
                    // we're still inside current
                    self.offset += 1;
                    break;
                }
            }
            self.offset = 0;
            self.curr = item.right;
        }
        Some(value)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        if let Some(curr) = self.curr {
            let node = curr.parent.as_node().unwrap();
            let len = (node.content_len - self.index) as usize;
            (len, Some(len))
        } else {
            (0, Some(0))
        }
    }
}

/// Locates an [ItemPosition] within the sequence component of a `this` node at a given content
/// `index`, splitting blocks if necessary so that the position falls exactly on a block boundary.
///
/// Ported from the former `TextRef`/`ArrayRef` internals as part of the [NodeRef] unification.
fn find_position(this: NodePtr, txn: &mut TransactionMut, index: u32) -> Option<ItemPosition> {
    let mut pos = ItemPosition {
        parent: this.into(),
        left: None,
        right: this.start,
        index: 0,
        current_attrs: None,
    };

    let mut format_ptrs = HashMap::new();
    let store = &mut *txn.doc;
    let encoding = store.options.offset_kind;
    let mut remaining = index;
    while let Some(right) = pos.right {
        if remaining == 0 {
            break;
        }

        if !right.is_deleted() {
            match &right.content {
                ItemContent::Format(key, value) => {
                    if let Any::Null = value.as_ref() {
                        format_ptrs.remove(key);
                    } else {
                        format_ptrs.insert(key.clone(), pos.right.clone());
                    }
                }
                _ => {
                    let mut block_len = right.len();
                    let content_len = right.content_len(encoding);
                    if remaining < content_len {
                        // split right item
                        let offset = if let ItemContent::String(str) = &right.content {
                            str.block_offset(remaining, encoding)
                        } else {
                            remaining
                        };
                        store
                            .blocks
                            .split_block(right, offset, OffsetKind::Utf16)
                            .unwrap();
                        block_len -= offset;
                        remaining = 0;
                    } else {
                        remaining -= content_len;
                    }
                    pos.index += block_len;
                }
            }
        }
        pos.left = pos.right.take();
        pos.right = if let Some(item) = pos.left.as_deref() {
            item.right
        } else {
            None
        };
    }

    for (_, block_ptr) in format_ptrs {
        if let Some(item) = block_ptr {
            if let ItemContent::Format(key, value) = &item.content {
                let attrs = pos.current_attrs.get_or_init();
                update_current_attributes(attrs, key, value.as_ref());
            }
        }
    }

    Some(pos)
}

/// Inserts a `value` at a given `pos` wrapping it with formatting blocks described by `attributes`.
fn insert(
    branch: NodePtr,
    txn: &mut TransactionMut,
    pos: &mut ItemPosition,
    value: In,
    mut attributes: Attrs,
) -> Option<ItemPtr> {
    pos.unset_missing(&mut attributes);
    minimize_attr_changes(pos, &attributes);
    let negated_attrs = insert_attributes(branch, txn, pos, attributes);

    let item = if let Some(item) = txn.create_item(&pos, value, None) {
        pos.right = Some(item);
        pos.forward();
        Some(item)
    } else {
        None
    };

    insert_negated_attributes(branch, txn, pos, negated_attrs);
    item
}

fn update_current_attributes(attrs: &mut Attrs, key: &str, value: &Any) {
    if let Any::Null = value {
        attrs.remove(key);
    } else {
        attrs.insert(key.into(), value.clone());
    }
}

fn remove(txn: &mut TransactionMut, pos: &mut ItemPosition, len: u32) {
    let encoding = txn.doc().options.offset_kind;
    let mut remaining = len;
    let start = pos.right.clone();
    let start_attrs = pos.current_attrs.clone();
    while let Some(item) = pos.right.as_deref() {
        if remaining == 0 {
            break;
        }

        if !item.is_deleted() {
            match &item.content {
                ItemContent::Embed(_) | ItemContent::String(_) | ItemContent::Node(_) => {
                    let content_len = item.content_len(encoding);
                    let ptr = pos.right.unwrap();
                    if remaining < content_len {
                        // split block
                        let offset = if let ItemContent::String(s) = &item.content {
                            s.block_offset(remaining, encoding)
                        } else {
                            len
                        };
                        remaining = 0;
                        txn.doc.blocks.split_block(ptr, offset, OffsetKind::Utf16);
                    } else {
                        remaining -= content_len;
                    };
                    txn.delete(ptr);
                }
                _ => {}
            }
        }

        pos.forward();
    }

    if remaining > 0 {
        panic!(
            "Couldn't remove {} elements from an array. Only {} of them were successfully removed.",
            len,
            len - remaining
        );
    }

    if let (Some(start), Some(start_attrs), Some(end_attrs)) =
        (start, start_attrs, pos.current_attrs.as_mut())
    {
        clean_format_gap(
            txn,
            Some(start),
            pos.right,
            start_attrs.as_ref(),
            end_attrs.as_mut(),
        );
    }
}

fn is_valid_target(item: ItemPtr) -> bool {
    if item.is_deleted() {
        true
    } else if let ItemContent::Format(_, _) = &item.content {
        true
    } else {
        false
    }
}

fn insert_format(
    this: NodePtr,
    txn: &mut TransactionMut,
    pos: &mut ItemPosition,
    mut len: u32,
    attrs: Attrs,
) {
    minimize_attr_changes(pos, &attrs);
    let mut negated_attrs = insert_attributes(this, txn, pos, attrs.clone()); //TODO: remove `attrs.clone()`
    let encoding = txn.doc().options.offset_kind;
    // iterate until first non-format or null is found
    // delete all formats with attributes[format.key] != null
    // also check the attributes after the first non-format as we do not want to insert redundant
    // negated attributes there
    while let Some(right) = pos.right {
        if !(len > 0 || (!negated_attrs.is_empty() && is_valid_target(right))) {
            break;
        }

        if !right.is_deleted() {
            match &right.content {
                ItemContent::Format(key, value) => {
                    if let Some(v) = attrs.get(key) {
                        if v == value.as_ref() {
                            negated_attrs.remove(key);
                        } else {
                            negated_attrs.insert(key.clone(), *value.clone());
                        }
                        txn.delete(right);
                    }
                }
                ItemContent::String(s) => {
                    let content_len = right.content_len(encoding);
                    if len < content_len {
                        // split block
                        let offset = s.block_offset(len, encoding);
                        let new_right =
                            txn.doc.blocks.split_block(right, offset, OffsetKind::Utf16);
                        pos.left = Some(right);
                        pos.right = new_right;
                        break;
                    }
                    len -= content_len;
                }
                _ => {
                    let content_len = right.len();
                    if len < content_len {
                        let new_right = txn.doc.blocks.split_block(right, len, OffsetKind::Utf16);
                        pos.left = Some(right);
                        pos.right = new_right;
                        break;
                    }
                    len -= content_len;
                }
            }
        }

        if !pos.forward() {
            break;
        }
    }

    insert_negated_attributes(this, txn, pos, negated_attrs);
}

fn minimize_attr_changes(pos: &mut ItemPosition, attrs: &Attrs) {
    // go right while attrs[right.key] === right.value (or right is deleted)
    while let Some(i) = pos.right.as_deref() {
        if !i.is_deleted() {
            if let ItemContent::Format(k, v) = &i.content {
                if let Some(v2) = attrs.get(k) {
                    if (v.as_ref()).eq(v2) {
                        pos.forward();
                        continue;
                    }
                }
            }

            break;
        } else {
            pos.forward();
        }
    }
}

fn insert_attributes(
    this: NodePtr,
    txn: &mut TransactionMut,
    pos: &mut ItemPosition,
    attrs: Attrs,
) -> Attrs {
    let mut negated_attrs = HashMap::with_capacity(attrs.len());
    let mut store = &mut *txn.doc;
    for (k, v) in attrs {
        let current_value = pos
            .current_attrs
            .as_ref()
            .and_then(|a| a.get(&k))
            .unwrap_or(&Any::Null);
        if &v != current_value {
            // save negated attribute (set null if currentVal undefined)
            negated_attrs.insert(k.clone(), current_value.clone());

            let client_id = store.options.client_id;
            let parent = TypePtr::Node(this);
            let item = Item::new(
                ID::new(client_id, store.blocks.get_clock(&client_id)),
                pos.left.clone(),
                pos.left.map(|ptr| ptr.last_id()),
                pos.right.clone(),
                pos.right.map(|ptr| ptr.id().clone()),
                parent,
                None,
                ItemContent::Format(k, v.into()),
            )
            .unwrap();
            let item_ptr = txn.integrate_item(item, 0);
            pos.right = item_ptr;
            pos.forward();
            store = &mut *txn.doc;
        }
    }
    negated_attrs
}

fn insert_negated_attributes(
    this: NodePtr,
    txn: &mut TransactionMut,
    pos: &mut ItemPosition,
    mut attrs: Attrs,
) {
    while let Some(item) = pos.right.as_deref() {
        if !item.is_deleted() {
            if let ItemContent::Format(key, value) = &item.content {
                if let Some(curr_val) = attrs.get(key) {
                    if curr_val == value.as_ref() {
                        attrs.remove(key);
                        pos.forward();
                        continue;
                    }
                }
            }

            break;
        } else {
            pos.forward();
        }
    }

    let mut store = &mut *txn.doc;
    for (k, v) in attrs {
        let client_id = store.options.client_id;
        let parent = TypePtr::Node(this);
        let item = Item::new(
            ID::new(client_id, store.blocks.get_clock(&client_id)),
            pos.left.clone(),
            pos.left.map(|ptr| ptr.last_id()),
            pos.right.clone(),
            pos.right.map(|ptr| ptr.id().clone()),
            parent,
            None,
            ItemContent::Format(k, v.into()),
        )
        .unwrap();
        let item_ptr = txn.integrate_item(item, 0);
        pos.right = item_ptr;
        pos.forward();
        store = &mut *txn.doc;
    }
}

fn clean_format_gap(
    txn: &mut TransactionMut,
    mut start: Option<ItemPtr>,
    mut end: Option<ItemPtr>,
    start_attrs: &Attrs,
    end_attrs: &mut Attrs,
) -> u32 {
    while let Some(item) = end.as_deref() {
        match &item.content {
            ItemContent::String(_) | ItemContent::Embed(_) => break,
            ItemContent::Format(key, value) if !item.is_deleted() => {
                update_current_attributes(end_attrs, key.as_ref(), value);
            }
            _ => {}
        }
        end = item.right.clone();
    }

    let mut cleanups = 0;
    while start != end {
        if let Some(item) = start.as_deref() {
            let right = item.right.clone();
            if !item.is_deleted() {
                if let ItemContent::Format(key, value) = &item.content {
                    let e = end_attrs.get(key).unwrap_or(&Any::Null);
                    let s = start_attrs.get(key).unwrap_or(&Any::Null);
                    if e != value.as_ref() || s == value.as_ref() {
                        txn.delete(start.unwrap());
                        cleanups += 1;
                    }
                }
            }
            start = right;
        } else {
            break;
        }
    }
    cleanups
}

#[derive(Debug, Clone)]
pub struct DeltaOptions {
    /// If `true`, retain rendered inserts with attributions.
    pub retain_inserts: bool,
    /// If `true`, retain rendered+attributed deletes only.
    pub retain_deletes: bool,
    /// Render child types as delta.
    pub deep: bool,
    pub items_to_render: Option<IdSet>,
    /// Used for computing `prev` in attributes.
    pub deleted_items: Option<IdSet>,
}

impl Default for DeltaOptions {
    fn default() -> Self {
        todo!()
    }
}
