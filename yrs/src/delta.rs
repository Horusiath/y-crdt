use crate::node::Attrs;
#[cfg(feature = "weak")]
use crate::weak::LinkSource;
use crate::{In, Out};
use std::collections::HashMap;
use std::sync::Arc;

/// Delta containing changes of a specific [Node](crate::NodeRef) with possible recursion over nested nodes.
///
/// Delta can be used for several cases:
/// 1. Describe changes made within a specific transaction scope.
/// 2. Describe all the properties a specific node.
/// 3. Describe changes yet to be made onto a node (applied via: [NodeRef::apply_delta]).
#[derive(Debug, Clone, PartialEq)]
pub struct Delta<T = Out> {
    /// Optional node name (e.g. XML tag). `None` for text/array/map types.
    pub name: Option<Arc<str>>,
    /// Sequential operations on the node's ordered children, like text, array or XML child nodes.
    pub children: Vec<Op<T>>,
    /// Operations on the node's attributes.
    pub attrs: HashMap<Arc<str>, AttrOp<T>>,

    #[cfg(feature = "weak")]
    pub link: Option<LinkSource>,
}

/// A single operation on the ordered children axis of a [Delta].
#[derive(Debug, Clone, PartialEq)]
pub enum Op<T = Out> {
    /// Retain (skip over) a number of elements, optionally applying a formatting change.
    Retain {
        len: u32,
        format: Option<Box<Attrs>>,
    },
    /// Insert a text string with optional formatting.
    InsertText {
        text: String,
        format: Option<Box<Attrs>>,
    },
    /// Insert one or more non-text content values with optional formatting.
    Insert {
        items: Vec<T>,
        format: Option<Box<Attrs>>,
    },
    /// Delete a number of elements. `prev_value` captures the deleted content
    /// when produced by diff/undo operations.
    Remove {
        len: u32,
        prev: Option<Box<Delta<T>>>,
    },
    /// Apply a nested delta to an embedded child node, optionally updating its formatting.
    Modify {
        delta: Box<Delta<T>>,
        format: Option<Box<Attrs>>,
    },
}

/// A single operation on the key-value attributes axis of a [Delta].
#[derive(Debug, Clone, PartialEq)]
pub enum AttrOp<T = Out> {
    /// Update attribute value.
    /// `prev_value` captures the old value when produced by diff/undo operations.
    Update { value: T, prev: Option<T> },
    /// Remove an attribute.
    /// `prev_value` captures the removed value when produced by diff/undo operations.
    Remove { prev: Option<T> },
    /// Apply a nested delta to an attribute whose value is itself a node.
    Modify { delta: Box<Delta<T>> },
}

impl<T> AttrOp<T> {
    #[inline]
    pub fn update(value: T, prev: Option<T>) -> Self {
        AttrOp::Update { value, prev }
    }

    #[inline]
    pub fn remove(prev: Option<T>) -> Self {
        AttrOp::Remove { prev }
    }

    pub fn modify(delta: impl Into<Box<Delta<T>>>) -> Self {
        AttrOp::Modify {
            delta: delta.into(),
        }
    }
}

impl<T> Delta<T> {
    pub fn with_name(name: impl Into<Arc<str>>) -> Self {
        Delta {
            name: Some(name.into()),
            children: Vec::new(),
            attrs: HashMap::new(),
            #[cfg(feature = "weak")]
            link: None,
        }
    }

    /// Recursively transforms all content values in this delta from type `T` to type `U`.
    pub fn map<U>(self, mut f: impl FnMut(T) -> U) -> Delta<U> {
        self.map_inner(&mut f)
    }

    fn map_inner<U>(self, f: &mut impl FnMut(T) -> U) -> Delta<U> {
        Delta {
            name: self.name,
            children: self
                .children
                .into_iter()
                .map(|op| op.map_inner(f))
                .collect(),
            attrs: self
                .attrs
                .into_iter()
                .map(|(k, op)| (k, op.map_inner(f)))
                .collect(),
            #[cfg(feature = "weak")]
            link: self.link.clone(),
        }
    }

