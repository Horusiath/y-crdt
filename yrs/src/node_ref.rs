use crate::block::{ID, Item, ItemContent, ItemPosition, ItemPtr, split_str};
use crate::delta::{AttrOp, Op};
use crate::id_set::DeleteSet;
use crate::iter::TxnIterator;
use crate::node::{Attrs, DeepObservable, Node, NodePtr, Observable, TypePtr, TypeRef};
use crate::slice::BlockSlice;
use crate::transaction::TransactionMut;
use crate::utils::OptionExt;
use crate::{Any, Delta, Doc, IdSet, In, NodeID, OffsetKind, Out, Transaction};
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
    fn seek(&self, index: u32) -> Cursor {
        //TODO: search markers / skip list indexing
        let mut cursor = Cursor::new(self.ptr.start, self.txn.doc.options.offset_kind);
        cursor.forward(index);
        cursor
    }

    pub fn txn(&self) -> &Transaction<D> {
        &self.txn
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
    pub fn iter(&self) -> Cursor {
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
    pub fn delta(&self, options: &DeltaOptions) -> Delta {
        // Content that was both inserted AND deleted by the formatter change is invisible, so what
        // gets formatter is `(I - D) U (D - I)`: the symmetric difference of both sets.
        let items = match (&options.items_to_render, &options.deleted_items) {
            (None, None) => None,
            (Some(inserted), None) => Some(inserted.clone()),
            (None, Some(deleted)) => Some(deleted.clone()),
            (Some(inserted), Some(deleted)) => {
                let mut items = inserted.diff(deleted);
                items.merge_with(deleted.diff(inserted));
                Some(items)
            }
        };
        let modified = match (options.deep, &items) {
            (true, Some(items)) => Some(self.compute_modified(items)),
            _ => None,
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

    /// Port of Yjs `computeModifiedFromItems`: maps every node touched by `items` onto a set of
    /// keys changed within it (a `None` key marks a change made to node's children).
    fn compute_modified(&self, items: &IdSet) -> Modified {
        let mut modified = Modified::new();
        let mut blocks = items.blocks();
        while let Some(block) = blocks.next(&*self.txn) {
            let BlockSlice::Item(slice) = block else {
                continue;
            };
            let mut current = Some(slice.ptr);
            while let Some(item) = current {
                let Some(parent) = item.parent.as_node() else {
                    break;
                };
                let keys = modified.entry(*parent).or_default();
                if !keys.insert(item.parent_sub.clone()) {
                    break; // has already been marked as modified
                }
                current = parent.item;
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

    /// Returns an insert position at a given index, skipping over deleted blocks (like Yjs does).
    fn insert_position(&mut self, index: u32) -> ItemPosition {
        let this = self.ptr;
        if let Some(mut pos) = find_position(this, &mut *self.txn, index) {
            while let Some(right) = pos.right.as_ref() {
                if right.is_deleted() {
                    pos.forward();
                } else {
                    break;
                }
            }
            pos
        } else {
            panic!("The type or the position doesn't exist!");
        }
    }

    /// Inserts a single value at the given index into the list (sequence) component of this type.
    ///
    /// Returns the integrated value or `None` if the inserted content was empty.
    fn insert_item(&mut self, index: u32, content: In) -> Option<ItemPtr> {
        let pos = self.insert_position(index);
        self.txn.create_item(&pos, content, None)
    }

    /// Inserts a single value at the given index.
    ///
    /// # Panics
    ///
    /// Panics if `index` is greater than the current length of this type's children.
    pub fn insert(&mut self, index: u32, value: impl Into<In>) -> Out {
        let ptr = self
            .insert_item(index, value.into())
            .expect("cannot insert empty value");
        ptr.content.get_last().unwrap()
    }

    /// Inserts multiple values one after another, starting at the given index.
    ///
    /// # Panics
    ///
    /// Panics if `index` is greater than the current length of this type's children.
    pub fn insert_range(&mut self, index: u32, values: impl IntoIterator<Item = impl Into<In>>) {
        let mut pos = self.insert_position(index);
        for value in values {
            if let Some(item) = self.txn.create_item(&pos, value.into(), None) {
                pos.right = Some(item);
                pos.forward(); // move insert position past the newly created item
            }
        }
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

        let ptr = self
            .txn
            .create_item(&pos, value.into(), Some(name))
            .unwrap();
        ptr.content.get_last().unwrap()
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
            ItemContent::Any(values) => values.get(self.offset as usize).cloned().map(Out::Any),
            ItemContent::String(slice) => {
                let c = slice.chars().nth(self.offset as usize)?;
                Some(Out::Any(Any::String(c.to_string().into())))
            }
            ItemContent::JSON(values) => values
                .get(self.offset as usize)
                .map(|json| Out::Any(Any::from(json.as_str()))),
            ItemContent::Binary(bin) if self.offset == 0 => {
                Some(Out::Any(Any::Buffer(bin.as_slice().into())))
            }
            ItemContent::Doc(_, options) if self.offset == 0 => {
                Some(Out::Doc(options.guid.clone()))
            }
            ItemContent::Embed(value) if self.offset == 0 => Some(Out::Any(value.clone())),
            ItemContent::Node(node) if self.offset == 0 => Some(Out::node(node.id())),
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

pub(crate) fn update_current_attributes(attrs: &mut Attrs, key: &str, value: &Any) {
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
    /// If `true`, retain formatter inserts with attributions.
    pub retain_inserts: bool,
    /// If `true`, retain formatter+attributed deletes only. Yrs has no attribution renderer yet,
    /// therefore deletes are never attributed and this option has no effect (see: [NodeRef::delta]).
    pub retain_deletes: bool,
    /// Render child types as delta.
    pub deep: bool,
    /// When weak links are used, how deep should we follow them.
    /// This is also to prevent infinite recursion from happening.
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
    /// Yjs `itemsToRender`: ids of the change being formatter. `None` renders the full state.
    items: Option<IdSet>,
    /// Ids deleted by the change being formatter. Used to resolve previous values of attributes.
    deleted: Option<&'a IdSet>,
    modified: Option<Modified>,
    /// If `true`, formatter inserts are retained instead of being formatter as inserts.
    retain_inserts: bool,
    /// Render child nodes as delta.
    deep: bool,
    /// When weak links are used, this property decides how deep can we follow through them.
    /// This happens because it's possible that weak links will form cycles, causing formatter
    /// to run into infinite loop.
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
            TypeRef::WeakLink(source) => Delta {
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
        let child = as_node(&item.content);
        let op = if item.is_deleted() {
            // Hard-deleted attribute within a change render: emit the remove op so that consumers
            // can apply the removal. In a full-state render the attribute is simply omitted.
            match &self.items {
                Some(items) if items.contains(&item.last_id()) => AttrOp::Remove {
                    prev: item.content.get_last(),
                },
                _ => return,
            }
        } else if let Some(child) = child.filter(|child| self.is_modified(*child)) {
            AttrOp::Modify {
                delta: Box::new(self.node(child)),
            }
        } else {
            let value = match child {
                Some(child) => self.node_value(child),
                None => match item.content.get_last() {
                    Some(value) => value,
                    None => return,
                },
            };
            AttrOp::Update {
                value,
                prev: self.prev_attr(item),
            }
        };
        d.attrs.insert(key.clone(), op);
    }

    /// Previous value of an updated attribute: a value overridden by a given map entry, as long as
    /// it was deleted by the change being formatter.
    fn prev_attr(&self, item: ItemPtr) -> Option<Out> {
        let deleted = self.deleted?;
        let left = item.left?;
        if deleted.contains(&left.last_id()) {
            left.content.get_last()
        } else {
            None
        }
    }

    /// Renders a child node as a value. Deep renders carry the child's own delta.
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

    /// Renders the ordered children of a node - a port of the sequence part of `YType.toDelta`.
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
                            // string-ish content deletes by length, other content deletes one
                            // element per piece
                            push_delete(d, piece_len(content, &range));
                        }
                    }
                }
            } else if let Some(items) = &self.items {
                // change render on alive plain content: inserted ranges become inserts, everything
                // else retains (position-only). `retain_inserts` retains previously inserted
                // content.
                let modified = as_node(content).filter(|child| self.is_modified(*child));
                for (range, exists) in slice(items, &item.id, item.len()) {
                    if !exists {
                        if let Some(child) = modified {
                            push_modify(d, self.node(child));
                        } else {
                            // mirror the piece-wise op sizes of the delete branch
                            push_retain(d, piece_len(content, &range), &changed_formats);
                        }
                    } else if self.retain_inserts {
                        if let Some(child) = modified {
                            push_modify(d, self.node(child));
                        } else {
                            push_retain(d, range.end - range.start, &changed_formats);
                        }
                    } else {
                        let offset = range.start - item.id.clock;
                        let len = range.end - range.start;
                        self.push_content(d, content, offset, len, &current_formats);
                    }
                }
            } else if self.retain_inserts {
                // attribution-overlay render: existing content is retained
                push_retain(d, content.len(self.encoding), &changed_formats);
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
            ItemContent::Node(node) => push_insert(d, self.node_value(NodePtr::from(node)), format),
            ItemContent::String(str) => {
                // clock offsets are always UTF-16 based
                let (_, str) = split_str(str, offset as usize, OffsetKind::Utf16);
                let (str, _) = split_str(str, len as usize, OffsetKind::Utf16);
                push_insert_text(d, str, format);
            }
            _ => {
                let mut buf = vec![Out::default(); len as usize];
                let read = content.read(offset as usize, &mut buf);
                for value in buf.into_iter().take(read) {
                    push_insert(d, value, format);
                }
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
                // `previous` tracks the value governing the current walk position in the consuming
                // state: markers deleted by this change (their value governed it until now) and
                // alive markers re-formatter by heal renders (`retain_inserts`) contribute to it.
                if self.items.is_some() && !self.retain_inserts {
                    previous.insert(key.clone(), value.clone());
                }
            } else {
                if self.retain_inserts {
                    previous.insert(key.clone(), value.clone());
                }
                update_format(current, key, value);
            }
            // the retain diff for the following spans is exactly the consuming state (`previous`)
            // -> new state (`current`), recomputed per key at every formatter marker
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

/// A node embedded within an item's content, if any.
fn as_node(content: &ItemContent) -> Option<NodePtr> {
    if let ItemContent::Node(node) = content {
        Some(NodePtr::from(node))
    } else {
        None
    }
}

/// Op size of a formatter piece: string-ish content is measured by its length, while any other
/// content counts as a single element.
fn piece_len(content: &ItemContent, range: &Range<u32>) -> u32 {
    match content {
        ItemContent::String(_) | ItemContent::Deleted(_) => range.end - range.start,
        _ => 1,
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

/// Formatting attributes attached to an emitted op. Empty attributes are represented as `None`.
fn format_of(attrs: &Attrs) -> Option<Box<Attrs>> {
    if attrs.is_empty() {
        None
    } else {
        Some(Box::new(attrs.clone()))
    }
}

fn same_format(format: Option<&Attrs>, attrs: &Attrs) -> bool {
    match format {
        None => attrs.is_empty(),
        Some(format) => format == attrs,
    }
}

fn push_insert_text(d: &mut Delta, text: &str, attrs: &Attrs) {
    if text.is_empty() {
        return;
    }
    if let Some(Op::InsertText { text: last, format }) = d.children.last_mut() {
        if same_format(format.as_deref(), attrs) {
            last.push_str(text);
            return;
        }
    }
    d.children.push(Op::InsertText {
        text: text.to_string(),
        format: format_of(attrs),
    });
}

fn push_insert(d: &mut Delta, value: Out, attrs: &Attrs) {
    if let Some(Op::Insert { items, format }) = d.children.last_mut() {
        if same_format(format.as_deref(), attrs) {
            items.push(value);
            return;
        }
    }
    d.children.push(Op::Insert {
        items: vec![value],
        format: format_of(attrs),
    });
}

fn push_retain(d: &mut Delta, len: u32, attrs: &Attrs) {
    if len == 0 {
        return;
    }
    if let Some(Op::Retain { len: last, format }) = d.children.last_mut() {
        if same_format(format.as_deref(), attrs) {
            *last += len;
            return;
        }
    }
    d.children.push(Op::Retain {
        len,
        format: format_of(attrs),
    });
}

fn push_delete(d: &mut Delta, len: u32) {
    if len == 0 {
        return;
    }
    if let Some(Op::Remove {
        len: last,
        prev: None,
    }) = d.children.last_mut()
    {
        *last += len;
        return;
    }
    d.children.push(Op::Remove { len, prev: None });
}

fn push_modify(d: &mut Delta, delta: Delta) {
    d.children.push(Op::Modify {
        delta: Box::new(delta),
        format: None,
    });
}
