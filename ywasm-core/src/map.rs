use crate::branch_abi;
use wasm_bindgen::prelude::wasm_bindgen;
use yrs::MapRef;

#[repr(transparent)]
pub struct YMap(MapRef);

branch_abi!(YMap, MapRef);

impl YMap {
    pub fn new(v: MapRef) -> Self {
        Self(v)
    }
}

#[wasm_bindgen]
impl YMap {}
