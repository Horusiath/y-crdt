use crate::block::{ID, Item, ItemContent, ItemPtr};
use crate::node::{Attrs, Node, NodePtr, TypePtr};
use crate::transaction::TransactionMut;
use crate::utils::OptionExt;
use crate::{Any, In, NodeRef, OffsetKind, Out};
use std::collections::HashMap;
use std::sync::Arc;

/// A position within the sequence component of a [Node], used to iterate over node's children and
/// keep track of active formatting attributes at its current position.
///
/// It can also be used to introduce modifications (inserting, removal and formatting) at
/// its position.
pub(crate) struct Cursor {
    parent: NodePtr,
    left: Option<ItemPtr>,
    right: Option<ItemPtr>,
    /// Offset inside of `right`.
    offset: u32,
    /// Number of visible elements before the cursor.
    index: u32,
    /// Formatting attributes active at the cursor position.
    attrs: Option<Box<Attrs>>,
    encoding: OffsetKind,
}

impl Cursor {
    /// Creates a cursor placed at the beginning of a `parent` node.
    pub fn new(parent: NodePtr, encoding: OffsetKind) -> Self {
        Cursor {
            parent,
            left: None,
            right: parent.start,
            offset: 0,
            index: 0,
            attrs: None,
            encoding,
        }
    }

    #[inline]
    pub fn right(&self) -> Option<ItemPtr> {
        self.right
    }

    #[inline]
    pub fn left(&self) -> Option<ItemPtr> {
        self.left
    }

    /// Creates a cursor placed at a given `index` of a `parent` node, or `None` if the index
    /// exceeds the node's length. An `index` inside of a character is rounded up past it.
    pub fn at(parent: NodePtr, encoding: OffsetKind, index: u32) -> Option<Self> {
        let mut cursor = Self::new(parent, encoding);
        if cursor.forward(index) >= index {
            Some(cursor)
        } else {
            None
        }
    }

    #[inline]
    pub fn parent(&self) -> NodePtr {
        self.parent
    }

    /// Number of visible elements before the cursor.
    #[inline]
    pub fn index(&self) -> u32 {
        self.index
    }

    /// Formatting attributes active at the cursor position.
    #[inline]
    pub fn attrs(&self) -> Option<&Attrs> {
        self.attrs.as_deref()
    }

    /// Returns `true` if there are no more items to the right of the cursor.
    #[inline]
    pub fn finished(&self) -> bool {
        self.right.is_none()
    }

    /// Moves the cursor over the (remainder of the) `right` item. Returns `false` if the cursor
    /// is already at the end.
    fn next_item(&mut self) -> bool {
        let Some(right) = self.right else {
            return false;
        };
        match &right.content {
            ItemContent::Format(key, value) => {
                if !right.is_deleted() {
                    update_attrs(self.attrs.get_or_init(), key, value);
                }
            }
            _ => {
                let len = right.visible_len(self.encoding);
                self.index += len - self.offset;
            }
        }
        self.offset = 0;
        self.left = Some(right);
        self.right = right.right;
        true
    }

    /// Moves the cursor over non-visible (deleted and non-countable) items.
    pub(crate) fn skip_hidden(&mut self) {
        while self.offset == 0
            && let Some(right) = self.right
            && (right.is_deleted() || !right.is_countable())
        {
            self.next_item();
        }
    }

    /// Moves the cursor forward by `len` visible elements, stopping at last visible element it
    /// moved over.
    ///
    /// Returns, number of elements it actually moved over. That number:
    /// - can be lower than `len` if the end of the node was reached
    /// - higher than `len` if the `len` pointed inside the character. In that case [Cursor] will
    ///   move over it and return value beyond the `len`.
    pub fn forward(&mut self, len: u32) -> u32 {
        let mut remaining = len;
        while remaining > 0
            && let Some(right) = self.right
        {
            let item_len = right.visible_len(self.encoding);
            let available = item_len - self.offset;
            if remaining < available {
                let offset = ceil_offset(right, self.offset + remaining, self.encoding);
                let moved = len - remaining + (offset - self.offset);
                if offset == item_len {
                    self.next_item();
                } else {
                    self.index += offset - self.offset;
                    self.offset = offset;
                }
                return moved;
            }
            remaining -= available;
            self.next_item();
        }
        len - remaining
    }

