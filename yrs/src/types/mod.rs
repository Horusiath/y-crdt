#![allow(unused_imports)]

use serde::{Serialize, Serializer};
use std::borrow::Borrow;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Formatter;
use std::ops::Deref;
use std::sync::Arc;

pub use map::Map;
pub use map::MapRef;
pub use text::Text;
pub use text::TextRef;

use crate::block::{ClientID, Item, ItemPtr, Prelim};
use crate::doc::Doc;
use crate::encoding::read::Error;
use crate::node::{Node, NodePtr};
use crate::transaction::Transaction;
use crate::types::array::{ArrayEvent, ArrayRef};
use crate::types::map::MapEvent;
use crate::types::text::TextEvent;
#[cfg(feature = "weak")]
use crate::types::weak::{LinkSource, WeakEvent, WeakRef};
use crate::types::xml::{XmlElementRef, XmlEvent, XmlTextEvent, XmlTextRef};
use crate::updates::decoder::{Decode, Decoder};
use crate::updates::encoder::{Encode, Encoder};
use crate::*;

pub mod array;
pub mod map;
pub mod text;
#[cfg(feature = "weak")]
pub mod weak;
pub mod xml;

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
    WeakLink(Arc<LinkSource>) = TYPE_REFS_WEAK,
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
    fn encode_weak_link<E: Encoder>(data: &LinkSource, encoder: &mut E) {
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
            IndexScope::Relative(id) | IndexScope::Nested(id) => {
                encoder.write_var(id.client.get());
                encoder.write_var(id.clock);
            }
            IndexScope::Root(name) => {
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
            IndexScope::Nested(id) => {
                encoder.write_var(id.client.get());
                encoder.write_var(id.clock);
            }
            IndexScope::Root(name) => {
                encoder.write_string(name);
            }
        }
    }

    #[cfg(feature = "weak")]
    fn decode_weak_link<D: Decoder>(decoder: &mut D) -> Result<Arc<LinkSource>, Error> {
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
                IndexScope::Root(name.into())
            } else {
                IndexScope::Nested(ID::new(
                    ClientID::new(decoder.read_var::<u64>()?),
                    decoder.read_var()?,
                ))
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
                IndexScope::Root(name.into())
            } else {
                IndexScope::Nested(ID::new(
                    ClientID::new(decoder.read_var::<u64>()?),
                    decoder.read_var()?,
                ))
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
        Ok(Arc::new(LinkSource::new(start, end)))
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
    type Event;

    /// Subscribes a given callback to be triggered whenever current y-type is changed.
    /// A callback is triggered whenever a transaction gets committed. This function does not
    /// trigger if changes have been observed by nested shared collections.
    ///
    /// All array-like event changes can be tracked by using [Event::delta] method.
    /// All map-like event changes can be tracked by using [Event::keys] method.
    /// All text-like event changes can be tracked by using [TextEvent::delta] method.
    ///
    /// Returns a [Subscription] which, when dropped, will unsubscribe current callback.
    fn observe<F>(&self, mut f: F) -> Subscription
    where
        F: FnMut(&Transaction<&Doc>, &Self::Event) + Send + Sync + 'static,
        Event: AsRef<Self::Event>,
    {
        let mut branch = NodePtr::from(self.as_ref());
        branch.observe(move |txn, e| {
            let mapped_event = e.as_ref();
            f(txn, mapped_event)
        })
    }

    fn observe_with<K, F>(&self, key: K, mut f: F)
    where
        K: Into<Origin>,
        F: FnMut(&Transaction<&Doc>, &Self::Event) + Send + Sync + 'static,
        Event: AsRef<Self::Event>,
    {
        let mut branch = NodePtr::from(self.as_ref());
        branch.observe_with(key.into(), move |txn, e| {
            let mapped_event = e.as_ref();
            f(txn, mapped_event)
        })
    }

    fn unobserve<K: Into<Origin>>(&self, key: K) -> bool {
        let mut branch = NodePtr::from(self.as_ref());
        branch.unobserve(&key.into())
    }
}

#[cfg(not(feature = "sync"))]
pub trait Observable: AsRef<Node> {
    type Event;

