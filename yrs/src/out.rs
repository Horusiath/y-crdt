use crate::block::ItemPtr;
use crate::{Any, In, NodeID, Uuid};
use std::convert::TryFrom;
use std::fmt::Formatter;
use std::sync::Arc;

/// Value that can be returned by Yrs data types. This includes [Any] which is an extension
/// representation of JSON, but also nested complex collaborative structures specific to Yrs.
#[derive(Debug, Clone, PartialEq)]
pub enum Out {
    /// Any value that it treated as a single element in its entirety.
    Any(Any),
    Node(NodeID),
    /// Subdocument identifier.
    Doc(Uuid),
}

impl Default for Out {
    fn default() -> Self {
        Out::Any(Any::Undefined)
    }
}

impl Out {
    /// Attempts to convert current [Out] value directly onto a different type, as along as it
    /// implements [TryFrom] trait. If conversion is not possible, the original value is returned.
    #[inline]
    pub fn cast<T>(self) -> Result<T, Self>
    where
        T: TryFrom<Self, Error = Self>,
    {
        T::try_from(self)
    }

    pub fn node_id(self) -> Option<NodeID> {
        match self {
            Out::Node(id) => Some(id),
            _ => None,
        }
    }
}

impl TryFrom<Out> for NodeID {
    type Error = Out;

    fn try_from(value: Out) -> Result<Self, Self::Error> {
        match value {
            Out::Node(id) => Ok(id),
            out => Err(out),
        }
    }
}

impl TryFrom<ItemPtr> for Out {
    type Error = ItemPtr;

    fn try_from(value: ItemPtr) -> Result<Self, Self::Error> {
        match value.content.get_last() {
            None => Err(value),
            Some(v) => Ok(v),
        }
    }
}

impl<T> From<T> for Out
where
    T: Into<Any>,
{
    fn from(v: T) -> Self {
        let any: Any = v.into();
        Out::Any(any)
    }
}

//FIXME: what we would like to have is an automatic trait implementation of TryFrom<Value> for
// any type that implements TryFrom<Any,Error=Any>, but this causes compiler error.
macro_rules! impl_try_from {
    ($t:ty) => {
        impl TryFrom<Out> for $t {
            type Error = Out;

            fn try_from(value: Out) -> Result<Self, Self::Error> {
                use std::convert::TryInto;
                match value {
                    Out::Any(any) => any.try_into().map_err(Out::Any),
                    other => Err(other),
                }
            }
        }
    };
}

impl_try_from!(bool);
impl_try_from!(f32);
impl_try_from!(f64);
impl_try_from!(i16);
impl_try_from!(i32);
impl_try_from!(u16);
impl_try_from!(u32);
impl_try_from!(i64);
impl_try_from!(isize);
impl_try_from!(String);
impl_try_from!(Arc<str>);
impl_try_from!(Vec<u8>);
impl_try_from!(Arc<[u8]>);

impl std::fmt::Display for Out {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Out::Any(value) => std::fmt::Display::fmt(value, f),
            Out::Node(node) => write!(f, "Node({})", node),
            Out::Doc(guid) => write!(f, "Doc({})", guid),
        }
    }
}
