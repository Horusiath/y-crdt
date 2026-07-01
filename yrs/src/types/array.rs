use crate::block::{EmbedPrelim, ItemContent, ItemPtr, Prelim, Unused};
use crate::block_iter::BlockIter;
use crate::encoding::read::Error;
use crate::encoding::serde::from_any;
use crate::transaction::TransactionMut;
use crate::types::{
    AsPrelim, Change, ChangeSet, DefaultPrelim, In, Node, NodePtr, Out, Path, RootRef, SharedRef,
    ToJson, TypeRef, event_change_set,
};
use crate::{Any, DeepObservable, Doc, ID, IndexedSequence, Observable, Transaction};
use serde::de::DeserializeOwned;
use std::cell::UnsafeCell;
use std::collections::HashSet;
use std::convert::{TryFrom, TryInto};
use std::iter::FromIterator;
use std::ops::{Deref, DerefMut};

/// A collection used to store data in an indexed sequence structure. This type is internally
/// implemented as a double linked list, which may squash values inserted directly one after another
/// into single list node upon transaction commit.
///
/// Reading a root-level type as an [ArrayRef] means treating its sequence components as a list, where
/// every countable element becomes an individual entity:
///
/// - JSON-like primitives (booleans, numbers, strings, JSON maps, arrays etc.) are counted
///   individually.
/// - Text chunks inserted by [Text] data structure: each character becomes an element of an
///   array.
/// - Embedded and binary values: they count as a single element even though they correspond of
///   multiple bytes.
///
/// Like all Yrs shared data types, [ArrayRef] is resistant to the problem of interleaving (situation
/// when elements inserted one after another may interleave with other peers concurrent inserts
/// after merging all updates together). In case of Yrs conflict resolution is solved by using
/// unique document id to determine correct and consistent ordering.
///
/// # Example
///
/// ```rust
/// use yrs::{Array, Doc, Map, MapPrelim, Any, any};
/// use yrs::types::ToJson;
///
/// let mut doc = Doc::new();
/// let array = doc.get_or_insert_array("array");
/// let mut txn = doc.transact_mut();
///
/// // insert single scalar value
/// array.insert(&mut txn, 0, "value");
/// array.remove_range(&mut txn, 0, 1);
///
/// assert_eq!(array.len(&txn), 0);
///
/// // insert multiple values at once
/// array.insert_range(&mut txn, 0, ["a", "b", "c"]);
/// assert_eq!(array.len(&txn), 3);
///
/// // get value
/// let value = array.get(&txn, 1);
/// assert_eq!(value, Some("b".into()));
///
/// // insert nested shared types
/// let map = array.insert(&mut txn, 1, MapPrelim::from([("key1", "value1")]));
/// map.insert(&mut txn, "key2", "value2");
///
/// assert_eq!(array.to_json(&txn), any!([
///   "a",
///   { "key1": "value1", "key2": "value2" },
///   "b",
///   "c"
/// ]));
/// ```
#[repr(transparent)]
#[derive(Debug, Clone)]
pub struct ArrayRef(NodePtr);

impl RootRef for ArrayRef {
    fn type_ref() -> TypeRef {
        TypeRef::Array
    }
}
impl SharedRef for ArrayRef {}
impl Array for ArrayRef {}
impl IndexedSequence for ArrayRef {}

#[cfg(feature = "weak")]
impl crate::Quotable for ArrayRef {}

impl ToJson for ArrayRef {
    fn to_json<D: Deref<Target = Doc>>(&self, txn: &Transaction<D>) -> Any {
        let mut walker = BlockIter::new(self.0);
        let len = self.0.len();
        let mut buf = vec![Out::default(); len as usize];
        let read = walker.slice(txn.doc(), &mut buf);
        if read == len {
            let res = buf.into_iter().map(|v| v.to_json(txn)).collect();
            Any::Array(res)
        } else {
            panic!(
                "Defect: Array::to_json didn't read all elements ({}/{})",
                read, len
            )
        }
    }
}

impl Eq for ArrayRef {}
impl PartialEq for ArrayRef {
    fn eq(&self, other: &Self) -> bool {
        self.0.id() == other.0.id()
    }
}

impl AsRef<Node> for ArrayRef {
    fn as_ref(&self) -> &Node {
        self.0.deref()
    }
}

impl DeepObservable for ArrayRef {}
impl Observable for ArrayRef {
    type Event = ArrayEvent;
}

impl TryFrom<ItemPtr> for ArrayRef {
    type Error = ItemPtr;

    fn try_from(value: ItemPtr) -> Result<Self, Self::Error> {
        if let Some(branch) = value.clone().as_node() {
            Ok(ArrayRef::from(branch))
        } else {
            Err(value)
        }
    }
}

impl TryFrom<Out> for ArrayRef {
    type Error = Out;

    fn try_from(value: Out) -> Result<Self, Self::Error> {
        match value {
            Out::YArray(value) => Ok(value),
            other => Err(other),
        }
    }
}

impl AsPrelim for ArrayRef {
    type Prelim = ArrayPrelim;

