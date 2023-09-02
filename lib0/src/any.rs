use crate::decoding::Read;
use crate::encoding::Write;
use crate::error::Error;
use std::cmp::PartialEq;
use std::collections::HashMap;
use std::convert::TryFrom;
use std::fmt::Formatter;

/// Max value to represent the safe integer in JavaScript.
pub const MAX_SAFE_INTEGER: i64 = i64::pow(2, 53) - 1;

/// Min value to represent the safe integer in JavaScript.
pub const MIN_SAFE_INT: i64 = -MAX_SAFE_INTEGER;

/// Composition of all numeric variants.
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum Number {
    /// Floating point number.
    Float(f64),
    /// Integer with value within bounds of [MIN_SAFE_INT]..[MAX_SAFE_INTEGER].
    Int(i64),
    /// Integer beyond bounds of [MIN_SAFE_INT]..[MAX_SAFE_INTEGER].
    BigInt(i64),
}

impl std::fmt::Display for Number {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Number::Float(n) => write!(f, "{}", n),
            Number::Int(n) => write!(f, "{}", n),
            Number::BigInt(n) => write!(f, "{}", n),
        }
    }
}

macro_rules! impl_float {
    ($t:ty) => {
        impl From<$t> for Number {
            #[inline]
            fn from(v: $t) -> Number {
                Number::Float(v as f64)
            }
        }
    };
}

macro_rules! impl_number {
    ($t:ty) => {
        impl From<$t> for Number {
            #[inline]
            fn from(v: $t) -> Number {
                Number::Int(v as i64)
            }
        }
    };
}

macro_rules! impl_big_number {
    ($t:ty) => {
        impl From<$t> for Number {
            fn from(v: $t) -> Number {
                let v = v as i64;
                if v <= MAX_SAFE_INTEGER && v >= MIN_SAFE_INT {
                    Number::Int(v)
                } else {
                    Number::BigInt(v)
                }
            }
        }
    };
}

impl_float!(f32);
impl_float!(f64);
impl_number!(i16);
impl_number!(i32);
impl_number!(u16);
impl_number!(u32);
impl_big_number!(isize);
impl_big_number!(i64);

impl TryFrom<u64> for Number {
    type Error = u64;

    fn try_from(v: u64) -> Result<Self, Self::Error> {
        if v <= MAX_SAFE_INTEGER as u64 {
            Ok(Number::Int(v as i64))
        } else if v <= i64::MAX as u64 {
            Ok(Number::BigInt(v as i64))
        } else {
            Err(v)
        }
    }
}

impl TryFrom<usize> for Number {
    type Error = usize;

    #[cfg(target_pointer_width = "32")]
    fn try_from(v: usize) -> Result<Self, Self::Error> {
        // for 32-bit architectures we know that usize will always fit,
        // so there's no need to check for length, but we stick to TryInto
        // trait to keep API compatibility
        Ok(Number::Int(v as i64))
    }

    #[cfg(not(target_pointer_width = "32"))]
    fn try_from(v: usize) -> Result<Self, Self::Error> {
        if let Ok(num) = Number::try_from(v as u64) {
            Ok(num)
        } else {
            Err(v)
        }
    }
}

/// Any is an enum with a potentially associated value that is used to represent JSON values
/// and supports efficient encoding of those values.
#[derive(Debug, Clone, PartialEq)]
pub enum Any {
    Null,
    Undefined,
    Bool(bool),
    Number(Number),
    String(Box<str>),
    Buffer(Box<[u8]>),
    Array(Box<[Any]>),
    Map(Box<HashMap<String, Any>>),
}

impl Any {
    pub fn decode<R: Read>(decoder: &mut R) -> Result<Self, Error> {
        Ok(match decoder.read_u8()? {
            // CASE 127: undefined
            127 => Any::Undefined,
            // CASE 126: null
            126 => Any::Null,
            // CASE 125: integer
            125 => Any::Number(Number::Int(decoder.read_var::<i64>()?)),
            // CASE 124: float32
            124 => Any::Number(Number::Float(decoder.read_f32()? as f64)),
            // CASE 123: float64
            123 => Any::Number(Number::Float(decoder.read_f64()?)),
            // CASE 122: bigint
            122 => Any::Number(Number::BigInt(decoder.read_i64()?)),
            // CASE 121: boolean (false)
            121 => Any::Bool(false),
            // CASE 120: boolean (true)
            120 => Any::Bool(true),
            // CASE 119: string
            119 => Any::String(Box::from(decoder.read_string()?)),
            // CASE 118: Map<string,Any>
            118 => {
                let len: usize = decoder.read_var()?;
                let mut map = HashMap::with_capacity(len);
                for _ in 0..len {
                    let key = decoder.read_string()?;
                    map.insert(key.to_owned(), Any::decode(decoder)?);
                }
                Any::Map(Box::new(map))
            }
            // CASE 117: Array<Any>
            117 => {
                let len: usize = decoder.read_var()?;
                let mut arr = Vec::with_capacity(len);
                for _ in 0..len {
                    arr.push(Any::decode(decoder)?);
                }
                Any::Array(arr.into_boxed_slice())
            }
            // CASE 116: buffer
            116 => Any::Buffer(Box::from(decoder.read_buf()?)),
            _ => return Err(Error::UnexpectedValue),
        })
    }

