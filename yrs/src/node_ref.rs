use crate::block::{BlockRange, ID, ItemContent, ItemPtr, split_str};
use crate::cursor::Cursor;
use crate::delta::{AttrOp, Attribution, Op};
use crate::formatter::{
    AttributedFragment, AttributedFragmentKind, DefaultFormatter, DeltaFormatter, RenderMode,
};
use crate::lib0;
use crate::node::{Attrs, DeepObservable, Node, NodePtr, Observable, Path, TypeRef};
use crate::slice::BlockSlice;
use crate::transaction::DocHandle;
use crate::{
    Any, Assoc, ContentIds, Delta, Doc, IdSet, In, NodeID, OffsetKind, Out, StickyIndex,
    Transaction, TransactionState,
};
use serde::Deserialize;
use serde::de::value::{MapDeserializer, SeqDeserializer};
use serde::de::{
    DeserializeOwned, DeserializeSeed, Deserializer, IntoDeserializer, MapAccess, Visitor,
};
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

    /// A view of this node rendered through a formatter `fmt`.
    pub fn formatted<'f, F: DeltaFormatter>(&self, fmt: &'f F) -> Formatted<'f, &Self, F> {
        Formatted { node: self, fmt }
    }

    /// A mutable view of this node rendered through a formatter `fmt`.
    pub fn formatted_mut<'f, F: DeltaFormatter>(
        &mut self,
        fmt: &'f F,
    ) -> Formatted<'f, &mut Self, F> {
        Formatted { node: self, fmt }
    }
}

/// A view of a node rendered through a formatter (see: [NodeRef::formatted]).
pub struct Formatted<'f, N, F> {
    node: N,
    fmt: &'f F,
}

impl<'f, 'n, T, D, F> Formatted<'f, &'n NodeRef<T>, F>
where
    T: Deref<Target = Transaction<D>>,
    D: DocHandle,
    F: DeltaFormatter,
{
    /// Returns a delta representation of this node's contents.
    pub fn delta(&self, options: &DeltaOptions) -> Delta {
        self.node.render(self.fmt, options)
    }

    /// Returns a [StickyIndex] equivalent to a formatted `index`.
    pub fn sticky_index(&self, index: u32, assoc: Assoc) -> Option<StickyIndex> {
        StickyIndex::at_fmt(self.node.txn(), self.node.ptr, index, assoc, self.fmt)
    }

    /// A path from the root type down to this node.
    pub fn path(&self) -> Path {
        let mut root = self.node.ptr;
        while let Some(item) = root.item {
            root = *item.parent.as_node().unwrap();
        }
        let kind = self.node.txn().doc().offset_kind();
        Node::path(root, self.node.ptr, kind, self.fmt)
    }
}

impl<'f, 'n, T, D, F> Formatted<'f, &'n mut NodeRef<T>, F>
where
    T: DerefMut<Target = Transaction<D>>,
    D: DocHandle + DerefMut,
    F: DeltaFormatter,
{
    /// Returns a delta representation of this node's contents.
    pub fn delta(&self, options: &DeltaOptions) -> Delta {
        self.node.render(self.fmt, options)
    }

    /// Applies a sequence of delta operations, with indexes counted in formatted content.
    pub fn apply_delta(&mut self, delta: Delta<In>) {
        let ptr = self.node.ptr;
        let (doc, state) = self.node.txn.split_mut();
        apply_delta(doc, state, ptr, delta, self.fmt)
    }
}

// read operations
impl<T, D> NodeRef<T>
where
    T: Deref<Target = Transaction<D>>,
    D: DocHandle,
{
    pub fn txn(&self) -> &Transaction<D> {
        &self.txn
    }

    fn seek(&self, index: u32) -> Option<Cursor<'static>> {
        Cursor::at(
            self.ptr,
            self.txn.doc.offset_kind(),
            &DefaultFormatter,
            index,
        )
    }

    /// The parent type, or `None` for root-level types.
    pub fn parent(&self) -> Option<NodeID> {
        let item = self.ptr.item?;
        let parent = item.parent.as_node()?;
        Some(parent.id())
    }

    /// The element name. `None` for text/array/map types and root nodes, `Some` for XML elements.
    pub fn name(&self) -> Option<&Arc<str>> {
        if self.ptr.item.is_none() {
            return None; // root nodes keep their key under `name`
        }
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

    pub fn deserialize<A: DeserializeOwned>(&self) -> crate::Result<A> {
        let node = NodeRef::new(self.ptr, &*self.txn);
        A::deserialize(node).map_err(|e| Box::new(e.into()))
    }

    /// Converts this type to a JSON-compatible representation.
    pub fn to_json(&self) -> Any {
        self.deserialize().unwrap()
    }

    /// Returns a delta representation of this type's contents.
    pub fn delta(&self, options: &DeltaOptions) -> Delta {
        self.render(&DefaultFormatter, options)
    }

    fn render<F: DeltaFormatter>(&self, fmt: &F, options: &DeltaOptions) -> Delta {
        let sym_diff;
        let items = match (&options.items_to_render, &options.deleted_items) {
            (None, None) => None,
            (Some(inserted), None) => Some(inserted),
            (None, Some(deleted)) => Some(deleted),
            (Some(inserted), Some(deleted)) => {
                // either inserted or deleted but not both at the same time
                let mut items = inserted.diff(deleted);
                items.merge_with(deleted.diff(inserted));
                sym_diff = items;
                Some(&sym_diff)
            }
        };
        let computed;
        let modified = match (&options.modified, items) {
            (Some(modified), _) => Some(modified),
            (None, Some(items)) if options.deep => {
                computed = compute_modified(self.txn.doc(), items);
                Some(&computed)
            }
            _ => None,
        };
        let render = DeltaWalk {
            fmt,
            encoding: self.txn.doc.options.offset_kind,
            items,
            inserted_items: options.inserted_items.as_ref(),
            deleted: options.deleted_items.as_ref(),
            modified,
            retain_inserts: options.retain_inserts,
            retain_deletes: options.retain_deletes,
            deep: options.deep,
            link_depth: options.link_depth,
        };
        render.node(self.ptr)
    }

    pub fn format_xml(&self) -> XmlFormat {
        XmlFormat::new(self)
    }
}

