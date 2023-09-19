use std::ops::{Deref, DerefMut};
use wasm_bindgen::prelude::wasm_bindgen;
use yrs::TransactionMut;

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
