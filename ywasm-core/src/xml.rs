use crate::branch_abi;
use wasm_bindgen::prelude::wasm_bindgen;
use yrs::{XmlElementRef, XmlFragmentRef, XmlTextRef};

#[repr(transparent)]
pub struct YXmlFragment(XmlFragmentRef);

branch_abi!(YXmlFragment, XmlFragmentRef);

impl YXmlFragment {
    pub fn new(v: XmlFragmentRef) -> Self {
        Self(v)
    }
}

#[wasm_bindgen]
impl YXmlFragment {}

#[repr(transparent)]
pub struct YXmlElement(XmlElementRef);

branch_abi!(YXmlElement, XmlElementRef);

impl YXmlElement {
    pub fn new(v: XmlElementRef) -> Self {
        Self(v)
    }
}

#[wasm_bindgen]
impl YXmlElement {}

#[repr(transparent)]
pub struct YXmlText(XmlTextRef);

branch_abi!(YXmlText, XmlTextRef);

impl YXmlText {
    pub fn new(v: XmlTextRef) -> Self {
        Self(v)
    }
}

#[wasm_bindgen]
impl YXmlText {}
