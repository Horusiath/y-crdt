use crate::block::{Block, Item, ItemContent, ItemPosition, ItemPtr};
use crate::encoding::read::Error;
use crate::event::Event;
use crate::updates::decoder::{Decode, Decoder};
use crate::updates::encoder::{Encode, Encoder};
use crate::{
    Any, Assoc, ClientID, Doc, ID, In, IndexScope, Observer, Origin, Out, StickyIndex,
    Subscription, Transaction, TransactionMut,
};
use serde::{Deserialize, Serialize, Serializer};
use std::borrow::Borrow;
use std::collections::{HashMap, HashSet, VecDeque};
use std::convert::TryFrom;
use std::fmt::Formatter;
use std::hash::{Hash, Hasher};
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;
use std::sync::Arc;

pub type Attrs = HashMap<Arc<str>, Any>;

/// A batch of [Event]s bubbled up from nested shared collections, passed to deep observers.
/// They are sorted by the length of their [Event::path], so that top-most events come first.
pub type Events<'txn> = [Event<'txn>];

/// A wrapper around [Node] cell, supplied with a bunch of convenience methods to operate on both
/// map-like and array-like contents of a [Node].
#[repr(transparent)]
#[derive(Clone, Copy, Hash)]
pub struct NodePtr(NonNull<Node>);

unsafe impl Send for NodePtr {}
unsafe impl Sync for NodePtr {}

impl NodePtr {
    pub(crate) fn trigger<'txn>(
        &mut self,
        txn: &'txn Transaction<&'txn Doc>,
        subs: HashSet<Option<Arc<str>>>,
    ) -> Option<Event<'txn>> {
        let e = self.make_event(txn, subs)?;
        self.observers.trigger(|fun| fun(txn, &e));
        Some(e)
    }

    pub(crate) fn trigger_deep<'txn>(
        &mut self,
        txn: &'txn Transaction<&'txn Doc>,
        e: &Events<'txn>,
    ) {
        self.deep_observers.trigger(|fun| fun(txn, e));
    }
}

impl TryFrom<ItemPtr> for NodePtr {
    type Error = ItemPtr;

    fn try_from(value: ItemPtr) -> Result<Self, Self::Error> {
        if let ItemContent::Node(branch) = &value.content {
            Ok(NodePtr::from(branch))
        } else {
            Err(value)
        }
    }
}

impl Into<TypePtr> for NodePtr {
    fn into(self) -> TypePtr {
        TypePtr::Node(self)
    }
}

impl Into<Origin> for NodePtr {
    fn into(self) -> Origin {
        let addr = self.0.as_ptr() as usize;
        let bytes = addr.to_be_bytes();
        Origin::from(bytes.as_ref())
    }
}

impl AsRef<Node> for NodePtr {
    fn as_ref(&self) -> &Node {
        self.deref()
    }
}

impl AsMut<Node> for NodePtr {
    fn as_mut(&mut self) -> &mut Node {
        self.deref_mut()
    }
}

impl Deref for NodePtr {
    type Target = Node;

    fn deref(&self) -> &Self::Target {
        unsafe { self.0.as_ref() }
    }
}

impl DerefMut for NodePtr {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.0.as_mut() }
    }
}

impl<'a> From<&'a mut Box<Node>> for NodePtr {
    fn from(branch: &'a mut Box<Node>) -> Self {
        let ptr = NonNull::from(branch.as_ref());
        NodePtr(ptr)
    }
}

impl<'a> From<&'a Box<Node>> for NodePtr {
    fn from(branch: &'a Box<Node>) -> Self {
        let b: &Node = &*branch;

        let ptr = unsafe { NonNull::new_unchecked(b as *const Node as *mut Node) };
        NodePtr(ptr)
    }
}

impl<'a> From<&'a Node> for NodePtr {
    fn from(branch: &'a Node) -> Self {
        let ptr = unsafe { NonNull::new_unchecked(branch as *const Node as *mut Node) };
        NodePtr(ptr)
    }
}

impl Into<Out> for NodePtr {
    /// Converts current branch data into a [Out]. Since branches represent only complex types,
    /// the result is always [Out::Node] pointing at a logical identifier of this branch.
    fn into(self) -> Out {
        Out::Node(self.id())
    }
}

impl Eq for NodePtr {}

#[cfg(not(test))]
impl PartialEq for NodePtr {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.0.as_ptr(), other.0.as_ptr())
    }
}

#[cfg(test)]
impl PartialEq for NodePtr {
    fn eq(&self, other: &Self) -> bool {
        if NonNull::eq(&self.0, &other.0) {
            true
        } else {
            let a: &Node = self.deref();
            let b: &Node = other.deref();
            a.eq(b)
        }
    }
}

impl std::fmt::Debug for NodePtr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.id())
    }
}

/// Node describes a content of a complex Yrs data structures, such as arrays or maps.
pub struct Node {
    /// A pointer to a first block of a indexed sequence component of this branch node. If `None`,
    /// it means that sequence is empty or a branch doesn't act as an indexed sequence. Indexed
    /// sequences include:
    ///
    /// - [Array]: all elements are stored as a double linked list, while the head of the list is
    ///   kept in this field.
    /// - [XmlElement]: this field acts as a head to a first child element stored within current XML
    ///   node.
    /// - [Text] and [XmlText]: this field point to a first chunk of text appended to collaborative
    ///   text data structure.
    pub(crate) start: Option<ItemPtr>,

