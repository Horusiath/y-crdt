use crate::js::{FromJs, IntoJs, JsPrelim};
use crate::transaction::Transaction;
use js_sys::Function;
use std::iter::FromIterator;
use std::ops::{Deref, DerefMut};
use wasm_bindgen::prelude::wasm_bindgen;
use wasm_bindgen::JsValue;
use yrs::types::ToJson;
use yrs::{
    Any, Array, ArrayRef, DeepObservable, Observable, SubscriptionId, TransactionMut, Value,
};

#[wasm_bindgen]
pub struct YArray(ArrayRef);

impl From<ArrayRef> for YArray {
    fn from(value: ArrayRef) -> Self {
        YArray(value)
    }
}

impl Deref for YArray {
    type Target = ArrayRef;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[wasm_bindgen]
impl YArray {
    /// Returns a number of elements stored within this instance of `YArray`.
    #[wasm_bindgen(js_name = length)]
    pub fn length(&self, txn: &Transaction) -> u32 {
        self.0.len(txn.deref())
    }

    /// Converts an underlying contents of this `YArray` instance into their JSON representation.
    #[wasm_bindgen(js_name = toJson)]
    pub fn to_json(&self, txn: &Transaction) -> JsValue {
        self.0.to_json(txn.deref()).into_js()
    }

    /// Inserts a given range of `items` into this `YArray` instance, starting at given `index`.
    #[wasm_bindgen(js_name = insert)]
    pub fn insert(&self, index: u32, items: Vec<JsValue>, txn: &mut Transaction) {
        self.0.insert_at(txn.deref_mut(), index, items)
    }

    /// Appends a range of `items` at the end of this `YArray` instance.
    #[wasm_bindgen(js_name = push)]
    pub fn push(&self, items: Vec<JsValue>, txn: &mut Transaction) {
        let txn: &mut TransactionMut = txn.deref_mut();
        let index = self.0.len(txn);
        let array_ref = self.deref();
        array_ref.insert_at(txn, index, items);
    }

    /// Deletes a range of items of given `length` from current `YArray` instance,
    /// starting from given `index`.
    #[wasm_bindgen(js_name = remove)]
    pub fn remove(&self, index: u32, length: u32, txn: &mut Transaction) {
        let array_ref = self.deref();
        array_ref.remove_range(txn.deref_mut(), index, length)
    }

    /// Moves element found at `source` index into `target` index position.
    #[wasm_bindgen(js_name = move)]
    pub fn move_content(&self, source: u32, target: u32, txn: &mut Transaction) {
        let array_ref = self.deref();
        array_ref.move_to(txn.deref_mut(), source, target);
    }

    /// Returns an element stored under given `index`.
    #[wasm_bindgen(js_name = get)]
    pub fn get(&self, index: u32, txn: &Transaction) -> Result<JsValue, JsValue> {
        let array_ref = self.deref();
        match array_ref.get(txn.deref(), index) {
            Some(value) => Ok(value.into_js()),
            None => Err(JsValue::from_str("index out of bounds")),
        }
    }

    /// Returns an iterator that can be used to traverse over the values stored withing this
    /// instance of `YArray`.
    ///
    /// Example:
    ///
    /// ```javascript
    /// import YDoc from 'ywasm'
    ///
    /// /// document on machine A
    /// const doc = new YDoc()
    /// const array = doc.getArray('name')
    /// const txn = doc.beginTransaction()
    /// try {
    ///     array.push(txn, ['hello', 'world'])
    ///     for (let item of array.values(txn)) {
    ///         console.log(item)
    ///     }
    /// } finally {
    ///     txn.free()
    /// }
    /// ```
    #[wasm_bindgen(js_name = values)]
    pub fn values(&self, txn: &Transaction) -> js_sys::Array {
        let iter = self.0.iter(txn.deref()).map(Value::into_js);
        js_sys::Array::from_iter(iter)
    }

    #[wasm_bindgen(js_name = observe)]
    pub fn observe(&mut self, callback: Function) -> SubscriptionId {
        self.0
            .observe(|txn, e| {
                todo!();
            })
            .into()
    }

    #[wasm_bindgen(js_name = unobserve)]
    pub fn unobserve(&mut self, subscription_id: SubscriptionId) {
        self.0.unobserve(subscription_id)
    }

    #[wasm_bindgen(js_name = observeDeep)]
    pub fn observe_deep(&mut self, callback: Function) -> SubscriptionId {
        self.0
            .observe_deep(|txn, e| {
                todo!();
            })
            .into()
    }

    #[wasm_bindgen(js_name = unobserveDeep)]
    pub fn unobserve_deep(&mut self, subscription_id: SubscriptionId) {
        self.0.unobserve_deep(subscription_id)
    }
}

impl<T: Array> ArrayExt for T {}

pub trait ArrayExt: Array {
    fn insert_at(&self, txn: &mut TransactionMut, index: u32, src: Vec<JsValue>) {
        let mut j = index;
        let mut i = 0;
        while i < src.len() {
            let mut anys = Vec::default();
            while i < src.len() {
                let js = &src[i];
                if let Ok(any) = Any::from_js(js.clone()) {
                    anys.push(any);
                    i += 1;
                } else {
                    break;
                }
            }

            if !anys.is_empty() {
                let len = anys.len() as u32;
                self.insert_range(txn, j, anys);
                j += len;
            } else {
                let js = &src[i];
                let wrapper = JsPrelim::from(js.clone());
                self.insert(txn, j, wrapper);
                i += 1;
                j += 1;
            }
        }
    }
}
