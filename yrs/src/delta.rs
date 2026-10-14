use crate::node::Attrs;
#[cfg(feature = "weak")]
use crate::weak::LinkSource;
use crate::{AttributeValue, ContentAttributes, In, Out};
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
        attribution: Option<Box<Attribution>>,
    },
    /// Insert a text string with optional formatting.
    InsertText {
        text: String,
        format: Option<Box<Attrs>>,
        attribution: Option<Box<Attribution>>,
    },
    /// Insert one or more non-text content values with optional formatting.
    Insert {
        items: Vec<T>,
        format: Option<Box<Attrs>>,
        attribution: Option<Box<Attribution>>,
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
        attribution: Option<Box<Attribution>>,
    },
}

/// A single operation on the key-value attributes axis of a [Delta].
#[derive(Debug, Clone, PartialEq)]
pub enum AttrOp<T = Out> {
    /// Update attribute value.
    /// `prev_value` captures the old value when produced by diff/undo operations.
    Update {
        value: T,
        prev: Option<T>,
        attribution: Option<Box<Attribution>>,
    },
    /// Remove an attribute.
    /// `prev_value` captures the removed value when produced by diff/undo operations.
    Remove {
        prev: Option<T>,
        attribution: Option<Box<Attribution>>,
    },
    /// Apply a nested delta to an attribute whose value is itself a node.
    Modify {
        delta: Box<Delta<T>>,
        attribution: Option<Box<Attribution>>,
    },
}

impl<T> AttrOp<T> {
    #[inline]
    pub fn update(value: T, prev: Option<T>) -> Self {
        AttrOp::Update {
            value,
            prev,
            attribution: None,
        }
    }

    #[inline]
    pub fn remove(prev: Option<T>) -> Self {
        AttrOp::Remove {
            prev,
            attribution: None,
        }
    }

    pub fn modify(delta: impl Into<Box<Delta<T>>>) -> Self {
        AttrOp::Modify {
            delta: delta.into(),
            attribution: None,
        }
    }

