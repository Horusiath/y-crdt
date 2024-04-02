use crate::block::Prelim;
use crate::branch::BranchPtr;
use crate::slice::ItemSlice;
use crate::{ReadTxn, TransactionMut, Value};

/// Raw unregistered cursor.
pub(crate) struct RawCursor {
    index: u32,
    parent: BranchPtr, //TODO: eventually this should be a &'txn Branch
    left: Option<ItemSlice>,
    right: Option<ItemSlice>,
}

impl RawCursor {
    pub fn new(branch: BranchPtr) -> Self {
        RawCursor {
            index: 0,
            parent: branch,
            left: None,
            right: branch.start.map(ItemSlice::from),
        }
    }

    pub fn index(&self) -> u32 {
        self.index
    }

    pub fn left(&self) -> Option<&ItemSlice> {
        todo!()
    }

    pub fn right(&self) -> Option<&ItemSlice> {
        todo!()
    }

    pub fn forward<T: ReadTxn>(&mut self, txn: &T, offset: u32) -> Result<(), CursorError> {
        todo!()
    }

    pub fn backward<T: ReadTxn>(&mut self, txn: &T, offset: u32) -> Result<(), CursorError> {
        todo!()
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

    pub fn insert<P: Prelim>(&mut self, txn: &TransactionMut, prelim: P) -> P::Return {
        todo!()
    }

    pub fn remove_range(&mut self, txn: &TransactionMut, len: u32) {
        todo!()
    }

    pub fn read_values<T: ReadTxn>(&mut self, t: &T, buf: &mut [Value]) -> u32 {
        todo!()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CursorError {
    #[error("cursor reached end of collection")]
    EndOfCollection,
}