impl<T, D> Display for NodeRef<T>
where
    T: Deref<Target = Transaction<D>>,
    D: DocHandle,
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
impl<T, D> NodeRef<T>
where
    T: DerefMut<Target = Transaction<D>>,
    D: DocHandle + DerefMut,
{
    pub fn txn_mut(&mut self) -> &mut Transaction<D> {
        &mut self.txn
    }

    /// Inserts a single value at the given index.
    ///
    /// # Panics
    ///
    /// Panics if `index` is greater than the current length of this type's children.
    pub fn insert(&mut self, index: u32, value: impl Into<In>) -> Out {
        let mut cursor = self.seek(index).unwrap();
        let (doc, state) = self.txn.split_mut();
        let item = cursor.insert(doc, state, value.into(), None).unwrap();
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
        if len == 0 {
            return;
        }
        let kind = self.txn.doc.offset_kind();
        if let Some(mut cursor) = Cursor::at(self.ptr, kind, &DefaultFormatter, index) {
            let (doc, state) = self.txn.split_mut();
            cursor.remove(doc, state, len);
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
        let (doc, state) = self.txn.split_mut();
        let item = state
            .create_map_item(doc, self.ptr, name.into(), value.into())
            .unwrap();
        item.content.get_last().unwrap()
    }

    /// Removes an attribute by name.
    pub fn remove_attr(&mut self, name: &str) -> Option<Out> {
        let (doc, state) = self.txn.split_mut();
        self.ptr.remove(doc, state, name)
    }

    /// Removes all attributes.
    pub fn clear_attrs(&mut self) {
        let (doc, state) = self.txn.split_mut();
        for &item in self.ptr.map.values() {
            state.delete(doc, item);
        }
    }

    /// Applies a sequence of delta operations to this type.
    pub fn apply_delta(&mut self, delta: Delta<In>) {
        let (doc, state) = self.txn.split_mut();
        apply_delta(doc, state, self.ptr, delta, &DefaultFormatter);
    }
}

/// Applies a sequence of delta operations to a given `node`, with indexes counted in content
/// formatted by `fmt`.
pub(crate) fn apply_delta<F: DeltaFormatter>(
    doc: &mut Doc,
    state: &mut TransactionState,
    node: NodePtr,
    delta: Delta<In>,
    fmt: &F,
) {
    if node.item.map(|item| item.is_deleted()).unwrap_or(false) {
        return; // current node is already deleted
    }

    if !delta.children.is_empty() {
        apply_ops(doc, state, node, delta.children, fmt);
    }

    if !delta.attrs.is_empty() {
        apply_attr_ops(doc, state, node, delta.attrs, fmt)
    }
}

fn apply_ops<F: DeltaFormatter>(
    doc: &mut Doc,
    state: &mut TransactionState,
    node: NodePtr,
    ops: Vec<Op<In>>,
    fmt: &F,
) {
    let mut cursor = Cursor::new(node, doc.offset_kind(), fmt);
    for op in ops {
        match op {
            Op::Retain {
                len, format: None, ..
            } => {
                let n = cursor.forward(len);
                assert_eq!(n, len)
            }
            Op::Retain {
                len,
                format: Some(attrs),
                ..
            } => {
                let n = cursor.format(doc, state, len, &attrs);
                assert_eq!(n, len)
            }
            Op::InsertText { text, format, .. } => {
                cursor.insert_text(doc, state, &text, format.as_deref());
            }
            Op::Insert { items, format, .. } => {
                cursor.insert_range(doc, state, items, format.as_deref());
            }
            Op::Remove { len, .. } => {
                let n = cursor.remove(doc, state, len);
                assert_eq!(n, len)
            }
            Op::Modify { delta, format, .. } => {
                cursor.skip_hidden();
                let item = cursor.right().unwrap();
                if item.is_deleted() {
                    cursor.forward(1);
                } else {
                    let child = item.as_node().unwrap();
                    apply_delta(doc, state, child, *delta, fmt);
                    match format {
                        Some(attrs) => {
                            cursor.format(doc, state, 1, &attrs);
                        }
                        None => {
                            cursor.forward(1);
                        }
                    }
                }
            }
        }
    }
}

fn apply_attr_ops<F: DeltaFormatter>(
    doc: &mut Doc,
    state: &mut TransactionState,
    node: NodePtr,
    attrs: HashMap<Arc<str>, AttrOp<In>>,
    fmt: &F,
) {
    for (key, op) in attrs {
        match op {
            AttrOp::Update { value, .. } => {
                state.create_map_item(doc, node, key, value).unwrap();
            }
            AttrOp::Remove { .. } => {
                node.remove(doc, state, &key);
            }
            AttrOp::Modify { delta, .. } => match node.map.get(&key).copied() {
                Some(item) => {
                    apply_delta(doc, state, item.as_node().unwrap(), *delta, fmt);
                }
                None => {
                    state
                        .create_map_item(doc, node, key, In::Node(delta))
                        .unwrap();
                }
            },
        }
    }
}

/// Deserializes node's children when sequence is requested, and node's attributes when map or
/// struct is requested. Otherwise, node is deserialized as `{ name?, attrs?, children? }` object.
impl<'de, T, D> Deserializer<'de> for NodeRef<T>
where
    T: Deref<Target = Transaction<D>>,
    D: DocHandle,
{
    type Error = lib0::Error;

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        visitor.visit_map(NodeAccess {
            node: NodeRef::new(self.ptr, &*self.txn),
            next: 0,
        })
    }

    fn deserialize_option<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        visitor.visit_some(self)
    }

    fn deserialize_newtype_struct<V>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_seq<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.children_deserializer().deserialize_any(visitor)
    }

    fn deserialize_tuple<V>(self, _len: usize, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_seq(visitor)
    }

    fn deserialize_tuple_struct<V>(
        self,
        _name: &'static str,
        _len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_seq(visitor)
    }

    fn deserialize_map<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.attrs_deserializer().deserialize_any(visitor)
    }

    fn deserialize_struct<V>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_map(visitor)
    }

    fn deserialize_ignored_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        visitor.visit_unit()
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 u8 u16 u32 u64 f32 f64 char str string bytes byte_buf unit
        unit_struct enum identifier
    }
}