    fn as_prelim<D: Deref<Target = Doc>>(&self, txn: &Transaction<D>) -> Self::Prelim {
        let mut prelim = Vec::with_capacity(self.len(txn) as usize);
        for value in self.iter(txn) {
            prelim.push(value.as_prelim(txn));
        }
        ArrayPrelim(prelim)
    }
}

impl DefaultPrelim for ArrayRef {
    type Prelim = ArrayPrelim;

    #[inline]
    fn default_prelim() -> Self::Prelim {
        ArrayPrelim::default()
    }
}

pub trait Array: AsRef<Node> + Sized {
    /// Returns a number of elements stored in current array.
    fn len<D: Deref<Target = Doc>>(&self, _txn: &Transaction<D>) -> u32 {
        self.as_ref().len()
    }

    /// Inserts a `value` at the given `index`. Inserting at index `0` is equivalent to prepending
    /// current array with given `value`, while inserting at array length is equivalent to appending
    /// that value at the end of it.
    ///
    /// Returns a reference to an integrated preliminary input.
    ///
    /// # Panics
    ///
    /// This method will panic if provided `index` is greater than the current length of an [ArrayRef].
    fn insert<V>(&self, txn: &mut TransactionMut, index: u32, value: V) -> V::Return
    where
        V: Prelim,
    {
        let mut walker = BlockIter::new(NodePtr::from(self.as_ref()));
        if walker.try_forward(txn.doc(), index) {
            let ptr = walker
                .insert_contents(txn, value)
                .expect("cannot insert empty value");
            if let Ok(integrated) = ptr.try_into() {
                integrated
            } else {
                panic!("Defect: unexpected integrated type")
            }
        } else {
            panic!("Index {} is outside of the range of an array", index);
        }
    }

    /// Inserts multiple `values` at the given `index`. Inserting at index `0` is equivalent to
    /// prepending current array with given `values`, while inserting at array length is equivalent
    /// to appending that value at the end of it.
    ///
    /// # Panics
    ///
    /// This method will panic if provided `index` is greater than the current length of an [ArrayRef].
    fn insert_range<T, V>(&self, txn: &mut TransactionMut, index: u32, values: T)
    where
        T: IntoIterator<Item = V>,
        V: Into<Any>,
    {
        let prelim = RangePrelim::new(values);
        if !prelim.is_empty() {
            self.insert(txn, index, prelim);
        }
    }

    /// Inserts given `value` at the end of the current array.
    ///
    /// Returns a reference to an integrated preliminary input.
    fn push_back<V>(&self, txn: &mut TransactionMut, value: V) -> V::Return
    where
        V: Prelim,
    {
        let len = self.len(txn);
        self.insert(txn, len, value)
    }

    /// Inserts given `value` at the beginning of the current array.
    ///
    /// Returns a reference to an integrated preliminary input.
    fn push_front<V>(&self, txn: &mut TransactionMut, content: V) -> V::Return
    where
        V: Prelim,
    {
        self.insert(txn, 0, content)
    }

    /// Removes a single element at provided `index`.
    fn remove(&self, txn: &mut TransactionMut, index: u32) {
        self.remove_range(txn, index, 1)
    }

    /// Removes a range of elements from current array, starting at given `index` up until
    /// a particular number described by `len` has been deleted. This method panics in case when
    /// not all expected elements were removed (due to insufficient number of elements in an array)
    /// or `index` is outside of the bounds of an array.
    fn remove_range(&self, txn: &mut TransactionMut, index: u32, len: u32) {
        let mut walker = BlockIter::new(NodePtr::from(self.as_ref()));
        if walker.try_forward(txn.doc(), index) {
            walker.delete(txn, len)
        } else {
            panic!("Index {} is outside of the range of an array", index);
        }
    }

    /// Retrieves a value stored at a given `index`. Returns `None` when provided index was out
    /// of the range of a current array.
    fn get<D: Deref<Target = Doc>>(&self, txn: &Transaction<D>, index: u32) -> Option<Out> {
        let mut walker = BlockIter::new(NodePtr::from(self.as_ref()));
        let doc = txn.doc();
        if walker.try_forward(doc, index) {
            walker.read_value(doc)
        } else {
            None
        }
    }

    /// Returns a value stored under a given `index` within current map, deserializing it into
    /// expected type if found. If value was not found, the `Any::Null` will be substituted and
    /// deserialized instead (i.e. into instance of `Option` type, if so desired).
    ///
    /// # Example
    ///
    /// ```rust
    /// use yrs::{Doc, In, Array, MapPrelim};
    ///
    /// let mut doc = Doc::new();
    /// let mut txn = doc.transact_mut();
    /// let array = txn.get_or_insert_array("array");
    ///
    /// // insert a multi-nested shared refs
    /// let alice = array.insert(&mut txn, 0, MapPrelim::from([
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
    /// let alice: Person = array.get_as(&txn, 0).unwrap();
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
    /// let bob: Option<Person> = array.get_as(&txn, 1).unwrap();
    /// assert_eq!(bob, None);
    /// ```
    fn get_as<D, V>(&self, txn: &Transaction<D>, index: u32) -> Result<V, Error>
    where
        D: Deref<Target = Doc>,
        V: DeserializeOwned,
    {
        let out = self.get(txn, index).unwrap_or(Out::Any(Any::Null));
        //TODO: we could probably optimize this step by not serializing to intermediate Any value
        let any = out.to_json(txn);
        from_any(&any)
    }

