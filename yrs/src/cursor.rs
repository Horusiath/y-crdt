use std::cmp::Ordering;
use std::convert::TryFrom;

use smallvec::SmallVec;

use crate::block::{Item, ItemContent, ItemPtr, Prelim};
use crate::branch::BranchPtr;
use crate::transaction::{ReadTxn, TransactionMut};
use crate::types::{TypePtr, Value};
use crate::{Assoc, BranchID, IndexScope, StickyIndex, ID};

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
    move_stack: MoveStack,
}

impl RawCursor {
    pub fn new(branch: BranchPtr) -> Self {
        let current_item = branch.start;
        let reached_end = branch.start.is_none();
        RawCursor {
            branch,
            current_item,
            reached_end,
            index: 0,
            block_offset: 0,
            move_stack: MoveStack::default(),
        }
    }

    /// Moves cursor forward until it reaches position defined by [StickyIndex].
    /// Returns true if index position has been found. Otherwise, it will reach
    /// the end of collection and return false.
    pub fn from_index<T: ReadTxn>(txn: &T, index: &StickyIndex) -> Option<Self> {
        let (branch, id) = match &index.scope {
            IndexScope::Relative(id) => {
                let store = txn.store();
                let block = store.get_item(id)?;
                let branch = *block.parent.as_branch()?;
                (branch, Some(id))
            }
            IndexScope::Nested(id) => {
                let branch = BranchID::Nested(*id).get_branch(txn)?;
                (branch, None)
            }
            IndexScope::Root(name) => {
                let branch = BranchID::Root(name.clone()).get_branch(txn)?;
                (branch, None)
            }
        };
        let mut cursor = Self::new(branch);
        if let Some(id) = id {
            if !cursor.forward_to(txn, id, index.assoc) {
                return None; // cursor didn't reach the desired index
            }
        }

        Some(cursor)
    }

    /// Convert a current cursor position into a serializable [StickyIndex].
    pub fn as_index(&self, assoc: Assoc) -> StickyIndex {
        let id = match assoc {
            Assoc::After => self.right(),
            Assoc::Before => self.left(),
        };
        let scope = match id {
            Some(id) => IndexScope::Relative(id),
            None => match self.branch.id() {
                BranchID::Nested(id) => IndexScope::Nested(id),
                BranchID::Root(name) => IndexScope::Root(name),
            },
        };
        StickyIndex::new(scope, assoc)
    }

    pub fn forward_to<T: ReadTxn>(&mut self, txn: &T, id: &ID, assoc: Assoc) -> bool {
        while let Some(item) = self.current_item {
            if item.contains(id) {
                let mut offset = id.clock - item.id.clock;
                if assoc == Assoc::After {
                    offset += 1;
                };
                self.block_offset = offset;
                return true;
            }

            self.next_item(txn);
        }
        false
    }

    /// Moves cursor to the beginning of the next item in a collection.
    pub fn next_item<T: ReadTxn>(&mut self, txn: &T) {
        let encoding = txn.store().options.offset_kind;
        if !self.finished() {
            if let Some(item) = self.current_item {
                self.block_offset = 0;
                if !self.forward(txn, item.content_len(encoding)) {
                    return;
                }
            }
        }
    }

    /// Returns true if current cursor reached the end of collection.
    #[inline]
    pub fn finished(&self) -> bool {
        (self.reached_end && self.move_stack.current_scope().is_none())
            || self.index == self.branch.content_len
    }

    pub fn current_item(&self) -> Option<ItemPtr> {
        self.current_item
    }

    pub fn left(&self) -> Option<ID> {
        let item = self.current_item?;
        if self.reached_end {
            Some(item.last_id())
        } else if self.block_offset == 0 {
            let left = item.left?;
            Some(left.last_id())
        } else {
            let mut id = item.id;
            id.clock += self.block_offset;
            Some(id)
        }
    }

