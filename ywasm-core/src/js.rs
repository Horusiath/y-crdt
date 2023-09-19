use crate::array::ArrayExt;
use crate::branch_ref::BranchRef;
use crate::doc::YDoc;
use std::collections::HashMap;
use wasm_bindgen::JsValue;
use yrs::block::{ItemContent, Prelim, Unused};
use yrs::types::xml::XmlPrelim;
use yrs::types::{Branch, BranchPtr, TypeRef};
use yrs::{
    Any, ArrayRef, Map, MapRef, Text, TextRef, TransactionMut, Value, XmlElementRef, XmlFragment,
    XmlFragmentRef, XmlTextRef,
};

/// Conversion trait for Rust values to be returned to JavaScript host as JS objects.
pub trait IntoJs {
    type Return: AsRef<JsValue>;
    fn into_js(self) -> Self::Return;
}

impl IntoJs for JsValue {
    type Return = Self;

    #[inline]
    fn into_js(self) -> Self::Return {
        self
    }
}

impl IntoJs for Any {
    type Return = JsValue;

    fn into_js(self) -> Self::Return {
        use js_sys::{Array, Object, Reflect, Uint8Array};
        match self {
            Any::Null => JsValue::NULL,
            Any::Undefined => JsValue::UNDEFINED,
            Any::Bool(v) => JsValue::from_bool(v),
            Any::Number(v) => JsValue::from_f64(v),
            Any::BigInt(v) => JsValue::from(v),
            Any::String(v) => JsValue::from(v.as_ref()),
            Any::Buffer(v) => {
                let v = Uint8Array::from(v.as_ref());
                v.into()
            }
            Any::Array(v) => {
                let a = Array::new();
                for value in v.iter() {
                    a.push(&value.clone().into_js());
                }
                a.into()
            }
            Any::Map(v) => {
                let m = Object::new();
                for (k, v) in v.as_ref() {
                    let key = JsValue::from(k);
                    let value = v.clone().into_js();
                    Reflect::set(&m, &key, &value).unwrap();
                }
                m.into()
            }
        }
    }
}

impl IntoJs for Value {
    type Return = JsValue;

    fn into_js(self) -> Self::Return {
        match self {
            Value::Any(v) => v.into_js(),
            Value::YArray(v) => BranchRef::from(v).into_js(),
            Value::YText(v) => BranchRef::from(v).into_js(),
            Value::YMap(v) => BranchRef::from(v).into_js(),
            Value::YXmlElement(v) => BranchRef::from(v).into_js(),
            Value::YXmlText(v) => BranchRef::from(v).into_js(),
            Value::YXmlFragment(v) => BranchRef::from(v).into_js(),
            Value::YDoc(doc) => YDoc::from(doc).into_js(),
        }
    }
}

/// Conversion trait used from incoming JavaScript objects to be mapped onto Rust types.
pub trait FromJs: Sized {
    fn from_js(js: JsValue) -> Result<Self, JsValue>;
}

impl FromJs for Any {
    fn from_js(js: JsValue) -> Result<Self, JsValue> {
        if let Some(str) = js.as_string() {
            Ok(Any::from(str))
        } else if js.is_bigint() {
            let i = js_sys::BigInt::from(js.clone()).unchecked_into_f64();
            Ok(Any::BigInt(i as i64))
        } else if js.is_null() {
            Ok(Any::Null)
        } else if js.is_undefined() {
            Ok(Any::Undefined)
        } else if let Some(f) = js.as_f64() {
            Ok(Any::Number(f))
        } else if let Some(b) = js.as_bool() {
            Ok(Any::Bool(b))
        } else if js_sys::Array::is_array(&js) {
            let array = js_sys::Array::from(&js);
            let mut result = Vec::with_capacity(array.length() as usize);
            for value in array.iter() {
                result.push(Any::from_js(value)?);
            }
            Ok(Any::from(result))
        } else if js.is_object() {
            let object = js_sys::Object::from(js.clone());
            let entries = js_sys::Object::entries(&object);
            let mut map = HashMap::new();
            for tuple in entries.iter() {
                let tuple = js_sys::Array::from(&tuple);
                let key: String = tuple.get(0).as_string().unwrap();
                let value = Any::from_js(tuple.get(1)).unwrap();
                map.insert(key, value);
            }
            Ok(Any::from(map))
        } else {
            Err(js)
        }
    }
}

pub trait IteratorIntoJs {
    type Return: AsRef<JsValue>;
    fn iter_into_js(self) -> Self::Return;
}