    /// Returns an iterator, that can be used to lazely traverse over all values stored in a current
    /// array.
    fn iter<'a, D: Deref<Target = Doc>>(&self, txn: &'a Transaction<D>) -> ArrayIter<'a> {
        ArrayIter::new(self.as_ref(), txn.doc())
    }
}

pub struct ArrayIter<'a> {
    inner: BlockIter,
    doc: &'a Doc,
}

impl<'a> ArrayIter<'a> {
    pub fn new(array: &Node, doc: &'a Doc) -> Self {
        ArrayIter {
            inner: BlockIter::new(NodePtr::from(array)),
            doc,
        }
    }
}

impl<'a> Iterator for ArrayIter<'a> {
    type Item = Out;

    fn next(&mut self) -> Option<Self::Item> {
        if self.inner.finished() {
            None
        } else {
            let mut buf = [Out::default(); 1];
            if self.inner.slice(self.doc, &mut buf) != 0 {
                Some(std::mem::replace(&mut buf[0], Out::default()))
            } else {
                None
            }
        }
    }
}

impl From<NodePtr> for ArrayRef {
    fn from(inner: NodePtr) -> Self {
        ArrayRef(inner)
    }
}

/// A preliminary array. It can be used to initialize an [ArrayRef], when it's about to be nested
/// into another Yrs data collection, such as [Map] or another [ArrayRef].
#[repr(transparent)]
#[derive(Debug, PartialEq, Default)]
pub struct ArrayPrelim(Vec<In>);

impl Deref for ArrayPrelim {
    type Target = Vec<In>;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for ArrayPrelim {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<ArrayPrelim> for In {
    #[inline]
    fn from(value: ArrayPrelim) -> Self {
        In::Array(value)
    }
}

impl<T> FromIterator<T> for ArrayPrelim
where
    T: Into<In>,
{
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        ArrayPrelim(iter.into_iter().map(|v| v.into()).collect())
    }
}

impl<I, T> From<I> for ArrayPrelim
where
    I: IntoIterator<Item = T>,
    T: Into<In>,
{
    fn from(iter: I) -> Self {
        ArrayPrelim(iter.into_iter().map(|v| v.into()).collect())
    }
}

impl Into<EmbedPrelim<ArrayPrelim>> for ArrayPrelim {
    #[inline]
    fn into(self) -> EmbedPrelim<ArrayPrelim> {
        EmbedPrelim::Shared(self)
    }
}

/// Prelim range defines a way to insert multiple elements effectively at once one after another
/// in an efficient way, provided that these elements correspond to a primitive JSON-like types.
#[repr(transparent)]
struct RangePrelim(Vec<Any>);

impl RangePrelim {
    fn new<I, T>(iter: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<Any>,
    {
        RangePrelim(iter.into_iter().map(|v| v.into()).collect())
    }

    #[inline]
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Event generated by [ArrayRef::observe] method. Emitted during transaction commit phase.
pub struct ArrayEvent {
    pub(crate) current_target: NodePtr,
    target: ArrayRef,
    change_set: UnsafeCell<Option<Box<ChangeSet<Change>>>>,
}

impl ArrayEvent {
    pub(crate) fn new(branch_ref: NodePtr) -> Self {
        let current_target = branch_ref.clone();
        ArrayEvent {
            target: ArrayRef::from(branch_ref),
            current_target,
            change_set: UnsafeCell::new(None),
        }
    }

    /// Returns an [ArrayRef] instance which emitted this event.
    pub fn target(&self) -> &ArrayRef {
        &self.target
    }

    /// Returns a path from root type down to [ArrayRef] instance which emitted this event.
    pub fn path(&self) -> Path {
        Node::path(self.current_target, self.target.0)
    }

    /// Returns summary of changes made over corresponding [ArrayRef] collection within
    /// a bounds of current transaction.
    pub fn delta<D: Deref<Target = Doc>>(&self, txn: &Transaction<D>) -> &[Change] {
        self.changes(txn).delta.as_slice()
    }

    /// Returns a collection of block identifiers that have been added within a bounds of
    /// current transaction.
    pub fn inserts<D: Deref<Target = Doc>>(&self, txn: &Transaction<D>) -> &HashSet<ID> {
        &self.changes(txn).added
    }

    /// Returns a collection of block identifiers that have been removed within a bounds of
    /// current transaction.
    pub fn removes<D: Deref<Target = Doc>>(&self, txn: &Transaction<D>) -> &HashSet<ID> {
        &self.changes(txn).deleted
    }

    fn changes<D: Deref<Target = Doc>>(&self, txn: &Transaction<D>) -> &ChangeSet<Change> {
        let change_set = unsafe { self.change_set.get().as_mut().unwrap() };
        change_set.get_or_insert_with(|| Box::new(event_change_set(txn, self.target.0.start)))
    }
}
