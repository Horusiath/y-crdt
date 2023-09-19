use crate::js::{FromJs, IntoJs};
use std::ops::{Deref, DerefMut};
use wasm_bindgen::convert::{FromWasmAbi, IntoWasmAbi};
use wasm_bindgen::describe::{inform, WasmDescribe, U32};
use wasm_bindgen::JsValue;
use yrs::Doc;

#[repr(transparent)]
#[derive(Clone)]
pub struct YDoc(Doc);

impl From<Doc> for YDoc {
    #[inline]
    fn from(value: Doc) -> Self {
        YDoc(value)
    }
}

impl Into<Doc> for YDoc {
    #[inline]
    fn into(self) -> Doc {
        self.0
    }
}

impl WasmDescribe for YDoc {
    fn describe() {
        inform(U32)
    }
}

impl FromWasmAbi for YDoc {
    type Abi = u32;

    unsafe fn from_abi(js: Self::Abi) -> Self {
        YDoc(Doc::from_raw(js as *const _))
    }
}

impl IntoWasmAbi for YDoc {
    type Abi = u32;

    fn into_abi(self) -> Self::Abi {
        self.0.into_raw() as u32
    }
}

impl FromJs for YDoc {
    fn from_js(js: JsValue) -> Result<Self, JsValue> {
        let ptr = js.into_abi();
        let branch = unsafe { Self::from_abi(ptr) };
        Ok(branch)
    }
}

impl IntoJs for YDoc {
    type Return = JsValue;

    fn into_js(self) -> Self::Return {
        let ptr = self.into_abi();
        unsafe { JsValue::from_abi(ptr) }
    }
}

impl Deref for YDoc {
    type Target = Doc;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for YDoc {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
