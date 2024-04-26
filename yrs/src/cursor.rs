use std::cmp::Ordering;
use std::convert::TryFrom;

use crate::block::{Item, ItemContent, ItemPtr, Prelim};
use crate::branch::{Branch, BranchPtr};
use crate::iter::{IntoBlockIter, MoveIter, TxnDoubleEndedIterator, TxnIterator};
use crate::slice::ItemSlice;
use crate::types::TypePtr;
use crate::{Assoc, IndexScope, ReadTxn, StickyIndex, TransactionMut, Value, ID};

#[derive(Debug, Clone)]
pub struct RawCursor<'branch> {
    /// The branch that this cursor is iterating over.
    branch: &'branch Branch,
    /// Internal iterator over the branch's blocks.
    move_iter: MoveIter,
    /// The last item that was returned by the iterator.
    last_item: Option<ItemPtr>,
    /// The current index of the cursor: length of elements from the cursor start position
    /// (see: [Item::content_len]).
    index: u32,
    /// Offset within the current item - counted as block len - where the cursor points to.
    /// (see: [Item::len])
    offset: u32,
}

impl<'branch> RawCursor<'branch> {
    pub fn new(branch: &'branch Branch) -> Self {
        let iter = branch.start.to_iter().moved();
        Self {
            move_iter: iter,
            index: 0,
            offset: 0,
            branch,
            last_item: None,
        }
    }

    #[inline]
    pub fn index(&self) -> u32 {
        self.index
    }

    pub fn position(&self) -> Option<ID> {
        let item = self.last_item?;
        let mut id = item.id;
        id.clock += self.offset;
        Some(id)
    }

    pub fn seek<T: ReadTxn>(&mut self, txn: &T, index: u32) -> bool {
        match index.cmp(&self.index) {
            Ordering::Less => self.backward(txn, self.index - index),
            Ordering::Equal => return true,
            Ordering::Greater => self.forward(txn, self.index - index),
        }
    }

    pub fn forward<T: ReadTxn>(&mut self, txn: &T, offset: u32) -> bool {
        let mut remaining = offset;
        let encoding = txn.store().options.offset_kind;
        let mut current = self.last_item;
        if current.is_none() {
            if self.move_iter.block_iter.next.is_none() {
                self.move_iter.block_iter.next = self.branch.start;
            }
            current = self.next(txn);
        }
        loop {
            if let Some(item) = current {
                if !item.is_deleted() && item.is_countable() {
                    let len = item.content_len(encoding);
                    // n: how many elements can we move forward within the current item
                    let n = (len - self.offset).min(remaining);
                    self.offset += n;
                    self.index += n;
                    remaining -= n;
                    if remaining == 0 {
                        break;
                    }
                }
            } else {
                break;
            }
            current = self.next(txn);
        }
        remaining == 0
    }

    pub fn backward<T: ReadTxn>(&mut self, txn: &T, offset: u32) -> bool {
        let mut remaining = offset;
        if self.offset >= remaining {
            // offset we're looking for is within the range of current item
            self.index -= remaining;
            self.offset -= remaining;
            return true;
        } else if self.offset > 0 {
            // we'll move to next item shortly, trim searched offset by the current item
            remaining -= self.offset;
            self.index -= self.offset;
            self.offset = 0;
        }

        // check if our internal iterator haven't been initialized yet
        if self.move_iter.block_iter.next.is_none() {
            self.move_iter.block_iter.next = self.last_item;
            self.move_iter.next_back(txn);
        }

        let encoding = txn.store().options.offset_kind;
        while let Some(item) = self.next_back(txn) {
            self.last_item = Some(item);
            if !item.is_deleted() && item.is_countable() {
                let len = item.content_len(encoding);
                if remaining <= len {
                    // the offset we're looking for is within current item
                    self.offset = len - remaining;
                    self.index -= remaining;
                    return true;
                } else {
                    // adjust length and offset and jump to next item
                    self.index -= len;
                    remaining -= len;
                }
            }
        }
        remaining == 0
    }

