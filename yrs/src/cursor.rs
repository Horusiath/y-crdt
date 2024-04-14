use crate::block::{Item, ItemContent, ItemPtr, Prelim};
use crate::branch::BranchPtr;
use crate::moving::{Move, StickyIndex};
use crate::slice::ItemSlice;
use crate::transaction::{ReadTxn, TransactionMut};
use crate::types::{TypePtr, Value};
use crate::{Assoc, ID};
use std::cmp::Ordering;

/// Struct used for iterating over the sequence of item's values with respect to a potential
/// [Move] markers that may change their order.
#[derive(Debug, Clone)]
pub(crate) struct RawCursor {
    /// Current shared collection scope.
    branch: BranchPtr,
    /// Current human-readable index within the shared collection scope.
    index: u32,
    /// Position of cursor within the current block.
    block_offset: u32,
    /// A block where a cursor is located.
    current_item: Option<ItemPtr>,
    /// Flag to indicate if cursor has reached the end of the block list.
    reached_end: bool,
    curr_move: Option<ItemPtr>,
    curr_move_start: Option<ItemPtr>,
    curr_move_end: Option<ItemPtr>,
    moved_stack: Vec<StackItem>,
}

impl RawCursor {
    pub fn new(branch: BranchPtr) -> Self {
        let current_item = branch.start;
        let reached_end = branch.start.is_none();
        RawCursor {
            branch,
            current_item,
            reached_end,
            curr_move: None,
            curr_move_start: None,
            curr_move_end: None,
            index: 0,
            block_offset: 0,
            moved_stack: Vec::default(),
        }
    }

    /// Returns true if current cursor reached the end of collection.
    #[inline]
    pub fn finished(&self) -> bool {
        (self.reached_end && self.curr_move.is_none()) || self.index == self.branch.content_len
    }

    /// Returns an item slice pointing to the current position of a cursor within the block list.
    #[inline]
    pub fn current(&self) -> Option<ItemSlice> {
        let item = self.current_item?;
        Some(ItemSlice::new(item, self.block_offset, item.len - 1))
    }

    /// Moves cursor position to a given index.
    /// Returns false if index was outside the collection boundaries.
    pub fn seek<T: ReadTxn>(&mut self, txn: &T, index: u32) -> bool {
        match index.cmp(&self.index) {
            Ordering::Less => self.backward(txn, self.index - index),
            Ordering::Equal => true,
            Ordering::Greater => self.forward(txn, index - self.index),
        }
    }

    fn can_forward(&self, ptr: Option<ItemPtr>, len: u32) -> bool {
        if !self.reached_end || self.curr_move.is_some() {
            if len > 0 {
                return true;
            } else if let Some(item) = ptr.as_deref() {
                return !item.is_countable()
                    || item.is_deleted()
                    || ptr == self.curr_move_end
                    || (self.reached_end && self.curr_move_end.is_none())
                    || item.moved != self.curr_move;
            }
        }

        false
    }

    /// Moves cursor by given number of elements to the right.
    pub fn forward<T: ReadTxn>(&mut self, txn: &T, mut len: u32) -> bool {
        if len == 0 && self.current_item.is_none() {
            return true;
        }

        if self.index + len > self.branch.content_len() || self.current_item.is_none() {
            return false;
        }

        let mut item = self.current_item;
        self.index += len;
        if self.block_offset != 0 {
            len += self.block_offset;
            self.block_offset = 0;
        }

        let encoding = txn.store().options.offset_kind;
        while self.can_forward(item, len) {
            if item == self.curr_move_end
                || (self.reached_end && self.curr_move_end.is_none() && self.curr_move.is_some())
            {
                item = self.curr_move; // we iterate to the right after the current condition
                self.pop(txn);
            } else if item.is_none() {
                return false;
            } else if let Some(i) = item.as_deref() {
                if i.is_countable() && !i.is_deleted() && i.moved == self.curr_move && len > 0 {
                    let item_len = i.content_len(encoding);
                    if item_len > len {
                        self.block_offset = len;
                        len = 0;
                        break;
                    } else {
                        len -= item_len;
                    }
                } else if let ItemContent::Move(m) = &i.content {
                    if i.moved == self.curr_move {
                        if let Some(ptr) = self.curr_move {
                            self.moved_stack.push(StackItem::new(
                                self.curr_move_start,
                                self.curr_move_end,
                                ptr,
                            ));
                        }

                        let (start, end) = m.get_moved_coords(txn);
                        self.curr_move = item;
                        self.curr_move_start = start;
                        self.curr_move_end = end;
                        item = start;
                        continue;
                    }
                }
            }

            if self.reached_end {
                return false;
            }

            match item.as_deref() {
                Some(i) if i.right.is_some() => item = i.right,
                _ => self.reached_end = true, //TODO: we need to ensure to iterate further if this.currMoveEnd === null
            }
        }

        self.index -= len;
        self.current_item = item;
        true
    }