    /// A map component of this branch node, used by some of the specialized complex types
    /// including:
    ///
    /// - [Map]: all of the map elements are based on this field. The value of each entry points
    ///   to the last modified value.
    /// - [XmlElement]: this field stores attributes assigned to a given XML node.
    pub(crate) map: HashMap<Arc<str>, ItemPtr>,

    /// Unique identifier of a current branch node. It can be contain either a named string - which
    /// means, this branch is a root-level complex data structure - or a block identifier. In latter
    /// case it means, that this branch is a complex type (eg. Map or Array) nested inside of
    /// another complex type.
    pub(crate) item: Option<ItemPtr>,

    /// For root-level types, this is a name of a branch.
    pub(crate) name: Option<Arc<str>>,

    /// A length of an indexed sequence component of a current branch node. Map component elements
    /// are computed on demand.
    pub block_len: u32,

    pub content_len: u32,

    /// An identifier of an underlying complex data type (eg. is it an Array or a Map).
    pub(crate) type_ref: TypeRef,

    pub(crate) has_formatting: bool,

    pub(crate) observers: Observer<ObserveFn>,

    pub(crate) deep_observers: Observer<DeepObserveFn>,
}

#[cfg(feature = "sync")]
type ObserveFn = Box<dyn FnMut(&Transaction<&Doc>, &Event) + Send + Sync + 'static>;
#[cfg(feature = "sync")]
type DeepObserveFn = Box<dyn FnMut(&Transaction<&Doc>, &Events) + Send + Sync + 'static>;

#[cfg(not(feature = "sync"))]
type ObserveFn = Box<dyn FnMut(&Transaction<&Doc>, &Event) + 'static>;
#[cfg(not(feature = "sync"))]
type DeepObserveFn = Box<dyn FnMut(&Transaction<&Doc>, &Events) + 'static>;

impl std::fmt::Debug for Node {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

impl Eq for Node {}

impl PartialEq for Node {
    fn eq(&self, other: &Self) -> bool {
        self.item == other.item
            && self.start == other.start
            && self.map == other.map
            && self.block_len == other.block_len
            && self.type_ref == other.type_ref
    }
}

impl Node {
    pub fn new(name: Option<Arc<str>>, type_ref: TypeRef) -> Box<Self> {
        Box::new(Self {
            start: None,
            map: HashMap::default(),
            block_len: 0,
            content_len: 0,
            item: None,
            name,
            type_ref,
            observers: Observer::default(),
            deep_observers: Observer::default(),
            has_formatting: false,
        })
    }

    pub fn is_deleted(&self) -> bool {
        match self.item {
            Some(ptr) => ptr.is_deleted(),
            None => false,
        }
    }

    pub fn id(&self) -> NodeID {
        if let Some(ptr) = self.item {
            NodeID::Nested(ptr.id)
        } else if let Some(name) = &self.name {
            NodeID::Root(name.clone())
        } else {
            unreachable!("Could not get ID for branch")
        }
    }

    pub fn as_subdoc_guid(&self) -> Option<crate::Uuid> {
        let item = self.item?;
        item.content.as_subdoc_guid().cloned()
    }

    /// Returns an identifier of an underlying complex data type (eg. is it an Array or a Map).
    pub fn type_ref(&self) -> &TypeRef {
        &self.type_ref
    }

    pub(crate) fn repair_type_ref(&mut self, type_ref: TypeRef) {
        if self.type_ref == TypeRef::Undefined {
            self.type_ref = type_ref;
        }
    }

    /// Returns a length of an indexed sequence component of a current branch node.
    /// Map component elements are computed on demand.
    pub fn len(&self) -> u32 {
        self.block_len
    }

    pub fn content_len(&self) -> u32 {
        self.content_len
    }

    /// Get iterator over Block entries of an array component of a current root type.
    /// Deleted blocks are skipped by this iterator.
    pub(crate) fn iter<'a>(&'a self, doc: &'a Doc) -> Iter<'a> {
        Iter::new(self.start.as_ref(), doc)
    }

    /// Returns a materialized value of non-deleted entry under a given `key` of a map component
    /// of a current root type.
    pub(crate) fn get(&self, _doc: &Doc, key: &str) -> Option<Out> {
        let item = self.map.get(key)?;
        if !item.is_deleted() {
            item.content.get_last()
        } else {
            None
        }
    }

    /// Given an `index` parameter, returns an item content reference which contains that index
    /// together with an offset inside of this content, which points precisely to an `index`
    /// location within wrapping item content.
    /// If `index` was outside of the array component boundary of current branch node, `None` will
    /// be returned.
    pub(crate) fn get_at(&self, mut index: u32) -> Option<(&ItemContent, usize)> {
        let mut ptr = self.start.as_ref();
        while let Some(item) = ptr.map(ItemPtr::deref) {
            let len = item.len();
            if !item.is_deleted() && item.is_countable() {
                if index < len {
                    return Some((&item.content, index as usize));
                }

                index -= len;
            }
            ptr = item.right.as_ref();
        }

        None
    }

