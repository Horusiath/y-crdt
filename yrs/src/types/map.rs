use crate::block::{EmbedPrelim, ItemContent, ItemPosition, ItemPtr, Prelim};
use crate::encoding::read::Error;
use crate::encoding::serde::from_any;
use crate::transaction::{Transaction, TransactionMut};
use crate::types::{EntryChange, In, Node, NodePtr, Out, Path, ToJson, TypeRef, event_keys};
use crate::*;
use serde::de::DeserializeOwned;
use std::cell::UnsafeCell;
use std::collections::{HashMap, HashSet};
use std::convert::{TryFrom, TryInto};
use std::iter::FromIterator;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

/// Collection used to store key-value entries in an unordered manner. Keys are always represented
/// as UTF-8 strings. Values can be any value type supported by Yrs: JSON-like primitives as well as
/// shared data types.
///
/// In terms of conflict resolution, [MapRef] uses logical last-write-wins principle, meaning the past
/// updates are automatically overridden and discarded by newer ones, while concurrent updates made
/// by different peers are resolved into a single value using document id seniority to establish
/// order.
///
/// # Example
///
/// ```rust
/// use yrs::{any, Doc, Map, MapPrelim};
/// use yrs::types::ToJson;
///
/// let mut doc = Doc::new();
/// let map = doc.get_or_insert_map("map");
/// let mut txn = doc.transact_mut();
///
/// // insert value
/// map.insert(&mut txn, "key1", "value1");
///
/// // insert nested shared type
/// let nested = map.insert(&mut txn, "key2", MapPrelim::from([("inner", "value2")]));
/// nested.insert(&mut txn, "inner2", 100);
///
/// assert_eq!(map.to_json(&txn), any!({
///   "key1": "value1",
///   "key2": {
///     "inner": "value2",
///     "inner2": 100
///   }
/// }));
///
/// // get value
/// assert_eq!(map.get(&txn, "key1"), Some("value1".into()));
///
/// // remove entry
/// map.remove(&mut txn, "key1");
/// assert_eq!(map.get(&txn, "key1"), None);
/// ```
#[repr(transparent)]
#[derive(Debug, Clone)]
pub struct MapRef(NodePtr);

impl RootRef for MapRef {
    fn type_ref() -> TypeRef {
        TypeRef::Map
    }
}
impl SharedRef for MapRef {}
impl Map for MapRef {}

impl DeepObservable for MapRef {}
impl Observable for MapRef {
    type Event = MapEvent;
}

impl ToJson for MapRef {
    fn to_json<D: Deref<Target = Doc>>(&self, txn: &Transaction<D>) -> Any {
        let inner = self.0;
        let mut res = HashMap::new();
        for (key, item) in inner.map.iter() {
            if !item.is_deleted() {
                let last = item.content.get_last().unwrap_or(Out::Any(Any::Null));
                res.insert(key.to_string(), last.to_json(txn));
            }
        }
        Any::from(res)
    }
}

impl AsRef<Node> for MapRef {
    fn as_ref(&self) -> &Node {
        self.0.deref()
    }
}

impl Eq for MapRef {}
impl PartialEq for MapRef {
    fn eq(&self, other: &Self) -> bool {
        self.0.id() == other.0.id()
    }
}

impl TryFrom<ItemPtr> for MapRef {
    type Error = ItemPtr;

    fn try_from(value: ItemPtr) -> Result<Self, Self::Error> {
        if let Some(branch) = value.clone().as_node() {
            Ok(MapRef::from(branch))
        } else {
            Err(value)
        }
    }
}

impl TryFrom<Out> for MapRef {
    type Error = Out;

    fn try_from(value: Out) -> Result<Self, Self::Error> {
        match value {
            Out::YMap(value) => Ok(value),
            other => Err(other),
        }
    }
}

impl AsPrelim for MapRef {
    type Prelim = MapPrelim;

