use crate::block::{BlockRange, Item, ItemContent, split_str};
use crate::delta::Attribution;
use crate::id_map::{ContentAttribute, ContentAttributes, Diff, IdMap};
use crate::meta::{ContentIds, ContentMap};
use crate::slice::BlockSlice;
use crate::{ClientID, DocHandle, ID, IdSet, OffsetKind, Snapshot, Transaction};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::hash::Hash;
use std::ops::Range;
use std::sync::Arc;

/// What the delta builder needs from an attribute value: who, and when.
pub trait AttributeValue: Clone + Eq + Hash {
    /// User id for `insert`/`delete` attributes.
    fn attributor(&self) -> Option<Arc<str>>;
    /// Timestamp for `insertAt`/`deleteAt` attributes. `0` means not set.
    fn timestamp(&self) -> u64;
}

/// Provided [AttributeValue] implementation, typed as Yjs declares it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(untagged)]
pub enum AttrValue {
    Attributor(Arc<str>),
    /// Never `0`.
    Timestamp(u64),
}

impl<'de> Deserialize<'de> for AttrValue {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = AttrValue;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                write!(f, "attributor string or timestamp number")
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<AttrValue, E> {
                Ok(AttrValue::Attributor(v.into()))
            }

            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<AttrValue, E> {
                Ok(AttrValue::Timestamp(v))
            }

            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<AttrValue, E> {
                Ok(AttrValue::Timestamp(v as u64))
            }

            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<AttrValue, E> {
                Ok(AttrValue::Timestamp(v as u64))
            }
        }
        deserializer.deserialize_any(V)
    }
}

impl AttributeValue for AttrValue {
    fn attributor(&self) -> Option<Arc<str>> {
        match self {
            AttrValue::Attributor(by) => Some(by.clone()),
            AttrValue::Timestamp(_) => None,
        }
    }

    fn timestamp(&self) -> u64 {
        match self {
            AttrValue::Attributor(_) => 0,
            AttrValue::Timestamp(ts) => *ts,
        }
    }
}

/// No attribute values: for formatters that only mark fragments as attributed.
impl AttributeValue for () {
    #[inline]
    fn attributor(&self) -> Option<Arc<str>> {
        None
    }

    #[inline]
    fn timestamp(&self) -> u64 {
        0
    }
}

impl ContentAttribute<AttrValue> {
    pub fn insert(by: impl Into<Arc<str>>) -> Self {
        ContentAttribute::new("insert", AttrValue::Attributor(by.into()))
    }

    pub fn insert_at(ts: u64) -> Self {
        ContentAttribute::new("insertAt", AttrValue::Timestamp(ts))
    }

    pub fn delete(by: impl Into<Arc<str>>) -> Self {
        ContentAttribute::new("delete", AttrValue::Attributor(by.into()))
    }

    pub fn delete_at(ts: u64) -> Self {
        ContentAttribute::new("deleteAt", AttrValue::Timestamp(ts))
    }
}

/// How the fragments of an item are requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderMode {
    Retain = 0,
    Visible = 1,
    Insert = 2,
    Fresh = 3,
}

/// One fragment of a formatted item. Pieces never copy item content. Unlike `yjs` we cannot easily
/// reference block contents so we keep them separate.
#[derive(Debug, Clone)]
pub struct AttributedFragment<'f, A> {
    /// Clock range within the formatted item.
    pub range: Range<u32>,
    /// Effective deleted flag (a restored deletion renders as alive).
    pub deleted: bool,
    /// `None` = unattributed.
    pub attrs: Option<ContentAttributes<A>>,
    /// Derived from the render mode, `deleted` and `attrs`.
    pub render: bool,
    /// Fresh content that was deleted in the same transaction ([RenderMode::Fresh]): even in a
    /// [DeltaOptions::retain_deletes] render it must produce an *insert* — the consuming state
    /// (e.g. the maintained `delta` cache) has never seen this content,
    /// so there is nothing to retain.
    pub fresh: bool,
    pub content: AttributedFragmentKind<'f>,
}

impl<'f, A> AttributedFragment<'f, A> {
    /// A piece of the item's own content.
    pub fn new(
        range: Range<u32>,
        deleted: bool,
        attrs: Option<ContentAttributes<A>>,
        mode: RenderMode,
    ) -> Self {
        let render = match mode {
            RenderMode::Retain => false,
            RenderMode::Visible => !deleted || attrs.is_some(),
            RenderMode::Insert | RenderMode::Fresh => true,
        };
        AttributedFragment {
            range,
            deleted,
            attrs,
            render,
            fresh: mode == RenderMode::Fresh,
            content: AttributedFragmentKind::Own,
        }
    }

