use std::ops::{Deref, DerefMut};
use wasm_bindgen::prelude::wasm_bindgen;
use yrs::{ReadTxn, Store, Subdocs, TransactionMut, WriteTxn};

#[wasm_bindgen]
#[repr(transparent)]
pub struct Transaction(TransactionMut<'static>);

impl Deref for Transaction {
    type Target = TransactionMut<'static>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Transaction {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl ReadTxn for Transaction {
    fn store(&self) -> &Store {
        self.0.store()
    }
}

impl<'txn> From<TransactionMut<'txn>> for Transaction {
    fn from(value: TransactionMut<'txn>) -> Self {
        Self(unsafe { std::mem::transmute(value) })
    }
}

impl WriteTxn for Transaction {
    fn store_mut(&mut self) -> &mut Store {
        self.0.store_mut()
    }

    fn subdocs_mut(&mut self) -> &mut Subdocs {
        self.0.subdocs_mut()
    }
}