    pub fn next_id(&mut self) -> Option<ID> {
        self.skip_hidden();

        let item = self.right?;
        let mut id = item.id.clone();
        id.clock += clock_offset(item, self.offset, self.encoding);
        Some(id)
    }

    /// Returns the element placed right after the cursor without moving it.
    pub fn get(&self) -> Option<Out> {
        let mut offset = self.offset;
        let mut curr = self.right;
        while let Some(item) = curr {
            if item.visible_len(self.encoding) > offset {
                let mut buf = [Out::default()];
                read_item(item, offset, &mut buf, self.encoding);
                return Some(std::mem::take(&mut buf[0]));
            }
            offset = 0;
            curr = item.right;
        }
        None
    }

    /// Reads elements following the cursor into `buf`, moving the cursor over them.
    /// Returns the number of elements read.
    pub fn read(&mut self, buf: &mut [Out]) -> usize {
        let mut read = 0;
        while read < buf.len() {
            self.skip_hidden();
            let Some(item) = self.right else {
                break;
            };
            let (n, units) = read_item(item, self.offset, &mut buf[read..], self.encoding);
            read += n;
            self.offset += units;
            self.index += units;
            if self.offset >= item.visible_len(self.encoding) {
                self.next_item();
            }
        }
        read
    }

    /// Splits the `right` item if the cursor is placed inside of it, so that the cursor
    /// falls on a block boundary.
    pub fn split(&mut self, txn: &mut TransactionMut) {
        if self.offset > 0
            && let Some(right) = self.right
        {
            self.right = split_item(txn, right, self.offset, self.encoding);
            self.left = Some(right);
            self.offset = 0;
        }
    }

    /// Inserts a string of `text` at the cursor position, moving the cursor after it.
    pub fn insert_text(
        &mut self,
        txn: &mut TransactionMut,
        text: &str,
        attrs: Option<&Attrs>,
    ) -> Option<ItemPtr> {
        if text.is_empty() {
            return None;
        }
        self.insert_content(txn, ItemContent::String(text.into()), attrs, None)
    }

    /// Inserts a single `value` at the cursor position, moving the cursor after it.
    pub fn insert(
        &mut self,
        txn: &mut TransactionMut,
        value: In,
        attrs: Option<&Attrs>,
    ) -> Option<ItemPtr> {
        match value {
            In::Any(any) => self.insert_content(txn, ItemContent::Any(vec![any]), attrs, None),
            In::Node(delta) => {
                let (node, remainder) = Node::from_delta(*delta);
                self.insert_content(txn, ItemContent::Node(node), attrs, remainder)
            }
            In::Doc(doc) => {
                let content = ItemContent::Doc(None, doc.options.clone());
                self.insert_content(txn, content, attrs, None)
            }
        }
    }

    /// Inserts `values` at the cursor position, moving the cursor after them.
    /// Consecutive [In::Any] values are packed into a single block.
    pub fn insert_range<I>(&mut self, txn: &mut TransactionMut, values: I, attrs: Option<&Attrs>)
    where
        I: IntoIterator<Item = In>,
    {
        let mut pending = Vec::new();
        for value in values {
            match value {
                In::Any(any) => pending.push(any),
                other => {
                    if !pending.is_empty() {
                        let content = ItemContent::Any(std::mem::take(&mut pending));
                        self.insert_content(txn, content, attrs, None);
                    }
                    self.insert(txn, other, attrs);
                }
            }
        }
        if !pending.is_empty() {
            self.insert_content(txn, ItemContent::Any(pending), attrs, None);
        }
    }

