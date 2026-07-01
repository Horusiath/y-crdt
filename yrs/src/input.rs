use crate::{Any, Delta, Doc, Out};

/// A wrapper around [Out] type that enables it to be used as a type to be inserted into
/// shared collections. If [In] contains a shared type, it will be inserted as a deep
/// copy of the original type: therefore none of the changes applied to the original type will
/// affect the deep copy.
#[derive(Debug, PartialEq)]
pub enum In {
    Any(Any),
    Node(Delta<In>),
    Doc(Doc),
}

impl From<Any> for In {
    #[inline]
    fn from(value: Any) -> Self {
        In::Any(value)
    }
}

macro_rules! impl_from_any {
    ($t:ty) => {
        impl From<$t> for In {
            #[inline]
            fn from(value: $t) -> Self {
                In::Any(Any::from(value))
            }
        }
    };
}

impl_from_any!(bool);
impl_from_any!(i16);
impl_from_any!(i32);
impl_from_any!(i64);
impl_from_any!(u16);
impl_from_any!(u32);
impl_from_any!(f32);
impl_from_any!(f64);
impl_from_any!(String);
impl_from_any!(std::sync::Arc<str>);
impl_from_any!(Vec<u8>);
impl_from_any!(&[u8]);
