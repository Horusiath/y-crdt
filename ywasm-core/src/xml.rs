use std::ops::Deref;
use wasm_bindgen::prelude::wasm_bindgen;
use yrs::{XmlElementRef, XmlFragmentRef, XmlTextRef};

#[wasm_bindgen]
pub struct YXmlFragment(XmlFragmentRef);

impl From<XmlFragmentRef> for YXmlFragment {
    fn from(value: XmlFragmentRef) -> Self {
        YXmlFragment(value)
    }
}

impl Deref for YXmlFragment {
    type Target = XmlFragmentRef;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[wasm_bindgen]
impl YXmlFragment {}

#[wasm_bindgen]
pub struct YXmlElement(XmlElementRef);

impl From<XmlElementRef> for YXmlElement {
    fn from(value: XmlElementRef) -> Self {
        YXmlElement(value)
    }
}

impl Deref for YXmlElement {
    type Target = XmlElementRef;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[wasm_bindgen]
impl YXmlElement {}

#[wasm_bindgen]
pub struct YXmlText(XmlTextRef);

impl From<XmlTextRef> for YXmlText {
    fn from(value: XmlTextRef) -> Self {
        YXmlText(value)
    }
}

impl Deref for YXmlText {
    type Target = XmlTextRef;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[wasm_bindgen]
impl YXmlText {}