    /// Removes an entry under given `key` of a map component of a current root type, returning
    /// a materialized representation of value stored underneath if entry existed prior deletion.
    pub(crate) fn remove(&self, txn: &mut TransactionMut, key: &str) -> Option<Out> {
        let item = *self.map.get(key)?;
        let prev = if !item.is_deleted() {
            item.content.get_last()
        } else {
            None
        };
        txn.delete(item);
        prev
    }

    /// Returns a first non-deleted item from an array component of a current root type.
    pub(crate) fn first(&self) -> Option<&Item> {
        let mut ptr = self.start.as_ref();
        while let Some(item) = ptr.map(ItemPtr::deref) {
            if item.is_deleted() {
                ptr = item.right.as_ref();
            } else {
                return Some(item);
            }
        }

        None
    }

    /// Given an `index` and start block `ptr`, returns a pair of block pointers.
    ///
    /// If `index` happens to point inside of an existing block content, such block will be split at
    /// position of an `index`. In such case left tuple value contains end of a block pointer on
    /// a left side of an `index` and a pointer to a block directly on the right side of an `index`.
    ///
    /// If `index` point to the end of a block and no splitting is necessary, tuple will return only
    /// left side (beginning of a block), while right side will be `None`.
    ///
    /// If `index` is outside the range of an array component of current branch node, both tuple
    /// values will be `None`.
    fn index_to_ptr(
        txn: &mut TransactionMut,
        mut ptr: Option<ItemPtr>,
        mut index: u32,
    ) -> (Option<ItemPtr>, Option<ItemPtr>) {
        let encoding = txn.doc.options.offset_kind;
        while let Some(item) = ptr {
            let content_len = item.content_len(encoding);
            if !item.is_deleted() && item.is_countable() {
                if index == content_len {
                    let left = ptr;
                    let right = item.right.clone();
                    return (left, right);
                } else if index < content_len {
                    let index = if let ItemContent::String(s) = &item.content {
                        s.block_offset(index, encoding)
                    } else {
                        index
                    };
                    let right = txn.doc.blocks.split_block(item, index, encoding);
                    return (ptr, right);
                }
                index -= content_len;
            }
            ptr = item.right.clone();
        }
        (None, None)
    }
    /// Removes up to a `len` of countable elements from current branch sequence, starting at the
    /// given `index`. Returns number of removed elements.
    pub(crate) fn remove_at(&self, txn: &mut TransactionMut, index: u32, len: u32) -> u32 {
        let mut remaining = len;
        let start = { self.start };
        let (_, mut ptr) = if index == 0 {
            (None, start)
        } else {
            Node::index_to_ptr(txn, start, index)
        };
        while remaining > 0 {
            if let Some(item) = ptr {
                let encoding = txn.doc().options.offset_kind;
                if !item.is_deleted() {
                    let content_len = item.content_len(encoding);
                    let (l, r) = if remaining < content_len {
                        let offset = if let ItemContent::String(s) = &item.content {
                            s.block_offset(remaining, encoding)
                        } else {
                            remaining
                        };
                        remaining = 0;
                        let new_right = txn.doc.blocks.split_block(item, offset, encoding);
                        (item, new_right)
                    } else {
                        remaining -= content_len;
                        (item, item.right.clone())
                    };
                    txn.delete(l);
                    ptr = r;
                } else {
                    ptr = item.right.clone();
                }
            } else {
                break;
            }
        }

        len - remaining
    }

    /// Inserts a preliminary `value` into a current branch indexed sequence component at the given
    /// `index`. Returns an item reference created as a result of this operation.
    pub(crate) fn insert_at(
        &self,
        txn: &mut TransactionMut,
        index: u32,
        value: In,
    ) -> Option<ItemPtr> {
        let (start, parent) = {
            if index <= self.len() {
                (self.start, NodePtr::from(self))
            } else {
                panic!("Cannot insert item at index over the length of an array")
            }
        };
        let (left, right) = if index == 0 {
            (None, self.start)
        } else {
            Node::index_to_ptr(txn, start, index)
        };
        let pos = ItemPosition {
            parent: parent.into(),
            left,
            right,
            index: 0,
            current_attrs: None,
        };

        txn.create_item(&pos, value, None)
    }

    pub(crate) fn path(from: NodePtr, to: NodePtr) -> Path {
        let parent = from;
        let mut child = to;
        let mut path = VecDeque::default();
        while let Some(item) = &child.item {
            if parent.item == child.item {
                break;
            }
            let item_id = item.id.clone();
            let parent_sub = item.parent_sub.clone();
            child = *item.parent.as_node().unwrap();
            if let Some(parent_sub) = parent_sub {
                // parent is map-ish
                path.push_front(PathSegment::Key(parent_sub));
            } else {
                // parent is array-ish
                let mut i = 0;
                let mut c = child.start;
                while let Some(ptr) = c {
                    if ptr.id() == &item_id {
                        break;
                    }
                    if !ptr.is_deleted() && ptr.is_countable() {
                        i += ptr.len();
                    }
                    c = ptr.right;
                }
                path.push_front(PathSegment::Index(i));
            }
        }
        path
    }