    fn observe<F>(&self, mut f: F) -> Subscription
    where
        F: FnMut(&Transaction<&Doc>, &Self::Event) + 'static,
        Event: AsRef<Self::Event>,
    {
        let mut branch = NodePtr::from(self.as_ref());
        branch.observe(move |txn, e| {
            let mapped_event = e.as_ref();
            f(txn, mapped_event)
        })
    }

    fn observe_with<K, F>(&self, key: K, mut f: F)
    where
        K: Into<Origin>,
        F: FnMut(&Transaction<&Doc>, &Self::Event) + 'static,
        Event: AsRef<Self::Event>,
    {
        let mut branch = NodePtr::from(self.as_ref());
        branch.observe_with(key.into(), move |txn, e| {
            let mapped_event = e.as_ref();
            f(txn, mapped_event)
        })
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

#[derive(Debug)]
pub(crate) struct Entries<'a> {
    iter: std::collections::hash_map::Iter<'a, Arc<str>, ItemPtr>,
    _doc: &'a Doc,
}

impl<'a> Entries<'a> {
    pub fn new(source: &'a HashMap<Arc<str>, ItemPtr>, doc: &'a Doc) -> Self {
        Entries {
            iter: source.iter(),
            _doc: doc,
        }
    }
}

impl<'a> Iterator for Entries<'a> {
    type Item = (&'a str, &'a Item);

    fn next(&mut self) -> Option<Self::Item> {
        let (mut key, mut ptr) = self.iter.next()?;
        while ptr.is_deleted() {
            (key, ptr) = self.iter.next()?;
        }
        Some((key, ptr))
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

pub(crate) struct ChangeSet<D> {
    added: HashSet<ID>,
    deleted: HashSet<ID>,
    delta: Vec<D>,
}

impl<D> ChangeSet<D> {
    pub fn new(added: HashSet<ID>, deleted: HashSet<ID>, delta: Vec<D>) -> Self {
        ChangeSet {
            added,
            deleted,
            delta,
        }
    }
}

/// A single change done over an array-component of shared data type.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    /// Determines a change that resulted in adding a consecutive number of new elements:
    /// - For [Array] it's a range of inserted elements.
    /// - For [XmlElement] it's a range of inserted child XML nodes.
    Added(Vec<Out>),

    /// Determines a change that resulted in removing a consecutive range of existing elements,
    /// either XML child nodes for [XmlElement] or various elements stored in an [Array].
    Removed(u32),

    /// Determines a number of consecutive unchanged elements. Used to recognize non-edited spaces
    /// between [Change::Added] and/or [Change::Removed] chunks.
    Retain(u32),
}

/// A single change done over a map-component of shared data type.
#[derive(Clone, PartialEq)]
pub enum EntryChange {
    /// Informs about a new value inserted under specified entry.
    Inserted(Out),

    /// Informs about a change of old value (1st field) to a new one (2nd field) under
    /// a corresponding entry.
    Updated(Out, Out),

    /// Informs about a removal of a corresponding entry - contains a removed value.
    Removed(Out),
}

impl std::fmt::Debug for EntryChange {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            EntryChange::Inserted(out) => write!(f, "Inserted({out:?})"),
            EntryChange::Updated(old, new) => write!(f, "Updated({old:?}, {new:?})"),
            EntryChange::Removed(out) => {
                f.write_str("Removed(")?;
                // To avoid panicking on removed references, output the type name rather than the reference.
                match out {
                    Out::Any(any) => write!(f, "{any:?}")?,
                    Out::YText(_) => write!(f, "YText")?,
                    Out::YArray(_) => write!(f, "YArray")?,
                    Out::YMap(_) => write!(f, "YMap")?,
                    Out::YXmlElement(_) => write!(f, "YXmlElement")?,
                    Out::YXmlFragment(_) => write!(f, "YXmlFragment")?,
                    Out::YXmlText(_) => write!(f, "YXmlText")?,
                    Out::Doc(_) => write!(f, "YDoc")?,
                    #[cfg(feature = "weak")]
                    Out::YWeakLink(_) => write!(f, "YWeakLink")?,
                    Out::UndefinedRef(_) => write!(f, "UndefinedRef")?,
                }
                f.write_str(")")
            }
        }
    }
}