    pub fn insert<P>(&mut self, txn: &mut TransactionMut, prelim: P) -> P::Return
    where
        P: Prelim,
    {
        let (left, right) = self.split(txn);
        let id = txn.store.next_id();
        let parent = TypePtr::Branch(BranchPtr::from(self.branch));
        let (mut content, remainder) = prelim.into_content(txn);
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
        let len = block_ptr.content_len(txn.store().options.offset_kind);
        self.index += len;
        if let Some(right) = right {
            self.last_item = block_ptr.right;
            self.offset = 0;
        } else {
            self.last_item = Some(block_ptr);
            self.offset = len;
        }

        let result = P::Return::try_from(block_ptr);
        result.ok().unwrap()
    }

    pub fn remove(&mut self, txn: &mut TransactionMut, len: u32) -> bool {
        let encoding = txn.store().options.offset_kind;
        let mut remaining = len;
        let mut current = self.last_item;
        while remaining > 0 {
            if let Some(mut item) = current {
                if !item.is_deleted() && item.is_countable() {
                    let item_len = item.content_len(encoding);
                    let del_len = remaining.min(item_len - self.offset);
                    if self.offset != 0 || item_len != del_len {
                        let slice = ItemSlice::new(item, self.offset, self.offset + del_len);
                        item = txn.store.materialize(slice);
                    }
                    self.offset += del_len;
                    txn.delete(item);
                }
            }
            current = self.next(txn);
        }
        remaining == 0
    }

    pub fn read<T: ReadTxn>(&mut self, txn: &T, buf: &mut [Value]) -> u32 {
        let mut read = 0u32;
        let buf_len = buf.len() as u32;
        while read < buf_len {
            if let Some(item) = self.last_item {
                if !item.is_deleted() && item.is_countable() {
                    let n = item
                        .content
                        .read(self.offset as usize, &mut buf[read as usize..])
                        as u32;

                    self.offset += n;
                    self.index += n;
                    read += n;
                    if read >= buf_len {
                        break;
                    }
                }
            }
            let next = self.move_iter.next(txn);
            if next.is_none() {
                break;
            }
            self.last_item = next;
            self.offset = 0;
        }
        read
    }

    pub fn as_sticky_index(&self, assoc: Assoc) -> StickyIndex {
        let scope = match self.position() {
            None => IndexScope::from_branch(BranchPtr::from(self.branch)),
            Some(id) => IndexScope::Relative(id),
        };
        StickyIndex::new(scope, assoc)
    }

    pub fn from_sticky_index<T: ReadTxn>(index: &StickyIndex) -> Option<Self> {
        todo!()
    }

    /// Reset current cursor position to the start of the parent collection.
    pub fn reset(&mut self) {
        *self = RawCursor::new(self.branch);
    }

    /// Force to reload current cursor position, reiterating over the parent collection to
    /// a current index position from the start.
    pub fn refresh<T: ReadTxn>(&mut self, txn: &T) {
        let index = self.index;
        self.reset();
        self.forward(txn, index);
    }

    fn neighbours(&self) -> (Option<ID>, Option<ID>) {
        if let Some(item) = self.last_item {
            if self.offset == 0 {
                // we're at the beginning of the right item
                (item.left.map(|i| i.last_id()), Some(item.id))
            } else if self.offset == item.len {
                // we're at the end of the left item
                (Some(item.last_id()), item.right.map(|i| i.id))
            } else {
                let left = ID::new(item.id.client, item.id.clock + self.offset - 1);
                let right = ID::new(item.id.client, item.id.clock + self.offset);
                (Some(left), Some(right))
            }
        } else {
            (None, None)
        }
    }

    /// Splits current item at the cursor position and returns the right item created this way.
    fn split(&mut self, txn: &mut TransactionMut) -> (Option<ItemPtr>, Option<ItemPtr>) {
        if let Some(item) = self.last_item {
            if self.offset == 0 {
                // we're at the beginning of the right item
                (item.left, Some(item))
            } else if self.offset == item.len {
                // we're at the end of the left item
                (Some(item), item.right)
            } else {
                let item = txn
                    .store
                    .materialize(ItemSlice::new(item, self.offset, item.len - 1));
                self.last_item = Some(item);
                self.offset = 0;
                (item.left, Some(item))
            }
        } else if let Some(item) = self.next(txn) {
            // we might be at the beginning of the collection, try to iterate to next element
            self.last_item = Some(item);
            self.offset = 0;
            (item.left, Some(item))
        } else {
            (None, None)
        }
    }