    // Encode data with efficient binary format.
    //
    // Differences to JSON:
    // • Transforms data to a binary format (not to a string)
    // • Encodes undefined, NaN, and ArrayBuffer (these can't be represented in JSON)
    // • Numbers are efficiently encoded either as a variable length integer, as a
    //   32 bit float, as a 64 bit float, or as a 64 bit bigint.
    //
    // Encoding table:
    //
    // | Data Type           | Prefix   | Encoding Method    | Comment |
    // | ------------------- | -------- | ------------------ | ------- |
    // | undefined           | 127      |                    | Functions, symbol, and everything that cannot be identified is encoded as undefined |
    // | null                | 126      |                    | |
    // | integer             | 125      | writeVarInt        | Only encodes 32 bit signed integers |
    // | float32             | 124      | writeFloat32       | |
    // | float64             | 123      | writeFloat64       | |
    // | bigint              | 122      | writeBigInt64      | |
    // | boolean (false)     | 121      |                    | True and false are different data types so we save the following byte |
    // | boolean (true)      | 120      |                    | - 0b01111000 so the last bit determines whether true or false |
    // | string              | 119      | writeVarString     | |
    // | object<string,any>  | 118      | custom             | Writes {length} then {length} key-value pairs |
    // | array<any>          | 117      | custom             | Writes {length} then {length} json values |
    // | Uint8Array          | 116      | writeVarUint8Array | We use Uint8Array for any kind of binary data |
    //
    // Reasons for the decreasing prefix:
    // We need the first bit for extendability (later we may want to encode the
    // prefix with writeVarUint). The remaining 7 bits are divided as follows:
    // [0-30]   the beginning of the data range is used for custom purposes
    //          (defined by the function that uses this library)
    // [31-127] the end of the data range is used for data encoding by
    //          lib0/encoding.js
    pub fn encode<W: Write>(&self, encoder: &mut W) {
        match self {
            Any::Undefined => {
                // TYPE 127: undefined
                encoder.write_u8(127)
            }
            Any::Null => {
                // TYPE 126: null
                encoder.write_u8(126)
            }
            Any::Bool(bool) => {
                // TYPE 120/121: boolean (true/false)
                encoder.write_u8(if *bool { 120 } else { 121 })
            }
            Any::String(str) => {
                // TYPE 119: String
                encoder.write_u8(119);
                encoder.write_string(&str)
            }
            Any::Number(num) => {
                match *num {
                    Number::Float(num) => {
                        if ((num as f32) as f64) == num {
                            // TYPE 124: FLOAT32
                            encoder.write_u8(124);
                            encoder.write_f32(num as f32)
                        } else {
                            // TYPE 123: FLOAT64
                            encoder.write_u8(123);
                            encoder.write_f64(num)
                        }
                    }
                    Number::Int(num) => {
                        // TYPE 125: INTEGER
                        encoder.write_u8(125);
                        encoder.write_var(num)
                    }
                    Number::BigInt(num) => {
                        // TYPE 122: BigInt
                        encoder.write_u8(122);
                        encoder.write_i64(num)
                    }
                }
            }
            Any::Array(arr) => {
                // TYPE 117: Array
                encoder.write_u8(117);
                encoder.write_var(arr.len() as u64);
                for el in arr.iter() {
                    el.encode(encoder);
                }
            }
            Any::Map(map) => {
                // TYPE 118: Map
                encoder.write_u8(118);
                encoder.write_var(map.len() as u64);
                for (key, value) in map.as_ref() {
                    encoder.write_string(&key);
                    value.encode(encoder);
                }
            }
            Any::Buffer(buf) => {
                // TYPE 116: Buffer
                encoder.write_u8(116);
                encoder.write_buf(&buf)
            }
        }
    }

