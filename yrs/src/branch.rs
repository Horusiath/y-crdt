use crate::block::{Item, ItemContent, ItemPosition, ItemPtr, Prelim};
use crate::store::WeakStoreRef;
use crate::types::array::ArrayEvent;
use crate::types::map::MapEvent;
use crate::types::text::TextEvent;
use crate::types::weak::WeakEvent;
use crate::types::xml::{XmlEvent, XmlTextEvent};
use crate::types::{
    DeepEventsSubscription, Entries, Event, Events, Observers, Path, PathSegment, TypeRef,
};
use crate::{
    ArrayRef, MapRef, Observer, Origin, ReadTxn, SubscriptionId, TextRef, TransactionMut, Value,
    WeakRef, XmlElementRef, XmlFragmentRef, XmlTextRef, ID,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Formatter;
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;
use std::sync::Arc;

/// Branch describes a content of a complex Yrs data structures, such as arrays or maps.
pub struct Branch {
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

    pub(crate) store: Option<WeakStoreRef>,

    /// A length of an indexed sequence component of a current branch node. Map component elements
    /// are computed on demand.
    pub block_len: u32,

    pub content_len: u32,

    /// An identifier of an underlying complex data type (eg. is it an Array or a Map).
    pub(crate) type_ref: TypeRef,

    pub(crate) observers: Option<Observers>,

    pub(crate) deep_observers: Option<Observer<Arc<dyn Fn(&TransactionMut, &Events)>>>,
}

impl std::fmt::Debug for Branch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

impl Eq for Branch {}

impl PartialEq for Branch {
    fn eq(&self, other: &Self) -> bool {
        self.item == other.item
            && self.start == other.start
            && self.map == other.map
            && self.block_len == other.block_len
            && self.type_ref == other.type_ref
    }
}

impl Branch {
    pub fn new(type_ref: TypeRef) -> Box<Self> {
        Box::new(Self {
            start: None,
            map: HashMap::default(),
            block_len: 0,
            content_len: 0,
            item: None,
            store: None,
            type_ref,
            observers: None,
            deep_observers: None,
        })
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

    /// Get iterator over (String, Block) entries of a map component of a current root type.
    /// Deleted blocks are skipped by this iterator.
    pub(crate) fn entries<'a, T: ReadTxn + 'a>(&'a self, txn: &'a T) -> Entries<'a, &'a T, T> {
        Entries::from_ref(&self.map, txn)
    }

    /// Get iterator over Block entries of an array component of a current root type.
    /// Deleted blocks are skipped by this iterator.
    pub(crate) fn iter<'a, T: ReadTxn + 'a>(&'a self, txn: &'a T) -> Iter<'a, T> {
        Iter::new(self.start.as_ref(), txn)
    }

    /// Returns a materialized value of non-deleted entry under a given `key` of a map component
    /// of a current root type.
    pub(crate) fn get<T: ReadTxn>(&self, _txn: &T, key: &str) -> Option<Value> {
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
    pub(crate) fn remove(&self, txn: &mut TransactionMut, key: &str) -> Option<Value> {
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
    /// If `index` is outside of the range of an array component of current branch node, both tuple
    /// values will be `None`.
    fn index_to_ptr(
        txn: &mut TransactionMut,
        mut ptr: Option<ItemPtr>,
        mut index: u32,
    ) -> (Option<ItemPtr>, Option<ItemPtr>) {
        let encoding = txn.store.options.offset_kind;
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
                    let right = txn.store.blocks.split_block(item, index, encoding);
                    if let Some(_) = item.moved {
                        if let Some(src) = right {
                            if let Some(&prev_dst) = txn.prev_moved.get(&item) {
                                txn.prev_moved.insert(src, prev_dst);
                            }
                        }
                    }
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
            Branch::index_to_ptr(txn, start, index)
        };
        while remaining > 0 {
            if let Some(item) = ptr {
                let encoding = txn.store().options.offset_kind;
                if !item.is_deleted() {
                    let content_len = item.content_len(encoding);
                    let (l, r) = if remaining < content_len {
                        let offset = if let ItemContent::String(s) = &item.content {
                            s.block_offset(remaining, encoding)
                        } else {
                            remaining
                        };
                        remaining = 0;
                        let new_right = txn.store.blocks.split_block(item, offset, encoding);
                        if let Some(_) = item.moved {
                            if let Some(src) = new_right {
                                if let Some(&prev_dst) = txn.prev_moved.get(&item) {
                                    txn.prev_moved.insert(src, prev_dst);
                                }
                            }
                        }
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
    pub(crate) fn insert_at<V: Prelim>(
        &self,
        txn: &mut TransactionMut,
        index: u32,
        value: V,
    ) -> ItemPtr {
        let (start, parent) = {
            if index <= self.len() {
                (self.start, BranchPtr::from(self))
            } else {
                panic!("Cannot insert item at index over the length of an array")
            }
        };
        let (left, right) = if index == 0 {
            (None, None)
        } else {
            Branch::index_to_ptr(txn, start, index)
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

    pub(crate) fn path(from: BranchPtr, to: BranchPtr) -> Path {
        let parent = from;
        let mut child = to;
        let mut path = VecDeque::default();
        while let Some(item) = &child.item {
            if parent.item == child.item {
                break;
            }
            let item_id = item.id.clone();
            let parent_sub = item.parent_sub.clone();
            child = *item.parent.as_branch().unwrap();
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

    pub fn observe_deep<F>(&mut self, f: F) -> DeepEventsSubscription
    where
        F: Fn(&TransactionMut, &Events) -> () + 'static,
    {
        let eh = self.deep_observers.get_or_insert_with(Observer::default);
        eh.subscribe(Arc::new(f))
    }

    pub fn unobserve_deep(&mut self, subscription_id: SubscriptionId) {
        if let Some(eh) = self.deep_observers.as_mut() {
            eh.unsubscribe(subscription_id);
        }
    }

    pub(crate) fn is_parent_of(&self, mut ptr: Option<ItemPtr>) -> bool {
        while let Some(i) = ptr.as_deref() {
            if let Some(parent) = i.parent.as_branch() {
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
}

pub(crate) struct Iter<'a, T> {
    ptr: Option<&'a ItemPtr>,
    _txn: &'a T,
}

impl<'a, T: ReadTxn> Iter<'a, T> {
    fn new(ptr: Option<&'a ItemPtr>, txn: &'a T) -> Self {
        Iter { ptr, _txn: txn }
    }
}

impl<'a, T: ReadTxn> Iterator for Iter<'a, T> {
    type Item = &'a Item;

    fn next(&mut self) -> Option<Self::Item> {
        let item = self.ptr.take()?;
        self.ptr = item.right.as_ref();
        Some(item)
    }
}

/// A wrapper around [Branch] cell, supplied with a bunch of convenience methods to operate on both
/// map-like and array-like contents of a [Branch].
#[repr(transparent)]
#[derive(Clone, Copy, Hash)]
pub struct BranchPtr(NonNull<Branch>);

impl BranchPtr {
    pub(crate) fn trigger(
        &self,
        txn: &TransactionMut,
        subs: HashSet<Option<Arc<str>>>,
    ) -> Option<Event> {
        if let Some(observers) = self.observers.as_ref() {
            Some(observers.publish(*self, txn, subs))
        } else {
            match self.type_ref {
                TypeRef::Array => Some(Event::Array(ArrayEvent::new(*self))),
                TypeRef::Map => Some(Event::Map(MapEvent::new(*self, subs))),
                TypeRef::Text => Some(Event::Text(TextEvent::new(*self))),
                TypeRef::XmlText => Some(Event::XmlText(XmlTextEvent::new(*self, subs))),
                TypeRef::XmlElement(_) | TypeRef::XmlFragment => {
                    Some(Event::XmlFragment(XmlEvent::new(*self, subs)))
                }
                #[cfg(feature = "weak")]
                TypeRef::WeakLink(_) => Some(Event::Weak(WeakEvent::new(*self))),
                TypeRef::XmlHook | TypeRef::SubDoc | TypeRef::Undefined => None,
            }
        }
    }

    pub(crate) fn trigger_deep(&self, txn: &TransactionMut, e: &Events) {
        if let Some(o) = self.deep_observers.as_ref() {
            for fun in o.callbacks() {
                fun(txn, e);
            }
        }
    }
}

impl Into<TypePtr> for BranchPtr {
    fn into(self) -> TypePtr {
        TypePtr::Branch(self)
    }
}

impl Into<Origin> for BranchPtr {
    fn into(self) -> Origin {
        let addr = self.0.as_ptr() as usize;
        let bytes = addr.to_be_bytes();
        Origin::from(bytes.as_ref())
    }
}

impl AsRef<Branch> for BranchPtr {
    fn as_ref(&self) -> &Branch {
        self.deref()
    }
}

impl AsMut<Branch> for BranchPtr {
    fn as_mut(&mut self) -> &mut Branch {
        self.deref_mut()
    }
}

impl Deref for BranchPtr {
    type Target = Branch;

    fn deref(&self) -> &Self::Target {
        unsafe { self.0.as_ref() }
    }
}

impl DerefMut for BranchPtr {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.0.as_mut() }
    }
}

impl<'a> From<&'a mut Box<Branch>> for BranchPtr {
    fn from(branch: &'a mut Box<Branch>) -> Self {
        let ptr = NonNull::from(branch.as_mut());
        BranchPtr(ptr)
    }
}

impl<'a> From<&'a Box<Branch>> for BranchPtr {
    fn from(branch: &'a Box<Branch>) -> Self {
        let b: &Branch = &*branch;

        let ptr = unsafe { NonNull::new_unchecked(b as *const Branch as *mut Branch) };
        BranchPtr(ptr)
    }
}

impl<'a> From<&'a Branch> for BranchPtr {
    fn from(branch: &'a Branch) -> Self {
        let ptr = unsafe { NonNull::new_unchecked(branch as *const Branch as *mut Branch) };
        BranchPtr(ptr)
    }
}

impl Into<Value> for BranchPtr {
    /// Converts current branch data into a [Value]. It uses a type ref information to resolve,
    /// which value variant is a correct one for this branch. Since branch represent only complex
    /// types [Value::Any] will never be returned from this method.
    fn into(self) -> Value {
        match self.type_ref() {
            TypeRef::Array => Value::YArray(ArrayRef::from(self)),
            TypeRef::Map => Value::YMap(MapRef::from(self)),
            TypeRef::Text => Value::YText(TextRef::from(self)),
            TypeRef::XmlElement(_) => Value::YXmlElement(XmlElementRef::from(self)),
            TypeRef::XmlFragment => Value::YXmlFragment(XmlFragmentRef::from(self)),
            TypeRef::XmlText => Value::YXmlText(XmlTextRef::from(self)),
            //TYPE_REFS_XML_HOOK => Value::YXmlHook(XmlHookRef::from(self)),
            #[cfg(feature = "weak")]
            TypeRef::WeakLink(_) => Value::YWeakLink(WeakRef::from(self)),
            _ => Value::UndefinedRef(self),
        }
    }
}

impl Eq for BranchPtr {}

#[cfg(not(test))]
impl PartialEq for BranchPtr {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.0.as_ptr(), other.0.as_ptr())
    }
}

#[cfg(test)]
impl PartialEq for BranchPtr {
    fn eq(&self, other: &Self) -> bool {
        if NonNull::eq(&self.0, &other.0) {
            true
        } else {
            let a: &Branch = self.deref();
            let b: &Branch = other.deref();
            a.eq(b)
        }
    }
}

impl std::fmt::Debug for BranchPtr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let branch: &Branch = &self;
        write!(f, "{}", branch)
    }
}

/// Type pointer - used to localize a complex [Branch] node within a scope of a document store.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum TypePtr {
    /// Temporary value - used only when block is deserialized right away, but had not been
    /// integrated into block store yet. As part of block integration process, items are
    /// repaired and their fields (including parent) are being rewired.
    Unknown,

    /// Pointer to another block. Used in nested data types ie. YMap containing another YMap.
    Branch(BranchPtr),

    /// Temporary state representing top-level type.
    Named(Arc<str>),

    /// Temporary state representing nested-level type.
    ID(ID),
}

impl TypePtr {
    pub(crate) fn as_branch(&self) -> Option<&BranchPtr> {
        if let TypePtr::Branch(ptr) = self {
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
            TypePtr::Branch(ptr) => {
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