    pub fn right(&self) -> Option<ID> {
        let item = self.current_item?;
        if self.reached_end {
            None
        } else if self.block_offset == item.len {
            item.right.as_ref().map(|r| r.id)
        } else {
            let mut id = item.id;
            id.clock += self.block_offset;
            Some(id)
        }
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
        let move_scope = self.move_stack.current_scope();
        if !self.reached_end || move_scope.is_some() {
            if len > 0 {
                return true;
            } else if let Some(item) = ptr.as_deref() {
                let (curr_move, curr_move_end) = match move_scope {
                    None => (None, None),
                    Some(scope) => (Some(scope.dest), scope.end),
                };
                return !item.is_countable()
                    || item.is_deleted()
                    || ptr == curr_move_end
                    || (self.reached_end && curr_move_end.is_none())
                    || item.moved != curr_move;
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
            let move_scope = self.move_stack.current_scope();
            let (curr_move, curr_move_end) = match move_scope {
                None => (None, None),
                Some(scope) => (Some(scope.dest), scope.end),
            };
            if item == curr_move_end
                || (self.reached_end && curr_move_end.is_none() && curr_move.is_some())
            {
                item = curr_move; // we iterate to the right after the current condition
                self.move_stack.descent(txn);
                self.reached_end = false;
            } else if item.is_none() {
                return false;
            } else if let Some(i) = item.as_deref() {
                if i.is_countable() && !i.is_deleted() && i.moved == curr_move && len > 0 {
                    let item_len = i.content_len(encoding);
                    if len < item_len {
                        self.block_offset = len;
                        len = 0;
                        break;
                    } else {
                        len -= item_len;
                    }
                } else if let ItemContent::Move(m) = &i.content {
                    if i.moved == curr_move {
                        let (start, end) = m.get_moved_coords(txn);
                        self.move_stack
                            .push(MoveScope::new(start, end, item.unwrap()));
                        self.move_stack.current_scope();
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
        let mut move_scope = self.move_stack.current_scope();
        if let Some(i) = item.as_deref() {
            if let ItemContent::Move(_) = &i.content {
                item = i.left;
            } else {
                len +=
                    if i.is_countable() && !i.is_deleted() && i.moved == move_scope.map(|s| s.dest)
                    {
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
            let (curr_move, curr_move_start) = match move_scope {
                None => (None, None),
                Some(scope) => (Some(scope.dest), scope.start),
            };

            if i.is_countable() && !i.is_deleted() && i.moved == curr_move {
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
                if i.moved == curr_move {
                    let (start, end) = m.get_moved_coords(txn);
                    self.move_stack
                        .push(MoveScope::new(start, end, item.unwrap()));
                    move_scope = self.move_stack.current_scope();
                    item = start;
                    continue;
                }
            }

            if item == curr_move_start {
                item = curr_move; // we iterate to the left after the current condition
                move_scope = self.move_stack.descent(txn);
                self.reached_end = false;
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
            let move_scope = self.move_stack.current_scope();
            while let Some(block) = item.as_deref() {
                i = block;
                if !i.is_deleted()
                    && i.is_countable()
                    && !self.reached_end
                    && remaining > 0
                    && i.moved == move_scope.map(|s| s.dest)
                    && item != move_scope.and_then(|s| s.end)
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

    pub(crate) fn read<T: ReadTxn>(&mut self, txn: &T, buf: &mut [Value]) -> u32 {
        let mut len = buf.len() as u32;
        if self.index + len > self.branch.content_len() {
            return 0;
        }
        self.index += len;
        let mut next_item = self.current_item;
        let encoding = txn.store().options.offset_kind;
        let mut read = 0u32;
        while len > 0 {
            let mut move_scope = self.move_stack.current_scope();
            if !self.reached_end {
                while let Some(item) = next_item {
                    if Some(item) != move_scope.and_then(|s| s.end)
                        && item.is_countable()
                        && !self.reached_end
                        && len > 0
                    {
                        if !item.is_deleted() && item.moved == move_scope.map(|s| s.dest) {
                            // we're iterating inside a block
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
                if (!self.reached_end || move_scope.is_some()) && len > 0 {
                    // always set nextItem before any method call
                    self.current_item = next_item;
                    if !self.forward(txn, 0) || self.current_item.is_none() {
                        return read;
                    }
                    next_item = self.current_item;
                }
            } else if move_scope.is_some() {
                // reached end but move stack still has some items,
                // so we try to pop move frames and move on the
                // first non-null right neighbor of the popped move block
                while let Some(scope) = move_scope {
                    next_item = scope.dest.right;
                    move_scope = self.move_stack.descent(txn);
                    self.reached_end = false;
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
    pub fn split(&mut self, txn: &mut TransactionMut) -> (Option<ItemPtr>, Option<ItemPtr>) {
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
        if self.read(txn, &mut buf) != 0 {
            Some(std::mem::replace(&mut buf[0], Value::default()))
        } else {
            None
        }
    }

    pub fn insert<V: Prelim>(&mut self, txn: &mut TransactionMut, value: V) -> V::Return {
        self.reduce_moves(txn);
        let (left, right) = self.split(txn);
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

        let result = V::Return::try_from(block_ptr);
        result.ok().unwrap()
    }

    fn reduce_moves(&mut self, txn: &mut TransactionMut) {
        let mut item = self.current_item;
        if item.is_some() {
            let mut scope = self.move_stack.current_scope();
            while item == scope.and_then(|s| s.start) {
                item = scope.map(|s| s.dest);
                scope = self.move_stack.descent(txn);
                self.reached_end = false;
            }
            self.current_item = item;
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct MoveStack(Option<Box<SmallVec<[MoveScope; 1]>>>);

impl MoveStack {
    /// Returns a current scope of move operation.
    /// If `None`, it means that currently iterated elements were not moved anywhere.
    /// Otherwise, we are iterating over consecutive range of elements that have been
    /// relocated.
    pub fn current_scope(&self) -> Option<&MoveScope> {
        if let Some(stack) = &self.0 {
            stack.last()
        } else {
            None
        }
    }

    /// Pushes a new scope on top of current move stack. This happens when we touched
    /// a new block that contains a move content.
    pub fn push(&mut self, scope: MoveScope) {
        let stack = self.0.get_or_insert_with(Default::default);
        stack.push(scope)
    }

    /// Removes the latest scope from the move stack. Usually done when we detected that
    /// iterator reached the boundary of a move scope and we need to go back to the
    /// original destination.
    ///
    /// This method DOES NOT check if the next scope item on a stack was not changed due to
    /// corresponding items being shrunk/enlarged as part of another transaction operation.
    pub fn pop_unchecked(&mut self) -> Option<MoveScope> {
        if let Some(stack) = &mut self.0 {
            stack.pop()
        } else {
            None
        }
    }

    /// Removes the latest scope from the move stack. Returns a new move scope item at the top of
    /// the stack.
    pub fn descent<T: ReadTxn>(&mut self, txn: &T) -> Option<&MoveScope> {
        if let Some(stack) = &mut self.0 {
            stack.pop();
            if let Some(next) = stack.last_mut() {
                // We need to check if Move scope start/end item pointers haven't changed i.e.
                // because corresponding items have been split or squashed. If so, we need to
                // recompute the new start/end item pointers based on Move content data.
                if let ItemContent::Move(m) = &next.dest.content {
                    if (m.start.assoc == Assoc::Before && (m.start.within_range(next.start)))
                        || (m.end.assoc == Assoc::Before && m.end.within_range(next.end))
                    {
                        let (start, end) = m.get_moved_coords(txn);
                        next.start = start;
                        next.end = end;
                    }
                }
                return Some(next);
            }
        }
        None
    }
}

#[derive(Debug, Clone)]
pub struct MoveScope {
    /// First block moved in this scope.
    pub start: Option<ItemPtr>,
    /// Last block moved in this scope.
    pub end: Option<ItemPtr>,
    /// A.k.a. return address for the move range. Block pointer where the (start, end)
    /// range has been moved. It always contains an item with move content.
    pub dest: ItemPtr,
}

impl MoveScope {
    pub fn new(start: Option<ItemPtr>, end: Option<ItemPtr>, dest: ItemPtr) -> Self {
        MoveScope { start, end, dest }
    }
}
