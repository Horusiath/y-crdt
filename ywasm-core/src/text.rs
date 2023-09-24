use crate::branch_abi;
use wasm_bindgen::prelude::wasm_bindgen;
use yrs::TextRef;

#[repr(transparent)]
pub struct YText(TextRef);

branch_abi!(YText, TextRef);

impl YText {
    pub fn new(v: TextRef) -> Self {
        Self(v)
    }
}

#[wasm_bindgen]
impl YText {}