    #[cfg(feature = "sync")]
    pub fn observe<F>(&mut self, f: F) -> Subscription
    where
        F: FnMut(&Transaction<&Doc>, &Event) + Send + Sync + 'static,
    {
        self.observers.subscribe(Box::new(f))
    }

    #[cfg(not(feature = "sync"))]
    pub fn observe<F>(&mut self, f: F) -> Subscription
    where
        F: FnMut(&Transaction<&Doc>, &Event) + 'static,
    {
        self.observers.subscribe(Box::new(f))
    }

    #[cfg(feature = "sync")]
    pub fn observe_with<F>(&mut self, key: Origin, f: F)
    where
        F: FnMut(&Transaction<&Doc>, &Event) + Send + Sync + 'static,
    {
        self.observers.subscribe_with(key, Box::new(f))
    }

    #[cfg(not(feature = "sync"))]
    pub fn observe_with<F>(&mut self, key: Origin, f: F)
    where
        F: FnMut(&Transaction<&Doc>, &Event) + 'static,
    {
        self.observers.subscribe_with(key, Box::new(f))
    }

    pub fn unobserve(&mut self, key: &Origin) -> bool {
        self.observers.unsubscribe(key)
    }

    #[cfg(feature = "sync")]
    pub fn observe_deep<F>(&mut self, f: F) -> Subscription
    where
        F: FnMut(&Transaction<&Doc>, &Events) + Send + Sync + 'static,
    {
        self.deep_observers.subscribe(Box::new(f))
    }

    #[cfg(not(feature = "sync"))]
    pub fn observe_deep<F>(&mut self, f: F) -> Subscription
    where
        F: FnMut(&Transaction<&Doc>, &Events) + 'static,
    {
        self.deep_observers.subscribe(Box::new(f))
    }

    #[cfg(feature = "sync")]
    pub fn observe_deep_with<F>(&mut self, key: Origin, f: F)
    where
        F: FnMut(&Transaction<&Doc>, &Events) + Send + Sync + 'static,
    {
        self.deep_observers.subscribe_with(key, Box::new(f))
    }

    #[cfg(not(feature = "sync"))]
    pub fn observe_deep_with<F>(&mut self, key: Origin, f: F)
    where
        F: FnMut(&Transaction<&Doc>, &Events) + 'static,
    {
        self.deep_observers.subscribe_with(key, Box::new(f))
    }

    pub(crate) fn is_parent_of(&self, mut ptr: Option<ItemPtr>) -> bool {
        while let Some(i) = ptr.as_deref() {
            if let Some(parent) = i.parent.as_node() {
                if parent.deref() == self {
                    return true;
                }
                ptr = parent.item;
            } else {
                break;
            }
        }
        false
    }

    pub(crate) fn make_event<'txn>(
        &self,
        txn: &'txn Transaction<&'txn Doc>,
        keys: HashSet<Option<Arc<str>>>,
    ) -> Option<Event<'txn>> {
        todo!()
    }
}

pub(crate) struct Iter<'a> {
    ptr: Option<&'a ItemPtr>,
    _doc: &'a Doc,
}

impl<'a> Iter<'a> {
    fn new(ptr: Option<&'a ItemPtr>, doc: &'a Doc) -> Self {
        Iter { ptr, _doc: doc }
    }
}

impl<'a> Iterator for Iter<'a> {
    type Item = &'a Item;

    fn next(&mut self) -> Option<Self::Item> {
        let item = self.ptr.take()?;
        self.ptr = item.right.as_ref();
        Some(item)
    }
}

/// An unique logical identifier of a shared collection. Can be shared across document boundaries
/// to reference to the same logical entity across different replicas of a document.
#[derive(Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum NodeID {
    Nested(ID),
    Root(Arc<str>),
}

impl NodeID {
    #[inline]
    pub fn root<N: Into<Arc<str>>>(name: N) -> Self {
        NodeID::Root(name.into())
    }

    #[inline]
    pub fn nested(block_id: ID) -> Self {
        NodeID::Nested(block_id)
    }

    #[inline]
    pub fn get_root<K: Borrow<str>>(doc: &Doc, name: K) -> Option<NodePtr> {
        doc.get_type(name)
    }

    pub fn get_nested(doc: &Doc, id: &ID) -> Option<NodePtr> {
        let block = doc.blocks.get_block(id)?;
        if let Block::Item(block) = block.as_ref() {
            if let ItemContent::Node(branch) = &block.content {
                return Some(NodePtr::from(&*branch));
            }
        }
        None
    }

    pub fn get_node(&self, doc: &Doc) -> Option<NodePtr> {
        match self {
            NodeID::Root(name) => Self::get_root(doc, name.as_ref()),
            NodeID::Nested(id) => Self::get_nested(doc, id),
        }
    }
}

impl From<&str> for NodeID {
    #[inline]
    fn from(name: &str) -> Self {
        NodeID::Root(name.into())
    }
}

impl From<String> for NodeID {
    #[inline]
    fn from(name: String) -> Self {
        NodeID::Root(name.into())
    }
}

impl From<Arc<str>> for NodeID {
    #[inline]
    fn from(name: Arc<str>) -> Self {
        NodeID::Root(name)
    }
}

impl From<ID> for NodeID {
    #[inline]
    fn from(id: ID) -> Self {
        NodeID::Nested(id)
    }
}