    /// Port of Yjs `insertContent`. `children` are applied to a node `content` once it's integrated.
    fn insert_content(
        &mut self,
        txn: &mut TransactionMut,
        content: ItemContent,
        attrs: Option<&Attrs>,
        children: Option<crate::Delta<In>>,
    ) -> Option<ItemPtr> {
        self.split(txn);
        let mut attrs = attrs.cloned().unwrap_or_default();
        if let Some(current) = self.attrs.as_deref() {
            // attributes active at the cursor position, but not requested, must be unset
            for key in current.keys() {
                attrs.entry(key.clone()).or_insert(Any::Null);
            }
        }
        self.minimize_format_changes(&attrs);
        let negated = self.insert_formats(txn, &attrs);
        let item = self.integrate(txn, content)?;
        if let Some(children) = children
            && let Some(node) = item.content.as_node()
        {
            NodeRef::new(node, &mut *txn).apply_delta(children);
        }
        self.right = Some(item);
        self.next_item();
        self.insert_negated_formats(txn, negated);
        Some(item)
    }

    /// Port of Yjs `formatText`: applies `attrs` to `len` elements following the cursor, moving
    /// the cursor after them. Returns the number of elements formatted.
    pub fn format(&mut self, txn: &mut TransactionMut, mut len: u32, attrs: &Attrs) -> u32 {
        let start_len = len;
        self.split(txn);
        self.minimize_format_changes(attrs);
        let mut negated = self.insert_formats(txn, attrs);
        // iterate until first non-format or null is found
        // delete all formats with attrs[format.key] != null
        // also check the attributes after the first non-format as we do not want to insert redundant
        // negated attributes there
        while let Some(right) = self.right {
            let is_format = matches!(right.content, ItemContent::Format(_, _));
            if !(len > 0 || (!negated.is_empty() && (right.is_deleted() || is_format))) {
                break;
            }
            match &right.content {
                ItemContent::Format(key, value) => {
                    if !right.is_deleted() {
                        if let Some(attr) = attrs.get(key) {
                            if attr == value.as_ref() {
                                negated.remove(key);
                            } else {
                                if len == 0 {
                                    // no need to further extend negated attributes
                                    break;
                                }
                                negated.insert(key.clone(), *value.clone());
                            }
                            txn.delete(right);
                        } else {
                            update_attrs(self.attrs.get_or_init(), key, value);
                        }
                    }
                }
                _ => {
                    let right_len = right.visible_len(self.encoding);
                    if len < right_len {
                        split_item(txn, right, len, self.encoding);
                        len = 0;
                    } else {
                        len -= right_len;
                    }
                }
            }
            self.next_item();
        }
        self.insert_negated_formats(txn, negated);
        start_len - len
    }

    /// Port of Yjs `deleteText`: removes `len` elements following the cursor.
    /// Returns the number of elements removed.
    pub fn remove(&mut self, txn: &mut TransactionMut, len: u32) -> u32 {
        self.split(txn);
        let start = self.right;
        let start_attrs = self.attrs.clone();
        let mut remaining = len;
        while remaining > 0
            && let Some(item) = self.right
        {
            if !item.is_deleted() && item.is_countable() {
                if remaining < item.content_len(self.encoding) {
                    split_item(txn, item, remaining, self.encoding);
                }
                remaining = remaining.saturating_sub(item.content_len(self.encoding));
                txn.delete(item);
            }
            self.next_item();
        }
        if start.is_some() {
            self.clean_format_gap(txn, start, start_attrs.as_deref());
        }
        len - remaining
    }

    /// Integrates a new item with a given `content` between `left` and `right` neighbors.
    fn integrate(&self, txn: &mut TransactionMut, content: ItemContent) -> Option<ItemPtr> {
        let id = ID::new(txn.doc.options.client_id, txn.doc.get_local_state());
        let item = Item::new(
            id,
            self.left,
            self.left.map(|ptr| ptr.last_id()),
            self.right,
            self.right.map(|ptr| ptr.id().clone()),
            TypePtr::Node(self.parent),
            None,
            content,
        )?;
        txn.integrate_item(item, 0)
    }