    fn as_prelim<D: Deref<Target = Doc>>(&self, txn: &Transaction<D>) -> Self::Prelim {
        let mut prelim = HashMap::with_capacity(self.len(txn) as usize);
        for (key, &ptr) in self.0.map.iter() {
            if !ptr.is_deleted() {
                if let Ok(value) = Out::try_from(ptr) {
                    prelim.insert(key.clone(), value.as_prelim(txn));
                }
            }
        }
        MapPrelim(prelim)
    }
}

impl DefaultPrelim for MapRef {
    type Prelim = MapPrelim;

    #[inline]
    fn default_prelim() -> Self::Prelim {
        MapPrelim::default()
    }
}

pub trait Map: AsRef<Node> + Sized {
    /// Returns a number of entries stored within current map.
    fn len<D: Deref<Target = Doc>>(&self, _txn: &Transaction<D>) -> u32 {
        let mut len = 0;
        let inner = self.as_ref();
        for item in inner.map.values() {
            //TODO: maybe it would be better to just cache len in the map itself?
            if !item.is_deleted() {
                len += 1;
            }
        }
        len
    }

    /// Returns an iterator that enables to traverse over all keys of entries stored within
    /// current map. These keys are not ordered.
    fn keys<'a, D: Deref<Target = Doc>>(&'a self, txn: &'a Transaction<D>) -> Keys<'a> {
        Keys(MapIter::new(self.as_ref(), txn.doc()))
    }

    /// Returns an iterator that enables to traverse over all values stored within current map.
    fn values<'a, D: Deref<Target = Doc>>(&'a self, txn: &'a Transaction<D>) -> Values<'a> {
        Values(MapIter::new(self.as_ref(), txn.doc()))
    }

    /// Returns an iterator that enables to traverse over all entries - tuple of key-value pairs -
    /// stored within current map.
    fn iter<'a, D: Deref<Target = Doc>>(&'a self, txn: &'a Transaction<D>) -> MapIter<'a> {
        MapIter::new(self.as_ref(), txn.doc())
    }

