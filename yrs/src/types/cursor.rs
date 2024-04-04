use crate::block::{Item, ItemContent, ItemPtr, Prelim};
use crate::branch::BranchPtr;
use crate::iter::{IntoBlockIter, MoveIter, TxnDoubleEndedIterator, TxnIterator};
use crate::slice::ItemSlice;
use crate::types::TypePtr;
use crate::{ReadTxn, TransactionMut, Value, ID};
use std::convert::{TryFrom, TryInto};

/// Raw unregistered cursor.
pub(crate) struct RawCursor {
    index: u32,
    offset: u32,
    parent: BranchPtr, //TODO: eventually this should be a &'txn Branch
    iter: MoveIter,
    current: Option<ItemPtr>,
}

impl RawCursor {
    pub fn new(branch: BranchPtr) -> Self {
        let iter = branch.start.to_iter().moved();
        RawCursor {
            index: 0,
            offset: 0,
            iter,
            parent: branch,
            current: branch.start,
        }
    }

    pub fn index(&self) -> u32 {
        self.index
    }

    pub fn neighbours(&self) -> (Option<ID>, Option<ID>) {
        match self.current {
            None => (None, None),
            Some(ptr) => {
                if self.offset == 0 {
                    let left = ptr.left.map(|ptr| ptr.last_id());
                    let right = Some(ptr.id);
                    (left, right)
                } else {
                    let left = ID::new(ptr.id.client, ptr.id.clock + self.offset);
                    let right = ID::new(ptr.id.client, ptr.id.clock + self.offset - 1);
                    (Some(left), Some(right))
                }
            }
        }
    }

    pub fn left(&self) -> Option<ItemSlice> {
        if self.offset == 0 {
            let ptr = self.current?;
            ptr.left.map(ItemSlice::from)
        } else {
            Some(ItemSlice::new(self.current?, 0, self.offset))
        }
    }

    pub fn right(&self) -> Option<ItemSlice> {
        if self.offset == 0 {
            self.current.map(ItemSlice::from)
        } else {
            let ptr = self.current?;
            Some(ItemSlice::new(ptr, self.offset, ptr.len - 1))
        }
    }

    pub fn forward<T: ReadTxn>(&mut self, txn: &T, mut offset: u32) -> Result<(), CursorError> {
        let encoding = txn.store().options.offset_kind;
        if self.offset > 0 {
            // we're in the middle of an existing item
            let item = match self.current {
                None => return Err(CursorError::EndOfCollection),
                Some(item) => item,
            };
            let len = item.content_len(encoding);
            offset += self.offset;
            if offset < len - 1 {
                // offset param is still within the current item
                self.index += offset;
                self.offset = offset;
                return Ok(());
            } else if offset >= len {
                // we will jump to next item shortly, adjust the length of the current offset
                offset -= len;
                self.offset = 0;
                self.index += len;
            }
        }
        while let Some(item) = self.iter.next(txn) {
            self.current = Some(item);
            if !item.is_deleted() && item.is_countable() {
                let len = item.content_len(encoding);
                if offset < len - 1 {
                    // the offset we're looking for is within current item
                    self.offset = offset;
                    self.index += offset;
                    offset = 0;
                    break;
                } else {
                    // adjust length and offset and jump to next item
                    self.index += len;
                    offset -= len;
                }
            }
        }
        if offset == 0 {
            Ok(())
        } else {
            Err(CursorError::EndOfCollection)
        }
    }

    pub fn backward<T: ReadTxn>(&mut self, txn: &T, mut offset: u32) -> Result<(), CursorError> {
        let encoding = txn.store().options.offset_kind;
        if self.offset >= offset {
            // offset we're looking for is within the range of current item
            self.index -= offset;
            self.offset -= offset;
            return Ok(());
        } else {
            // we'll move to next item shortly, trim searched offset by the current item
            offset -= self.offset;
            self.index -= self.offset;
            self.offset = 0;
        }

        while let Some(item) = self.iter.next_back(txn) {
            self.current = Some(item);
            if !item.is_deleted() && item.is_countable() {
                let len = item.content_len(encoding);
                if offset < len - 1 {
                    // the offset we're looking for is within current item
                    self.offset = len - offset - 1;
                    self.index -= offset;
                    offset = 0;
                    break;
                } else {
                    // adjust length and offset and jump to next item
                    self.index -= len;
                    offset -= len;
                }
            }
        }
        if offset == 0 {
            Ok(())
        } else {
            Err(CursorError::EndOfCollection)
        }
    }