/// An alias for map of attributes used as formatting parameters by [Text] and [XmlText] types.
pub type Attrs = HashMap<Arc<str>, Any>;

pub(crate) fn event_keys<D: Deref<Target = Doc>>(
    txn: &Transaction<D>,
    target: NodePtr,
    keys_changed: &HashSet<Option<Arc<str>>>,
) -> HashMap<Arc<str>, EntryChange> {
    let mut keys = HashMap::new();
    for opt in keys_changed.iter() {
        if let Some(key) = opt {
            let block = target.map.get(key.as_ref()).cloned();
            if let Some(item) = block.as_deref() {
                if item.id.clock >= txn.before_state().get(&item.id.client) {
                    let mut prev = item.left;
                    while let Some(p) = prev.as_deref() {
                        if !txn.has_added(&p.id) {
                            break;
                        }
                        prev = p.left;
                    }

                    if txn.has_deleted(&item.id) {
                        if let Some(prev) = prev.as_deref() {
                            if txn.has_deleted(&prev.id) {
                                let old_value = prev.content.get_last().unwrap_or_default();
                                keys.insert(key.clone(), EntryChange::Removed(old_value));
                            }
                        }
                    } else {
                        let new_value = item.content.get_last().unwrap();
                        if let Some(prev) = prev.as_deref() {
                            if txn.has_deleted(&prev.id) {
                                let old_value = prev.content.get_last().unwrap_or_default();
                                keys.insert(
                                    key.clone(),
                                    EntryChange::Updated(old_value, new_value),
                                );

                                continue;
                            }
                        }

                        keys.insert(key.clone(), EntryChange::Inserted(new_value));
                    }
                } else if txn.has_deleted(&item.id) {
                    let old_value = item.content.get_last().unwrap_or_default();
                    keys.insert(key.clone(), EntryChange::Removed(old_value));
                }
            }
        }
    }

    keys
}

pub(crate) fn event_change_set<D: Deref<Target = Doc>>(
    txn: &Transaction<D>,
    start: Option<ItemPtr>,
) -> ChangeSet<Change> {
    let mut added = HashSet::new();
    let mut deleted = HashSet::new();
    let mut delta = Vec::new();
    let mut last_op = None;

    let mut current = start;
    loop {
        if let Some(item) = current {
            if item.is_deleted() {
                if txn.has_deleted(&item.id) && !txn.has_added(&item.id) {
                    let removed = match last_op.take() {
                        None => 0,
                        Some(Change::Removed(c)) => c,
                        Some(other) => {
                            delta.push(other);
                            0
                        }
                    };
                    last_op = Some(Change::Removed(removed + item.len()));
                    deleted.insert(item.id);
                } // else nop
            } else {
                if txn.has_added(&item.id) {
                    let mut inserts = match last_op.take() {
                        None => Vec::with_capacity(item.len() as usize),
                        Some(Change::Added(values)) => values,
                        Some(other) => {
                            delta.push(other);
                            Vec::with_capacity(item.len() as usize)
                        }
                    };
                    inserts.append(&mut item.content.get_content());
                    last_op = Some(Change::Added(inserts));
                    added.insert(item.id);
                } else {
                    let retain = match last_op.take() {
                        None => 0,
                        Some(Change::Retain(c)) => c,
                        Some(other) => {
                            delta.push(other);
                            0
                        }
                    };
                    last_op = Some(Change::Retain(retain + item.len()));
                }
            }
        } else {
            break;
        }

        current = if let Some(i) = current.as_deref() {
            i.right
        } else {
            None
        };
    }

    match last_op.take() {
        None | Some(Change::Retain(_)) => { /* do nothing */ }
        Some(change) => delta.push(change),
    }

    ChangeSet::new(added, deleted, delta)
}

pub struct Events<'a>(Vec<&'a Event>);

impl<'a> Events<'a> {
    pub(crate) fn new(events: &Vec<&'a Event>) -> Self {
        let mut events = events.clone();
        events.sort_by(|&a, &b| {
            let path1 = a.path();
            let path2 = b.path();
            path1.len().cmp(&path2.len())
        });
        Events(events)
    }

    pub fn iter(&self) -> EventsIter {
        EventsIter(self.0.iter())
    }
}