    fn into_iter<'a, D: Deref<Target = Doc>>(self, txn: &'a Transaction<D>) -> MapIntoIter<'a> {
        let branch_ptr = NodePtr::from(self.as_ref());
        MapIntoIter::new(branch_ptr, txn.doc())
    }

    /// Inserts a new `value` under given `key` into current map. Returns an integrated value.
    fn insert<K, V>(&self, txn: &mut TransactionMut, key: K, value: V) -> V::Return
    where
        K: Into<Arc<str>>,
        V: Prelim,
    {
        let key = key.into();
        let pos = {
            let inner = self.as_ref();
            let left = inner.map.get(&key);
            ItemPosition {
                parent: NodePtr::from(inner).into(),
                left: left.cloned(),
                right: None,
                index: 0,
                current_attrs: None,
            }
        };

        let ptr = txn
            .create_item(&pos, value, Some(key))
            .expect("Cannot insert empty value");
        if let Ok(integrated) = ptr.try_into() {
            integrated
        } else {
            panic!("Defect: unexpected integrated type")
        }
    }

    /// Tries to update a value stored under a given `key` within current map, if it's different
    /// from the current one. Returns `true` if the value was updated, `false` otherwise.
    ///
    /// The main difference from [Map::insert] is that this method will not insert a new value if
    /// it's the same as the current one. It's important distinction when dealing with shared types,
    /// as inserting an element will force previous value to be tombstoned, causing minimal memory
    /// overhead.
    ///
    /// # Example
    ///
    /// ```rust
    /// use yrs::{Doc, Map};
    ///
    /// let mut doc = Doc::new();
    /// let mut txn = doc.transact_mut();
    /// let map = txn.get_or_insert_map("map");
    ///
    /// assert!(map.try_update(&mut txn, "key", 1)); // created a new entry
    /// assert!(!map.try_update(&mut txn, "key", 1)); // unchanged value doesn't trigger inserts...
    /// assert!(map.try_update(&mut txn, "key", 2)); // ... but changed one does
    /// ```
    fn try_update<K, V>(&self, txn: &mut TransactionMut, key: K, value: V) -> bool
    where
        K: Into<Arc<str>>,
        V: Into<Any>,
    {
        let key = key.into();
        let value = value.into();
        let branch = self.as_ref();
        if let Some(item) = branch.map.get(&key) {
            if !item.is_deleted() {
                if let ItemContent::Any(content) = &item.content {
                    if let Some(last) = content.last() {
                        if last == &value {
                            return false;
                        }
                    }
                }
            }
        }

        self.insert(txn, key, value);
        true
    }

    /// Returns an existing instance of a type stored under a given `key` within current map.
    /// If the given entry was not found, has been deleted or its type is different from expected,
    /// that entry will be reset to a given type and its reference will be returned.
    fn get_or_init<K, V>(&self, txn: &mut TransactionMut, key: K) -> V
    where
        K: Into<Arc<str>>,
        V: DefaultPrelim + TryFrom<Out>,
    {
        let key = key.into();
        let branch = self.as_ref();
        if let Some(value) = branch.get(txn.doc(), &key) {
            if let Ok(value) = value.try_into() {
                return value;
            }
        }
        let value = V::default_prelim();
        self.insert(txn, key, value)
    }

    /// Removes a stored within current map under a given `key`. Returns that value or `None` if
    /// no entry with a given `key` was present in current map.
    ///
    /// ### Removing nested shared types
    ///
    /// In case when a nested shared type (eg. [MapRef], [ArrayRef], [TextRef]) is being removed,
    /// all of its contents will also be deleted recursively. A returned value will contain a
    /// reference to a current removed shared type (which will be empty due to all of its elements
    /// being deleted), **not** the content prior the removal.
    fn remove(&self, txn: &mut TransactionMut, key: &str) -> Option<Out> {
        let ptr = NodePtr::from(self.as_ref());
        ptr.remove(txn, key)
    }

    /// Returns [WeakPrelim] to a given `key`, if it exists in a current map.
    #[cfg(feature = "weak")]
    fn link<D: Deref<Target = Doc>>(
        &self,
        _txn: &Transaction<D>,
        key: &str,
    ) -> Option<crate::WeakPrelim<Self>> {
        let ptr = NodePtr::from(self.as_ref());
        let block = ptr.map.get(key)?;
        let start = StickyIndex::from_id(block.id().clone(), Assoc::Before);
        let end = StickyIndex::from_id(block.id().clone(), Assoc::After);
        let link = crate::WeakPrelim::new(start, end);
        Some(link)
    }

    /// Returns a value stored under a given `key` within current map, or `None` if no entry
    /// with such `key` existed.
    fn get<D: Deref<Target = Doc>>(&self, txn: &Transaction<D>, key: &str) -> Option<Out> {
        let ptr = NodePtr::from(self.as_ref());
        ptr.get(txn.doc(), key)
    }

    /// Returns a value stored under a given `key` within current map, deserializing it into expected
    /// type if found. If value was not found, the `Any::Null` will be substituted and deserialized
    /// instead (i.e. into instance of `Option` type, if so desired).
    ///
    /// # Example
    ///
    /// ```rust
    /// use yrs::{Doc, In, Map, MapPrelim};
    ///
    /// let mut doc = Doc::new();
    /// let mut txn = doc.transact_mut();
    /// let map = txn.get_or_insert_map("map");
    ///
    /// // insert a multi-nested shared refs
    /// let alice = map.insert(&mut txn, "Alice", MapPrelim::from([
    ///   ("name", In::from("Alice")),
    ///   ("age", In::from(30)),
    ///   ("address", MapPrelim::from([
    ///     ("city", In::from("London")),
    ///     ("street", In::from("Baker st.")),
    ///   ]).into())
    /// ]));
    ///
    /// // define Rust types to map from the shared refs
    ///
    /// #[derive(Debug, PartialEq, serde::Deserialize)]
    /// struct Person {
    ///   name: String,
    ///   age: u32,
    ///   address: Option<Address>,
    /// }
    ///
    /// #[derive(Debug, PartialEq, serde::Deserialize)]
    /// struct Address {
    ///   city: String,
    ///   street: String,
    /// }
    ///
    /// // retrieve and deserialize the value across multiple shared refs
    /// let alice: Person = map.get_as(&txn, "Alice").unwrap();
    /// assert_eq!(alice, Person {
    ///   name: "Alice".to_string(),
    ///   age: 30,
    ///   address: Some(Address {
    ///     city: "London".to_string(),
    ///     street: "Baker st.".to_string(),
    ///   })
    /// });
    ///
    /// // try to retrieve value that doesn't exist
    /// let bob: Option<Person> = map.get_as(&txn, "Bob").unwrap();
    /// assert_eq!(bob, None);
    /// ```
    fn get_as<D, V>(&self, txn: &Transaction<D>, key: &str) -> Result<V, Error>
    where
        D: Deref<Target = Doc>,
        V: DeserializeOwned,
    {
        let ptr = NodePtr::from(self.as_ref());
        let out = ptr.get(txn.doc(), key).unwrap_or(Out::Any(Any::Null));
        //TODO: we could probably optimize this step by not serializing to intermediate Any value
        let any = out.to_json(txn);
        from_any(&any)
    }

    /// Checks if an entry with given `key` can be found within current map.
    fn contains_key<D: Deref<Target = Doc>>(&self, _txn: &Transaction<D>, key: &str) -> bool {
        if let Some(item) = self.as_ref().map.get(key) {
            !item.is_deleted()
        } else {
            false
        }
    }

    /// Clears the contents of current map, effectively removing all of its entries.
    fn clear(&self, txn: &mut TransactionMut) {
        for (_, ptr) in self.as_ref().map.iter() {
            txn.delete(ptr.clone());
        }
    }
}