    pub fn seek<T: ReadTxn>(&mut self, txn: &T, index: u32) -> Result<(), CursorError> {
        let diff: i32 = index as i32 - self.index as i32;
        if diff > 0 {
            self.forward(txn, diff as u32)
        } else if diff < 0 {
            self.backward(txn, (-diff) as u32)
        } else {
            Ok(())
        }
    }

    pub fn insert<P: Prelim>(&mut self, txn: &mut TransactionMut, value: P) -> P::Return {
        let left = self.left().map(|slice| txn.store.materialize(slice));
        let right = self.right().map(|slice| txn.store.materialize(slice));

        let id = txn.store.next_id();
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
            TypePtr::Branch(self.parent),
            None,
            content,
        );
        let mut ptr = ItemPtr::from(&mut block);

        ptr.integrate(txn, 0);

        txn.store_mut().blocks.push_block(block);

        if let Some(remainder) = remainder {
            remainder.integrate(txn, inner_ref.unwrap().into())
        }

        self.move_to_next_item(txn);
        if let Ok(integrated) = ptr.try_into() {
            integrated
        } else {
            panic!("Defect: unexpected integrated type")
        }
    }

    fn move_to_next_item<T: ReadTxn>(&mut self, txn: &T) {
        self.offset = 0;
        if let Some(item) = self.iter.next(txn) {
            self.current = Some(item);
            self.index = item.content_len(txn.store().options.offset_kind);
        }
    }

    pub fn remove_range(&mut self, txn: &mut TransactionMut, len: u32) {
        todo!()
    }

    pub fn read_values<T: ReadTxn>(&mut self, t: &T, buf: &mut [Value]) -> u32 {
        todo!()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CursorError {
    #[error("cursor reached the end of collection")]
    EndOfCollection,
}

#[cfg(test)]
mod test {
    use crate::{Array, Doc, Transact, ID};

    #[test]
    fn cursor_forward() {
        let doc = Doc::with_client_id(1);
        let array = doc.get_or_insert_array("array");
        let mut txn = doc.transact_mut();

        // block slicing: [1,2][3][4,5][6,7,8][9]

        array.insert_range(&mut txn, 0, [9]); // id: <1#0>
        array.insert_range(&mut txn, 0, [6, 7, 8]); // id: <1#1..3>
        array.insert_range(&mut txn, 0, [4, 5]); // id: <1#4..5>
        array.insert_range(&mut txn, 0, [3]); // id: <1#6>
        array.insert_range(&mut txn, 0, [1, 2]); // id: <1#7..8>

        let mut c = array.as_ref().cursor();

        c.forward(&txn, 0).unwrap();
        assert_eq!(c.index(), 0);
        assert_eq!(c.neighbours(), (None, Some(ID::new(1, 7))));

        c.forward(&txn, 1).unwrap();
        assert_eq!(c.index(), 1);
        assert_eq!(c.neighbours(), (Some(ID::new(1, 7)), Some(ID::new(1, 8))));

        c.forward(&txn, 1).unwrap();
        assert_eq!(c.index(), 2);
        assert_eq!(c.neighbours(), (Some(ID::new(1, 8)), Some(ID::new(1, 6))));

        c.forward(&txn, 1).unwrap();
        assert_eq!(c.index(), 3);
        assert_eq!(c.neighbours(), (Some(ID::new(1, 6)), Some(ID::new(1, 4))));

        c.forward(&txn, 1).unwrap();
        assert_eq!(c.index(), 4);
        assert_eq!(c.neighbours(), (Some(ID::new(1, 4)), Some(ID::new(1, 5))));

        c.forward(&txn, 1).unwrap();
        assert_eq!(c.index(), 5);
        assert_eq!(c.neighbours(), (Some(ID::new(1, 5)), Some(ID::new(1, 1))));

        c.forward(&txn, 1).unwrap();
        assert_eq!(c.index(), 6);
        assert_eq!(c.neighbours(), (Some(ID::new(1, 1)), Some(ID::new(1, 2))));

        c.forward(&txn, 1).unwrap();
        assert_eq!(c.index(), 7);
        assert_eq!(c.neighbours(), (Some(ID::new(1, 2)), Some(ID::new(1, 3))));

        c.forward(&txn, 1).unwrap();
        assert_eq!(c.index(), 8);
        assert_eq!(c.neighbours(), (Some(ID::new(1, 3)), Some(ID::new(1, 0))));

        c.forward(&txn, 1).unwrap();
        assert_eq!(c.index(), 9);
        assert_eq!(c.neighbours(), (Some(ID::new(1, 0)), None));
    }
}