    #[inline]
    pub fn insert_text(mut self, text: impl AsRef<str>) -> Self {
        self.insert_text_with(text);
        self
    }

    pub fn insert_text_with(&mut self, text: impl AsRef<str>) {
        let str = text.as_ref();
        if str.is_empty() {
            return;
        } else if let Some(Op::InsertText { text, format: None }) = self.children.last_mut() {
            text.push_str(str);
        } else {
            self.children.push(Op::InsertText {
                text: str.into(),
                format: None,
            });
        }
    }

    /// Appends a text insert operation with formatting attributes.
    #[inline]
    pub fn insert_text_fmt(mut self, text: impl AsRef<str>, format: &Attrs) -> Self {
        self.insert_text_fmt_with(text, format);
        self
    }

    /// Appends a text insert operation with formatting attributes.
    pub fn insert_text_fmt_with(&mut self, text: impl AsRef<str>, attrs: &Attrs) {
        let str = text.as_ref();
        if str.is_empty() {
            return;
        } else if let Some(Op::InsertText { text, format }) = self.children.last_mut()
            && format_eq(format.as_deref(), Some(attrs))
        {
            text.push_str(str);
        } else {
            let format = if attrs.is_empty() {
                None
            } else {
                Some(Box::new(attrs.clone()))
            };
            self.children.push(Op::InsertText {
                text: str.into(),
                format,
            });
        }
    }

    /// Appends a delete operation for `len` elements.
    #[inline]
    pub fn remove(mut self, count: u32) -> Self {
        self.remove_with(count);
        self
    }

    /// Appends a delete operation for `len` elements.
    pub fn remove_with(&mut self, count: u32) {
        if count == 0 {
            return;
        } else if let Some(Op::Remove { len, prev: None }) = self.children.last_mut() {
            *len += count;
        } else {
            self.children.push(Op::Remove {
                len: count,
                prev: None,
            });
        }
    }

    /// Appends a retain (skip) operation for `len` elements.
    #[inline]
    pub fn retain(mut self, count: u32) -> Self {
        self.retain_with(count);
        self
    }

    pub fn retain_with(&mut self, count: u32) {
        if count == 0 {
            return;
        } else if let Some(Op::Retain { len, format: None }) = self.children.last_mut() {
            *len += count;
        } else {
            self.children.push(Op::Retain {
                len: count,
                format: None,
            });
        }
    }

    /// Appends a retain operation with a formatting change.
    #[inline]
    pub fn retain_fmt(mut self, count: u32, format: &Attrs) -> Self {
        self.retain_fmt_with(count, format);
        self
    }

    pub fn retain_fmt_with(&mut self, count: u32, attrs: &Attrs) {
        if count == 0 {
            return;
        } else if let Some(Op::Retain { len, format }) = self.children.last_mut()
            && format_eq(format.as_deref(), Some(attrs))
        {
            *len += count;
        } else {
            let format = if attrs.is_empty() {
                None
            } else {
                Some(Box::new(attrs.clone()))
            };
            self.children.push(Op::Retain { len: count, format });
        }
    }

    /// Appends a modify operation that applies a nested delta to an embedded child.
    #[inline]
    pub fn modify(mut self, delta: Self) -> Self {
        self.modify_with(delta);
        self
    }

    pub fn modify_with(&mut self, delta: Self) {
        self.children.push(Op::Modify {
            delta: Box::new(delta),
            format: None,
        });
    }

    /// Appends a modify operation with a formatting change.
    #[inline]
    pub fn modify_fmt(mut self, delta: Self, format: &Attrs) -> Self {
        self.modify_fmt_with(delta, format);
        self
    }

    pub fn modify_fmt_with(&mut self, delta: Self, format: &Attrs) {
        self.children.push(Op::Modify {
            delta: Box::new(delta),
            format: Some(Box::new(format.clone())),
        });
    }