    #[inline]
    pub fn len(&self) -> u32 {
        self.range.end - self.range.start
    }
}

/// Where the content of a [AttributedFragment] comes from.
#[derive(Debug, Clone, Copy)]
pub enum AttributedFragmentKind<'f> {
    /// Read from the block itself, at `range.start - item.id.clock`.
    Own,
    /// Read from content the formatter holds.
    Restored {
        content: &'f ItemContent,
        offset: u32,
    },
    /// Only its length is known. Renders only as a delete op.
    Opaque,
}

/// Decides how the content of a node is rendered.
pub trait DeltaFormatter {
    /// Attribute value type carried by fragments.
    type Attr: AttributeValue;
    /// Whether any part of `item` is formatted non-generically; `false` takes the fast path.
    fn contains(&self, item: &Item) -> bool;
    /// Splits a `range` of `item` into fragments, each with its own deleted flag, attributes and
    /// render decision.
    fn read_content<'f>(
        &'f self,
        out: &mut Vec<AttributedFragment<'f, Self::Attr>>,
        item: &Item,
        range: BlockRange,
        mode: RenderMode,
    );
    /// Formatted length of a claimed item, measured in `kind` units of the formatted doc.
    fn content_len(&self, item: &Item, kind: OffsetKind) -> u32;

    fn attributed(&self) -> &IdSet;
}

/// The generic path: claims nothing.
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultFormatter;

impl DeltaFormatter for DefaultFormatter {
    type Attr = ();

    #[inline(always)]
    fn contains(&self, _item: &Item) -> bool {
        false
    }

    #[inline]
    fn read_content<'f>(
        &'f self,
        _out: &mut Vec<AttributedFragment<'f, ()>>,
        _item: &Item,
        _range: BlockRange,
        _mode: RenderMode,
    ) {
    }

    #[inline(always)]
    fn content_len(&self, item: &Item, kind: OffsetKind) -> u32 {
        item.visible_len(kind)
    }

    #[inline]
    fn attributed(&self) -> &IdSet {
        &EMPTY_ID_SET
    }
}

static EMPTY_ID_SET: IdSet = IdSet::new();

/// Formatted length of an `item` in `kind` units of its doc (a port of Yjs
/// `rendererContentLength`).
#[inline]
pub(crate) fn formatted_len<F: DeltaFormatter>(fmt: &F, item: &Item, kind: OffsetKind) -> u32 {
    if fmt.contains(item) {
        fmt.content_len(item, kind)
    } else {
        item.visible_len(kind)
    }
}

/// Calculates native length (as defined by `kind`) from UTF-16 (yjs-compatible) clock length.
pub(crate) fn native_len(content: &ItemContent, offset: u32, len: u32, kind: OffsetKind) -> u32 {
    match (content, kind) {
        (ItemContent::String(s), OffsetKind::Bytes) => {
            let (_, rest) = split_str(s.as_str(), offset as usize, OffsetKind::Utf16);
            let (slice, _) = split_str(rest, len as usize, OffsetKind::Utf16);
            slice.len() as u32
        }
        _ => len,
    }
}

/// Calculates clock length of a given range (length compatible with Yjs UTF-16 string length).
pub(crate) fn clock_len(item: &Item, clock: u32, len: u32, kind: OffsetKind) -> u32 {
    match (&item.content, kind) {
        (ItemContent::String(s), OffsetKind::Bytes) => {
            let offset = (clock - item.id.clock) as usize;
            let (_, rest) = split_str(s.as_str(), offset, OffsetKind::Utf16);
            let (slice, _) = split_str(rest, len as usize, OffsetKind::Bytes);
            slice.encode_utf16().count() as u32
        }
        _ => len,
    }
}

/// Total length of `ranges` covered by the `item`, with elements counted via `kind`.
fn covered_len(
    item: &Item,
    ranges: impl IntoIterator<Item = (Range<u32>, bool)>,
    kind: OffsetKind,
) -> u32 {
    let mut len = 0;
    for (range, exists) in ranges {
        if exists {
            len += native_len(
                &item.content,
                range.start - item.id.clock,
                range.end - range.start,
                kind,
            );
        }
    }
    len
}