pub struct MapIter<'a> {
    iter: std::collections::hash_map::Iter<'a, Arc<str>, ItemPtr>,
    _doc: &'a Doc,
}

impl<'a> MapIter<'a> {
    pub fn new(branch: &'a Node, doc: &'a Doc) -> Self {
        MapIter {
            iter: branch.map.iter(),
            _doc: doc,
        }
    }
}

impl<'a> Iterator for MapIter<'a> {
    type Item = (&'a str, Out);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let (key, ptr) = self.iter.next()?;
            if ptr.is_deleted() {
                continue;
            }
            if let Some(content) = ptr.content.get_last() {
                return Some((key, content));
            }
        }
    }
}

pub struct MapIntoIter<'a> {
    _doc: &'a Doc,
    entries: std::collections::hash_map::IntoIter<Arc<str>, ItemPtr>,
}

impl<'a> MapIntoIter<'a> {
    fn new(map: NodePtr, doc: &'a Doc) -> Self {
        let entries = map.map.clone().into_iter();
        MapIntoIter { _doc: doc, entries }
    }
}

impl<'a> Iterator for MapIntoIter<'a> {
    type Item = (Arc<str>, Out);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let (key, item) = self.entries.next()?;
            if item.is_deleted() {
                continue;
            }
            if let Some(content) = item.content.get_last() {
                return Some((key, content));
            }
        }
    }
}

/// An unordered iterator over the keys of a [Map].
pub struct Keys<'a>(MapIter<'a>);

impl<'a> Iterator for Keys<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        let (key, _) = self.0.next()?;
        Some(key)
    }
}

/// Iterator over the values of a [Map].
pub struct Values<'a>(MapIter<'a>);