impl std::fmt::Display for NodeID {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            NodeID::Nested(id) => write!(f, "{}", id),
            NodeID::Root(name) => write!(f, "'{}'", name),
        }
    }
}

impl std::fmt::Debug for NodeID {
    #[inline]
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

/// Type ref identifier for an [ArrayRef] type.
pub const TYPE_REFS_ARRAY: u8 = 0;

/// Type ref identifier for a [MapRef] type.
pub const TYPE_REFS_MAP: u8 = 1;

/// Type ref identifier for a [TextRef] type.
pub const TYPE_REFS_TEXT: u8 = 2;

/// Type ref identifier for a [XmlElementRef] type.
pub const TYPE_REFS_XML_ELEMENT: u8 = 3;

/// Type ref identifier for a [XmlFragmentRef] type. Used for compatibility.
pub const TYPE_REFS_XML_FRAGMENT: u8 = 4;

/// Type ref identifier for a [XmlHookRef] type. Used for compatibility.
pub const TYPE_REFS_XML_HOOK: u8 = 5;

/// Type ref identifier for a [XmlTextRef] type.
pub const TYPE_REFS_XML_TEXT: u8 = 6;

/// Type ref identifier for a [WeakRef] type.
pub const TYPE_REFS_WEAK: u8 = 7;

/// Type ref identifier for a [DocRef] type.
pub const TYPE_REFS_DOC: u8 = 9;

/// Placeholder type ref identifier for non-specialized AbstractType. Used only for root-level types
/// which have been integrated from remote peers before they were defined locally.
pub const TYPE_REFS_UNDEFINED: u8 = 15;

#[repr(u8)]
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum TypeRef {
    Array = TYPE_REFS_ARRAY,
    Map = TYPE_REFS_MAP,
    Text = TYPE_REFS_TEXT,
    XmlElement(Arc<str>) = TYPE_REFS_XML_ELEMENT,
    XmlFragment = TYPE_REFS_XML_FRAGMENT,
    XmlHook = TYPE_REFS_XML_HOOK,
    XmlText = TYPE_REFS_XML_TEXT,
    SubDoc = TYPE_REFS_DOC,
    #[cfg(feature = "weak")]
    WeakLink(Arc<crate::weak::LinkSource>) = TYPE_REFS_WEAK,
    Undefined = TYPE_REFS_UNDEFINED,
}

impl TypeRef {
    pub fn kind(&self) -> u8 {
        match self {
            TypeRef::Array => TYPE_REFS_ARRAY,
            TypeRef::Map => TYPE_REFS_MAP,
            TypeRef::Text => TYPE_REFS_TEXT,
            TypeRef::XmlElement(_) => TYPE_REFS_XML_ELEMENT,
            TypeRef::XmlFragment => TYPE_REFS_XML_FRAGMENT,
            TypeRef::XmlHook => TYPE_REFS_XML_HOOK,
            TypeRef::XmlText => TYPE_REFS_XML_TEXT,
            TypeRef::SubDoc => TYPE_REFS_DOC,
            #[cfg(feature = "weak")]
            TypeRef::WeakLink(_) => TYPE_REFS_WEAK,
            TypeRef::Undefined => TYPE_REFS_UNDEFINED,
        }
    }

    #[cfg(feature = "weak")]
    fn encode_weak_link<E: Encoder>(data: &crate::weak::LinkSource, encoder: &mut E) {
        encoder.write_type_ref(TYPE_REFS_WEAK);
        let mut info = 0u8;
        let is_single = data.is_single();
        if !is_single {
            info |= WEAK_REF_FLAGS_QUOTE;
        };
        if data.quote_start.is_root() || data.quote_end.is_root() {
            info |= WEAK_REF_FLAGS_PARENT_ROOT;
        }
        if !data.quote_start.is_relative() {
            info |= WEAK_REF_FLAGS_START_UNBOUNDED;
        }
        if !data.quote_end.is_relative() {
            info |= WEAK_REF_FLAGS_END_UNBOUNDED;
        }
        if data.quote_start.assoc == Assoc::After {
            info |= WEAK_REF_FLAGS_START_ASSOC;
        }
        if data.quote_end.assoc == Assoc::After {
            info |= WEAK_REF_FLAGS_END_ASSOC;
        }
        encoder.write_u8(info);
        match data.quote_start.scope() {
            IndexScope::Relative(id) | IndexScope::Absolute(NodeID::Nested(id)) => {
                encoder.write_var(id.client.get());
                encoder.write_var(id.clock);
            }
            IndexScope::Absolute(NodeID::Root(name)) => {
                encoder.write_string(name);
            }
        }

        match data.quote_end.scope() {
            IndexScope::Relative(id) if !is_single => {
                encoder.write_var(id.client.get());
                encoder.write_var(id.clock);
            }
            IndexScope::Relative(id) => {
                // for single element id is the same as start so we can infer it
            }
            IndexScope::Absolute(NodeID::Nested(id)) => {
                encoder.write_var(id.client.get());
                encoder.write_var(id.clock);
            }
            IndexScope::Absolute(NodeID::Root(name)) => {
                encoder.write_string(name);
            }
        }
    }