/// Renders content with attributions - a port of Yjs `AttributionsRenderer`.
///
/// Content covered by the attributions always renders, carrying its attribution. Content in
/// `rendered_content` renders normally, even if deleted (restoring deleted content requires
/// `skip_gc`). By default, it's the alive content of the doc.
#[derive(Debug, Clone)]
pub struct AttributionsFormatter<A: AttributeValue = AttrValue> {
    /// Inserts and deletes merged.
    render_as: IdMap<A>,
    /// Coverage of `render_as`. The small set `contains` checks. May over-approximate what
    /// actually renders — `read_content` remains authoritative.
    attributed: IdSet,
    /// Custom "content that renders normally" set, or `None` to use the implicit default.
    rendered_content: Option<IdSet>,
    /// `rendered_content ∪ attributed`, only with a custom set — the single cheap
    /// "does this produce visible output?" set.
    /// Only materialized when a custom `rendered_content` is supplied.
    rendered: Option<IdSet>,
}

impl<A: AttributeValue> AttributionsFormatter<A> {
    pub fn new(attributions: &ContentMap<A>) -> Self {
        let mut render_as = attributions.inserts.clone();
        render_as.merge_with(attributions.deletes.clone());
        let attributed = render_as.as_id_set();
        AttributionsFormatter {
            render_as,
            attributed,
            rendered_content: None,
            rendered: None,
        }
    }

    /// Sets the content which renders normally, replacing the doc's alive content.
    pub fn with_rendered_content(mut self, rendered_content: IdSet) -> Self {
        self.rendered = Some(rendered_content.merge(&self.attributed));
        self.rendered_content = Some(rendered_content);
        self
    }

    fn push<'f>(
        out: &mut Vec<AttributedFragment<'f, A>>,
        range: Range<u32>,
        deleted: bool,
        in_rc: bool,
        attrs: Option<&ContentAttributes<A>>,
        mode: RenderMode,
    ) {
        if in_rc || attrs.is_some() {
            out.push(AttributedFragment::new(
                range,
                deleted && !in_rc,
                attrs.cloned(),
                mode,
            ));
        } else if deleted && mode != RenderMode::Retain && mode != RenderMode::Fresh {
            out.push(AttributedFragment::new(range, true, None, mode));
        }
    }
}

impl<A: AttributeValue> DeltaFormatter for AttributionsFormatter<A> {
    type Attr = A;

    fn contains(&self, item: &Item) -> bool {
        let range = BlockRange::new(item.id, item.len());
        if self.attributed.intersects(&range) {
            return true;
        }
        match &self.rendered_content {
            None => false,
            Some(rc) if item.is_deleted() => rc.intersects(&range),
            Some(rc) => !rc.contains_range(&range),
        }
    }

    fn read_content<'f>(
        &'f self,
        out: &mut Vec<AttributedFragment<'f, A>>,
        item: &Item,
        range: BlockRange,
        mode: RenderMode,
    ) {
        let deleted = item.is_deleted();
        match &self.rendered_content {
            // alive content renders normally
            None => {
                for (range, attrs) in self.render_as.slice(&range) {
                    Self::push(out, range, deleted, !deleted, attrs, mode);
                }
            }
            // partition by the rendered content first, then by attributions
            Some(rc) => {
                for (rc_range, in_rc) in rc.slice(&range) {
                    let rc_range = range.subrange(rc_range);
                    for (range, attrs) in self.render_as.slice(&rc_range) {
                        Self::push(out, range, deleted, in_rc, attrs, mode);
                    }
                }
            }
        }
    }

    fn content_len(&self, item: &Item, kind: OffsetKind) -> u32 {
        if !item.is_countable() {
            return 0;
        }
        let range = BlockRange::new(item.id, item.len());
        match &self.rendered {
            None if !item.is_deleted() => item.content_len(kind),
            None => {
                let slice = self.render_as.slice(&range);
                covered_len(item, slice.map(|(r, a)| (r, a.is_some())), kind)
            }
            Some(rendered) => covered_len(item, rendered.slice(&range), kind),
        }
    }

    #[inline]
    fn attributed(&self) -> &IdSet {
        &self.attributed
    }
}

/// Renders the content of a doc as it was at a snapshot, or the changes between two snapshots.
/// Requires GC to be turned off [crate::Options::skip_gc].
///
/// Content inserted or deleted between the snapshots is attributed (with no attribute values).
#[derive(Debug, Clone)]
pub struct SnapshotFormatter {
    prev: Snapshot,
    next: Snapshot,
    /// Unlike yjs (which uses IdMap to encode 3 different states), this set only contains a subset
    /// of `attributed` that contains "change" marker.
    changed: IdSet,
    /// Coverage of `attrs` (everything up to `next`). Content *after* the snapshot must be
    /// hidden rather than rendered normally, which a finite set cannot express:
    /// [SnapshotFormatter::contains] additionally claims all future content.
    attributed: IdSet,
}