    /// Return number of countable elements remaining in a current item.
    fn remaining<T: ReadTxn>(&self, txn: &T) -> u32 {
        if let Some(item) = self.last_item {
            if !item.is_deleted() && item.is_countable() {
                let encoding = txn.store().options.offset_kind;
                let len = item.content_len(encoding);
                return len - self.offset;
            }
        }
        0
    }
}

impl<'branch> TxnIterator for RawCursor<'branch> {
    type Item = ItemPtr;

    fn next<T: ReadTxn>(&mut self, txn: &T) -> Option<Self::Item> {
        self.index += self.remaining(txn);
        if let Some(next) = self.move_iter.next(txn) {
            self.last_item = Some(next);
            self.offset = 0;
            Some(next)
        } else {
            None
        }
    }
}

impl<'branch> TxnDoubleEndedIterator for RawCursor<'branch> {
    fn next_back<T: ReadTxn>(&mut self, txn: &T) -> Option<Self::Item> {
        if let Some(item) = self.last_item {
            if !item.is_deleted() && item.is_countable() {
                self.index -= self.offset;
            }
        }
        if let Some(next) = self.move_iter.next_back(txn) {
            let item_len = next.content_len(txn.store().options.offset_kind);
            self.last_item = Some(next);
            self.offset = item_len;
            Some(next)
        } else {
            self.move_iter = self.branch.start.to_iter().moved();
            None
        }
    }
}

#[cfg(test)]
mod test {
    use crate::{Array, Doc, Transact, Value, ID};

    #[test]
    fn cursor_push_back() {
        let doc = Doc::with_client_id(1);
        let array = doc.get_or_insert_array("array");
        let mut txn = doc.transact_mut();
        let mut cursor = array.as_ref().cursor();

        cursor.insert(&mut txn, 1);
        assert_eq!(cursor.index(), 1);

        cursor.insert(&mut txn, 2);
        assert_eq!(cursor.index(), 2);

        cursor.insert(&mut txn, 3);
        assert_eq!(cursor.index(), 3);

        cursor.reset(); // reset cursor to the start position
        assert_eq!(cursor.index(), 0);

        let mut buf = [
            Value::default(),
            Value::default(),
            Value::default(),
            Value::default(),
        ];
        let read = cursor.read(&txn, &mut buf);
        assert_eq!(read, 3);
        assert_eq!(buf, [1.into(), 2.into(), 3.into(), Value::default()]);
        assert_eq!(cursor.index(), 3);
    }

    #[test]
    fn cursor_forward() {
        let doc = Doc::with_client_id(1);
        let array = doc.get_or_insert_array("array");
        let mut txn = doc.transact_mut();

        // blocks: [1,2][3][4,5][6,7,8][9]

        array.insert_range(&mut txn, 0, [9]); // id: <1#0>
        array.insert_range(&mut txn, 0, [6, 7, 8]); // id: <1#1..3>
        array.insert_range(&mut txn, 0, [4, 5]); // id: <1#4..5>
        array.insert_range(&mut txn, 0, [3]); // id: <1#6>
        array.insert_range(&mut txn, 0, [1, 2]); // id: <1#7..8>

        let mut c = array.as_ref().cursor();

        assert!(c.forward(&txn, 0), "move to index 0");
        assert_eq!(c.index(), 0);
        assert_eq!(c.neighbours(), (None, Some(ID::new(1, 7))));

        assert!(c.forward(&txn, 1), "move to index 1");
        assert_eq!(c.index(), 1);
        assert_eq!(c.neighbours(), (Some(ID::new(1, 7)), Some(ID::new(1, 8))));

        assert!(c.forward(&txn, 1), "move to index 2");
        assert_eq!(c.index(), 2);
        assert_eq!(c.neighbours(), (Some(ID::new(1, 8)), Some(ID::new(1, 6))));

        assert!(c.forward(&txn, 1), "move to index 3");
        assert_eq!(c.index(), 3);
        assert_eq!(c.neighbours(), (Some(ID::new(1, 6)), Some(ID::new(1, 4))));

        assert!(c.forward(&txn, 1), "move to index 4");
        assert_eq!(c.index(), 4);
        assert_eq!(c.neighbours(), (Some(ID::new(1, 4)), Some(ID::new(1, 5))));

        assert!(c.forward(&txn, 1), "move to index 5");
        assert_eq!(c.index(), 5);
        assert_eq!(c.neighbours(), (Some(ID::new(1, 5)), Some(ID::new(1, 1))));

        assert!(c.forward(&txn, 1), "move to index 6");
        assert_eq!(c.index(), 6);
        assert_eq!(c.neighbours(), (Some(ID::new(1, 1)), Some(ID::new(1, 2))));

        assert!(c.forward(&txn, 1), "move to index 7");
        assert_eq!(c.index(), 7);
        assert_eq!(c.neighbours(), (Some(ID::new(1, 2)), Some(ID::new(1, 3))));

        assert!(c.forward(&txn, 1), "move to index 8");
        assert_eq!(c.index(), 8);
        assert_eq!(c.neighbours(), (Some(ID::new(1, 3)), Some(ID::new(1, 0))));

        assert!(c.forward(&txn, 1), "move to index 9");
        assert_eq!(c.index(), 9);
        assert_eq!(c.neighbours(), (Some(ID::new(1, 0)), None));
    }