    #[cfg(feature = "weak")]
    fn decode_weak_link<D: Decoder>(
        decoder: &mut D,
    ) -> Result<Arc<crate::weak::LinkSource>, Error> {
        let flags = decoder.read_u8()?;
        let is_single = flags & WEAK_REF_FLAGS_QUOTE == 0;
        let start_assoc = if flags & WEAK_REF_FLAGS_START_ASSOC == WEAK_REF_FLAGS_START_ASSOC {
            Assoc::After
        } else {
            Assoc::Before
        };
        let end_assoc = if flags & WEAK_REF_FLAGS_END_ASSOC == WEAK_REF_FLAGS_END_ASSOC {
            Assoc::After
        } else {
            Assoc::Before
        };
        let is_start_unbounded =
            flags & WEAK_REF_FLAGS_START_UNBOUNDED == WEAK_REF_FLAGS_START_UNBOUNDED;
        let is_end_unbounded = flags & WEAK_REF_FLAGS_END_UNBOUNDED == WEAK_REF_FLAGS_END_UNBOUNDED;
        let is_parent_root = flags & WEAK_REF_FLAGS_PARENT_ROOT == WEAK_REF_FLAGS_PARENT_ROOT;
        let start_scope = if is_start_unbounded {
            if is_parent_root {
                let name = decoder.read_string()?;
                IndexScope::Absolute(NodeID::Root(name.into()))
            } else {
                IndexScope::Absolute(NodeID::Nested(ID::new(
                    ClientID::new(decoder.read_var::<u64>()?),
                    decoder.read_var()?,
                )))
            }
        } else {
            IndexScope::Relative(ID::new(
                ClientID::new(decoder.read_var::<u64>()?),
                decoder.read_var()?,
            ))
        };

        let end_scope = if is_end_unbounded {
            if is_parent_root {
                let name = decoder.read_string()?;
                IndexScope::Absolute(NodeID::Root(name.into()))
            } else {
                IndexScope::Absolute(NodeID::Nested(ID::new(
                    ClientID::new(decoder.read_var::<u64>()?),
                    decoder.read_var()?,
                )))
            }
        } else if is_single {
            start_scope.clone()
        } else {
            IndexScope::Relative(ID::new(
                ClientID::new(decoder.read_var::<u64>()?),
                decoder.read_var()?,
            ))
        };
        let start = StickyIndex::new(start_scope, start_assoc);
        let end = StickyIndex::new(end_scope, end_assoc);
        Ok(Arc::new(crate::weak::LinkSource::new(start, end)))
    }
}

impl std::fmt::Display for TypeRef {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            TypeRef::Array => write!(f, "Array"),
            TypeRef::Map => write!(f, "Map"),
            TypeRef::Text => write!(f, "Text"),
            TypeRef::XmlElement(name) => write!(f, "XmlElement({})", name),
            TypeRef::XmlFragment => write!(f, "XmlFragment"),
            TypeRef::XmlHook => write!(f, "XmlHook"),
            TypeRef::XmlText => write!(f, "XmlText"),
            TypeRef::SubDoc => write!(f, "Doc"),
            #[cfg(feature = "weak")]
            TypeRef::WeakLink(_) => write!(f, "WeakRef"),
            TypeRef::Undefined => write!(f, "(undefined)"),
        }
    }
}

/// Marks is weak ref is quotation spanning over multiple elements.
const WEAK_REF_FLAGS_QUOTE: u8 = 0b0000_0001;
/// Marks is start boundary of weak ref is [Assoc::After].
const WEAK_REF_FLAGS_START_ASSOC: u8 = 0b0000_0010;
/// Marks is end boundary of weak ref is [Assoc::After].
const WEAK_REF_FLAGS_END_ASSOC: u8 = 0b0000_0100;
/// Marks if start boundary of weak ref is unbounded.
const WEAK_REF_FLAGS_START_UNBOUNDED: u8 = 0b0000_1000;
/// Marks if end boundary of weak ref is unbounded.
const WEAK_REF_FLAGS_END_UNBOUNDED: u8 = 0b0001_0000;
/// Marks if weak ref references a root type. Only needed for both sides unbounded elements.
const WEAK_REF_FLAGS_PARENT_ROOT: u8 = 0b0010_0000;

impl Encode for TypeRef {
    fn encode<E: Encoder>(&self, encoder: &mut E) {
        match self {
            TypeRef::Array => encoder.write_type_ref(TYPE_REFS_ARRAY),
            TypeRef::Map => encoder.write_type_ref(TYPE_REFS_MAP),
            TypeRef::Text => encoder.write_type_ref(TYPE_REFS_TEXT),
            TypeRef::XmlElement(name) => {
                encoder.write_type_ref(TYPE_REFS_XML_ELEMENT);
                encoder.write_key(&name);
            }
            TypeRef::XmlFragment => encoder.write_type_ref(TYPE_REFS_XML_FRAGMENT),
            TypeRef::XmlHook => encoder.write_type_ref(TYPE_REFS_XML_HOOK),
            TypeRef::XmlText => encoder.write_type_ref(TYPE_REFS_XML_TEXT),
            TypeRef::SubDoc => encoder.write_type_ref(TYPE_REFS_DOC),
            #[cfg(feature = "weak")]
            TypeRef::WeakLink(data) => Self::encode_weak_link(data, encoder),
            TypeRef::Undefined => encoder.write_type_ref(TYPE_REFS_UNDEFINED),
        }
    }
}