    #[cfg(not(feature = "lib0-serde"))]
    pub fn to_json(&self, buf: &mut String) {
        use std::fmt::Write;

        fn quoted(buf: &mut String, s: &str) {
            buf.reserve(s.len() + 2);
            buf.push('"');
            for c in s.chars() {
                match c {
                    '\\' => buf.push_str("\\\\"),
                    '\u{0008}' => buf.push_str("\\b"),
                    '\u{000c}' => buf.push_str("\\f"),
                    '\n' => buf.push_str("\\n"),
                    '\r' => buf.push_str("\\r"),
                    '\t' => buf.push_str("\\t"),
                    '"' => buf.push_str("\\\""),
                    c if c.is_control() => write!(buf, "\\u{:04x}", c as u32).unwrap(),
                    c => buf.push(c),
                }
            }
            buf.push('"');
        }

        match self {
            Any::Null => buf.push_str("null"),
            Any::Bool(value) => write!(buf, "{}", value).unwrap(),
            Any::Number(value) => write!(buf, "{}", value).unwrap(),
            Any::String(value) => quoted(buf, value.as_ref()),
            Any::Array(values) => {
                buf.push('[');
                let mut i = values.iter();
                if let Some(value) = i.next() {
                    value.to_json(buf);
                }
                while let Some(value) = i.next() {
                    buf.push(',');
                    value.to_json(buf);
                }
                buf.push(']');
            }
            Any::Map(entries) => {
                buf.push('{');
                let mut i = entries.iter();
                if let Some((key, value)) = i.next() {
                    quoted(buf, key.as_str());
                    buf.push(':');
                    value.to_json(buf);
                }
                while let Some((key, value)) = i.next() {
                    buf.push(',');
                    quoted(buf, key.as_str());
                    buf.push(':');
                    value.to_json(buf);
                }
                buf.push('}');
            }
            other => panic!(
                "Serialization of {} into JSON representation is not supported",
                other
            ),
        }
    }

    #[cfg(not(feature = "lib0-serde"))]
    pub fn from_json(src: &str) -> Result<Self, Error> {
        use crate::json_parser::JsonParser;
        let mut parser = JsonParser::new(src.chars());
        Ok(parser.parse()?)
    }

    #[cfg(feature = "lib0-serde")]
    pub fn from_json(src: &str) -> Result<Self, Error> {
        Ok(serde_json::from_str(src)?)
    }

    #[cfg(feature = "lib0-serde")]
    pub fn to_json(&self, buf: &mut String) {
        use serde::Serialize;
        use serde_json::Serializer;

        let buf = unsafe { buf.as_mut_vec() };
        let mut cursor = std::io::Cursor::new(buf);

        let mut s = Serializer::new(cursor);
        self.serialize(&mut s).unwrap();
    }
}

impl std::fmt::Display for Any {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Any::Null => f.write_str("null"),
            Any::Undefined => f.write_str("undefined"),
            Any::Bool(value) => write!(f, "{}", value),
            Any::Number(value) => write!(f, "{}", value),
            Any::String(value) => f.write_str(value.as_ref()),
            Any::Array(values) => {
                write!(f, "[")?;
                let mut i = values.iter();
                if let Some(value) = i.next() {
                    value.fmt(f)?;
                }
                while let Some(value) = i.next() {
                    write!(f, ", ")?;
                    value.fmt(f)?;
                }
                write!(f, "]")
            }
            Any::Map(entries) => {
                write!(f, "{{")?;
                let mut i = entries.iter();
                if let Some((key, value)) = i.next() {
                    write!(f, "{}: {}", key, value)?;
                }
                while let Some((key, value)) = i.next() {
                    write!(f, ", {}: {}", key, value)?;
                }
                write!(f, "}}")
            }
            Any::Buffer(value) => {
                f.write_str("0x")?;
                for &byte in value.iter() {
                    write!(f, "{:02x}", byte)?;
                }
                Ok(())
            }
        }
    }
}

impl From<bool> for Any {
    #[inline]
    fn from(v: bool) -> Any {
        Any::Bool(v)
    }
}

impl From<Vec<u8>> for Any {
    fn from(v: Vec<u8>) -> Any {
        Any::Buffer(v.into_boxed_slice())
    }
}

impl<T> From<Vec<T>> for Any
where
    T: Into<Any>,
{
    fn from(v: Vec<T>) -> Any {
        let mut array = Vec::with_capacity(v.len());
        for value in v {
            array.push(value.into())
        }
        Any::Array(array.into_boxed_slice())
    }
}

impl<T> From<HashMap<String, T>> for Any
where
    T: Into<Any>,
{
    fn from(v: HashMap<String, T>) -> Any {
        let mut map = HashMap::with_capacity(v.len());
        for (key, value) in v {
            map.insert(key, value.into());
        }
        Any::Map(Box::new(map))
    }
}

impl<T> From<T> for Any
where
    T: Into<Number>,
{
    #[inline]
    fn from(value: T) -> Self {
        Any::Number(value.into())
    }
}

impl From<String> for Any {
    #[inline]
    fn from(v: String) -> Any {
        Any::String(v.into())
    }
}

impl From<&str> for Any {
    #[inline]
    fn from(v: &str) -> Any {
        Any::String(Box::from(v))
    }
}

impl<T> From<Option<T>> for Any
where
    T: Into<Any>,
{
    #[inline]
    fn from(v: Option<T>) -> Any {
        match v {
            None => Any::Null,
            Some(value) => value.into(),
        }
    }
}