impl SnapshotFormatter {
    pub fn new(prev: Snapshot, next: Snapshot) -> Self {
        let mut chnged = next.delete_set.diff(&prev.delete_set);
        let mut unchanged = IdSet::new();
        for (&client, &clock) in next.state_map.iter() {
            let prev_clock = prev.state_map.get(&client).min(clock);
            unchanged.insert(crate::ID::new(client, 0), prev_clock);
            chnged.insert(crate::ID::new(client, prev_clock), clock - prev_clock);
        }
        chnged.diff_with(&prev.delete_set);
        unchanged.diff_with(&prev.delete_set);
        let attributed = unchanged.merge(&chnged);
        SnapshotFormatter {
            prev,
            next,
            changed: chnged,
            attributed,
        }
    }

    /// Renders the doc as it was at a given `snapshot`.
    pub fn single(snapshot: Snapshot) -> Self {
        Self::new(snapshot.clone(), snapshot)
    }

    pub fn prev(&self) -> &Snapshot {
        &self.prev
    }

    pub fn next(&self) -> &Snapshot {
        &self.next
    }

    fn next_clock(&self, client: &ClientID) -> u32 {
        self.next.state_map.get(client)
    }

    /// Pushes a frag e t of `range`. Returns `false` if the range is future content.
    fn push(
        &self,
        out: &mut Vec<AttributedFragment<'_, ()>>,
        client: ClientID,
        range: Range<u32>,
        changed: bool,
        mode: RenderMode,
    ) -> bool {
        if self.next_clock(&client) <= range.start {
            return false; // future content
        }
        let deleted = self.next.delete_set.contains(&ID::new(client, range.start));
        if mode != RenderMode::Retain || !deleted || changed {
            let attrs = changed.then(ContentAttributes::new);
            out.push(AttributedFragment::new(range, deleted, attrs, mode));
        }
        true
    }
}

impl DeltaFormatter for SnapshotFormatter {
    type Attr = ();

    fn contains(&self, item: &Item) -> bool {
        let range = item.range();
        // claim future content, so that it's hidden
        self.next_clock(&range.client) < range.clock + range.len
            || self.attributed.intersects(&range)
    }

    fn read_content<'f>(
        &'f self,
        out: &mut Vec<AttributedFragment<'f, ()>>,
        _item: &Item,
        range: BlockRange,
        mode: RenderMode,
    ) {
        let client = range.client;
        if self.next_clock(&client) <= range.clock {
            return; // future content
        }
        for (outer, exists) in self.attributed.slice(&range) {
            if !exists {
                if !self.push(out, client, outer, false, mode) {
                    return;
                }
                continue;
            }
            for (range, changed) in self.changed.slice(&range.subrange(outer)) {
                if !self.push(out, client, range, changed, mode) {
                    return;
                }
            }
        }
    }

    fn content_len(&self, item: &Item, kind: OffsetKind) -> u32 {
        if !item.is_countable() {
            0
        } else if item.is_deleted() {
            let slice = self.attributed.slice(&BlockRange::new(item.id, item.len()));
            covered_len(item, slice, kind)
        } else {
            item.content_len(kind)
        }
    }

    #[inline]
    fn attributed(&self) -> &IdSet {
        &self.attributed
    }
}

/// Renders the difference between two docs as attributed changes - the formatter part of Yjs
/// `DiffRenderer`.
///
/// Content inserted since the base doc renders as an attributed insert, content deleted since as
/// an attributed delete. Deleted content already garbage collected by the formatted doc renders
/// only as a delete op, unless restored (see: [restore_content]).
#[derive(Debug, Clone)]
pub struct DiffFormatter<A: AttributeValue = AttrValue> {
    /// Content inserted since the base doc.
    inserts: IdMap<A>,
    /// Content deleted since the base doc, minus `inserts`.
    deletes: IdMap<A>,
    /// Coverage of both maps.
    attributed: IdSet,
    attributions: Option<ContentMap<A>>,
    /// Content of deleted ranges, which the formatted doc no longer holds.
    restored: BTreeMap<ID, ItemContent>,
}

