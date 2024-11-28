use crate::types::TypeRef;
use crate::{Array, Map, MapRef, Out, ReadTxn};
use serde::de::{Error, Visitor};
use serde::ser::SerializeSeq;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::VecDeque;
use std::fmt::{write, Display, Formatter};
use std::iter::FromIterator;
use std::sync::Arc;

/// A path describing nesting structure between shared collections containing each other. It's a
/// collection of segments which refer to either index (in case of [Array] or [XmlElement]) or
/// string key (in case of [Map]) where successor shared collection can be found within subsequent
/// parent types.
#[repr(transparent)]
#[derive(Debug, Clone, PartialOrd, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Path(VecDeque<PathSegment>);

impl Path {
    #[inline]
    pub(crate) fn new(path: VecDeque<PathSegment>) -> Self {
        Self(path)
    }
    /// Inserts a new [PathSegment] (index or field accessor) at the end of the current path.
    #[inline]
    pub fn push_back<P>(&mut self, segment: P)
    where
        P: Into<PathSegment>,
    {
        self.0.push_back(segment.into());
    }

    /// Inserts a new [PathSegment] (index or field accessor) at the beginning of the current path.
    #[inline]
    pub fn push_front<P>(&mut self, segment: P)
    where
        P: Into<PathSegment>,
    {
        self.0.push_front(segment.into());
    }

    /// Pops the last[PathSegment] from the current path.
    #[inline]
    pub fn pop_back(&mut self) -> Option<PathSegment> {
        self.0.pop_back()
    }

    /// Pops the first [PathSegment] from the current path.
    #[inline]
    pub fn pop_front(&mut self) -> Option<PathSegment> {
        self.0.pop_front()
    }

    /// Appends a sequence of [PathSegment]s to the end of the current path.
    pub fn extend<P>(&mut self, segments: impl IntoIterator<Item = P>)
    where
        P: Into<PathSegment>,
    {
        self.0.extend(segments.into_iter().map(Into::into));
    }

    /// Checks if the current path has any segments in it.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns number of path segments within the current path.
    #[inline]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns an iterator over the [PathSegment]s of the current path.
    #[inline]
    pub fn iter(&self) -> PathIter {
        self.0.iter()
    }
}

impl IntoIterator for Path {
    type Item = PathSegment;
    type IntoIter = PathIntoIter;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<P> FromIterator<P> for Path
where
    P: Into<PathSegment>,
{
    fn from_iter<T: IntoIterator<Item = P>>(iter: T) -> Self {
        Self(VecDeque::from_iter(iter.into_iter().map(Into::into)))
    }
}

impl Display for Path {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "$")?;
        for segment in self.iter() {
            write!(f, "{}", segment)?;
        }
        Ok(())
    }
}

pub type PathIter<'a> = std::collections::vec_deque::Iter<'a, PathSegment>;
pub type PathIntoIter = std::collections::vec_deque::IntoIter<PathSegment>;

/// A single segment of a [Path]. It can be either a key (in case of [Map])
/// or an index (in case of [Array] or [XmlElement]).
#[derive(Debug, Clone, PartialOrd, PartialEq, Eq, Hash)]
pub enum PathSegment {
    /// Key segments are used to inform how to access child shared collections within a [Map] types.
    Key(Arc<str>),

    /// Index segments are used to inform how to access child shared collections within an [Array]
    /// or [XmlElement] types.
    Index(u32),
}

impl From<Arc<str>> for PathSegment {
    fn from(value: Arc<str>) -> Self {
        PathSegment::Key(value)
    }
}

impl From<String> for PathSegment {
    fn from(value: String) -> Self {
        PathSegment::Key(value.into())
    }
}

impl<'a> From<&'a str> for PathSegment {
    fn from(value: &'a str) -> Self {
        PathSegment::Key(value.into())
    }
}

impl From<u32> for PathSegment {
    fn from(value: u32) -> Self {
        PathSegment::Index(value)
    }
}

impl Display for PathSegment {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            PathSegment::Key(key) => write!(f, "['{}']", key),
            PathSegment::Index(index) => write!(f, "[{}]", index),
        }
    }
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

impl<'de> Deserialize<'de> for PathSegment {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct PathSegmentVisitor;
        impl<'de> Visitor<'de> for PathSegmentVisitor {
            type Value = PathSegment;

            fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
                formatter.write_str("a path segment")
            }

            fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E>
            where
                E: Error,
            {
                Ok(PathSegment::Index(v as u32))
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: Error,
            {
                Ok(PathSegment::Key(v.into()))
            }
        }

        deserializer.deserialize_any(PathSegmentVisitor)
    }
}