impl<'a> Iterator for Values<'a> {
    type Item = Out;

    fn next(&mut self) -> Option<Self::Item> {
        let (_, value) = self.0.next()?;
        Some(value)
    }
}

impl From<NodePtr> for MapRef {
    fn from(inner: NodePtr) -> Self {
        MapRef(inner)
    }
}

/// A preliminary map. It can be used to early initialize the contents of a [MapRef], when it's about
/// to be inserted into another Yrs collection, such as [ArrayRef] or another [MapRef].
#[repr(transparent)]
#[derive(Debug, PartialEq, Default)]
pub struct MapPrelim(HashMap<Arc<str>, In>);

impl Deref for MapPrelim {
    type Target = HashMap<Arc<str>, In>;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for MapPrelim {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<MapPrelim> for In {
    #[inline]
    fn from(value: MapPrelim) -> Self {
        In::Map(value)
    }
}

impl<S, T> FromIterator<(S, T)> for MapPrelim
where
    S: Into<Arc<str>>,
    T: Into<In>,
{
    fn from_iter<I: IntoIterator<Item = (S, T)>>(iter: I) -> Self {
        MapPrelim(
            iter.into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        )
    }
}

impl<S, T, const C: usize> From<[(S, T); C]> for MapPrelim
where
    S: Into<Arc<str>>,
    T: Into<In>,
{
    fn from(map: [(S, T); C]) -> Self {
        let mut m = HashMap::with_capacity(C);
        for (key, value) in map {
            m.insert(key.into(), value.into());
        }
        MapPrelim(m)
    }
}

impl Prelim for MapPrelim {
    type Return = MapRef;

    fn into_content(self, _txn: &mut TransactionMut) -> (ItemContent, Option<Self>) {
        let inner = Node::new(TypeRef::Map);
        (ItemContent::Node(inner), Some(self))
    }

    fn integrate(self, txn: &mut TransactionMut, inner_ref: NodePtr) {
        let map = MapRef::from(inner_ref);
        for (key, value) in self.0 {
            map.insert(txn, key, value);
        }
    }
}

impl Into<EmbedPrelim<MapPrelim>> for MapPrelim {
    #[inline]
    fn into(self) -> EmbedPrelim<MapPrelim> {
        EmbedPrelim::Shared(self)
    }
}

/// Event generated by [Map::observe] method. Emitted during transaction commit phase.
pub struct MapEvent {
    pub(crate) current_target: NodePtr,
    target: MapRef,
    keys: UnsafeCell<Result<HashMap<Arc<str>, EntryChange>, HashSet<Option<Arc<str>>>>>,
}

impl MapEvent {
    pub(crate) fn new(branch_ref: NodePtr, key_changes: HashSet<Option<Arc<str>>>) -> Self {
        let current_target = branch_ref.clone();
        MapEvent {
            target: MapRef::from(branch_ref),
            current_target,
            keys: UnsafeCell::new(Err(key_changes)),
        }
    }

    /// Returns a [Map] instance which emitted this event.
    pub fn target(&self) -> &MapRef {
        &self.target
    }

    /// Returns a path from root type down to [Map] instance which emitted this event.
    pub fn path(&self) -> Path {
        Node::path(self.current_target, self.target.0)
    }

    /// Returns a summary of key-value changes made over corresponding [Map] collection within
    /// bounds of current transaction.
    pub fn keys<D: Deref<Target = Doc>>(
        &self,
        txn: &Transaction<D>,
    ) -> &HashMap<Arc<str>, EntryChange> {
        let keys = unsafe { self.keys.get().as_mut().unwrap() };

        match keys {
            Ok(keys) => {
                return keys;
            }
            Err(subs) => {
                let subs = event_keys(txn, self.target.0, subs);
                *keys = Ok(subs);
                if let Ok(keys) = keys {
                    keys
                } else {
                    panic!("Defect: should not happen");
                }
            }
        }
    }
}