    #[test]
    fn cursor_backward() {
        let doc = Doc::with_client_id(1);
        let array = doc.get_or_insert_array("array");
        let mut txn = doc.transact_mut();

        // blocks: [1,2][3][4,5][6,7,8][9]

        array.insert_range(&mut txn, 0, [9]); // id: <1#0>
        array.insert_range(&mut txn, 0, [6, 7, 8]); // id: <1#1..3>
        array.insert_range(&mut txn, 0, [4, 5]); // id: <1#4..5>
        array.insert_range(&mut txn, 0, [3]); // id: <1#6>
        array.insert_range(&mut txn, 0, [1, 2]); // id: <1#7..8>

        let mut c = array.as_ref().cursor();

        assert!(c.forward(&txn, 9), "move to index 9");
        assert_eq!(c.index(), 9);
        assert_eq!(c.neighbours(), (Some(ID::new(1, 0)), None));

        assert!(c.backward(&txn, 1), "move to index 8");
        assert_eq!(c.index(), 8);
        assert_eq!(c.neighbours(), (Some(ID::new(1, 3)), Some(ID::new(1, 0))));

        assert!(c.backward(&txn, 1), "move to index 7");
        assert_eq!(c.index(), 7);
        assert_eq!(c.neighbours(), (Some(ID::new(1, 2)), Some(ID::new(1, 3))));

        assert!(c.backward(&txn, 1), "move to index 6");
        assert_eq!(c.index(), 6);
        assert_eq!(c.neighbours(), (Some(ID::new(1, 1)), Some(ID::new(1, 2))));

        assert!(c.backward(&txn, 1), "move to index 5");
        assert_eq!(c.index(), 5);
        assert_eq!(c.neighbours(), (Some(ID::new(1, 5)), Some(ID::new(1, 1))));

        assert!(c.backward(&txn, 1), "move to index 4");
        assert_eq!(c.index(), 4);
        assert_eq!(c.neighbours(), (Some(ID::new(1, 4)), Some(ID::new(1, 5))));

        assert!(c.backward(&txn, 1), "move to index 3");
        assert_eq!(c.index(), 3);
        assert_eq!(c.neighbours(), (Some(ID::new(1, 6)), Some(ID::new(1, 4))));

        assert!(c.backward(&txn, 1), "move to index 2");
        assert_eq!(c.index(), 2);
        assert_eq!(c.neighbours(), (Some(ID::new(1, 8)), Some(ID::new(1, 6))));

        assert!(c.backward(&txn, 1), "move to index 1");
        assert_eq!(c.index(), 1);
        assert_eq!(c.neighbours(), (Some(ID::new(1, 7)), Some(ID::new(1, 8))));

        assert!(c.backward(&txn, 1), "move to index 0");
        assert_eq!(c.index(), 0);
        assert_eq!(c.neighbours(), (None, Some(ID::new(1, 7))));
    }
}