#[cfg(test)]
mod test {
    use crate::path::{Path, PathSegment};
    use crate::{
        any, Any, Array, ArrayPrelim, Doc, Map, MapPrelim, ReadTxn, Transact, TransactionMut,
        WriteTxn,
    };
    use serde::Deserialize;
    use serde_json::json;
    use std::iter::FromIterator;
    use jsonpath_rust::JsonPath;

    #[test]
    fn path_should_display_as_json_path() {
        //TODO: once json path is ready, check if we can parse it
        let path = Path::deserialize(json!(["addresses", 2, "city"])).unwrap();
        assert_eq!(path.to_string(), "$['addresses'][2]['city']".to_owned());
    }

    #[test]
    fn path_serialization() {
        let path = Path::deserialize(json!(["addresses", 2, "city"])).unwrap();
        let serialized = serde_json::to_string(&path).unwrap();
        assert_eq!(serialized, r#"["addresses",2,"city"]"#);
    }

    #[test]
    fn resolve_path_any() {
        let value = any!({
            "name": "John",
            "age": 30,
            "address": {
                "city": "New York",
                "street": "5th Avenue",
            },
            "friends": [
                { "name": "Alice" },
                { "name": "Bob" },
            ],
        });
        let actual = value.at_path(Path::from_iter(["age"]));
        assert_eq!(actual, Some(&any!(30)));

        let actual = value.at_path(Path::from_iter(["address"]));
        assert_eq!(
            actual,
            Some(&any!({
                "city": "New York",
                "street": "5th Avenue",
            }))
        );

        let path = Path::from_iter(["address", "city"]);
        let actual = value.at_path(path);
        assert_eq!(actual, Some(&any!("New York")));

        let path = Path::from_iter(vec![PathSegment::from("friends"), 0u32.into()]);
        let actual = value.at_path(path);
        assert_eq!(actual, Some(&any!({ "name": "Alice" })));

        let path = Path::deserialize(json!(["friends", 1, "name"])).unwrap();
        let actual = value.at_path(path);
        assert_eq!(actual, Some(&any!("Bob")));

        let path = Path::deserialize(json!(["friends", 1, "surname"])).unwrap();
        let actual = value.at_path(path);
        assert_eq!(actual, None);
    }

    #[test]
    fn json_path_descent() {
        let doc = Doc::new();
        let mut txn = doc.transact_mut();
        setup_test_data(&mut txn);

        let actual = txn.at_path(JsonPath::)
    }

    fn setup_test_data(txn: &mut TransactionMut) {
        let root = txn.get_or_insert_map("root");
        let store = root.insert(txn, "store", MapPrelim::default());
        let book = store.insert(txn, "book", ArrayPrelim::default());
        book.insert(
            txn,
            0,
            MapPrelim::from([
                ("category", Any::from("reference")),
                ("author", "Nigel Rees".into()),
                ("title", "Sayings of the Century".into()),
                ("price", (8.95).into()),
            ]),
        );
        book.insert(
            txn,
            1,
            MapPrelim::from([
                ("category", Any::from("fiction")),
                ("author", "Evelyn Waugh".into()),
                ("title", "Sword of Honour".into()),
                ("price", (12.99).into()),
            ]),
        );
        book.insert(
            txn,
            2,
            MapPrelim::from([
                ("category", Any::from("fiction")),
                ("author", "Herman Melville".into()),
                ("title", "Moby Dick".into()),
                ("isbn", "0-553-21311-3".into()),
                ("price", (8.99).into()),
            ]),
        );
        book.insert(
            txn,
            3,
            MapPrelim::from([
                ("category", Any::from("fiction")),
                ("author", "J. R. R. Tolkien".into()),
                ("title", "The Lord of the Rings".into()),
                ("isbn", "0-395-19395-8".into()),
                ("price", (22.99).into()),
            ]),
        );
        let bicycle = store.insert(
            txn,
            "bicycle",
            MapPrelim::from([("color", Any::from("red")), ("price", (19.95).into())]),
        );
        let array = root.insert(
            txn,
            "array",
            ArrayPrelim::from([0, 1, 2, 3, 4, 5, 6, 7, 8, 9]),
        );
        let orders = root.insert(
            txn,
            "orders",
            ArrayPrelim::from([
                any!({
                    "ref":[1,2,3],
                    "id":1,
                    "filled": true
                }),
                any!({
                    "ref":[4,5,6],
                    "id":2,
                    "filled": false
                }),
                any!({
                    "ref":[7,8,9],
                    "id":3,
                    "filled": null
                }),
            ]),
        );
        let expensive = root.insert(txn, "expensive", 10);
    }
}
