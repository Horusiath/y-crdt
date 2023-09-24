use crate::js::{FromJs, IntoJs};
use wasm_bindgen::convert::{FromWasmAbi, IntoWasmAbi};
use wasm_bindgen::describe::{inform, WasmDescribe, RUST_STRUCT};
use wasm_bindgen::JsValue;
use yrs::types::{Branch, BranchPtr};

#[repr(transparent)]
pub(crate) struct BranchRef(BranchPtr);

impl BranchRef {
    #[inline]
    pub fn into_ptr(self) -> BranchPtr {
        self.0
    }
}

impl WasmDescribe for BranchRef {
    fn describe() {
        inform(RUST_STRUCT);
        for c in "BranchRef".chars() {
            inform(c as u32);
        }
    }
}

impl FromWasmAbi for BranchRef {
    type Abi = u32;

    #[inline]
    unsafe fn from_abi(js: Self::Abi) -> Self {
        let ptr: *mut Branch = js as *mut Branch;
        assert!(!ptr.is_null());
        BranchRef(BranchPtr::from(ptr.as_ref().unwrap()))
    }
}

impl IntoWasmAbi for BranchRef {
    type Abi = u32;

    #[inline]
    fn into_abi(self) -> Self::Abi {
        let ptr = self.0.as_ref() as *const Branch;
        ptr as u32
    }
}

impl IntoJs for BranchRef {
    type Return = JsValue;

    fn into_js(self) -> Self::Return {
        let ptr = self.into_abi();
        unsafe { JsValue::from_abi(ptr) }
    }
}

impl FromJs for BranchRef {
    fn from_js(js: JsValue) -> Result<Self, JsValue> {
        let ptr = js.into_abi();
        let branch = unsafe { Self::from_abi(ptr) };
        Ok(branch)
    }
}

impl<T> From<T> for BranchRef
where
    T: AsRef<Branch>,
{
    #[inline]
    fn from(value: T) -> Self {
        let ptr = BranchPtr::from(value.as_ref());
        BranchRef(ptr)
    }
}