    /// Moves the cursor over deleted items and formatting markers which already match `attrs`.
    fn minimize_format_changes(&mut self, attrs: &Attrs) {
        while let Some(right) = self.right {
            let skip = if right.is_deleted() {
                true
            } else if let ItemContent::Format(key, value) = &right.content {
                attrs.get(key).unwrap_or(&Any::Null) == value.as_ref()
            } else {
                false
            };
            if !skip {
                break;
            }
            self.next_item();
        }
    }

    /// Inserts formatting markers for `attrs` differing from the ones active at the cursor
    /// position. Returns attributes that need to be restored once the formatted content ends.
    fn insert_formats(&mut self, txn: &mut TransactionMut, attrs: &Attrs) -> Attrs {
        let mut negated = Attrs::new();
        for (key, value) in attrs {
            let current = self
                .attrs
                .as_deref()
                .and_then(|a| a.get(key))
                .unwrap_or(&Any::Null);
            if value != current {
                negated.insert(key.clone(), current.clone());
                let content = ItemContent::Format(key.clone(), Box::new(value.clone()));
                self.right = self.integrate(txn, content);
                self.next_item();
            }
        }
        negated
    }

    /// Inserts formatting markers restoring `negated` attributes, unless they're already
    /// restored by the markers following the cursor.
    fn insert_negated_formats(&mut self, txn: &mut TransactionMut, mut negated: Attrs) {
        while let Some(right) = self.right {
            let skip = if right.is_deleted() {
                true
            } else if let ItemContent::Format(key, value) = &right.content {
                if negated.get(key) == Some(value.as_ref()) {
                    negated.remove(key);
                    true
                } else {
                    false
                }
            } else {
                false
            };
            if !skip {
                break;
            }
            self.next_item();
        }
        for (key, value) in negated {
            let content = ItemContent::Format(key, Box::new(value));
            self.right = self.integrate(txn, content);
            self.next_item();
        }
    }

    /// Removes formatting markers made redundant by removal of the content starting at `start`.
    fn clean_format_gap(
        &mut self,
        txn: &mut TransactionMut,
        mut start: Option<ItemPtr>,
        start_attrs: Option<&Attrs>,
    ) -> u32 {
        let curr = self.right;
        let mut end = start;
        let mut end_formats: HashMap<Arc<str>, ItemPtr> = HashMap::new();
        while let Some(item) = end
            && (!item.is_countable() || item.is_deleted())
        {
            if !item.is_deleted()
                && let ItemContent::Format(key, _) = &item.content
            {
                end_formats.insert(key.clone(), item);
            }
            end = item.right;
        }

        let mut cleanups = 0;
        let mut reached_curr = false;
        while start != end
            && let Some(item) = start
        {
            if start == curr {
                reached_curr = true;
            }
            if !item.is_deleted()
                && let ItemContent::Format(key, value) = &item.content
            {
                let value = value.as_ref();
                let start_value = start_attrs.and_then(|a| a.get(key)).unwrap_or(&Any::Null);
                if end_formats.get(key) != Some(&item) || start_value == value {
                    // either this format is overwritten or it's not necessary because
                    // the format already existed
                    txn.delete(item);
                    cleanups += 1;
                    let attrs = self.attrs.get_or_init();
                    if !reached_curr
                        && attrs.get(key).unwrap_or(&Any::Null) == value
                        && start_value != value
                    {
                        update_attrs(attrs, key, start_value);
                    }
                }
                if !reached_curr && !item.is_deleted() {
                    update_attrs(self.attrs.get_or_init(), key, value);
                }
            }
            start = item.right;
        }
        cleanups
    }
}

impl Iterator for Cursor {
    type Item = Out;