    /// Attribution metadata of this operation.
    pub fn attribution(&self) -> Option<&Attribution> {
        match self {
            AttrOp::Update { attribution, .. }
            | AttrOp::Remove { attribution, .. }
            | AttrOp::Modify { attribution, .. } => attribution.as_deref(),
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
        self.push_text(text.as_ref(), None, None);
        self
    }

    pub fn insert_text_with(&mut self, text: impl AsRef<str>) {
        self.push_text(text.as_ref(), None, None);
    }

    /// Appends a text insert operation with formatting attributes.
    #[inline]
    pub fn insert_text_fmt(mut self, text: impl AsRef<str>, format: &Attrs) -> Self {
        self.push_text(text.as_ref(), Some(format), None);
        self
    }

    /// Appends a text insert operation with formatting attributes.
    pub fn insert_text_fmt_with(&mut self, text: impl AsRef<str>, attrs: &Attrs) {
        self.push_text(text.as_ref(), Some(attrs), None);
    }

    /// Appends a text insert operation with formatting attributes and attribution.
    #[inline]
    pub fn insert_text_attributed(
        mut self,
        text: impl AsRef<str>,
        format: Option<&Attrs>,
        attribution: Option<Attribution>,
    ) -> Self {
        self.push_text(text.as_ref(), format, attribution);
        self
    }

    pub fn insert_text_attributed_with(
        &mut self,
        text: impl AsRef<str>,
        format: Option<&Attrs>,
        attribution: Option<Attribution>,
    ) {
        self.push_text(text.as_ref(), format, attribution);
    }

    fn push_text(&mut self, str: &str, attrs: Option<&Attrs>, attribution: Option<Attribution>) {
        if str.is_empty() {
            return;
        } else if let Some(Op::InsertText {
            text,
            format,
            attribution: a,
        }) = self.children.last_mut()
            && format_eq(format.as_deref(), attrs)
            && a.as_deref() == attribution.as_ref()
        {
            text.push_str(str);
        } else {
            self.children.push(Op::InsertText {
                text: str.into(),
                format: boxed_format(attrs),
                attribution: attribution.map(Box::new),
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
        self.push_retain(count, None, None);
        self
    }

    pub fn retain_with(&mut self, count: u32) {
        self.push_retain(count, None, None);
    }

    /// Appends a retain operation with a formatting change.
    #[inline]
    pub fn retain_fmt(mut self, count: u32, format: &Attrs) -> Self {
        self.push_retain(count, Some(format), None);
        self
    }

    pub fn retain_fmt_with(&mut self, count: u32, attrs: &Attrs) {
        self.push_retain(count, Some(attrs), None);
    }

    #[inline]
    pub fn retain_attributed(
        mut self,
        count: u32,
        format: Option<&Attrs>,
        attribution: Option<Attribution>,
    ) -> Self {
        self.push_retain(count, format, attribution);
        self
    }

    pub fn retain_attributed_with(
        &mut self,
        count: u32,
        format: Option<&Attrs>,
        attribution: Option<Attribution>,
    ) {
        self.push_retain(count, format, attribution);
    }

    fn push_retain(&mut self, count: u32, attrs: Option<&Attrs>, attribution: Option<Attribution>) {
        if count == 0 {
            return;
        } else if let Some(Op::Retain {
            len,
            format,
            attribution: a,
        }) = self.children.last_mut()
            && format_eq(format.as_deref(), attrs)
            && a.as_deref() == attribution.as_ref()
        {
            *len += count;
        } else {
            self.children.push(Op::Retain {
                len: count,
                format: boxed_format(attrs),
                attribution: attribution.map(Box::new),
            });
        }
    }

    /// Appends a modify operation that applies a nested delta to an embedded child.
    #[inline]
    pub fn modify(mut self, delta: Self) -> Self {
        self.modify_attributed_with(delta, None, None);
        self
    }

    pub fn modify_with(&mut self, delta: Self) {
        self.modify_attributed_with(delta, None, None);
    }

    /// Appends a modify operation with a formatting change.
    #[inline]
    pub fn modify_fmt(mut self, delta: Self, format: &Attrs) -> Self {
        self.modify_attributed_with(delta, Some(format), None);
        self
    }

    pub fn modify_fmt_with(&mut self, delta: Self, format: &Attrs) {
        self.modify_attributed_with(delta, Some(format), None);
    }

    #[inline]
    pub fn modify_attributed(
        mut self,
        delta: Self,
        format: Option<&Attrs>,
        attribution: Option<Attribution>,
    ) -> Self {
        self.modify_attributed_with(delta, format, attribution);
        self
    }

    pub fn modify_attributed_with(
        &mut self,
        delta: Self,
        format: Option<&Attrs>,
        attribution: Option<Attribution>,
    ) {
        self.children.push(Op::Modify {
            delta: Box::new(delta),
            format: format.map(|f| Box::new(f.clone())),
            attribution: attribution.map(Box::new),
        });
    }

    /// Removes an attribute by key.
    #[inline]
    pub fn remove_attr(mut self, key: impl Into<Arc<str>>) -> Self {
        self.remove_attr_attributed_with(key, None);
        self
    }

    pub fn remove_attr_with(&mut self, key: impl Into<Arc<str>>) {
        self.remove_attr_attributed_with(key, None);
    }

    #[inline]
    pub fn remove_attr_attributed(
        mut self,
        key: impl Into<Arc<str>>,
        attribution: Option<Attribution>,
    ) -> Self {
        self.remove_attr_attributed_with(key, attribution);
        self
    }

    pub fn remove_attr_attributed_with(
        &mut self,
        key: impl Into<Arc<str>>,
        attribution: Option<Attribution>,
    ) {
        let op = AttrOp::Remove {
            prev: None,
            attribution: attribution.map(Box::new),
        };
        self.attrs.insert(key.into(), op);
    }

    /// Applies a nested delta to an attribute whose value is a node.
    #[inline]
    pub fn modify_attr(mut self, key: impl Into<Arc<str>>, delta: Self) -> Self {
        self.modify_attr_attributed_with(key, delta, None);
        self
    }

    pub fn modify_attr_with(&mut self, key: impl Into<Arc<str>>, delta: Self) {
        self.modify_attr_attributed_with(key, delta, None);
    }

    #[inline]
    pub fn modify_attr_attributed(
        mut self,
        key: impl Into<Arc<str>>,
        delta: Self,
        attribution: Option<Attribution>,
    ) -> Self {
        self.modify_attr_attributed_with(key, delta, attribution);
        self
    }

    pub fn modify_attr_attributed_with(
        &mut self,
        key: impl Into<Arc<str>>,
        delta: Self,
        attribution: Option<Attribution>,
    ) {
        let op = AttrOp::Modify {
            delta: Box::new(delta),
            attribution: attribution.map(Box::new),
        };
        self.attrs.insert(key.into(), op);
    }

    /// Removes trailing retains without formatting, as they don't change anything - a port of
    /// lib0 `DeltaBuilder.done`.
    pub(crate) fn trim_retains(&mut self) {
        while let Some(Op::Retain {
            format: None,
            attribution: None,
            ..
        }) = self.children.last()
        {
            self.children.pop();
        }
    }

    /// Composes another delta on top of this one. The result represents
    /// applying `self` followed by `other`.
    pub fn join(mut self, other: Self) -> Self {
        self.join_with(other);
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
        self.push_insert(std::iter::once(content.into()), None, None);
        self
    }

    pub fn insert_with(&mut self, content: impl Into<T>) {
        self.push_insert(std::iter::once(content.into()), None, None);
    }

    /// Appends a single content value insert operation with formatting attributes.
    #[inline]
    pub fn insert_fmt(mut self, content: impl Into<T>, format: &Attrs) -> Self {
        self.push_insert(std::iter::once(content.into()), Some(format), None);
        self
    }

    pub fn insert_fmt_with(&mut self, content: impl Into<T>, attrs: &Attrs) {
        self.push_insert(std::iter::once(content.into()), Some(attrs), None);
    }

    pub fn insert_range(mut self, contents: impl IntoIterator<Item = T>) -> Self {
        self.push_insert(contents, None, None);
        self
    }

    pub fn insert_range_with(&mut self, contents: impl IntoIterator<Item = T>) {
        self.push_insert(contents, None, None);
    }

    pub fn insert_range_fmt(
        mut self,
        contents: impl IntoIterator<Item = T>,
        attrs: &Attrs,
    ) -> Self {
        self.push_insert(contents, Some(attrs), None);
        self
    }

    pub fn insert_range_fmt_with(&mut self, contents: impl IntoIterator<Item = T>, attrs: &Attrs) {
        self.push_insert(contents, Some(attrs), None);
    }

    pub fn insert_range_attributed(
        mut self,
        contents: impl IntoIterator<Item = T>,
        format: Option<&Attrs>,
        attribution: Option<Attribution>,
    ) -> Self {
        self.push_insert(contents, format, attribution);
        self
    }

    pub fn insert_range_attributed_with(
        &mut self,
        contents: impl IntoIterator<Item = T>,
        format: Option<&Attrs>,
        attribution: Option<Attribution>,
    ) {
        self.push_insert(contents, format, attribution);
    }

    fn push_insert(
        &mut self,
        contents: impl IntoIterator<Item = T>,
        attrs: Option<&Attrs>,
        attribution: Option<Attribution>,
    ) {
        if let Some(Op::Insert {
            items,
            format,
            attribution: a,
        }) = self.children.last_mut()
            && format_eq(format.as_deref(), attrs)
            && a.as_deref() == attribution.as_ref()
        {
            items.extend(contents);
        } else {
            self.children.push(Op::Insert {
                items: contents.into_iter().collect(),
                format: boxed_format(attrs),
                attribution: attribution.map(Box::new),
            });
        }
    }

    /// Sets an attribute to the given value.
    pub fn insert_attr(mut self, key: impl Into<Arc<str>>, value: impl Into<T>) -> Self {
        self.insert_attr_attributed_with(key, value, None);
        self
    }

    pub fn insert_attr_with(&mut self, key: impl Into<Arc<str>>, value: impl Into<T>) {
        self.insert_attr_attributed_with(key, value, None);
    }

    /// Sets an attribute to the given value, attaching attribution to the operation.
    pub fn insert_attr_attributed(
        mut self,
        key: impl Into<Arc<str>>,
        value: impl Into<T>,
        attribution: Option<Attribution>,
    ) -> Self {
        self.insert_attr_attributed_with(key, value, attribution);
        self
    }

    pub fn insert_attr_attributed_with(
        &mut self,
        key: impl Into<Arc<str>>,
        value: impl Into<T>,
        attribution: Option<Attribution>,
    ) {
        let op = AttrOp::Update {
            value: value.into(),
            prev: None,
            attribution: attribution.map(Box::new),
        };
        self.attrs.insert(key.into(), op);
    }
}

fn boxed_format(attrs: Option<&Attrs>) -> Option<Box<Attrs>> {
    match attrs {
        Some(attrs) if !attrs.is_empty() => Some(Box::new(attrs.clone())),
        _ => None,
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

    /// Returns the attribution metadata of this operation, if any.
    pub fn attribution(&self) -> Option<&Attribution> {
        match self {
            Op::InsertText { attribution, .. }
            | Op::Insert { attribution, .. }
            | Op::Retain { attribution, .. }
            | Op::Modify { attribution, .. } => attribution.as_deref(),
            Op::Remove { .. } => None,
        }
    }

    /// Recursively transforms content values from type `T` to type `U`.
    pub fn map<U>(self, mut f: impl FnMut(T) -> U) -> Op<U> {
        self.map_inner(&mut f)
    }

    fn map_inner<U>(self, f: &mut impl FnMut(T) -> U) -> Op<U> {
        match self {
            Op::InsertText {
                text,
                format,
                attribution,
            } => Op::InsertText {
                text,
                format,
                attribution,
            },
            Op::Insert {
                items: content,
                format,
                attribution,
            } => Op::Insert {
                items: content.into_iter().map(&mut *f).collect(),
                format,
                attribution,
            },
            Op::Remove {
                len,
                prev: prev_value,
            } => Op::Remove {
                len,
                prev: prev_value.map(|d| Box::new(d.map_inner(f))),
            },
            Op::Retain {
                len,
                format,
                attribution,
            } => Op::Retain {
                len,
                format,
                attribution,
            },
            Op::Modify {
                delta,
                format,
                attribution,
            } => Op::Modify {
                delta: Box::new(delta.map_inner(f)),
                format,
                attribution,
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
                attribution,
            } => AttrOp::Update {
                value: f(value),
                prev: prev_value.map(&mut *f),
                attribution,
            },
            AttrOp::Remove {
                prev: prev_value,
                attribution,
            } => AttrOp::Remove {
                prev: prev_value.map(f),
                attribution,
            },
            AttrOp::Modify { delta, attribution } => AttrOp::Modify {
                delta: Box::new(delta.map_inner(f)),
                attribution,
            },
        }
    }
}

pub type Attribute = Arc<str>;

/// Provenance metadata on a content/attr op: *who/what* inserted, deleted, or formatted it.
///
/// The canonical shape below is a convention — apply/diff/equality treat attribution as
/// an **opaque** object (never branching on these field names), so custom keys flow through
/// — with ONE exception: the nested `format` key merges per inner key (one level),
/// so applying `{format:{italic:[…]}}` *adds* italic while keeping a pre-existing
/// `{format:{bold:[…]}}`, and `{format:{bold:null}}` removes just that inner key
/// (an emptied `format` is dropped). Every other key is flat/wholesale.
///
/// The `format`/`format_at` part exists only on children (content ops), never on node attribute ops.
/// As an *update value* on `retain`/`modify`/`*Attr`, attribution uses the same unified tri-state
/// as {@link Formats} (`undefined` skip / `null` clear / `{k:v}` set / `{k:null}` remove) — see
/// the delta `readme.md` "Formats & Attributions". The blanket `null`-clear channel caveat on
/// {@link Formats} applies here too; note `rebase` does not reconcile attribution at all
/// (concurrent attribution edits don't converge), so prefer deterministic attribution assignment
/// outside of concurrent editing.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Attribution {
    /// Attributors of an insertion. `Some(vec![])` means "inserted, by nobody in particular".
    pub insert: Option<Vec<Attribute>>,
    /// Timestamp of the insertion; `0` means not set.
    pub insert_at: u64,
    /// Attributors of a deletion.
    pub delete: Option<Vec<Attribute>>,
    /// Timestamp of the deletion; `0` means not set.
    pub delete_at: u64,
    /// Attributors of formatting changes per format key. A `None` value is a per-key clear.
    pub format: Option<HashMap<Arc<str>, Option<Vec<Attribute>>>>,
    /// Timestamp of the formatting change; `0` means not set.
    pub format_at: u64,
}

impl Attribution {
    pub(crate) fn resolve<A: AttributeValue>(
        attrs: Option<&ContentAttributes<A>>,
        deleted: bool,
    ) -> Option<Self> {
        let attrs = attrs?;
        let mut a = Attribution::default();
        if deleted {
            a.delete = Some(Vec::new());
        } else {
            a.insert = Some(Vec::new());
        }
        for attr in attrs.iter() {
            let value = attr.value();
            match attr.name() {
                // ignored if it doesn't match the piece's deleted flag
                "insert" => {
                    if let (Some(list), Some(by)) = (&mut a.insert, value.attributor()) {
                        list.push(by);
                    }
                }
                "delete" => {
                    if let (Some(list), Some(by)) = (&mut a.delete, value.attributor()) {
                        list.push(by);
                    }
                }
                "insertAt" => {
                    let ts = value.timestamp();
                    if ts != 0 {
                        a.insert_at = ts;
                    }
                }
                "deleteAt" => {
                    let ts = value.timestamp();
                    if ts != 0 {
                        a.delete_at = ts;
                    }
                }
                _ => {}
            }
        }
        Some(a)
    }

    /// `{ insert: by }`
    pub fn insert<A: Into<Attribute>>(by: impl IntoIterator<Item = A>) -> Self {
        Attribution {
            insert: Some(by.into_iter().map(Into::into).collect()),
            ..Default::default()
        }
    }

    /// `{ delete: by }`
    pub fn delete<A: Into<Attribute>>(by: impl IntoIterator<Item = A>) -> Self {
        Attribution {
            delete: Some(by.into_iter().map(Into::into).collect()),
            ..Default::default()
        }
    }

    /// `{ format: { key: by } }`
    pub fn format<A: Into<Attribute>>(key: &str, by: impl IntoIterator<Item = A>) -> Self {
        Attribution::default().with_format(key, by)
    }

    /// Adds `format[key] = by`.
    pub fn with_format<A: Into<Attribute>>(
        mut self,
        key: &str,
        by: impl IntoIterator<Item = A>,
    ) -> Self {
        let by = by.into_iter().map(Into::into).collect();
        self.format
            .get_or_insert_with(HashMap::new)
            .insert(key.into(), Some(by));
        self
    }

    fn is_empty(&self) -> bool {
        self.insert.is_none()
            && self.insert_at == 0
            && self.delete.is_none()
            && self.delete_at == 0
            && self.format.is_none()
            && self.format_at == 0
    }

    fn resolved(mut self) -> Option<Self> {
        if let Some(format) = &mut self.format {
            format.retain(|_, by| by.is_some());
            if format.is_empty() {
                self.format = None;
            }
        }
        (!self.is_empty()).then_some(self)
    }
}

impl Attribution {
    /// Merges `right` over `left`, overriding `left`'s fields if they were defined on the `right`.
    /// Attribution's format entries are merged individually.
    /// Returns `None` if the resulting [Attribution] is empty.
    pub fn combine(left: Option<&Attribution>, right: Option<&Attribution>) -> Option<Attribution> {
        let own = match right {
            Some(own) if !own.is_empty() => own,
            _ => return left.filter(|a| !a.is_empty()).cloned(),
        };
        let mut r = left.cloned().unwrap_or_default();
        if own.insert.is_some() {
            r.insert = own.insert.clone();
        }
        if own.insert_at != 0 {
            r.insert_at = own.insert_at;
        }
        if own.delete.is_some() {
            r.delete = own.delete.clone();
        }
        if own.delete_at != 0 {
            r.delete_at = own.delete_at;
        }
        if own.format_at != 0 {
            r.format_at = own.format_at;
        }
        if let Some(format) = &own.format {
            let f = r.format.get_or_insert_with(HashMap::new);
            for (key, by) in format {
                f.insert(key.clone(), by.clone());
            }
            if f.is_empty() {
                r.format = None;
            }
        }
        (!r.is_empty()).then_some(r)
    }

    /// Like [Attribution::combine], but for data operations (inserts, attribute updates): per-key
    /// format clears are dropped from the result.
    pub(crate) fn combine_data(
        ambient: Option<&Attribution>,
        own: Option<&Attribution>,
    ) -> Option<Attribution> {
        Self::combine(ambient, own)?.resolved()
    }
}