impl DiffFormatter {
    /// Renders a `diff` (ex. [ContentIds::doc_diff]) with no attributions.
    pub fn unattributed(diff: &ContentIds) -> Self {
        DiffFormatter::new(diff, None)
    }
}

impl<A: AttributeValue> DiffFormatter<A> {
    /// Renders a `diff` (ex. [ContentIds::doc_diff]) with optional `attributions`.
    pub fn new(diff: &ContentIds, attributions: Option<ContentMap<A>>) -> Self {
        // unlike Yjs: content both inserted and deleted never renders, whether gc'd or not
        let deletes = diff.deletes.diff(&diff.inserts);
        let attrs = attributions.as_ref();
        let insert_map = extract(attrs.map(|a| &a.inserts), &diff.inserts);
        let delete_map = extract(attrs.map(|a| &a.deletes), &deletes);
        let attributed = diff.inserts.merge(&deletes);
        DiffFormatter {
            inserts: insert_map,
            deletes: delete_map,
            attributed,
            attributions,
            restored: BTreeMap::new(),
        }
    }

    /// New `changes` appeared in the formatted doc. They must not contain base doc content.
    pub fn extend(&mut self, changes: &ContentIds) {
        let attrs = self.attributions.as_ref();
        let inserts = extract(attrs.map(|a| &a.inserts), &changes.inserts);
        self.inserts.merge_with(inserts);
        let mut deletes = extract(attrs.map(|a| &a.deletes), &changes.deletes);
        deletes.diff_with(&self.inserts);
        self.deletes.merge_with(deletes);
        self.attributed
            .merge_with(changes.inserts.merge(&changes.deletes));
    }

    /// `changes` became base content (accepted, or synced into the base doc). Returns the ids
    /// whose attribution changed.
    pub fn retract(&mut self, changes: &ContentIds) -> IdSet {
        self.inserts.diff_with(&changes.inserts);
        self.deletes.diff_with(&changes.deletes);
        let mut evicted = changes.inserts.merge(&changes.deletes);
        self.attributed.diff_with(&evicted);
        // an evicted range may still be covered by the other map
        for (&client, ranges) in evicted.iter() {
            for range in ranges.iter() {
                let range = BlockRange::new(ID::new(client, range.start), range.end - range.start);
                for map in [&self.inserts, &self.deletes] {
                    for (r, attrs) in map.slice(&range) {
                        if attrs.is_some() {
                            self.attributed
                                .insert(ID::new(client, r.start), r.end - r.start);
                        }
                    }
                }
            }
        }
        self.restored.retain(|id, content| {
            let range = BlockRange::new(*id, content.len(OffsetKind::Utf16));
            !changes.deletes.contains_range(&range)
        });
        evicted.diff_with(&changes.inserts.intersect(&changes.deletes));
        evicted
    }

    /// Supplies the `content` of a deleted range starting at `id`.
    pub fn restore(&mut self, id: ID, content: ItemContent) {
        if content.len(OffsetKind::Utf16) != 0 {
            self.restored.insert(id, content);
        }
    }

    /// Restores the deleted content this formatter renders from `src`, a doc which still holds it
    /// (ex. the base doc). Garbage collected content not restored renders only as a delete op.
    pub fn restore_content<D: DocHandle>(&mut self, src: &Transaction<D>) {
        let mut missing = self.deletes.as_id_set();
        for (id, content) in self.restored.iter() {
            missing.remove_range(&BlockRange::new(*id, content.len(OffsetKind::Utf16)));
        }
        for slice in missing.iter_blocks(&src.doc().blocks) {
            let BlockSlice::Item(slice) = slice else {
                continue;
            };
            let item = &*slice.ptr;
            // node content can't be copied out of its doc
            if let ItemContent::Deleted(_) | ItemContent::Node(_) = item.content {
                continue;
            }
            let content = slice_content(&item.content, slice.start, slice.end - slice.start + 1);
            self.restore(
                ID::new(item.id.client, item.id.clock + slice.start),
                content,
            );
        }
    }