    /// Removes an attribute by key.
    #[inline]
    pub fn remove_attr(mut self, key: impl Into<Arc<str>>) -> Self {
        self.remove_attr_with(key);
        self
    }
    pub fn remove_attr_with(&mut self, key: impl Into<Arc<str>>) {
        self.attrs.insert(key.into(), AttrOp::Remove { prev: None });
    }

    /// Applies a nested delta to an attribute whose value is a node.
    #[inline]
    pub fn modify_attr(mut self, key: impl Into<Arc<str>>, delta: Self) -> Self {
        self.modify_attr_with(key, delta);
        self
    }
    pub fn modify_attr_with(&mut self, key: impl Into<Arc<str>>, delta: Self) {
        self.attrs.insert(
            key.into(),
            AttrOp::Modify {
                delta: Box::new(delta),
            },
        );
    }

    /// Removes trailing retains without formatting, as they don't change anything - a port of
    /// lib0 `DeltaBuilder.done`.
    pub(crate) fn trim_retains(&mut self) {
        while let Some(Op::Retain { format: None, .. }) = self.children.last() {
            self.children.pop();
        }
    }

    /// Composes another delta on top of this one. The result represents
    /// applying `self` followed by `other`.
    pub fn join(mut self, other: Self) -> Self {
        self.children.extend(other.children);
        for (key, value) in other.attrs {
            self.attrs.insert(key, value);
        }
        self
    }

    pub fn join_with(&mut self, other: Self) {
        self.children.extend(other.children);
        for (key, value) in other.attrs {
            self.attrs.insert(key, value);
        }
    }

    /// Appends a single content value insert operation.
    #[inline]
    pub fn insert(mut self, content: impl Into<T>) -> Self {
        self.insert_with(content);
        self
    }

    pub fn insert_with(&mut self, content: impl Into<T>) {
        if let Some(Op::Insert {
            items,
            format: None,
        }) = self.children.last_mut()
        {
            items.push(content.into());
        } else {
            self.children.push(Op::Insert {
                items: vec![content.into()],
                format: None,
            });
        }
    }

    /// Appends a single content value insert operation with formatting attributes.
    #[inline]
    pub fn insert_fmt(mut self, content: impl Into<T>, format: &Attrs) -> Self {
        self.insert_fmt_with(content, format);
        self
    }
    pub fn insert_fmt_with(&mut self, content: impl Into<T>, attrs: &Attrs) {
        if let Some(Op::Insert { items, format }) = self.children.last_mut()
            && format_eq(format.as_deref(), Some(attrs))
        {
            items.push(content.into());
        } else {
            let format = if attrs.is_empty() {
                None
            } else {
                Some(Box::new(attrs.clone()))
            };
            self.children.push(Op::Insert {
                items: vec![content.into()],
                format,
            });
        }
    }

    pub fn insert_range(mut self, contents: impl IntoIterator<Item = T>) -> Self {
        self.insert_range_with(contents);
        self
    }

    pub fn insert_range_with(&mut self, contents: impl IntoIterator<Item = T>) {
        if let Some(Op::Insert {
            items,
            format: None,
        }) = self.children.last_mut()
        {
            items.extend(contents);
        } else {
            self.children.push(Op::Insert {
                items: contents.into_iter().collect(),
                format: None,
            });
        }
    }

    pub fn insert_range_fmt(
        mut self,
        contents: impl IntoIterator<Item = T>,
        attrs: &Attrs,
    ) -> Self {
        self.insert_range_fmt_with(contents, attrs);
        self
    }

    pub fn insert_range_fmt_with(&mut self, contents: impl IntoIterator<Item = T>, attrs: &Attrs) {
        if let Some(Op::Insert { items, format }) = self.children.last_mut()
            && format_eq(format.as_deref(), Some(&attrs))
        {
            items.extend(contents);
        } else {
            let format = if attrs.is_empty() {
                None
            } else {
                Some(Box::new(attrs.clone()))
            };
            self.children.push(Op::Insert {
                items: contents.into_iter().collect(),
                format,
            });
        }
    }

