use std::ops::Deref;
use wasm_bindgen::prelude::wasm_bindgen;
use yrs::TextRef;

#[wasm_bindgen]
pub struct YText(TextRef);

impl From<TextRef> for YText {
    fn from(value: TextRef) -> Self {
        YText(value)
    }
}

impl Deref for YText {
    type Target = TextRef;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[wasm_bindgen]
impl YText {}