impl<I, V> IteratorIntoJs for I
where
    I: Iterator<Item = V>,
    V: IntoJs,
{
    type Return = js_sys::Array;

    fn iter_into_js(self) -> Self::Return {
        let array = js_sys::Array::new();
        for value in self {
            let js_value = value.into_js();
            array.push(js_value.as_ref());
        }
        array
    }
}

static FIELD_TYPE: &str = "__type";
static FIELD_NAME: &str = "name";
static FIELD_PRELIM: &str = "prelim";

#[repr(transparent)]
pub struct JsPrelim(JsValue);

impl JsPrelim {
    pub fn type_ref(&self) -> Option<TypeRef> {
        let field = js_sys::Reflect::get(&self.0, &JsValue::from_str(FIELD_TYPE)).ok()?;
        let value = field.as_f64()? as u8;
        match value {
            yrs::types::TYPE_REFS_ARRAY => Some(TypeRef::Array),
            yrs::types::TYPE_REFS_MAP => Some(TypeRef::Map),
            yrs::types::TYPE_REFS_TEXT => Some(TypeRef::Text),
            yrs::types::TYPE_REFS_XML_ELEMENT => {
                let name = js_sys::Reflect::get(&self.0, &JsValue::from_str(FIELD_TYPE)).ok()?;
                Some(TypeRef::XmlElement(name.as_string()?.into()))
            }
            yrs::types::TYPE_REFS_XML_FRAGMENT => Some(TypeRef::XmlFragment),
            yrs::types::TYPE_REFS_XML_TEXT => Some(TypeRef::XmlText),
            yrs::types::TYPE_REFS_DOC => Some(TypeRef::SubDoc),
            _ => Some(TypeRef::Undefined),
        }
    }
}

impl From<JsValue> for JsPrelim {
    #[inline]
    fn from(value: JsValue) -> Self {
        JsPrelim(value)
    }
}

impl XmlPrelim for JsPrelim {}

impl Prelim for JsPrelim {
    type Return = Unused;

    fn into_content(self, _txn: &mut TransactionMut) -> (ItemContent, Option<Self>) {
        if let Some(type_ref) = self.type_ref() {
            match type_ref {
                TypeRef::SubDoc => {
                    if let Ok(doc) = YDoc::from_js(self.0) {
                        (ItemContent::Doc(None, doc.into()), None)
                    } else {
                        panic!("Cannot integrate this type")
                    }
                }
                other => (ItemContent::Type(Branch::new(other)), Some(self)),
            }
        } else if let Ok(any) = Any::from_js(self.0) {
            (ItemContent::Any(vec![any]), None)
        } else {
            panic!("Cannot integrate this type")
        }
    }

    fn integrate(self, txn: &mut TransactionMut, inner_ref: BranchPtr) {
        if let Some(type_ref) = self.type_ref() {
            if let Ok(js) = js_sys::Reflect::get(&self.0, &JsValue::from_str(FIELD_PRELIM)) {
                match type_ref {
                    TypeRef::Array if js.is_array() => {
                        let array_ref = ArrayRef::from(inner_ref);
                        let arr: Vec<_> = js_sys::Array::from(&js).into_iter().collect();
                        array_ref.insert_at(txn, 0, arr)
                    }
                    TypeRef::Map if js.is_object() => {
                        let map_ref = MapRef::from(inner_ref);
                        let map = js_sys::Map::from(js);
                        map.for_each(&mut |value, key| {
                            let key = key.as_string().unwrap();
                            let value = JsPrelim(value);
                            map_ref.insert(txn, key, value);
                        })
                    }
                    TypeRef::Text if js.is_string() => {
                        let text_ref = TextRef::from(inner_ref);
                        let text = js.as_string().unwrap();
                        text_ref.insert(txn, 0, &text);
                    }
                    TypeRef::XmlElement(_) if js.is_array() => {
                        let xml_ref = XmlElementRef::from(inner_ref);
                        for child in js_sys::Array::from(&js) {
                            xml_ref.push_back(txn, JsPrelim(child));
                        }
                    }
                    TypeRef::XmlFragment if js.is_array() => {
                        let xml_ref = XmlFragmentRef::from(inner_ref);
                        for child in js_sys::Array::from(&js) {
                            xml_ref.push_back(txn, JsPrelim(child));
                        }
                    }
                    TypeRef::XmlText if js.is_string() => {
                        let xml_ref = XmlTextRef::from(inner_ref);
                        let text = js.as_string().unwrap();
                        xml_ref.insert(txn, 0, &text);
                    }
                    _ => { /* do nothing */ }
                }
            }
        }
    }
}