pub struct EventsIter<'a>(std::slice::Iter<'a, &'a Event>);

impl<'a> Iterator for EventsIter<'a> {
    type Item = &'a Event;

    fn next(&mut self) -> Option<Self::Item> {
        let e = self.0.next()?;
        Some(e)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

impl<'a> ExactSizeIterator for EventsIter<'a> {
    fn len(&self) -> usize {
        self.0.len()
    }
}

/// Generalized wrapper around events fired by specialized shared data types.
pub enum Event {
    Text(TextEvent),
    Array(ArrayEvent),
    Map(MapEvent),
    XmlFragment(XmlEvent),
    XmlText(XmlTextEvent),
    #[cfg(feature = "weak")]
    Weak(WeakEvent),
}

impl AsRef<TextEvent> for Event {
    fn as_ref(&self) -> &TextEvent {
        if let Event::Text(e) = self {
            e
        } else {
            panic!("subscribed callback expected TextRef collection");
        }
    }
}

impl AsRef<ArrayEvent> for Event {
    fn as_ref(&self) -> &ArrayEvent {
        if let Event::Array(e) = self {
            e
        } else {
            panic!("subscribed callback expected ArrayRef collection");
        }
    }
}

impl AsRef<MapEvent> for Event {
    fn as_ref(&self) -> &MapEvent {
        if let Event::Map(e) = self {
            e
        } else {
            panic!("subscribed callback expected MapRef collection");
        }
    }
}

impl AsRef<XmlTextEvent> for Event {
    fn as_ref(&self) -> &XmlTextEvent {
        if let Event::XmlText(e) = self {
            e
        } else {
            panic!("subscribed callback expected XmlTextRef collection");
        }
    }
}

impl AsRef<XmlEvent> for Event {
    fn as_ref(&self) -> &XmlEvent {
        if let Event::XmlFragment(e) = self {
            e
        } else {
            panic!("subscribed callback expected Xml node");
        }
    }
}

#[cfg(feature = "weak")]
impl AsRef<WeakEvent> for Event {
    fn as_ref(&self) -> &WeakEvent {
        if let Event::Weak(e) = self {
            e
        } else {
            panic!("subscribed callback expected WeakRef reference");
        }
    }
}

impl Event {
    pub(crate) fn set_current_target(&mut self, target: NodePtr) {
        match self {
            Event::Text(e) => e.current_target = target,
            Event::Array(e) => e.current_target = target,
            Event::Map(e) => e.current_target = target,
            Event::XmlText(e) => e.current_target = target,
            Event::XmlFragment(e) => e.current_target = target,
            #[cfg(feature = "weak")]
            Event::Weak(e) => e.current_target = target,
        }
    }

    /// Returns a path from root type to a shared type which triggered current [Event]. This path
    /// consists of string names or indexes, which can be used to access nested type.
    pub fn path(&self) -> Path {
        match self {
            Event::Text(e) => e.path(),
            Event::Array(e) => e.path(),
            Event::Map(e) => e.path(),
            Event::XmlText(e) => e.path(),
            Event::XmlFragment(e) => e.path(),
            #[cfg(feature = "weak")]
            Event::Weak(e) => e.path(),
        }
    }

    /// Returns a shared data types which triggered current [Event].
    pub fn target(&self) -> Out {
        match self {
            Event::Text(e) => Out::YText(e.target().clone()),
            Event::Array(e) => Out::YArray(e.target().clone()),
            Event::Map(e) => Out::YMap(e.target().clone()),
            Event::XmlText(e) => Out::YXmlText(e.target().clone()),
            Event::XmlFragment(e) => match e.target() {
                XmlOut::Element(n) => Out::YXmlElement(n.clone()),
                XmlOut::Fragment(n) => Out::YXmlFragment(n.clone()),
                XmlOut::Text(n) => Out::YXmlText(n.clone()),
            },
            #[cfg(feature = "weak")]
            Event::Weak(e) => Out::YWeakLink(e.as_target().clone()),
        }
    }
}

pub trait ToJson {
    /// Converts all contents of a current type into a JSON-like representation.
    fn to_json<D: Deref<Target = Doc>>(&self, txn: &Transaction<D>) -> Any;
}