impl<T, D> NodeRef<T>
where
    T: Deref<Target = Transaction<D>>,
    D: DocHandle,
{
    /// Deserializer of node's children as a sequence.
    fn children_deserializer<'a>(
        &'a self,
    ) -> SeqDeserializer<impl Iterator<Item = OutDeserializer<'a, D>>, lib0::Error>
    where
        D: 'a,
    {
        let txn = &*self.txn;
        SeqDeserializer::new(self.iter().map(move |value| OutDeserializer { value, txn }))
    }

    /// Deserializer of node's attributes as a map.
    fn attrs_deserializer<'a, 'de>(
        &'a self,
    ) -> MapDeserializer<'de, impl Iterator<Item = (&'a str, OutDeserializer<'a, D>)>, lib0::Error>
    where
        D: 'a,
    {
        let txn = &*self.txn;
        MapDeserializer::new(
            self.attrs()
                .map(move |(key, value)| (key, OutDeserializer { value, txn })),
        )
    }
}

/// Fields of the default representation of a deserialized node.
const NODE_FIELDS: [&str; 3] = ["name", "attrs", "children"];

/// [MapAccess] over the default `{ name?, attrs?, children? }` representation of a node.
/// Fields which are absent or empty are skipped.
struct NodeAccess<'a, D: DocHandle> {
    node: NodeRef<&'a Transaction<D>>,
    /// Index of the next [NODE_FIELDS] entry to visit.
    next: usize,
}

impl<'de, 'a, D> MapAccess<'de> for NodeAccess<'a, D>
where
    D: DocHandle,
{
    type Error = lib0::Error;

    fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>, Self::Error>
    where
        K: DeserializeSeed<'de>,
    {
        while let Some(&key) = NODE_FIELDS.get(self.next) {
            let present = match self.next {
                0 => self.node.name().is_some(),
                1 => self.node.attr_len() > 0,
                _ => self.node.len() > 0,
            };
            if present {
                return seed.deserialize(key.into_deserializer()).map(Some);
            }
            self.next += 1;
        }
        Ok(None)
    }

    fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value, Self::Error>
    where
        V: DeserializeSeed<'de>,
    {
        let field = self.next;
        self.next += 1;
        match field {
            0 => {
                let name: &str = self.node.name().map_or("", |name| name);
                seed.deserialize(name.into_deserializer())
            }
            1 => seed.deserialize(self.node.attrs_deserializer()),
            _ => seed.deserialize(self.node.children_deserializer()),
        }
    }
}

/// Deserializer of node's child or attribute value. Nested nodes are resolved using `txn`.
struct OutDeserializer<'a, D: DocHandle> {
    value: Out,
    txn: &'a Transaction<D>,
}

impl<'de, 'a, D> IntoDeserializer<'de, lib0::Error> for OutDeserializer<'a, D>
where
    D: DocHandle,
{
    type Deserializer = Self;

    fn into_deserializer(self) -> Self::Deserializer {
        self
    }
}

/// Forwards [Deserializer] methods to the deserializer of the underlying [Out] variant.
macro_rules! forward_out {
    ($($method:ident($($arg:ident: $ty:ty),*);)*) => {$(
        fn $method<V>(self, $($arg: $ty,)* visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            match self.value {
                Out::Any(any) => any.$method($($arg,)* visitor),
                Out::Node(node) => match self.txn.node(node.id.clone()) {
                    Some(n) => n.$method($($arg,)* visitor),
                    None => Err(lib0::Error::Custom(format!("node {} not found", node.id))),
                },
                Out::Doc(guid) => {
                    let guid: &str = &guid;
                    guid.into_deserializer().$method($($arg,)* visitor)
                }
            }
        }
    )*};
}