    fn next(&mut self) -> Option<Self::Item> {
        let mut buf = [Out::default()];
        if self.read(&mut buf) == 0 {
            None
        } else {
            Some(std::mem::take(&mut buf[0]))
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.parent.content_len.saturating_sub(self.index) as usize;
        (0, Some(remaining))
    }
}

fn update_attrs(attrs: &mut Attrs, key: &Arc<str>, value: &Any) {
    if let Any::Null = value {
        attrs.remove(key);
    } else {
        attrs.insert(key.clone(), value.clone());
    }
}

/// Maps an `offset` inside of an `item` expressed in `lib0` units onto clock units.
fn clock_offset(item: ItemPtr, offset: u32, encoding: OffsetKind) -> u32 {
    match &item.content {
        ItemContent::String(s) => {
            let (_, offset) = char_boundary(s, offset, encoding);
            s.block_offset(offset, encoding)
        }
        _ => offset,
    }
}

/// Rounds an `offset` inside of an `item` up to the nearest character boundary.
fn ceil_offset(item: ItemPtr, offset: u32, encoding: OffsetKind) -> u32 {
    match &item.content {
        ItemContent::String(s) => char_boundary(s, offset, encoding).1,
        _ => offset,
    }
}

/// Maps an `offset` (in `lib0` units) onto a byte index of `s`, rounding it up to the nearest
/// character boundary. Returns the byte index and the rounded offset.
fn char_boundary(s: &str, offset: u32, encoding: OffsetKind) -> (usize, u32) {
    match encoding {
        OffsetKind::Bytes => {
            let mut i = (offset as usize).min(s.len());
            while !s.is_char_boundary(i) {
                i += 1;
            }
            (i, i as u32)
        }
        OffsetKind::Utf16 => {
            let mut units = 0;
            for (i, c) in s.char_indices() {
                if units >= offset {
                    return (i, units);
                }
                units += c.len_utf16() as u32;
            }
            (s.len(), units)
        }
    }
}

/// Splits an `item` at a given `offset` (in `lib0` units), returning the right half.
fn split_item(
    txn: &mut TransactionMut,
    item: ItemPtr,
    offset: u32,
    encoding: OffsetKind,
) -> Option<ItemPtr> {
    let offset = clock_offset(item, offset, encoding);
    txn.doc.blocks.split_block(item, offset, OffsetKind::Utf16)
}

/// Reads elements of an `item` starting at `offset` into `buf`. Returns the number of elements
/// read and the number of `lib0` units the cursor has to move by, counting from `offset`.
fn read_item(item: ItemPtr, offset: u32, buf: &mut [Out], encoding: OffsetKind) -> (usize, u32) {
    match &item.content {
        ItemContent::String(s) => {
            let (start, boundary) = char_boundary(s, offset, encoding);
            let mut read = 0;
            let mut units = boundary - offset;
            for c in s[start..].chars().take(buf.len()) {
                buf[read] = Out::Any(Any::String(c.to_string().into()));
                read += 1;
                units += match encoding {
                    OffsetKind::Bytes => c.len_utf8(),
                    OffsetKind::Utf16 => c.len_utf16(),
                } as u32;
            }
            (read, units)
        }
        content => {
            let read = content.read(offset as usize, buf);
            (read, read as u32)
        }
    }
}

#[cfg(test)]
mod test {
    use super::Cursor;
    use crate::node::{Attrs, NodePtr};
    use crate::transaction::TransactionMut;
    use crate::{Any, Delta, DeltaOptions, Doc, In, OffsetKind, Options, Out};

    fn cursor(txn: &mut TransactionMut, index: u32) -> Cursor {
        let id = txn.node_mut("test").unwrap().id();
        let node: NodePtr = txn.doc.node(id).unwrap();
        Cursor::at(node, txn.doc.options.offset_kind, index).unwrap()
    }

    fn bold() -> Attrs {
        Attrs::from([("b".into(), Any::Bool(true))])
    }

    #[test]
    fn insert_inside_block() {
        let mut doc = Doc::new();
        let mut txn = doc.transact_mut();
        cursor(&mut txn, 0).insert_text(&mut txn, "abcd", None);
        cursor(&mut txn, 2).insert_text(&mut txn, "XY", None);
        assert_eq!(txn.node("test").unwrap().to_string(), "abXYcd");
    }

