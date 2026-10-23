use crate::block::{BlockRange, ID, Item, ItemContent, ItemPtr};
use crate::delta::Attribution;
use crate::formatter::{
    AttributedFragment, AttributedFragmentKind, DefaultFormatter, DeltaFormatter, Last, RenderMode,
    clock_len, formatted_len, native_len,
};
use crate::node::{Attrs, Node, NodePtr, TypePtr};
use crate::node_ref::FormatAttribution;
use crate::transaction::TransactionState;
use crate::utils::OptionExt;
use crate::{Any, Doc, IdSet, In, OffsetKind, Out};
use std::collections::HashMap;
use std::sync::Arc;

/// A position within the sequence component of a [Node], used to iterate over node's children and
/// keep track of active formatting attributes at its current position.
///
/// It can also be used to introduce modifications (inserting, removal and formatting) at
/// its position. Visibility and lengths of items are decided by a formatter `F`.
pub(crate) struct Cursor<'f, F: DeltaFormatter = DefaultFormatter> {
    parent: NodePtr,
    left: Option<ItemPtr>,
    right: Option<ItemPtr>,
    /// Offset inside of `right`.
    offset: u32,
    /// Number of visible elements before the cursor.
    index: u32,
    /// Formatting attributes active at the cursor position.
    attrs: Option<Box<Attrs>>,
    /// Format attribution at the cursor position, set once an attributed marker was passed.
    fmt_attribution: Option<Box<FormatAttribution>>,
    encoding: OffsetKind,
    fmt: &'f F,
}

impl<'f, F: DeltaFormatter> Cursor<'f, F> {
    /// Creates a cursor placed at the beginning of a `parent` node.
    pub fn new(parent: NodePtr, encoding: OffsetKind, fmt: &'f F) -> Self {
        Cursor {
            parent,
            left: None,
            right: parent.start,
            offset: 0,
            index: 0,
            attrs: None,
            fmt_attribution: None,
            encoding,
            fmt,
        }
    }

    #[inline]
    fn block_len(&self, item: &Item) -> u32 {
        formatted_len(self.fmt, item, self.encoding)
    }