    fn reduce_moves(&mut self, txn: &mut TransactionMut) {
        let mut item = self.current_item;
        if item.is_some() {
            while item == self.curr_move_start {
                item = self.curr_move;
                self.pop(txn);
            }
            self.current_item = item;
        }
    }

    /// Moves cursor by given number of elements to the left.
    pub fn backward<T: ReadTxn>(&mut self, txn: &T, mut len: u32) -> bool {
        if self.index < len {
            return false;
        }
        self.index -= len;
        let encoding = txn.store().options.offset_kind;
        if self.reached_end {
            if let Some(next_item) = self.current_item.as_deref() {
                self.block_offset = if next_item.is_countable() && !next_item.is_deleted() {
                    next_item.content_len(encoding)
                } else {
                    0
                };
            }
        }
        if self.block_offset >= len {
            self.block_offset -= len;
            return true;
        }
        let mut item = self.current_item;
        if let Some(i) = item.as_deref() {
            if let ItemContent::Move(_) = &i.content {
                item = i.left;
            } else {
                len += if i.is_countable() && !i.is_deleted() && i.moved == self.curr_move {
                    i.content_len(encoding)
                } else {
                    0
                };
                len -= self.block_offset;
            }
        }
        self.block_offset = 0;
        while let Some(i) = item.as_deref() {
            if len == 0 {
                break;
            }

            if i.is_countable() && !i.is_deleted() && i.moved == self.curr_move {
                let item_len = i.content_len(encoding);
                if len < item_len {
                    self.block_offset = item_len - len;
                    len = 0;
                } else {
                    len -= item_len;
                }
                if len == 0 {
                    break;
                }
            } else if let ItemContent::Move(m) = &i.content {
                if i.moved == self.curr_move {
                    if let Some(curr_move) = self.curr_move {
                        self.moved_stack.push(StackItem::new(
                            self.curr_move_start,
                            self.curr_move_end,
                            curr_move,
                        ));
                    }
                    let (start, end) = m.get_moved_coords(txn);
                    self.curr_move = item;
                    self.curr_move_start = start;
                    self.curr_move_end = end;
                    item = start;
                    continue;
                }
            }

            if item == self.curr_move_start {
                item = self.curr_move; // we iterate to the left after the current condition
                self.pop(txn);
            }

            item = if let Some(i) = item.as_deref() {
                i.left
            } else {
                None
            };
        }
        self.current_item = item;
        true
    }

    /// We keep the moved-stack across several transactions. Local or remote changes can invalidate
    /// "moved coords" on the moved-stack.
    ///
    /// The reason for this is that if assoc < 0, then getMovedCoords will return the target.right
    /// item. While the computed item is on the stack, it is possible that a user inserts something
    /// between target and the item on the stack. Then we expect that the newly inserted item
    /// is supposed to be on the new computed item.
    fn pop<T: ReadTxn>(&mut self, txn: &T) {
        let mut start = None;
        let mut end = None;
        let mut moved = None;
        if let Some(stack_item) = self.moved_stack.pop() {
            moved = Some(stack_item.moved_to);
            start = stack_item.start;
            end = stack_item.end;

            let moved_item = stack_item.moved_to;
            if let ItemContent::Move(m) = &moved_item.content {
                if m.start.assoc == Assoc::Before && (m.start.within_range(start))
                    || (m.end.within_range(end))
                {
                    let (s, e) = m.get_moved_coords(txn);
                    start = s;
                    end = e;
                }
            }
        }
        self.curr_move = moved;
        self.curr_move_start = start;
        self.curr_move_end = end;
        self.reached_end = false;
    }