    #[test]
    fn remove_inside_block() {
        let mut doc = Doc::new();
        let mut txn = doc.transact_mut();
        cursor(&mut txn, 0).insert_text(&mut txn, "abcdef", None);
        let removed = cursor(&mut txn, 1).remove(&mut txn, 3);
        assert_eq!(removed, 3);
        assert_eq!(txn.node("test").unwrap().to_string(), "aef");
    }

    #[test]
    fn format_range() {
        let mut doc = Doc::new();
        let mut txn = doc.transact_mut();
        cursor(&mut txn, 0).insert_text(&mut txn, "abcd", None);
        cursor(&mut txn, 1).format(&mut txn, 2, &bold());
        let actual = txn.node("test").unwrap().delta(&DeltaOptions::default());
        let expected = Delta::<Out>::default()
            .insert_text("a")
            .insert_text_fmt("bc", &bold())
            .insert_text("d");
        assert_eq!(actual, expected);
    }

    #[test]
    fn insert_inherits_active_attrs() {
        let mut doc = Doc::new();
        let mut txn = doc.transact_mut();
        cursor(&mut txn, 0).insert_text(&mut txn, "ab", Some(&bold()));
        let mut c = cursor(&mut txn, 1);
        assert_eq!(c.attrs(), Some(&bold()));
        c.insert_text(&mut txn, "X", Some(&bold()));
        let actual = txn.node("test").unwrap().delta(&DeltaOptions::default());
        assert_eq!(
            actual,
            Delta::<Out>::default().insert_text_fmt("aXb", &bold())
        );
    }

    #[test]
    fn remove_cleans_format_gap() {
        let mut doc = Doc::new();
        let mut txn = doc.transact_mut();
        cursor(&mut txn, 0).insert_text(&mut txn, "abc", None);
        cursor(&mut txn, 1).format(&mut txn, 1, &bold());
        cursor(&mut txn, 1).remove(&mut txn, 1);
        let actual = txn.node("test").unwrap().delta(&DeltaOptions::default());
        assert_eq!(actual, Delta::<Out>::default().insert_text("ac"));
    }

    #[test]
    fn iterate_values() {
        let mut doc = Doc::new();
        let mut txn = doc.transact_mut();
        let values = [In::from(1), In::from(2), In::from(3)];
        cursor(&mut txn, 0).insert_range(&mut txn, values, None);
        let actual: Vec<Out> = cursor(&mut txn, 1).collect();
        assert_eq!(actual, vec![Out::from(2), Out::from(3)]);
    }

    #[test]
    fn iterate_multibyte_text() {
        let mut doc = Doc::new();
        let mut txn = doc.transact_mut();
        cursor(&mut txn, 0).insert_text(&mut txn, "a😀b", None);
        let actual: Vec<Out> = cursor(&mut txn, 1).collect();
        assert_eq!(actual, vec![Out::from("😀"), Out::from("b")]);
    }

    /// UTF-16 index 2 falls between halves of a surrogate pair.
    #[test]
    fn iterate_from_inside_surrogate_pair() {
        let mut doc = Doc::with_options(Options {
            offset_kind: OffsetKind::Utf16,
            ..Options::default()
        });
        let mut txn = doc.transact_mut();
        cursor(&mut txn, 0).insert_text(&mut txn, "a😀b", None);
        let mut c = cursor(&mut txn, 2);
        assert_eq!(c.index(), 3);
        assert_eq!(c.next(), Some(Out::from("b")));
        assert_eq!(c.next(), None);
    }

    /// Byte index 2 falls inside of a 4-byte character.
    #[test]
    fn insert_inside_multibyte_char() {
        let mut doc = Doc::with_options(Options {
            offset_kind: OffsetKind::Bytes,
            ..Options::default()
        });
        let mut txn = doc.transact_mut();
        cursor(&mut txn, 0).insert_text(&mut txn, "a😀b", None);
        cursor(&mut txn, 2).insert_text(&mut txn, "X", None);
        assert_eq!(txn.node("test").unwrap().to_string(), "a😀Xb");
    }
}