    /// Visible fragments of an item claimed by the formatter.
    fn visible_fragments(&self, item: &Item) -> Vec<AttributedFragment<'f, F::Attr>> {
        let mut fragments = Vec::new();
        let fmt: &'f F = self.fmt;
        fmt.read_content(&mut fragments, item, item.range(), RenderMode::Retain);
        fragments.retain(|p| !p.deleted || p.attrs.is_some());
        fragments
    }

    /// Attribution of a format marker `item`, as a render resolves it.
    fn marker_attribution(&self, item: &Item) -> Option<Attribution> {
        if !self.fmt.contains(item) {
            return None;
        }
        let mut last = Last::default();
        let fmt: &'f F = self.fmt;
        fmt.read_content(&mut last, item, item.range(), RenderMode::Visible);
        let p = last.0?;
        Attribution::resolve(p.attrs.as_ref(), p.deleted)
    }

    /// Maps an `offset` inside of an `item` expressed in `lib0` units onto clock units.
    fn clock_offset(&self, item: ItemPtr, offset: u32) -> u32 {
        if offset == 0 || !self.fmt.contains(&item) {
            return clock_offset(item, offset, self.encoding);
        }
        let mut remaining = offset;
        for p in self.visible_fragments(&item) {
            let len = native_len(
                &item.content,
                p.range.start - item.id.clock,
                p.len(),
                self.encoding,
            );
            if remaining <= len {
                let clock = p.range.start
                    + clock_len(
                        &item.content,
                        p.range.start - item.id.clock,
                        remaining,
                        self.encoding,
                    );
                return clock - item.id.clock;
            }
            remaining -= len;
        }
        item.len()
    }

    /// Splits an `item` at a given clock, returning the right half.
    fn split_at(doc: &mut Doc, item: ItemPtr, clock: u32) -> Option<ItemPtr> {
        doc.blocks
            .split_block(item, clock - item.id.clock, OffsetKind::Utf16)
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
    pub fn at(parent: NodePtr, encoding: OffsetKind, fmt: &'f F, index: u32) -> Option<Self> {
        let mut cursor = Self::new(parent, encoding, fmt);
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
                let deleted = right.is_deleted();
                let attribution = self.marker_attribution(&right);
                let attrs = self.attrs.get_or_init();
                if attribution.is_some() || self.fmt_attribution.is_some() {
                    // same as in a render (see: DeltaWalk::process_format)
                    let a = self.fmt_attribution.get_or_init();
                    a.before(key, value, attribution.as_ref(), deleted, attrs);
                    if !deleted {
                        update_attrs(attrs, key, value);
                    }
                    a.after(key, attribution.as_ref(), deleted, attrs, false);
                } else if !deleted {
                    update_attrs(attrs, key, value);
                }
            }
            _ => {
                let len = self.block_len(&right);
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
            && self.block_len(&right) == 0
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
            let item_len = self.block_len(&right);
            let available = item_len - self.offset;
            if remaining < available {
                let offset = if self.fmt.contains(&right) {
                    self.offset + remaining
                } else {
                    ceil_offset(right, self.offset + remaining, self.encoding)
                };
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
        id.clock += self.clock_offset(item, self.offset);
        Some(id)
    }

    /// Splits the `right` item if the cursor is placed inside of it, so that the cursor
    /// falls on a block boundary.
    pub fn split(&mut self, doc: &mut Doc) {
        if self.offset > 0
            && let Some(right) = self.right
        {
            let clock = right.id.clock + self.clock_offset(right, self.offset);
            self.right = Self::split_at(doc, right, clock);
            self.left = Some(right);
            self.offset = 0;
        }
    }

    /// Inserts a string of `text` at the cursor position, moving the cursor after it.
    pub fn insert_text(
        &mut self,
        doc: &mut Doc,
        state: &mut TransactionState,
        text: &str,
        attrs: Option<&Attrs>,
    ) -> Option<ItemPtr> {
        if text.is_empty() {
            return None;
        }
        self.insert_content(doc, state, ItemContent::String(text.into()), attrs, None)
    }

    /// Inserts a single `value` at the cursor position, moving the cursor after it.
    pub fn insert(
        &mut self,
        doc: &mut Doc,
        state: &mut TransactionState,
        value: In,
        attrs: Option<&Attrs>,
    ) -> Option<ItemPtr> {
        match value {
            In::Any(any) => {
                self.insert_content(doc, state, ItemContent::Any(vec![any]), attrs, None)
            }
            In::Node(delta) => {
                let (node, remainder) = Node::from_delta(*delta);
                self.insert_content(doc, state, ItemContent::Node(node), attrs, remainder)
            }
            In::Doc(subdoc) => {
                let content = ItemContent::Doc(None, subdoc.options.clone());
                self.insert_content(doc, state, content, attrs, None)
            }
        }
    }

    /// Inserts `values` at the cursor position, moving the cursor after them.
    /// Consecutive [In::Any] values are packed into a single block.
    pub fn insert_range<I>(
        &mut self,
        doc: &mut Doc,
        state: &mut TransactionState,
        values: I,
        attrs: Option<&Attrs>,
    ) where
        I: IntoIterator<Item = In>,
    {
        let mut pending = Vec::new();
        for value in values {
            match value {
                In::Any(any) => pending.push(any),
                other => {
                    if !pending.is_empty() {
                        let content = ItemContent::Any(std::mem::take(&mut pending));
                        self.insert_content(doc, state, content, attrs, None);
                    }
                    self.insert(doc, state, other, attrs);
                }
            }
        }
        if !pending.is_empty() {
            self.insert_content(doc, state, ItemContent::Any(pending), attrs, None);
        }
    }

    /// Port of Yjs `insertContent`. `children` are applied to a node `content` once it's integrated.
    fn insert_content(
        &mut self,
        doc: &mut Doc,
        state: &mut TransactionState,
        content: ItemContent,
        attrs: Option<&Attrs>,
        children: Option<crate::Delta<In>>,
    ) -> Option<ItemPtr> {
        self.split(doc);
        let mut attrs = attrs.cloned().unwrap_or_default();
        if let Some(current) = self.attrs.as_deref() {
            // attributes active at the cursor position, but not requested, must be unset
            for key in current.keys() {
                attrs.entry(key.clone()).or_insert(Any::Null);
            }
        }
        self.minimize_format_changes(&attrs);
        let negated = self.insert_formats(doc, state, &attrs);
        let item = self.integrate(doc, state, content)?;
        if let Some(children) = children
            && let Some(node) = item.content.as_node()
        {
            crate::node_ref::apply_delta(
                doc,
                state,
                node,
                children,
                &DefaultFormatter,
                &mut crate::Delta::new(),
            );
        }
        self.right = Some(item);
        self.next_item();
        self.insert_negated_formats(doc, state, negated);
        Some(item)
    }

    /// Port of Yjs `formatText`: applies `attrs` to `len` elements following the cursor, moving
    /// the cursor after them. Returns the number of elements formatted.
    pub fn format(
        &mut self,
        doc: &mut Doc,
        state: &mut TransactionState,
        mut len: u32,
        attrs: &Attrs,
    ) -> u32 {
        let start_len = len;
        self.split(doc);
        self.minimize_format_changes(attrs);
        let mut negated = self.insert_formats(doc, state, attrs);
        // iterate until first non-format or null is found
        // delete all formats with attrs[format.key] != null
        // also check the attributes after the first non-format as we do not want to insert redundant
        // negated attributes there
        while let Some(right) = self.right {
            let is_format = matches!(right.content, ItemContent::Format(_, _));
            let hidden = right.is_deleted() && self.block_len(&right) == 0;
            if !(len > 0 || (!negated.is_empty() && (hidden || is_format))) {
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
                            state.delete(doc, right);
                        } else {
                            update_attrs(self.attrs.get_or_init(), key, value);
                        }
                    }
                }
                _ => {
                    let right_len = self.block_len(&right);
                    if len < right_len {
                        if self.fmt.contains(&right) {
                            self.split_pieces(doc, right, len);
                        } else {
                            split_item(doc, right, len, self.encoding);
                        }
                        len = 0;
                    } else {
                        len -= right_len;
                    }
                }
            }
            self.next_item();
        }
        self.insert_negated_formats(doc, state, negated);
        start_len - len
    }

    /// Removes `len` elements following the cursor.
    /// Returns the number of elements removed.
    pub fn remove(&mut self, doc: &mut Doc, state: &mut TransactionState, len: u32) -> u32 {
        self.split(doc);
        let start = self.right;
        let start_attrs = self.attrs.clone();
        let mut remaining = len;
        while remaining > 0
            && let Some(item) = self.right
        {
            if !item.is_deleted() && item.is_countable() {
                if remaining < item.content_len(self.encoding) {
                    split_item(doc, item, remaining, self.encoding);
                }
                remaining = remaining.saturating_sub(item.content_len(self.encoding));
                state.delete(doc, item);
            } else if self.fmt.contains(&item) {
                remaining = self.remove_attributed(doc, state, item, remaining);
            }
            self.next_item();
        }
        if start.is_some() {
            self.clean_format_gap(doc, state, start, start_attrs.as_deref());
        }
        len - remaining
    }

    /// Splits a claimed `item` after `len` visible elements (Yjs `formatText` renderer branch).
    fn split_pieces(&self, doc: &mut Doc, item: ItemPtr, len: u32) {
        let mut pieces = Vec::new();
        let fmt: &'f F = self.fmt;
        fmt.read_content(&mut pieces, &item, item.range(), RenderMode::Retain);
        let mut remaining = len as i64;
        let mut i = 0;
        while i < pieces.len() && remaining > 0 {
            let p = &pieces[i];
            if (!p.deleted || p.attrs.is_some()) && item.is_countable() {
                remaining -= native_len(
                    &item.content,
                    p.range.start - item.id.clock,
                    p.len(),
                    self.encoding,
                ) as i64;
            }
            i += 1;
        }
        if remaining < 0 || (remaining == 0 && i != pieces.len()) {
            let p = &pieces[i - 1];
            let consumed = (native_len(
                &item.content,
                p.range.start - item.id.clock,
                p.len(),
                self.encoding,
            ) as i64
                + remaining) as u32;
            let clock = clock_len(
                &item.content,
                p.range.start - item.id.clock,
                consumed,
                self.encoding,
            );
            Self::split_at(doc, item, p.range.start + clock);
        }
    }

    /// Removes up to `len` elements of an already deleted, but still rendered `item`: nothing is
    /// deleted, but the range is recorded in `attributed_deletes` (Yjs `deleteText` renderer
    /// branch). Returns the number of elements left to remove.
    fn remove_attributed(
        &self,
        doc: &mut Doc,
        state: &mut TransactionState,
        item: ItemPtr,
        mut len: u32,
    ) -> u32 {
        let mut pieces = Vec::new();
        let fmt: &'f F = self.fmt;
        fmt.read_content(&mut pieces, &item, item.range(), RenderMode::Retain);
        let mut split_clock = None;
        if item.is_countable() {
            for p in pieces.iter().filter(|p| p.attrs.is_some()) {
                if len == 0 {
                    // split, so that the cursor advances only past the consumed part
                    split_clock = Some(p.range.start);
                    break;
                }
                let plen = native_len(
                    &item.content,
                    p.range.start - item.id.clock,
                    p.len(),
                    self.encoding,
                );
                let n = plen.min(len);
                let clock_len = clock_len(
                    &item.content,
                    p.range.start - item.id.clock,
                    n,
                    self.encoding,
                );
                let deletes = state.attributed_deletes.get_or_insert_with(IdSet::new);
                deletes.insert(ID::new(item.id.client, p.range.start), clock_len);
                len -= n;
                if n < plen {
                    split_clock = Some(p.range.start + clock_len);
                    break;
                }
            }
        }
        let end = item.id.clock + item.len();
        let split_clock = split_clock.or_else(|| match pieces.last() {
            Some(p) if p.range.end < end => Some(p.range.end),
            _ => None,
        });
        if let Some(clock) = split_clock {
            Self::split_at(doc, item, clock);
        }
        len
    }

    /// Integrates a new item with a given `content` between `left` and `right` neighbors.
    fn integrate(
        &self,
        doc: &mut Doc,
        state: &mut TransactionState,
        content: ItemContent,
    ) -> Option<ItemPtr> {
        let id = ID::new(doc.options.client_id, doc.get_local_state());
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
        state.integrate_item(doc, item, 0)
    }

    /// Moves the cursor over deleted items and formatting markers which already match `attrs`.
    fn minimize_format_changes(&mut self, attrs: &Attrs) {
        while let Some(right) = self.right {
            let skip = if right.is_deleted() {
                self.block_len(&right) == 0
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
    fn insert_formats(
        &mut self,
        doc: &mut Doc,
        state: &mut TransactionState,
        attrs: &Attrs,
    ) -> Attrs {
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
                self.right = self.integrate(doc, state, content);
                self.next_item();
            }
        }
        negated
    }

    /// Inserts formatting markers restoring `negated` attributes, unless they're already
    /// restored by the markers following the cursor.
    fn insert_negated_formats(
        &mut self,
        doc: &mut Doc,
        state: &mut TransactionState,
        mut negated: Attrs,
    ) {
        while let Some(right) = self.right {
            let skip = if right.is_deleted() {
                self.block_len(&right) == 0
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
            self.right = self.integrate(doc, state, content);
            self.next_item();
        }
    }

    /// Removes formatting markers made redundant by removal of the content starting at `start`.
    fn clean_format_gap(
        &mut self,
        doc: &mut Doc,
        state: &mut TransactionState,
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
                    state.delete(doc, item);
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

    /// Moves the cursor over `len` visible elements, calling `f` over each unique content chunk
    /// moved over. Returns the number of elements moved over.
    pub fn read_chunks(&mut self, len: u32, mut f: impl FnMut(Chunk<'_>)) -> u32 {
        let mut remaining = len;
        while remaining > 0 {
            self.skip_hidden();
            let Some(item) = self.right else {
                break;
            };
            let n = (self.block_len(&item) - self.offset).min(remaining);
            if n == 0 {
                break;
            }
            let format = self.attrs.as_deref();
            let current_attribution = self.fmt_attribution.as_ref().and_then(|a| a.used.as_ref());
            if !self.fmt.contains(&item) {
                let start = clock_offset(item, self.offset, self.encoding);
                let end = clock_offset(item, self.offset + n, self.encoding);
                f(Chunk {
                    content: &item.content,
                    offset: start,
                    clock_len: end - start,
                    len: n,
                    attribution: Attribution::combine_data(current_attribution, None),
                    format,
                });
            } else {
                let (mut skip, mut take) = (self.offset, n);
                for p in self.visible_fragments(&item) {
                    let (content, base) = match p.content {
                        AttributedFragmentKind::Own => {
                            (&item.content, p.range.start - item.id.clock)
                        }
                        AttributedFragmentKind::Restored { content, offset } => (content, offset),
                        AttributedFragmentKind::Opaque => continue, // only its length is known
                    };
                    if take == 0 {
                        break;
                    } else if !content.is_countable() {
                        continue;
                    }
                    let plen = native_len(content, base, p.len(), self.encoding);
                    if skip >= plen {
                        skip -= plen;
                        continue;
                    }
                    let k = (plen - skip).min(take);
                    let start = clock_len(content, base, skip, self.encoding);
                    let clock_len = clock_len(content, base + start, k, self.encoding);
                    f(Chunk {
                        content,
                        offset: base + start,
                        clock_len,
                        len: k,
                        attribution: Attribution::combine_data(
                            current_attribution,
                            Attribution::resolve(p.attrs.as_ref(), p.deleted).as_ref(),
                        ),
                        format,
                    });
                    (skip, take) = (0, take - k);
                }
            }
            remaining = remaining.saturating_sub(self.forward(n));
        }
        len - remaining
    }
}

/// A uniform run of visible content (see: [Cursor::read_chunks]).
pub(crate) struct Chunk<'a> {
    /// Item's own content, or content restored by the formatter.
    pub content: &'a ItemContent,
    /// Clock offset of the run within `content`.
    pub offset: u32,
    pub clock_len: u32,
    /// Length of the run in the cursor's units.
    pub len: u32,
    pub attribution: Option<Attribution>,
    /// Formats active over the run.
    pub format: Option<&'a Attrs>,
}

impl Cursor<'static, DefaultFormatter> {
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
}

impl Iterator for Cursor<'static, DefaultFormatter> {
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
fn split_item(doc: &mut Doc, item: ItemPtr, offset: u32, encoding: OffsetKind) -> Option<ItemPtr> {
    let offset = clock_offset(item, offset, encoding);
    doc.blocks.split_block(item, offset, OffsetKind::Utf16)
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
    use crate::formatter::DefaultFormatter;
    use crate::node::{Attrs, TypeRef};
    use crate::{Any, Delta, DeltaOptions, Doc, In, OffsetKind, Options, Out};

    fn cursor(doc: &mut Doc, index: u32) -> Cursor<'static> {
        let node = doc.get_or_create_type("test", TypeRef::Undefined);
        Cursor::at(node, doc.options.offset_kind, &DefaultFormatter, index).unwrap()
    }

    fn bold() -> Attrs {
        Attrs::from([("b".into(), Any::Bool(true))])
    }

    #[test]
    fn insert_inside_block() {
        let mut doc = Doc::new();
        let mut txn = doc.transact_mut();
        let (doc, state) = txn.split_mut();
        cursor(doc, 0).insert_text(doc, state, "abcd", None);
        cursor(doc, 2).insert_text(doc, state, "XY", None);
        assert_eq!(txn.node("test").unwrap().to_string(), "abXYcd");
    }

    #[test]
    fn remove_inside_block() {
        let mut doc = Doc::new();
        let mut txn = doc.transact_mut();
        let (doc, state) = txn.split_mut();
        cursor(doc, 0).insert_text(doc, state, "abcdef", None);
        let removed = cursor(doc, 1).remove(doc, state, 3);
        assert_eq!(removed, 3);
        assert_eq!(txn.node("test").unwrap().to_string(), "aef");
    }

    #[test]
    fn format_range() {
        let mut doc = Doc::new();
        let mut txn = doc.transact_mut();
        let (doc, state) = txn.split_mut();
        cursor(doc, 0).insert_text(doc, state, "abcd", None);
        cursor(doc, 1).format(doc, state, 2, &bold());
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
        let (doc, state) = txn.split_mut();
        cursor(doc, 0).insert_text(doc, state, "ab", Some(&bold()));
        let mut c = cursor(doc, 1);
        assert_eq!(c.attrs(), Some(&bold()));
        c.insert_text(doc, state, "X", Some(&bold()));
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
        let (doc, state) = txn.split_mut();
        cursor(doc, 0).insert_text(doc, state, "abc", None);
        cursor(doc, 1).format(doc, state, 1, &bold());
        cursor(doc, 1).remove(doc, state, 1);
        let actual = txn.node("test").unwrap().delta(&DeltaOptions::default());
        assert_eq!(actual, Delta::<Out>::default().insert_text("ac"));
    }

    #[test]
    fn iterate_values() {
        let mut doc = Doc::new();
        let mut txn = doc.transact_mut();
        let (doc, state) = txn.split_mut();
        let values = [In::from(1), In::from(2), In::from(3)];
        cursor(doc, 0).insert_range(doc, state, values, None);
        let actual: Vec<Out> = cursor(doc, 1).collect();
        assert_eq!(actual, vec![Out::from(2), Out::from(3)]);
    }

    #[test]
    fn iterate_multibyte_text() {
        let mut doc = Doc::new();
        let mut txn = doc.transact_mut();
        let (doc, state) = txn.split_mut();
        cursor(doc, 0).insert_text(doc, state, "a😀b", None);
        let actual: Vec<Out> = cursor(doc, 1).collect();
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
        let (doc, state) = txn.split_mut();
        cursor(doc, 0).insert_text(doc, state, "a😀b", None);
        let mut c = cursor(doc, 2);
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
        let (doc, state) = txn.split_mut();
        cursor(doc, 0).insert_text(doc, state, "a😀b", None);
        cursor(doc, 2).insert_text(doc, state, "X", None);
        assert_eq!(txn.node("test").unwrap().to_string(), "a😀Xb");
    }
}
