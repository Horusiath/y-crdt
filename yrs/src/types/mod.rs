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