    /// Deletes given number of elements, starting from current cursor position.
    /// Returns a number of elements deleted.
    pub fn delete(&mut self, txn: &mut TransactionMut, len: u32) -> u32 {
        let mut remaining = len;
        let mut item = self.current_item;
        if self.index + remaining > self.branch.content_len() {
            return len - remaining;
        }

        let encoding = txn.store().options.offset_kind;
        let mut i: &Item;
        while remaining > 0 {
            while let Some(block) = item.as_deref() {
                i = block;
                if !i.is_deleted()
                    && i.is_countable()
                    && !self.reached_end
                    && remaining > 0
                    && i.moved == self.curr_move
                    && item != self.curr_move_end
                {
                    if self.block_offset > 0 {
                        let mut id = i.id.clone();
                        id.clock += self.block_offset;
                        let store = txn.store_mut();
                        item = store
                            .blocks
                            .get_item_clean_start(&id)
                            .map(|s| store.materialize(s));
                        i = item.as_deref().unwrap();
                        self.block_offset = 0;
                    }
                    if remaining < i.content_len(encoding) {
                        let mut id = i.id.clone();
                        id.clock += remaining;
                        let store = txn.store_mut();
                        store
                            .blocks
                            .get_item_clean_start(&id)
                            .map(|s| store.materialize(s));
                    }
                    let content_len = i.content_len(encoding);
                    remaining -= content_len;
                    txn.delete(item.unwrap());
                    if i.right.is_some() {
                        item = i.right;
                    } else {
                        self.reached_end = true;
                    }
                } else {
                    break;
                }
            }
            if remaining > 0 {
                self.current_item = item;
                if self.forward(txn, 0) {
                    item = self.current_item;
                } else {
                    panic!("Block iter couldn't move forward");
                }
            }
        }
        self.current_item = item;
        len - remaining
    }

    pub(crate) fn slice<T: ReadTxn>(&mut self, txn: &T, buf: &mut [Value]) -> u32 {
        let mut len = buf.len() as u32;
        if self.index + len > self.branch.content_len() {
            return 0;
        }
        self.index += len;
        let mut next_item = self.current_item;
        let encoding = txn.store().options.offset_kind;
        let mut read = 0u32;
        while len > 0 {
            if !self.reached_end {
                while let Some(item) = next_item {
                    if Some(item) != self.curr_move_end
                        && item.is_countable()
                        && !self.reached_end
                        && len > 0
                    {
                        if !item.is_deleted() && item.moved == self.curr_move {
                            // we're iterating inside of a block
                            let r = item
                                .content
                                .read(self.block_offset as usize, &mut buf[read as usize..])
                                as u32;
                            read += r;
                            len -= r;
                            if self.block_offset + r == item.content_len(encoding) {
                                self.block_offset = 0;
                            } else {
                                self.block_offset += r;
                                continue; // do not iterate to item.right
                            }
                        }

                        if item.right.is_some() {
                            next_item = item.right;
                        } else {
                            self.reached_end = true;
                        }
                    } else {
                        break;
                    }
                }
                if (!self.reached_end || self.curr_move.is_some()) && len > 0 {
                    // always set nextItem before any method call
                    self.current_item = next_item;
                    if !self.forward(txn, 0) || self.current_item.is_none() {
                        return read;
                    }
                    next_item = self.current_item;
                }
            } else if self.curr_move.is_some() {
                // reached end but move stack still has some items,
                // so we try to pop move frames and move on the
                // first non-null right neighbor of the popped move block
                while let Some(mov) = self.curr_move.as_deref() {
                    next_item = mov.right;
                    self.pop(txn);
                    if next_item.is_some() {
                        self.reached_end = false;
                        break;
                    }
                }
            } else {
                // reached end and move stack is empty
                next_item = None;
                break;
            }
        }
        self.current_item = next_item;
        if len < 0 {
            self.index -= len;
        }
        read
    }