    /// Sets an attribute to the given value.
    pub fn insert_attr(mut self, key: impl Into<Arc<str>>, value: impl Into<T>) -> Self {
        self.insert_attr_with(key, value);
        self
    }
    pub fn insert_attr_with(&mut self, key: impl Into<Arc<str>>, value: impl Into<T>) {
        self.attrs.insert(
            key.into(),
            AttrOp::Update {
                value: value.into(),
                prev: None,
            },
        );
    }
}

fn format_eq(a: Option<&Attrs>, b: Option<&Attrs>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), None) if a.is_empty() => true,
        (None, Some(b)) if b.is_empty() => true,
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

impl Delta<In> {
    pub fn new() -> Self {
        Delta {
            name: None,
            children: Vec::new(),
            attrs: HashMap::new(),
            #[cfg(feature = "weak")]
            link: None,
        }
    }

    #[cfg(feature = "weak")]
    pub fn link(link: LinkSource) -> Self {
        Delta {
            link: Some(link),
            ..Delta::default()
        }
    }
}

impl Delta<Out> {
    pub fn out() -> Self {
        Self::default()
    }
}

impl From<Delta<In>> for In {
    #[inline]
    fn from(value: Delta<In>) -> Self {
        In::Node(Box::new(value))
    }
}

impl<T> Default for Delta<T> {
    fn default() -> Self {
        Delta {
            name: None,
            children: vec![],
            attrs: Default::default(),
            #[cfg(feature = "weak")]
            link: None,
        }
    }
}

impl<T> Op<T> {
    /// Content length of this operation. Inserts, retains and modifies contribute to length.
    /// Deletes return 0 as they don't occupy space in the result.
    pub fn len(&self) -> u32 {
        match self {
            Op::InsertText { text, .. } => text.len() as u32,
            Op::Insert { items: content, .. } => content.len() as u32,
            Op::Remove { .. } => 0,
            Op::Retain { len, .. } => *len,
            Op::Modify { .. } => 1,
        }
    }

    /// Returns the formatting attributes on this operation, if any.
    pub fn format(&self) -> Option<&Attrs> {
        match self {
            Op::InsertText { format, .. }
            | Op::Insert { format, .. }
            | Op::Retain { format, .. }
            | Op::Modify { format, .. } => format.as_deref(),
            Op::Remove { .. } => None,
        }
    }

    /// Recursively transforms content values from type `T` to type `U`.
    pub fn map<U>(self, mut f: impl FnMut(T) -> U) -> Op<U> {
        self.map_inner(&mut f)
    }

    fn map_inner<U>(self, f: &mut impl FnMut(T) -> U) -> Op<U> {
        match self {
            Op::InsertText { text, format } => Op::InsertText { text, format },
            Op::Insert {
                items: content,
                format,
            } => Op::Insert {
                items: content.into_iter().map(&mut *f).collect(),
                format,
            },
            Op::Remove {
                len,
                prev: prev_value,
            } => Op::Remove {
                len,
                prev: prev_value.map(|d| Box::new(d.map_inner(f))),
            },
            Op::Retain { len, format } => Op::Retain { len, format },
            Op::Modify { delta, format } => Op::Modify {
                delta: Box::new(delta.map_inner(f)),
                format,
            },
        }
    }
}

impl<T> AttrOp<T> {
    /// Recursively transforms content values from type `T` to type `U`.
    pub fn map<U>(self, mut f: impl FnMut(T) -> U) -> AttrOp<U> {
        self.map_inner(&mut f)
    }

    fn map_inner<U>(self, f: &mut impl FnMut(T) -> U) -> AttrOp<U> {
        match self {
            AttrOp::Update {
                value,
                prev: prev_value,
            } => AttrOp::Update {
                value: f(value),
                prev: prev_value.map(&mut *f),
            },
            AttrOp::Remove { prev: prev_value } => AttrOp::Remove {
                prev: prev_value.map(f),
            },
            AttrOp::Modify { delta } => AttrOp::Modify {
                delta: Box::new(delta.map_inner(f)),
            },
        }
    }
}
