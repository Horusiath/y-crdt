use std::ops::Deref;
use wasm_bindgen::prelude::wasm_bindgen;
use yrs::MapRef;

#[wasm_bindgen]
pub struct YMap(MapRef);

impl From<MapRef> for YMap {
    fn from(value: MapRef) -> Self {
        YMap(value)
    }
}

impl Deref for YMap {
    type Target = MapRef;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[wasm_bindgen]
impl YMap {}