impl Decode for TypeRef {
    fn decode<D: Decoder>(decoder: &mut D) -> Result<Self, Error> {
        let type_ref = decoder.read_type_ref()?;
        match type_ref {
            TYPE_REFS_ARRAY => Ok(TypeRef::Array),
            TYPE_REFS_MAP => Ok(TypeRef::Map),
            TYPE_REFS_TEXT => Ok(TypeRef::Text),
            TYPE_REFS_XML_ELEMENT => Ok(TypeRef::XmlElement(decoder.read_key()?)),
            TYPE_REFS_XML_FRAGMENT => Ok(TypeRef::XmlFragment),
            TYPE_REFS_XML_HOOK => Ok(TypeRef::XmlHook),
            TYPE_REFS_XML_TEXT => Ok(TypeRef::XmlText),
            TYPE_REFS_DOC => Ok(TypeRef::SubDoc),
            #[cfg(feature = "weak")]
            TYPE_REFS_WEAK => {
                let source = Self::decode_weak_link(decoder)?;
                Ok(TypeRef::WeakLink(source))
            }
            TYPE_REFS_UNDEFINED => Ok(TypeRef::Undefined),
            _ => Err(Error::UnexpectedValue),
        }
    }
}

#[cfg(feature = "sync")]
pub trait Observable: AsRef<Node> {
    /// Subscribes a given callback to be triggered whenever current y-type is changed.
    /// A callback is triggered whenever a transaction gets committed. This function does not
    /// trigger if changes have been observed by nested shared collections.
    ///
    /// All array-like event changes can be tracked by using [Event::delta] method.
    /// All map-like event changes can be tracked by using [Event::keys] method.
    ///
    /// Returns a [Subscription] which, when dropped, will unsubscribe current callback.
    fn observe<F>(&self, f: F) -> Subscription
    where
        F: FnMut(&Transaction<&Doc>, &Event) + Send + Sync + 'static,
    {
        let mut branch = NodePtr::from(self.as_ref());
        branch.observe(f)
    }

    fn observe_with<K, F>(&self, key: K, f: F)
    where
        K: Into<Origin>,
        F: FnMut(&Transaction<&Doc>, &Event) + Send + Sync + 'static,
    {
        let mut branch = NodePtr::from(self.as_ref());
        branch.observe_with(key.into(), f)
    }

    fn unobserve<K: Into<Origin>>(&self, key: K) -> bool {
        let mut branch = NodePtr::from(self.as_ref());
        branch.unobserve(&key.into())
    }
}

#[cfg(not(feature = "sync"))]
pub trait Observable: AsRef<Node> {
    fn observe<F>(&self, f: F) -> Subscription
    where
        F: FnMut(&Transaction<&Doc>, &Event) + 'static,
    {
        let mut branch = NodePtr::from(self.as_ref());
        branch.observe(f)
    }

    fn observe_with<K, F>(&self, key: K, f: F)
    where
        K: Into<Origin>,
        F: FnMut(&Transaction<&Doc>, &Event) + 'static,
    {
        let mut branch = NodePtr::from(self.as_ref());
        branch.observe_with(key.into(), f)
    }

    fn unobserve<K: Into<Origin>>(&self, key: K) -> bool {
        let mut branch = NodePtr::from(self.as_ref());
        branch.unobserve(&key.into())
    }
}

/// Trait implemented by all Y-types, allowing for observing events which are emitted by
/// nested types.
#[cfg(feature = "sync")]
pub trait DeepObservable: AsRef<Node> {
    fn observe_deep<F>(&self, f: F) -> Subscription
    where
        F: FnMut(&Transaction<&Doc>, &Events) + Send + Sync + 'static,
    {
        let mut branch = NodePtr::from(self.as_ref());
        branch.observe_deep(f)
    }

    fn observe_deep_with<K, F>(&self, key: K, f: F)
    where
        K: Into<Origin>,
        F: FnMut(&Transaction<&Doc>, &Events) + Send + Sync + 'static,
    {
        let mut branch = NodePtr::from(self.as_ref());
        branch.observe_deep_with(key.into(), f)
    }

    fn unobserve_deep<K: Into<Origin>>(&self, key: K) -> bool {
        let mut branch = NodePtr::from(self.as_ref());
        branch.deep_observers.unsubscribe(&key.into())
    }
}

#[cfg(not(feature = "sync"))]
pub trait DeepObservable: AsRef<Node> {
    fn observe_deep<F>(&self, f: F) -> Subscription
    where
        F: FnMut(&Transaction<&Doc>, &Events) + Send + Sync + 'static,
    {
        let mut branch = NodePtr::from(self.as_ref());
        branch.observe_deep(f)
    }

    fn observe_deep_with<K, F>(&self, key: K, f: F)
    where
        K: Into<Origin>,
        F: FnMut(&Transaction<&Doc>, &Events) + 'static,
    {
        let mut branch = NodePtr::from(self.as_ref());
        branch.observe_deep_with(key.into(), f)
    }

    fn unobserve_deep<K: Into<Origin>>(&self, key: K) -> bool {
        let mut branch = NodePtr::from(self.as_ref());
        branch.deep_observers.unsubscribe(&key.into())
    }
}

