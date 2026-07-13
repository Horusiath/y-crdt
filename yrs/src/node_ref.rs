use crate::block::{ItemContent, ItemPosition, ItemPtr};
use crate::delta::{AttrOp, Op};
use crate::event::Event;
use crate::node::{Attrs, Node, NodePtr};
use crate::types::{Attrs, DeepObservable, Event, Observable};
use crate::{Any, Delta, Doc, IdSet, In, NodeID, OffsetKind, Out, Transaction};
use std::collections::Bound;
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

    /// Inserts a single value at the given index.
    pub fn insert(&mut self, index: u32, content: impl Into<In>) {
        todo!()
    }

    /// Inserts a string of text at the given index.
    pub fn insert_text(&mut self, index: u32, text: &str) {
        todo!()
    }

    /// Inserts a string of text at the given index with formatting attributes.
    pub fn insert_text_with(&mut self, index: u32, text: &str, attrs: Attrs) {
        todo!()
    }

    /// Applies formatting attributes to a range starting at `index` for `len` elements.
    pub fn format(&mut self, index: u32, len: u32, attrs: Attrs) {
        todo!()
    }

    /// Appends a string of text to the end of this type's children.
    pub fn push_text(&mut self, text: &str) {
        todo!()
    }

    /// Prepends content to the beginning of this type's children.
    pub fn push_front(&mut self, content: impl Into<In>) -> Out {
        todo!()
    }

    /// Appends content to the end of this type's children.
    pub fn push_back(&mut self, content: impl Into<In>) -> Out {
        todo!()
    }

    /// Removes `len` elements starting at `index`.
    pub fn remove(&mut self, index: u32, len: u32) {
        todo!()
    }

    /// Removes all element's matching the given `predicate`.
    /// Returns a number of elements removed.
    pub fn remove_filter<F>(&mut self, predicate: F) -> u32
    where
        F: FnMut(Out) -> bool,
    {
        todo!()
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