impl<'de, 'a, D> Deserializer<'de> for OutDeserializer<'a, D>
where
    D: DocHandle,
{
    type Error = lib0::Error;

    forward_out! {
        deserialize_any();
        deserialize_bool();
        deserialize_i8();
        deserialize_i16();
        deserialize_i32();
        deserialize_i64();
        deserialize_u8();
        deserialize_u16();
        deserialize_u32();
        deserialize_u64();
        deserialize_f32();
        deserialize_f64();
        deserialize_char();
        deserialize_str();
        deserialize_string();
        deserialize_bytes();
        deserialize_byte_buf();
        deserialize_option();
        deserialize_unit();
        deserialize_unit_struct(name: &'static str);
        deserialize_newtype_struct(name: &'static str);
        deserialize_seq();
        deserialize_tuple(len: usize);
        deserialize_tuple_struct(name: &'static str, len: usize);
        deserialize_map();
        deserialize_struct(name: &'static str, fields: &'static [&'static str]);
        deserialize_enum(name: &'static str, variants: &'static [&'static str]);
        deserialize_identifier();
        deserialize_ignored_any();
    }
}

#[derive(Debug, Clone, Default)]
pub struct DeltaOptions {
    /// If `true`, retain formatter inserts with attributions.
    pub retain_inserts: bool,
    /// If `true`, attributed deletes are retained instead of being rendered as inserts.
    pub retain_deletes: bool,
    /// Render child types as delta.
    pub deep: bool,
    /// When weak links are used, how deep should we follow them.
    /// This is also to prevent infinite recursion from happening.
    /// Default: 0 (don't jump through links).
    pub link_depth: u32,
    pub items_to_render: Option<IdSet>,
    /// Ids inserted by the change being rendered. Content both inserted and deleted by it renders
    /// as a fresh insert, even with `retain_deletes`.
    pub inserted_items: Option<IdSet>,
    /// Used for computing `prev` in attributes.
    pub deleted_items: Option<IdSet>,
    /// Nodes to render as modified children. Computed from `items_to_render` if not given.
    pub modified: Option<Modified>,
}

/// Nodes that should be formatter as modified children, mapped onto the keys changed within them.
/// A `None` key marks a change made to node's children.
pub type Modified = HashMap<NodePtr, HashSet<Option<Arc<str>>>>;

/// Renders changes of a transaction `state` made to a `node` (event delta).
pub(crate) fn get_delta<F: DeltaFormatter>(
    doc: &Doc,
    state: &TransactionState,
    node: NodePtr,
    deep: bool,
    fmt: &F,
) -> Delta {
    // content that was both inserted AND deleted by this transaction is normally invisible — but a
    // formatter may still render it. Render `(I ∪ D) − ((I ∩ D) − fmt.attributed)`: the symmetric
    // difference plus the attributed part of the intersection.
    let mut items = state.insert_set.diff(&state.delete_set);
    items.merge_with(state.delete_set.diff(&state.insert_set));
    let both = state.insert_set.intersect(&state.delete_set);
    if !both.is_empty() {
        let attributed = fmt.attributed();
        if !attributed.is_empty() {
            items.merge_with(both.intersect(attributed));
        }
    }
    let deep_modified;
    let modified = if deep {
        deep_modified = expand_modified(&state.changed);
        &deep_modified
    } else {
        &state.changed
    };
    let render = DeltaWalk {
        fmt,
        encoding: doc.options.offset_kind,
        items: Some(&items),
        inserted_items: Some(&state.insert_set),
        deleted: None,
        modified: Some(modified),
        retain_inserts: false,
        retain_deletes: true,
        deep,
        link_depth: 0,
    };
    render.node(node)
}

/// Maps every node touched by `items` onto a set of keys changed within it (a `None` key marks
/// a change made to node's children).
fn compute_modified(doc: &Doc, items: &IdSet) -> Modified {
    let mut modified = Modified::new();
    for block in items.iter_blocks(&doc.blocks) {
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

/// Renders a `diff` of `next` through a formatter `fmt` as a delta modifying every changed root
/// node (see: [Doc::diff_with]).
pub(crate) fn render_diff<F: DeltaFormatter>(next: &Doc, diff: ContentIds, fmt: &F) -> Delta<Out> {
    // don't render content which was both inserted and deleted
    let mut items = diff.inserts.diff(&diff.deletes);
    items.merge_with(diff.deletes);
    let opts = DeltaOptions {
        deep: true,
        retain_deletes: true,
        modified: Some(compute_modified(next, &items)),
        items_to_render: Some(items),
        // unlike Yjs: content inserted and deleted since `prev` is fresh, so it renders as
        // nothing whether `next` gc'd it or not
        inserted_items: Some(diff.inserts),
        ..Default::default()
    };
    let modified = opts.modified.as_ref().unwrap();
    let txn = next.transact();
    let mut delta = Delta::default();
    for (name, node) in next.types.iter() {
        let ptr = NodePtr::from(node);
        if modified.contains_key(&ptr) {
            let root = NodeRef::new(ptr, &txn);
            delta.modify_attr_with(name.clone(), root.formatted(fmt).delta(&opts));
        }
    }
    delta
}

fn expand_modified(changed: &Modified) -> Modified {
    let mut modified = changed.clone();
    for &node in changed.keys() {
        let mut current = node.item;
        while let Some(item) = current {
            let parent = *item.parent.as_node().unwrap();
            let keys = modified.entry(parent).or_default();
            if !keys.insert(item.parent_sub.clone()) {
                break; // has already been marked as modified
            }
            current = parent.item;
        }
    }
    modified
}

pub struct XmlFormat {
    delta: Delta<Out>,
}

impl XmlFormat {
    fn new<T, D>(node: &NodeRef<T>) -> Self
    where
        T: Deref<Target = Transaction<D>>,
        D: DocHandle,
    {
        XmlFormat {
            delta: node.delta(&DeltaOptions {
                deep: true,
                ..Default::default()
            }),
        }
    }

    fn delta_fmt(delta: &Delta, f: &mut Formatter<'_>) -> std::fmt::Result {
        if let Some(tag) = &delta.name {
            write!(f, "<{}", tag)?;

            for (key, op) in delta.attrs.iter() {
                write!(f, " {}=\"", key)?;
                match op {
                    AttrOp::Update {
                        value: Out::Any(value),
                        ..
                    } => {
                        write!(f, "{}", value)?;
                    }
                    AttrOp::Update {
                        value: Out::Node(node),
                        ..
                    } => {
                        if let Some(delta) = node.delta.as_deref() {
                            Self::delta_fmt(delta, f)?;
                        }
                    }
                    AttrOp::Modify { .. } => {}
                    _ => { /* ignore */ }
                }
                write!(f, "\"")?;
            }
            write!(f, ">")?;
        }

        for op in delta.children.iter() {
            let mut attrs = Vec::new();
            match op {
                Op::InsertText { text, format, .. } => {
                    if let Some(format) = format.as_deref() {
                        for (key, value) in format.iter() {
                            attrs.push((key, value));
                        }
                        attrs.sort_by(|x, y| x.0.cmp(y.0))
                    }

                    // write attributes as xml opening tags
                    for (node, at) in attrs.iter() {
                        write!(f, "<{}", node)?;
                        if let Any::Map(at) = at {
                            for (k, v) in at.iter() {
                                write!(f, " {}=\"{}\"", k, v)?;
                            }
                        }
                        write!(f, ">")?;
                    }

                    write!(f, "{}", text)?;

                    // write attributes as xml closing tags
                    attrs.reverse();
                    for (key, _) in attrs {
                        write!(f, "</{}>", key)?;
                    }
                }
                Op::Insert { items, .. } => {
                    for out in items {
                        if let Out::Node(node) = out
                            && let Some(delta) = node.delta.as_deref()
                        {
                            Self::delta_fmt(delta, f)?;
                        }
                    }
                }
                Op::Modify { .. } => {}
                _ => { /* ignore */ }
            }
        }

        if let Some(tag) = &delta.name {
            write!(f, "</{}>", tag)?;
        }
        Ok(())
    }
}

impl std::fmt::Display for XmlFormat {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Self::delta_fmt(&self.delta, f)
    }
}

struct DeltaWalk<'a, F: DeltaFormatter> {
    fmt: &'a F,
    encoding: OffsetKind,
    /// If `None` renders the full node state.
    /// Otherwise, will render only changes marked on [IdSet].
    items: Option<&'a IdSet>,
    /// Ids inserted by the change being rendered; content that is both in `insertedItems`
    /// and deleted renders as a *fresh* insert even under `retainDeletes` (it was never part
    /// of the consuming state)
    inserted_items: Option<&'a IdSet>,
    /// Ids deleted by the change being formatter. Used to resolve previous values of attributes.
    deleted: Option<&'a IdSet>,
    modified: Option<&'a Modified>,
    /// If `true`, retain rendered inserts with attributions.
    retain_inserts: bool,
    /// If `true`, retain rendered+attributed deletes only.
    retain_deletes: bool,
    /// Render child nodes as delta.
    deep: bool,
    link_depth: u32,
}

#[derive(Default)]
struct WalkState {
    /// Saves all current formats for insert
    current: Attrs,
    /// Saves changed formats for retain
    changed: Attrs,
    /// The value before changes
    prev: Attrs,
    /// Logic for format attribution
    /// Everything that comes after a format is formatted by the user that created it.
    /// Two exceptions:
    /// - the user resets formatting to the previously known formatting that is not attributed
    /// - the user deletes a format and hence restores the previously known formatting
    ///   that is not attributed.
    prev_unattributed: Attrs,
    used_attribution: Option<Attribution>,
}

impl WalkState {
    fn data(&self, own: Option<&Attribution>) -> Option<Attribution> {
        Attribution::combine_data(self.used_attribution.as_ref(), own)
    }

    fn instr(&self, own: Option<&Attribution>) -> Option<Attribution> {
        Attribution::combine(self.used_attribution.as_ref(), own)
    }

    /// Attribution of an instruction which clears its own former attribution when it has none.
    fn instr_or_clear(&self, own: Option<Attribution>) -> Option<Attribution> {
        match (own, &self.used_attribution) {
            (Some(own), _) => self.instr(Some(&own)),
            (None, None) => Some(Attribution::default()),
            // lib0 emits `{ insert: null, delete: null, .. }` over the ambient attribution,
            // which can't be expressed here: keep only the ambient format attribution
            (None, Some(used)) => Some(Attribution {
                format: used.format.clone(),
                format_at: used.format_at,
                ..Attribution::default()
            }),
        }
    }

    /// Recomputes the retain diff of a format `key`: previous → current.
    fn update_changed(&mut self, key: &Arc<str>) {
        let current = self.current.get(key).unwrap_or(&Any::Null);
        if current == self.prev.get(key).unwrap_or(&Any::Null) {
            self.changed.remove(key);
        } else {
            self.changed.insert(key.clone(), current.clone());
        }
    }
}

/// Piece-wise op sizes: string-ish content by length, other content one per piece.
fn piece_count(content: Option<&ItemContent>, len: u32) -> u32 {
    match content {
        None | Some(ItemContent::String(_)) | Some(ItemContent::Deleted(_)) => len,
        _ => 1,
    }
}

impl<'a, F: DeltaFormatter> DeltaWalk<'a, F> {
    fn node(&self, node: NodePtr) -> Delta {
        let attrs_to_render = self.modified.as_ref().and_then(|m| m.get(&node));
        let render_children =
            self.modified.is_none() || attrs_to_render.map_or(true, |keys| keys.contains(&None));
        let mut delta = match &node.type_ref {
            TypeRef::XmlElement(name) => Delta::with_name(name.clone()),
            #[cfg(feature = "weak")]
            TypeRef::WeakLink(source) if self.link_depth == 0 && self.is_inserted(node) => Delta {
                link: Some(source.clone()),
                ..Delta::default()
            },
            _ => Delta::default(),
        };
        self.attrs(&mut delta, node, attrs_to_render);
        if render_children {
            self.children(&mut delta, node);
            if self.items.is_some() {
                delta.trim_retains();
            }
        }
        delta
    }

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
        let mut pieces = Vec::new();
        let (deleted, attribution, content) = if !self.fmt.contains(&item) {
            (item.is_deleted(), None, Some(&item.content))
        } else {
            let fmt: &'a F = self.fmt;
            let range = BlockRange::new(item.id, item.len());
            fmt.read_content(&mut pieces, &item, range, RenderMode::Visible);
            let Some(p) = pieces.last() else {
                return; // the formatter surfaces nothing for this attribute
            };
            let content = match p.content {
                AttributedFragmentKind::Own => Some(&item.content),
                AttributedFragmentKind::Restored { content, .. } => Some(content),
                AttributedFragmentKind::Opaque => None,
            };
            let attribution = Attribution::resolve(p.attrs.as_ref(), p.deleted);
            (p.deleted, attribution, content)
        };
        let child = content.and_then(ItemContent::as_node);
        let op = if deleted {
            match attribution {
                // deleted attributed value is still visible: render it with its attribution
                Some(attribution) => {
                    let changed = match &self.items {
                        None => true,
                        Some(items) => {
                            items.contains(&item.last_id())
                                || child.is_some_and(|c| self.is_modified(c))
                        }
                    };
                    if !changed {
                        return;
                    }
                    let value = match child {
                        Some(child) => self.full_value(child),
                        None => match content.and_then(ItemContent::get_last) {
                            Some(value) => value,
                            None => return,
                        },
                    };
                    AttrOp::Update {
                        value,
                        prev: self.prev_attr(item),
                        attribution: Some(Box::new(attribution)),
                    }
                }
                // Hard-deleted attribute within a change render: emit the remove op. In a full
                // render the attribute is simply omitted.
                None => match &self.items {
                    Some(items) if items.contains(&item.last_id()) => {
                        AttrOp::remove(self.deleted.and_then(|_| item.content.get_last()))
                    }
                    _ => return,
                },
            }
        } else if let Some(child) = child.filter(|child| self.deep && self.is_modified(*child)) {
            AttrOp::modify(self.node(child))
        } else {
            let value = match child {
                Some(child) => self.node_value(child),
                None => match content.and_then(ItemContent::get_last) {
                    Some(value) => value,
                    None => return,
                },
            };
            AttrOp::Update {
                value,
                prev: self.prev_attr(item),
                attribution: attribution.map(Box::new),
            }
        };
        d.attrs.insert(key.clone(), op);
    }

    /// Value of a deleted, but still rendered attribute: a full-state render of its node.
    fn full_value(&self, node: NodePtr) -> Out {
        if !self.deep {
            return Out::node(node.id());
        }
        let walk = DeltaWalk {
            fmt: self.fmt,
            encoding: self.encoding,
            items: None,
            inserted_items: None,
            deleted: None,
            modified: None,
            retain_inserts: false,
            retain_deletes: false,
            deep: true,
            link_depth: self.link_depth,
        };
        walk.node_value(node)
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

    #[cfg(feature = "weak")]
    fn is_inserted(&self, node: NodePtr) -> bool {
        match (&self.items, node.item) {
            (None, _) => true,                                     // full render
            (Some(items), Some(item)) => items.contains(&item.id), // change render (ex. event delta)
            (Some(_), None) => false, // root type (they are never explicitly inserted)
        }
    }

    fn is_modified(&self, node: NodePtr) -> bool {
        match &self.modified {
            Some(modified) => modified.contains_key(&node),
            None => false,
        }
    }

    fn children(&self, d: &mut Delta, node: NodePtr) {
        let mut state = WalkState::default();
        let mut pieces: Vec<AttributedFragment<'a, F::Attr>> = Vec::new();
        let mut curr = node.start;
        while let Some(item) = curr {
            curr = item.right;
            if self.fmt.contains(&item) {
                pieces.clear();
                self.read_pieces(&mut pieces, item);
                for piece in pieces.iter() {
                    self.process_content(d, &mut state, item, piece);
                }
            } else {
                // generic fast path: content the renderer doesn't claim renders as-is — no attribution
                // lookups, no AttributedContent wrappers and, in the common full-coverage case, no
                // content slicing
                self.fast_path(d, &mut state, item);
            }
        }
    }

    fn fast_path(&self, d: &mut Delta, st: &mut WalkState, item: ItemPtr) {
        let content = &item.content;
        if let ItemContent::Format(..) = content {
            // format markers always flow through the shared state machine
            let mode = match &self.items {
                None => RenderMode::Visible,
                Some(items) if items.contains(&item.id) => RenderMode::Insert,
                Some(_) => RenderMode::Retain,
            };
            let range = item.id.clock..(item.id.clock + 1);
            let piece = AttributedFragment::new(range, item.is_deleted(), None, mode);
            self.process_content(d, st, item, &piece);
        } else if item.is_deleted() {
            // plain deleted content is invisible; in a change render, the ranges deleted by
            // this change emit `delete` ops - position-only, the content itself is not needed
            if let Some(items) = &self.items {
                for (range, exists) in items.slice(&BlockRange::new(item.id, item.len())) {
                    if exists {
                        d.remove_with(piece_count(Some(content), range.end - range.start));
                    }
                }
            }
        } else if let Some(items) = &self.items {
            // change render on alive plain content: inserted ranges become inserts, everything
            // else retains (position-only). `retain_inserts` (attribution-heal renders) retains
            // previously inserted content while clearing its former attribution.
            let modified = content.as_node().filter(|child| self.is_modified(*child));
            for (range, exists) in items.slice(&BlockRange::new(item.id, item.len())) {
                let len = range.end - range.start;
                if !exists {
                    if let Some(child) = modified {
                        d.modify_attributed_with(self.node(child), None, st.instr(None));
                    } else {
                        let count = piece_count(Some(content), len);
                        d.retain_attributed_with(count, Some(&st.changed), st.instr(None));
                    }
                } else if self.retain_inserts {
                    if let Some(child) = modified {
                        d.modify_attributed_with(self.node(child), None, st.instr_or_clear(None));
                    } else {
                        d.retain_attributed_with(len, Some(&st.changed), st.instr_or_clear(None));
                    }
                } else {
                    let offset = range.start - item.id.clock;
                    self.push_content(d, content, offset, len, &st.current, st.data(None));
                }
            }
        } else if self.retain_inserts {
            // attribution-overlay render: existing content is retained, clearing any formerly
            // cached own-attribution
            let len = content.len(self.encoding);
            d.retain_attributed_with(len, Some(&st.changed), st.instr_or_clear(None));
        } else {
            // full render: a plain insert of the whole item
            self.push_content(d, content, 0, item.len(), &st.current, st.data(None));
        }
    }

    /// Reads the pieces of an item claimed by the formatter, in a mode fitting the render.
    fn read_pieces(&self, pieces: &mut Vec<AttributedFragment<'a, F::Attr>>, item: ItemPtr) {
        let fmt: &'a F = self.fmt;
        let item_range = BlockRange::new(item.id, item.len());
        let Some(items) = &self.items else {
            fmt.read_content(pieces, &item, item_range, RenderMode::Visible);
            return;
        };
        // Fresh content deleted by the very change being rendered renders as an insert even
        // with `retain_deletes`. Freshness is decided per id range, not per item, as items may
        // merge across transactions. Format markers keep the retained path.
        let is_format = matches!(item.content, ItemContent::Format(..));
        let inserted = self.inserted_items.filter(|_| item.is_deleted());
        let check_fresh = inserted.is_some() && !is_format;
        let fresh_format = is_format && inserted.is_some_and(|ids| ids.contains(&item.id));
        let client = item.id.client;
        let block_range =
            |r: Range<u32>| BlockRange::new(ID::new(client, r.start), r.end - r.start);
        for (range, exists) in items.slice(&item_range) {
            let range = block_range(range);
            match inserted {
                Some(inserted) if exists && check_fresh => {
                    for (range, fresh) in inserted.slice(&range) {
                        let mode = if fresh {
                            RenderMode::Fresh
                        } else {
                            RenderMode::Insert
                        };
                        fmt.read_content(pieces, &item, block_range(range), mode);
                    }
                }
                _ => {
                    let mode = if exists && !fresh_format {
                        RenderMode::Insert
                    } else {
                        RenderMode::Retain
                    };
                    fmt.read_content(pieces, &item, range, mode);
                }
            }
        }
        if fresh_format {
            for piece in pieces.iter_mut() {
                piece.fresh = true;
            }
        }
    }

    /// Emits ops for a single (possibly attributed) piece and drives the format state machine.
    fn process_content(
        &self,
        d: &mut Delta,
        st: &mut WalkState,
        item: ItemPtr,
        p: &AttributedFragment<F::Attr>,
    ) {
        // render (attributed) content even if it was deleted
        let render_content = p.render && (!p.deleted || p.attrs.is_some());
        // content that was just deleted. It is not rendered as an insertion, because it doesn't
        // have any formats.
        let render_delete = p.render && p.deleted;
        // existing content that should be retained, only adding changed formats
        let retain_content = !p.render && (!p.deleted || p.attrs.is_some());
        let (content, offset) = match p.content {
            AttributedFragmentKind::Own => (Some(&item.content), p.range.start - item.id.clock),
            AttributedFragmentKind::Restored { content, offset } => (Some(content), offset),
            AttributedFragmentKind::Opaque => (None, 0),
        };
        let len = p.len();
        let retain_attributed = if p.deleted {
            self.retain_deletes && !p.fresh
        } else {
            self.retain_inserts
        };
        match content {
            None | Some(ItemContent::Deleted(_)) => {
                // fresh content renders as nothing: the consuming state never saw it
                if render_delete && !p.fresh {
                    d.remove_with(len);
                }
            }
            Some(ItemContent::Format(key, value)) => self.process_format(
                st,
                p,
                key,
                value,
                render_content || render_delete,
                retain_content,
            ),
            Some(content) => {
                let child = content.as_node();
                if render_content {
                    let attribution = Attribution::resolve(p.attrs.as_ref(), p.deleted);
                    if !retain_attributed {
                        let attribution = st.data(attribution.as_ref());
                        self.push_content(d, content, offset, len, &st.current, attribution);
                    } else if let (true, Some(child)) = (p.deleted, child) {
                        let attribution = match attribution {
                            Some(a) => st.instr(Some(&a)),
                            None => Some(Attribution::default()),
                        };
                        d.modify_attributed_with(self.node(child), None, attribution);
                    } else if let Some(child) = child.filter(|c| self.is_modified(*c)) {
                        // a de/re-attributed node the formatter still claims: descend so that the
                        // nested walk heals exactly the changed ids
                        let attribution = st.instr_or_clear(attribution);
                        d.modify_attributed_with(self.node(child), None, attribution);
                    } else {
                        let attribution = st.instr_or_clear(attribution);
                        d.retain_attributed_with(len, Some(&st.changed), attribution);
                    }
                } else if render_delete {
                    d.remove_with(piece_count(Some(content), len));
                } else if retain_content {
                    if let Some(child) = child.filter(|c| self.is_modified(*c)) {
                        d.modify_attributed_with(self.node(child), None, st.instr(None));
                    } else {
                        let count = piece_count(Some(content), len);
                        d.retain_attributed_with(count, Some(&st.changed), st.instr(None));
                    }
                }
            }
        }
    }

    /// Format state machine: everything that comes after a format marker is formatted by it, and
    /// attributed to whoever inserted (or deleted) the marker - unless it resets the formatting to
    /// the previously known unattributed value.
    fn process_format(
        &self,
        st: &mut WalkState,
        p: &AttributedFragment<F::Attr>,
        key: &Arc<str>,
        value: &Any,
        render: bool,
        retain: bool,
    ) {
        let attribution = Attribution::resolve(p.attrs.as_ref(), p.deleted);
        if attribution.is_some() && (p.deleted || !st.prev_unattributed.contains_key(key)) {
            let previous = if p.deleted {
                value.clone()
            } else {
                st.current.get(key).cloned().unwrap_or(Any::Null)
            };
            st.prev_unattributed.insert(key.clone(), previous);
        }

        // update formats
        if render {
            if p.deleted {
                // markers deleted by this change governed the consuming state until now
                if self.items.is_some() && !self.retain_inserts {
                    st.prev.insert(key.clone(), value.clone());
                }
            } else {
                if self.retain_inserts {
                    st.prev.insert(key.clone(), value.clone());
                }
                update_format(&mut st.current, key, value);
            }
            st.update_changed(key);
        } else if retain && !p.deleted {
            // an alive marker retained by this change: it doesn't change the formatting of the
            // content that follows it, it only describes it
            update_format(&mut st.current, key, value);
            st.changed.remove(key);
            st.prev.insert(key.clone(), value.clone());
        }

        // update attributions
        if attribution.is_none() && !st.prev_unattributed.contains_key(key) {
            return;
        }
        let mut formatting = st.used_attribution.clone().unwrap_or_default();
        let mut changed = formatting.format.take().unwrap_or_default();
        let current = st.current.get(key).unwrap_or(&Any::Null);
        let same_as_previous = st.prev_unattributed.get(key) == Some(current);
        match &attribution {
            None if !same_as_previous => { /* skip */ }
            Some(a) if !same_as_previous => {
                // an attributed marker opens attributed formatting of a key
                let by = changed.entry(key.clone()).or_insert(None);
                let mut list = by.take().unwrap_or_default();
                let attributors = if p.deleted { &a.delete } else { &a.insert };
                list.extend(attributors.iter().flatten().cloned());
                *by = Some(list);
                let at = if p.deleted { a.delete_at } else { a.insert_at };
                if at != 0 {
                    formatting.format_at = at;
                }
            }
            _ => {
                // an unattributed marker, or one resetting to the previous unattributed value,
                // ends the attributed range. A rendered one ends it explicitly in change renders.
                let explicit = attribution.is_some() && self.items.is_some() && (render || p.fresh);
                if explicit {
                    changed.insert(key.clone(), None);
                } else {
                    changed.remove(key);
                }
                st.prev_unattributed.remove(key);
            }
        }
        if changed.is_empty() {
            st.used_attribution = None;
        } else if let Some(a) = &attribution {
            let at = if p.deleted { a.delete_at } else { a.insert_at };
            if at != 0 {
                formatting.format_at = at;
            }
            formatting.format = Some(changed);
            st.used_attribution = Some(formatting);
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
        attribution: Option<Attribution>,
    ) {
        match content {
            ItemContent::Node(node) => {
                let value = self.node_value(NodePtr::from(node));
                d.insert_range_attributed_with(std::iter::once(value), Some(format), attribution)
            }
            ItemContent::String(str) => {
                // clock offsets are always UTF-16 based
                let (_, str) = split_str(str, offset as usize, OffsetKind::Utf16);
                let (str, _) = split_str(str, len as usize, OffsetKind::Utf16);
                d.insert_text_attributed_with(str, Some(format), attribution);
            }
            _ => {
                let mut buf = vec![Out::default(); len as usize];
                let read = content.read(offset as usize, &mut buf);
                buf.truncate(read);
                d.insert_range_attributed_with(buf, Some(format), attribution)
            }
        }
    }
}

/// Sets a format `value` under a given `key`, where a `null` value removes the format.
fn update_format(formats: &mut Attrs, key: &Arc<str>, value: &Any) {
    if let Any::Null = value {
        formats.remove(key);
    } else {
        formats.insert(key.clone(), value.clone());
    }
}

#[cfg(test)]
mod test {
    use crate::{Any, Delta, Doc, In, Out, any};
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq)]
    struct Inner {
        value: i32,
    }

    #[test]
    fn deserialize_default_shape() {
        let mut doc = Doc::new();
        let mut txn = doc.transact_mut();
        let p = Delta::with_name("p")
            .insert_attr("class", "x")
            .insert_text("ab");
        let Out::Node(p) = txn
            .node_mut("test")
            .unwrap()
            .push_back(In::Node(Box::new(p)))
        else {
            unreachable!()
        };
        let actual = txn.node(p.id).unwrap().to_json();
        let expected = any!({"name": "p", "attrs": {"class": "x"}, "children": ["a", "b"]});
        assert_eq!(actual, expected);
    }

    #[test]
    fn deserialize_children_as_seq() {
        let mut doc = Doc::new();
        let mut txn = doc.transact_mut();
        let mut root = txn.node_mut("test").unwrap();
        root.insert_attr("ignored", true);
        root.push_back(In::Node(Box::new(Delta::new().insert_attr("value", 1))));
        root.push_back(In::Node(Box::new(Delta::new().insert_attr("value", 2))));
        let actual = Vec::<Inner>::deserialize(txn.node("test").unwrap()).unwrap();
        assert_eq!(actual, vec![Inner { value: 1 }, Inner { value: 2 }]);
    }

    #[test]
    fn deserialize_attrs_as_struct() {
        #[derive(Debug, Deserialize, PartialEq)]
        struct Outer {
            title: String,
            inner: Inner,
            missing: Option<bool>,
        }

        let mut doc = Doc::new();
        let mut txn = doc.transact_mut();
        let mut root = txn.node_mut("test").unwrap();
        root.insert_attr("title", "hello");
        root.insert_attr(
            "inner",
            In::Node(Box::new(Delta::new().insert_attr("value", 1))),
        );
        root.push_back("ignored child");
        let actual = Outer::deserialize(txn.node("test").unwrap()).unwrap();
        let expected = Outer {
            title: "hello".into(),
            inner: Inner { value: 1 },
            missing: None,
        };
        assert_eq!(actual, expected);
    }
}