    /// Returns items to the left and right side of the current cursor. If cursor points in
    /// the middle of an item, that item will be split and new left and right items will be returned
    pub fn try_split(&mut self, txn: &mut TransactionMut) -> (Option<ItemPtr>, Option<ItemPtr>) {
        if self.block_offset > 0 {
            if let Some(ptr) = self.current_item {
                let mut item_id = ptr.id().clone();
                item_id.clock += self.block_offset;
                let store = txn.store_mut();
                self.current_item = store
                    .blocks
                    .get_item_clean_start(&item_id)
                    .map(|s| store.materialize(s));
                self.block_offset = 0;
            }
        }
        if self.reached_end {
            (self.current_item, None)
        } else {
            let right = self.current_item;
            let left = right.and_then(|ptr| ptr.left);
            (left, right)
        }
    }

    pub(crate) fn read_value<T: ReadTxn>(&mut self, txn: &T) -> Option<Value> {
        let mut buf = [Value::default()];
        if self.slice(txn, &mut buf) != 0 {
            Some(std::mem::replace(&mut buf[0], Value::default()))
        } else {
            None
        }
    }

    pub fn insert<V: Prelim>(&mut self, txn: &mut TransactionMut, value: V) -> ItemPtr {
        self.reduce_moves(txn);
        let (left, right) = self.try_split(txn);
        let id = {
            let store = txn.store();
            let client_id = store.options.client_id;
            let clock = store.blocks.get_clock(&client_id);
            ID::new(client_id, clock)
        };
        let parent = TypePtr::Branch(self.branch);
        let (mut content, remainder) = value.into_content(txn);
        let inner_ref = if let ItemContent::Type(inner_ref) = &mut content {
            Some(BranchPtr::from(inner_ref))
        } else {
            None
        };
        let mut block = Item::new(
            id,
            left,
            left.map(|ptr| ptr.last_id()),
            right,
            right.map(|r| *r.id()),
            parent,
            None,
            content,
        );
        let mut block_ptr = ItemPtr::from(&mut block);

        block_ptr.integrate(txn, 0);

        txn.store_mut().blocks.push_block(block);

        if let Some(remainder) = remainder {
            remainder.integrate(txn, inner_ref.unwrap().into())
        }

        if let Some(item) = right.as_deref() {
            self.current_item = item.right;
        } else {
            self.current_item = left;
            self.reached_end = true;
        }

        block_ptr
    }

    pub fn insert_move(&mut self, txn: &mut TransactionMut, start: StickyIndex, end: StickyIndex) {
        self.insert(txn, Move::new(start, end, -1));
    }

    pub fn values<'a, 'txn, T: ReadTxn>(
        &'a mut self,
        txn: &'txn mut TransactionMut<'txn>,
    ) -> Values<'a, 'txn> {
        Values::new(self, txn)
    }
}

pub struct Values<'a, 'txn> {
    iter: &'a mut RawCursor,
    txn: &'txn mut TransactionMut<'txn>,
}

impl<'a, 'txn> Values<'a, 'txn> {
    fn new(iter: &'a mut RawCursor, txn: &'txn mut TransactionMut<'txn>) -> Self {
        Values { iter, txn }
    }
}

impl<'a, 'txn> Iterator for Values<'a, 'txn> {
    type Item = Value;

    fn next(&mut self) -> Option<Self::Item> {
        if self.iter.reached_end || self.iter.index == self.iter.branch.content_len() {
            None
        } else {
            let mut buf = [Value::default()];
            if self.iter.slice(self.txn, &mut buf) != 0 {
                Some(std::mem::replace(&mut buf[0], Value::default()))
            } else {
                None
            }
        }
    }
}

#[derive(Debug, Clone)]
struct StackItem {
    start: Option<ItemPtr>,
    end: Option<ItemPtr>,
    moved_to: ItemPtr,
}

impl StackItem {
    fn new(start: Option<ItemPtr>, end: Option<ItemPtr>, moved_to: ItemPtr) -> Self {
        StackItem {
            start,
            end,
            moved_to,
        }
    }
}