    /// Splits a gc'd `range` into restored and opaque parts.
    fn gc_slices<'f>(
        &'f self,
        client: ClientID,
        mut range: Range<u32>,
        mut f: impl FnMut(Range<u32>, AttributedFragmentKind<'f>),
    ) {
        while range.start < range.end {
            let id = ID::new(client, range.start);
            let covering = self
                .restored
                .range(..=id)
                .next_back()
                .and_then(|(start, c)| {
                    let end = start.clock + c.len(OffsetKind::Utf16);
                    (start.client == client && end > id.clock).then_some((start.clock, end, c))
                });
            let (end, kind) = match covering {
                Some((start, end, content)) => {
                    let offset = range.start - start;
                    let kind = AttributedFragmentKind::Restored { content, offset };
                    (end.min(range.end), kind)
                }
                None => {
                    let next = self.restored.range(id..ID::new(client, range.end)).next();
                    let end = next.map_or(range.end, |(id, _)| id.clock);
                    (end, AttributedFragmentKind::Opaque)
                }
            };
            f(range.start..end, kind);
            range.start = end;
        }
    }

    fn push<'f>(
        &'f self,
        out: &mut Vec<AttributedFragment<'f, A>>,
        item: &Item,
        range: Range<u32>,
        attrs: Option<&ContentAttributes<A>>,
        mode: RenderMode,
    ) {
        if let ItemContent::Deleted(_) = item.content {
            self.gc_slices(item.id.client, range, |range, content| {
                let mut fragment = AttributedFragment::new(range, true, attrs.cloned(), mode);
                fragment.content = content;
                out.push(fragment);
            });
        } else {
            let fragment = AttributedFragment::new(range, item.is_deleted(), attrs.cloned(), mode);
            out.push(fragment);
        }
    }
}

/// Attributes all `ids` with matching `attrs`, or with an empty list.
fn extract<A: AttributeValue>(attrs: Option<&IdMap<A>>, ids: &IdSet) -> IdMap<A> {
    let all = IdMap::from_set(ids, Vec::new());
    match attrs {
        None => all,
        Some(attrs) => {
            let mut map = all.clone();
            map.intersect_with(attrs);
            map.merge_with(all);
            map
        }
    }
}

impl<A: AttributeValue> DeltaFormatter for DiffFormatter<A> {
    type Attr = A;

    #[inline]
    fn contains(&self, item: &Item) -> bool {
        self.attributed.intersects(&item.range())
    }

    fn read_content<'f>(
        &'f self,
        out: &mut Vec<AttributedFragment<'f, A>>,
        item: &Item,
        range: BlockRange,
        mode: RenderMode,
    ) {
        let deleted = item.is_deleted();
        let gc = matches!(item.content, ItemContent::Deleted(_));
        let map = if deleted {
            &self.deletes
        } else {
            &self.inserts
        };
        for (r, attrs) in map.slice(&range) {
            if deleted && attrs.is_none() {
                if matches!(mode, RenderMode::Retain | RenderMode::Fresh) {
                    continue;
                }
                if gc {
                    // gc'd content inserted since the base doc renders as nothing
                    for (r, inserted) in self.inserts.slice(&range.subrange(r)) {
                        if inserted.is_none() {
                            self.push(out, item, r, None, mode);
                        }
                    }
                    continue;
                }
            }
            self.push(out, item, r, attrs, mode);
        }
    }

    fn content_len(&self, item: &Item, kind: OffsetKind) -> u32 {
        if !item.is_deleted() {
            return if item.is_countable() {
                item.content_len(kind)
            } else {
                0
            };
        }
        let slices = self.deletes.slice(&item.range());
        if !matches!(item.content, ItemContent::Deleted(_)) {
            return if item.is_countable() {
                covered_len(item, slices.map(|(r, a)| (r, a.is_some())), kind)
            } else {
                0
            };
        }
        let mut len = 0;
        for (range, attrs) in slices {
            if attrs.is_some() {
                self.gc_slices(item.id.client, range, |range, content| {
                    if let AttributedFragmentKind::Restored { content, offset } = content {
                        if content.is_countable() {
                            len += native_len(content, offset, range.end - range.start, kind);
                        }
                    }
                });
            }
        }
        len
    }

    #[inline]
    fn attributed(&self) -> &IdSet {
        &self.attributed
    }
}

/// Copies a `len` long slice of `content` starting at clock `offset`.
fn slice_content(content: &ItemContent, offset: u32, len: u32) -> ItemContent {
    let range = offset as usize..(offset + len) as usize;
    match content {
        ItemContent::String(s) => {
            let (_, rest) = split_str(s.as_str(), range.start, OffsetKind::Utf16);
            let (slice, _) = split_str(rest, len as usize, OffsetKind::Utf16);
            ItemContent::String(slice.into())
        }
        ItemContent::Any(values) => ItemContent::Any(values[range].to_vec()),
        ItemContent::JSON(values) => ItemContent::JSON(values[range].to_vec()),
        other => other.clone(),
    }
}