/// Construct a JavaScript object via reflection.
///
/// # Examples
///
/// ```rust
/// use ywasm_core::js;
///
/// let boolean = js!(true);
/// let boolean2 = js!(false);
/// let string = js!("hello");
/// //let integer = js!(12i32);
/// //let array = js!([1, "hello", true]);
/// /*let value = js!({
///   code: 200,
///   reply: "hello",
///   nested: [
///     "ok",
///     { key: "value" }
///   ]
/// });*/
/// ```
#[macro_export(local_inner_macros)]
macro_rules! js {
    // Hide distracting implementation details from the generated rustdoc.
    ($($any:tt)+) => {
        js_internal!($($any)+)
    };
}

#[macro_export(local_inner_macros)]
#[doc(hidden)]
macro_rules! js_internal {/*
    (@array [$($items:expr,)*]) => {
        js_internal_array![$($items,)*]
    };

    // Done without trailing comma.
    (@array [$($items:expr),*]) => {
        js_internal_array![$($items),*]
    };

    // Next item is `null`.
    (@array [$($items:expr,)*] null $($rest:tt)*) => {
        js_internal!(@array [$($items,)* js_internal!(null)] $($rest)*)
    };

    // Next item is `true`.
    (@array [$($items:expr,)*] true $($rest:tt)*) => {
        js_internal!(@array [$($items,)* js_internal!(true)] $($rest)*)
    };

    // Next item is `false`.
    (@array [$($items:expr,)*] false $($rest:tt)*) => {
        js_internal!(@array [$($items,)* js_internal!(false)] $($rest)*)
    };

    // Next item is an array.
    (@array [$($items:expr,)*] [$($array:tt)*] $($rest:tt)*) => {
        js_internal!(@array [$($items,)* js_internal!([$($array)*])] $($rest)*)
    };

    // Next item is a map.
    (@array [$($items:expr,)*] {$($map:tt)*} $($rest:tt)*) => {
        js_internal!(@array [$($items,)* js_internal!({$($map)*})] $($rest)*)
    };

    // Next item is an expression followed by comma.
    (@array [$($items:expr,)*] $next:expr, $($rest:tt)*) => {
        js_internal!(@array [$($items,)* js_internal!($next),] $($rest)*)
    };

    // Last item is an expression with no trailing comma.
    (@array [$($items:expr,)*] $last:expr) => {
        js_internal!(@array [$($items,)* js_internal!($last)])
    };

    // Comma after the most recent item.
    (@array [$($items:expr),*] , $($rest:tt)*) => {
        js_internal!(@array [$($items,)*] $($rest)*)
    };

    // Unexpected token after most recent item.
    (@array [$($items:expr),*] $unexpected:tt $($rest:tt)*) => {
        js_unexpected!($unexpected)
    };

    (@object $object:ident () () ()) => {};

    // Insert the current entry followed by trailing comma.
    (@object $object:ident [$($key:tt)+] ($value:expr) , $($rest:tt)*) => {
        let js_key = wasm_bindgen::JsValue::from_str(stringify!($($key)+));
        js_sys::Relfect::set($object, &js_key, ($value).into_js().as_ref());
        js_internal!(@object $object () ($($rest)*) ($($rest)*));
    };

    // Current entry followed by unexpected token.
    (@object $object:ident [$($key:tt)+] ($value:expr) $unexpected:tt $($rest:tt)*) => {
        js_unexpected!($unexpected);
    };

    // Insert the last entry without trailing comma.
    (@object $object:ident [$($key:tt)+] ($value:expr)) => {
        let js_key = wasm_bindgen::JsValue::from_str(stringify!($($key)+));
        js_sys::Relfect::set($object, &js_key, ($value).into_js().as_ref());
    };

    // Next value is `null`.
    (@object $object:ident ($($key:tt)+) (: null $($rest:tt)*) $copy:tt) => {
        js_internal!(@object $object [$($key)+] (js_internal!(null)) $($rest)*);
    };

    // Next value is `true`.
    (@object $object:ident ($($key:tt)+) (: true $($rest:tt)*) $copy:tt) => {
        js_internal!(@object $object [$($key)+] (js_internal!(true)) $($rest)*);
    };

    // Next value is `false`.
    (@object $object:ident ($($key:tt)+) (: false $($rest:tt)*) $copy:tt) => {
        js_internal!(@object $object [$($key)+] (js_internal!(false)) $($rest)*);
    };

    // Next value is an array.
    (@object $object:ident ($($key:tt)+) (: [$($array:tt)*] $($rest:tt)*) $copy:tt) => {
        js_internal!(@object $object [$($key)+] (js_internal!([$($array)*])) $($rest)*);
    };

    // Next value is a map.
    (@object $object:ident ($($key:tt)+) (: {$($map:tt)*} $($rest:tt)*) $copy:tt) => {
        js_internal!(@object $object [$($key)+] (js_internal!({$($map)*})) $($rest)*);
    };

    // Next value is an expression followed by comma.
    (@object $object:ident ($($key:tt)+) (: $value:expr , $($rest:tt)*) $copy:tt) => {
        js_internal!(@object $object [$($key)+] (js_internal!($value)) , $($rest)*);
    };

    // Last value is an expression with no trailing comma.
    (@object $object:ident ($($key:tt)+) (: $value:expr) $copy:tt) => {
        js_internal!(@object $object [$($key)+] (js_internal!($value)));
    };

    // Missing value for last entry. Trigger a reasonable error message.
    (@object $object:ident ($($key:tt)+) (:) $copy:tt) => {
        // "unexpected end of macro invocation"
        js_internal!();
    };

    // Missing colon and value for last entry. Trigger a reasonable error
    // message.
    (@object $object:ident ($($key:tt)+) () $copy:tt) => {
        // "unexpected end of macro invocation"
        js_internal!();
    };

    // Misplaced colon. Trigger a reasonable error message.
    (@object $object:ident () (: $($rest:tt)*) ($colon:tt $($copy:tt)*)) => {
        // Takes no arguments so "no rules expected the token `:`".
        js_unexpected!($colon);
    };

    // Found a comma inside a key. Trigger a reasonable error message.
    (@object $object:ident ($($key:tt)*) (, $($rest:tt)*) ($comma:tt $($copy:tt)*)) => {
        // Takes no arguments so "no rules expected the token `,`".
        js_unexpected!($comma);
    };

    // Key is fully parenthesized. This avoids clippy double_parens false
    // positives because the parenthesization may be necessary here.
    (@object $object:ident () (($key:expr) : $($rest:tt)*) $copy:tt) => {
        js_internal!(@object $object ($key) (: $($rest)*) (: $($rest)*));
    };

    // Refuse to absorb colon token into key expression.
    (@object $object:ident ($($key:tt)*) (: $($unexpected:tt)+) $copy:tt) => {
        json_expect_expr_comma!($($unexpected)+);
    };

    // Munch a token into the current key.
    (@object $object:ident ($($key:tt)*) ($tt:tt $($rest:tt)*) $copy:tt) => {
        js_internal!(@object $object ($($key)* $tt) ($($rest)*) ($($rest)*));
    };
*/

    //////////////////////////////////////////////////////////////////////////
    // The main implementation.
    //
    // Must be invoked as: js_internal!($($json)+)
    //////////////////////////////////////////////////////////////////////////

    (null) => {
        wasm_bindgen::JsValue::NULL
    };

    (true) => {
        wasm_bindgen::JsValue::TRUE
    };

    (false) => {
        wasm_bindgen::JsValue::FALSE
    };

    ([]) => {
        js_sys::Array::new()
    };

    ([ $($tt:tt)+ ]) => {
        let array = js_sys::Array::new()
        js_internal!(@array [] $($tt)+)
        array
    };

    ({}) => {
        js_sys::Object::new()
    };

    ({ $($tt:tt)+ }) => {
        let object = js_sys::Object::new()
        js_internal!(@object object () ($($tt)+) ($($tt)+));
        object
    };
    // Any Serialize type: numbers, strings, struct literals, variables etc.
    // Must be below every other rule.
    ($other:expr) => {
        wasm_bindgen::JsValue::from($other)
    };
}

#[macro_export]
#[doc(hidden)]
macro_rules! js_internal_array {
    ($($content:tt)*) => {
        let array = js_sys::Array::new()
        for value in [$($content)*] {
            array.push(value)
        }
        array
    };
}

#[macro_export]
#[doc(hidden)]
macro_rules! js_unexpected {
    () => {};
}

#[macro_export]
#[doc(hidden)]
macro_rules! js_expect_expr_comma {
    ($e:expr , $($tt:tt)*) => {};
}