impl std::fmt::Display for Node {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.type_ref() {
            TypeRef::Array => {
                if let Some(ptr) = self.start {
                    write!(f, "YArray(start: {})", ptr)
                } else {
                    write!(f, "YArray")
                }
            }
            TypeRef::Map => {
                write!(f, "YMap(")?;
                let mut iter = self.map.iter();
                if let Some((k, v)) = iter.next() {
                    write!(f, "'{}': {}", k, v)?;
                }
                while let Some((k, v)) = iter.next() {
                    write!(f, ", '{}': {}", k, v)?;
                }
                write!(f, ")")
            }
            TypeRef::Text => {
                if let Some(ptr) = self.start.as_ref() {
                    write!(f, "YText(start: {})", ptr)
                } else {
                    write!(f, "YText")
                }
            }
            TypeRef::XmlFragment => {
                write!(f, "YXmlFragment")?;
                if let Some(start) = self.start.as_ref() {
                    write!(f, "(start: {})", start)?;
                }
                Ok(())
            }
            TypeRef::XmlElement(name) => {
                write!(f, "YXmlElement('{}',", name)?;
                if let Some(start) = self.start.as_ref() {
                    write!(f, "(start: {})", start)?;
                }
                if !self.map.is_empty() {
                    write!(f, " {{")?;
                    let mut iter = self.map.iter();
                    if let Some((k, v)) = iter.next() {
                        write!(f, "'{}': {}", k, v)?;
                    }
                    while let Some((k, v)) = iter.next() {
                        write!(f, ", '{}': {}", k, v)?;
                    }
                    write!(f, "}}")?;
                }
                Ok(())
            }
            TypeRef::XmlHook => {
                write!(f, "YXmlHook(")?;
                let mut iter = self.map.iter();
                if let Some((k, v)) = iter.next() {
                    write!(f, "'{}': {}", k, v)?;
                }
                while let Some((k, v)) = iter.next() {
                    write!(f, ", '{}': {}", k, v)?;
                }
                write!(f, ")")
            }
            TypeRef::XmlText => {
                if let Some(ptr) = self.start {
                    write!(f, "YXmlText(start: {})", ptr)
                } else {
                    write!(f, "YXmlText")
                }
            }
            TypeRef::SubDoc => {
                write!(f, "Subdoc")
            }
            #[cfg(feature = "weak")]
            TypeRef::WeakLink(w) => {
                if w.is_single() {
                    write!(f, "WeakRef({})", w.quote_start)
                } else {
                    write!(f, "WeakRef({}..{})", w.quote_start, w.quote_end)
                }
            }
            TypeRef::Undefined => {
                write!(f, "UnknownRef")?;
                if let Some(start) = self.start.as_ref() {
                    write!(f, "(start: {})", start)?;
                }
                if !self.map.is_empty() {
                    write!(f, " {{")?;
                    let mut iter = self.map.iter();
                    if let Some((k, v)) = iter.next() {
                        write!(f, "'{}': {}", k, v)?;
                    }
                    while let Some((k, v)) = iter.next() {
                        write!(f, ", '{}': {}", k, v)?;
                    }
                    write!(f, "}}")?;
                }
                Ok(())
            }
        }
    }
}

/// Type pointer - used to localize a complex [Node] node within a scope of a document store.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum TypePtr {
    /// Temporary value - used only when block is deserialized right away, but had not been
    /// integrated into block store yet. As part of block integration process, items are
    /// repaired and their fields (including parent) are being rewired.
    Unknown,

    /// Pointer to another block. Used in nested data types ie. YMap containing another YMap.
    Node(NodePtr),

    /// Temporary state representing top-level type.
    Named(Arc<str>),

    /// Temporary state representing nested-level type.
    ID(ID),
}

impl TypePtr {
    pub(crate) fn as_node(&self) -> Option<&NodePtr> {
        if let TypePtr::Node(ptr) = self {
            Some(ptr)
        } else {
            None
        }
    }
}

impl std::fmt::Display for TypePtr {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            TypePtr::Unknown => write!(f, "unknown"),
            TypePtr::Node(ptr) => {
                if let Some(i) = ptr.item {
                    write!(f, "{}", i.id())
                } else {
                    write!(f, "null")
                }
            }
            TypePtr::ID(id) => write!(f, "{}", id),
            TypePtr::Named(name) => write!(f, "{}", name),
        }
    }
}

/// A path describing nesting structure between shared collections containing each other. It's a
/// collection of segments which refer to either index (in case of [Array] or [XmlElement]) or
/// string key (in case of [Map]) where successor shared collection can be found within subsequent
/// parent types.
pub type Path = VecDeque<PathSegment>;

/// A single segment of a [Path].
#[derive(Debug, Clone, PartialEq)]
pub enum PathSegment {
    /// Key segments are used to inform how to access child shared collections within a [Map] types.
    Key(Arc<str>),

    /// Index segments are used to inform how to access child shared collections within an [Array]
    /// or [XmlElement] types.
    Index(u32),
}

impl Serialize for PathSegment {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            PathSegment::Key(key) => serializer.serialize_str(&*key),
            PathSegment::Index(i) => serializer.serialize_u32(*i),
        }
    }
}
