use crate::node::Attrs;
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
}

/// A single operation on the ordered children axis of a [Delta].
#[derive(Debug, Clone, PartialEq)]
pub enum Op<T = Out> {
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
    /// Retain (skip over) a number of elements, optionally applying a formatting change.
    Retain {
        len: u32,
        format: Option<Box<Attrs>>,
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

impl<T> Delta<T> {
    pub fn with_name(name: impl Into<Arc<str>>) -> Self {
        Delta {
            name: Some(name.into()),
            children: Vec::new(),
            attrs: HashMap::new(),
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
        }
    }

    pub fn insert_text(mut self, text: impl Into<String>) -> Self {
        self.children.push(Op::InsertText {
            text: text.into(),
            format: None,
        });
        self
    }

    /// Appends a text insert operation with formatting attributes.
    pub fn insert_text_with(mut self, text: impl Into<String>, format: Attrs) -> Self {
        self.children.push(Op::InsertText {
            text: text.into(),
            format: Some(Box::new(format)),
        });
        self
    }

    /// Appends a delete operation for `len` elements.
    pub fn remove(mut self, len: u32) -> Self {
        self.children.push(Op::Remove { len, prev: None });
        self
    }

    /// Appends a retain (skip) operation for `len` elements.
    pub fn retain(mut self, len: u32) -> Self {
        self.children.push(Op::Retain { len, format: None });
        self
    }

    /// Appends a retain operation with a formatting change.
    pub fn retain_with(mut self, len: u32, format: Attrs) -> Self {
        self.children.push(Op::Retain {
            len,
            format: Some(Box::new(format)),
        });
        self
    }

    /// Appends a modify operation that applies a nested delta to an embedded child.
    pub fn modify(mut self, delta: Self) -> Self {
        self.children.push(Op::Modify {
            delta: Box::new(delta),
            format: None,
        });
        self
    }

    /// Appends a modify operation with a formatting change.
    pub fn modify_with(mut self, delta: Self, format: Attrs) -> Self {
        self.children.push(Op::Modify {
            delta: Box::new(delta),
            format: Some(Box::new(format)),
        });
        self
    }

    /// Removes an attribute by key.
    pub fn remove_attr(mut self, key: impl Into<Arc<str>>) -> Self {
        self.attrs.insert(key.into(), AttrOp::Remove { prev: None });
        self
    }

    /// Applies a nested delta to an attribute whose value is a node.
    pub fn modify_attr(mut self, key: impl Into<Arc<str>>, delta: Self) -> Self {
        self.attrs.insert(
            key.into(),
            AttrOp::Modify {
                delta: Box::new(delta),
            },
        );
        self
    }

    /// Composes another delta on top of this one. The result represents
    /// applying `self` followed by `other`.
    pub fn join(&mut self, other: Self) -> &mut Self {
        self.children.extend(other.children);
        for (key, value) in other.attrs {
            self.attrs.insert(key, value);
        }
        self
    }
}

impl Delta<In> {
    pub fn new() -> Self {
        Delta {
            name: None,
            children: Vec::new(),
            attrs: HashMap::new(),
        }
    }

    /// Appends a single content value insert operation.
    pub fn insert(mut self, content: impl Into<In>) -> Self {
        if let Some(Op::Insert { items, format }) = self.children.last_mut() {
            if format.is_none() {
                items.push(content.into());
            }
        } else {
            self.children.push(Op::Insert {
                items: vec![content.into()],
                format: None,
            });
        }
        self
    }

    /// Appends a single content value insert operation with formatting attributes.
    pub fn insert_with(mut self, content: impl Into<In>, format: Attrs) -> Self {
        self.children.push(Op::Insert {
            items: vec![content.into()],
            format: Some(Box::new(format)),
        });
        self
    }

    /// Sets an attribute to the given value.
    pub fn insert_attr(mut self, key: impl Into<Arc<str>>, value: impl Into<In>) -> Self {
        self.attrs.insert(
            key.into(),
            AttrOp::Update {
                value: value.into(),
                prev: None,
            },
        );
        self
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
        In::Node(value)
    }
}

impl<T> Default for Delta<T> {
    fn default() -> Self {
        Delta {
            name: None,
            children: vec![],
            attrs: Default::default(),
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
