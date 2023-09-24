use crate::array::YArray;
use crate::js::{FromJs, IntoJs};
use crate::map::YMap;
use crate::text::YText;
use crate::xml::{YXmlElement, YXmlFragment, YXmlText};
use crate::Transaction;
use js_sys::Uint8Array;
use std::ops::{Deref, DerefMut};
use std::ptr::null;
use wasm_bindgen::convert::{FromWasmAbi, IntoWasmAbi, OptionIntoWasmAbi};
use wasm_bindgen::describe::{inform, WasmDescribe, U32};
use wasm_bindgen::prelude::wasm_bindgen;
use wasm_bindgen::JsValue;
use yrs::block::ClientID;
use yrs::{
    DestroySubscription, Doc, ReadTxn, SubdocsEvent, SubdocsEventIter, SubdocsSubscription,
    Transact, TransactionCleanupEvent, TransactionCleanupSubscription, UpdateSubscription,
};

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

impl OptionIntoWasmAbi for YDoc {
    fn none() -> Self::Abi {
        null::<YDoc>() as u32
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

#[wasm_bindgen]
impl YDoc {
    /// Creates a new ywasm document. If `id` parameter was passed it will be used as this document
    /// globally unique identifier (it's up to caller to ensure that requirement). Otherwise it will
    /// be assigned a randomly generated number.
    #[wasm_bindgen(constructor)]
    pub fn new(options: &JsValue) -> Self {
        fn parse_options(js: &JsValue) -> yrs::Options {
            let mut options = yrs::Options::default();
            options.offset_kind = yrs::OffsetKind::Utf16;
            if js.is_object() {
                if let Some(client_id) = js_sys::Reflect::get(js, &JsValue::from_str("clientID"))
                    .ok()
                    .and_then(|v| v.as_f64())
                {
                    options.client_id = client_id as u32 as ClientID;
                }

                if let Some(guid) = js_sys::Reflect::get(js, &JsValue::from_str("guid"))
                    .ok()
                    .and_then(|v| v.as_string())
                {
                    options.guid = guid.into();
                }

                if let Some(collection_id) =
                    js_sys::Reflect::get(js, &JsValue::from_str("collectionid"))
                        .ok()
                        .and_then(|v| v.as_string())
                {
                    options.collection_id = Some(collection_id);
                }

                if let Some(gc) = js_sys::Reflect::get(js, &JsValue::from_str("gc"))
                    .ok()
                    .and_then(|v| v.as_bool())
                {
                    options.skip_gc = !gc;
                }

                if let Some(auto_load) = js_sys::Reflect::get(js, &JsValue::from_str("autoLoad"))
                    .ok()
                    .and_then(|v| v.as_bool())
                {
                    options.auto_load = auto_load;
                }

                if let Some(should_load) =
                    js_sys::Reflect::get(js, &JsValue::from_str("shouldLoad"))
                        .ok()
                        .and_then(|v| v.as_bool())
                {
                    options.should_load = should_load;
                }
            }

            options
        }

        let options = parse_options(options);
        Doc::with_options(options).into()
    }

    /// Returns a parent document of this document or null if current document is not sub-document.
    #[wasm_bindgen(getter, js_name = parentDoc)]
    pub fn parent_doc(self) -> Option<YDoc> {
        let doc = self.0.parent_doc()?;
        Some(YDoc(doc))
    }

    /// Gets unique peer identifier of this `YDoc` instance.
    #[wasm_bindgen(getter)]
    pub fn id(self) -> f64 {
        self.deref().client_id() as f64
    }

    /// Gets globally unique identifier of this `YDoc` instance.
    #[wasm_bindgen(getter)]
    pub fn guid(self) -> String {
        self.deref().options().guid.to_string()
    }

    #[wasm_bindgen(getter, js_name = shouldLoad)]
    pub fn should_load(self) -> bool {
        self.deref().options().should_load
    }

    #[wasm_bindgen(getter, js_name = autoLoad)]
    pub fn auto_load(self) -> bool {
        self.deref().options().auto_load
    }

    /// Returns a new transaction for this document. Ywasm shared data types execute their
    /// operations in a context of a given transaction. Each document can have only one active
    /// transaction at the time - subsequent attempts will cause exception to be thrown.
    ///
    /// Transactions started with `doc.beginTransaction` can be released using `transaction.free`
    /// method.
    ///
    /// Example:
    ///
    /// ```javascript
    /// import YDoc from 'ywasm'
    ///
    /// // helper function used to simplify transaction
    /// // create/release cycle
    /// YDoc.prototype.transact = callback => {
    ///     const txn = this.writeTransaction()
    ///     try {
    ///         return callback(txn)
    ///     } finally {
    ///         txn.free()
    ///     }
    /// }
    ///
    /// const doc = new YDoc()
    /// const text = doc.getText('name')
    /// doc.transact(txn => text.insert(txn, 0, 'hello world'))
    /// ```
    #[wasm_bindgen(js_name = startTransaction)]
    pub fn start_transaction(self, origin: JsValue) -> Transaction {
        if origin.is_null() || origin.is_undefined() {
            Transaction::from(self.deref().transact_mut())
        } else {
            let abi = origin.into_abi();
            Transaction::from(self.deref().transact_mut_with(abi))
        }
    }

    /// Returns a `YText` shared data type, that's accessible for subsequent accesses using given
    /// `name`.
    ///
    /// If there was no instance with this name before, it will be created and then returned.
    ///
    /// If there was an instance with this name, but it was of different type, it will be projected
    /// onto `YText` instance.
    #[wasm_bindgen(js_name = getText)]
    pub fn get_text(self, name: &str) -> YText {
        self.deref().get_or_insert_text(name).into()
    }

    /// Returns a `YArray` shared data type, that's accessible for subsequent accesses using given
    /// `name`.
    ///
    /// If there was no instance with this name before, it will be created and then returned.
    ///
    /// If there was an instance with this name, but it was of different type, it will be projected
    /// onto `YArray` instance.
    #[wasm_bindgen(js_name = getArray)]
    pub fn get_array(self, name: &str) -> YArray {
        self.deref().get_or_insert_array(name).into()
    }

    /// Returns a `YMap` shared data type, that's accessible for subsequent accesses using given
    /// `name`.
    ///
    /// If there was no instance with this name before, it will be created and then returned.
    ///
    /// If there was an instance with this name, but it was of different type, it will be projected
    /// onto `YMap` instance.
    #[wasm_bindgen(js_name = getMap)]
    pub fn get_map(self, name: &str) -> YMap {
        self.deref().get_or_insert_map(name).into()
    }

    /// Returns a `YXmlFragment` shared data type, that's accessible for subsequent accesses using
    /// given `name`.
    ///
    /// If there was no instance with this name before, it will be created and then returned.
    ///
    /// If there was an instance with this name, but it was of different type, it will be projected
    /// onto `YXmlFragment` instance.
    #[wasm_bindgen(js_name = getXmlFragment)]
    pub fn get_xml_fragment(self, name: &str) -> YXmlFragment {
        self.deref().get_or_insert_xml_fragment(name).into()
    }

    /// Returns a `YXmlElement` shared data type, that's accessible for subsequent accesses using
    /// given `name`.
    ///
    /// If there was no instance with this name before, it will be created and then returned.
    ///
    /// If there was an instance with this name, but it was of different type, it will be projected
    /// onto `YXmlElement` instance.
    #[wasm_bindgen(js_name = getXmlElement)]
    pub fn get_xml_element(self, name: &str) -> YXmlElement {
        self.deref().get_or_insert_xml_element(name).into()
    }

    /// Returns a `YXmlText` shared data type, that's accessible for subsequent accesses using given
    /// `name`.
    ///
    /// If there was no instance with this name before, it will be created and then returned.
    ///
    /// If there was an instance with this name, but it was of different type, it will be projected
    /// onto `YXmlText` instance.
    #[wasm_bindgen(js_name = getXmlText)]
    pub fn get_xml_text(self, name: &str) -> YXmlText {
        self.deref().get_or_insert_xml_text(name).into()
    }

    /// Subscribes given function to be called any time, a remote update is being applied to this
    /// document. Function takes an `Uint8Array` as a parameter which contains a lib0 v1 encoded
    /// update.
    ///
    /// Returns an observer, which can be freed in order to unsubscribe this callback.
    #[wasm_bindgen(js_name = onUpdate)]
    pub fn on_update(self, f: js_sys::Function) -> YUpdateObserver {
        self.deref()
            .observe_update_v1(move |_, e| {
                let arg = Uint8Array::from(e.update.as_slice());
                f.call1(&JsValue::UNDEFINED, &arg).unwrap();
            })
            .unwrap()
            .into()
    }

    /// Subscribes given function to be called any time, a remote update is being applied to this
    /// document. Function takes an `Uint8Array` as a parameter which contains a lib0 v2 encoded
    /// update.
    ///
    /// Returns an observer, which can be freed in order to unsubscribe this callback.
    #[wasm_bindgen(js_name = onUpdateV2)]
    pub fn on_update_v2(self, f: js_sys::Function) -> YUpdateObserver {
        self.deref()
            .observe_update_v2(move |_, e| {
                let arg = Uint8Array::from(e.update.as_slice());
                f.call1(&JsValue::UNDEFINED, &arg).unwrap();
            })
            .unwrap()
            .into()
    }

    /// Subscribes given function to be called, whenever a transaction created by this document is
    /// being committed.
    ///
    /// Returns an observer, which can be freed in order to unsubscribe this callback.
    #[wasm_bindgen(js_name = onAfterTransaction)]
    pub fn on_after_transaction(self, f: js_sys::Function) -> YAfterTransactionObserver {
        self.deref()
            .observe_transaction_cleanup(move |_, e| {
                let arg: JsValue = YAfterTransactionEvent::new(e).into();
                f.call1(&JsValue::UNDEFINED, &arg).unwrap();
            })
            .unwrap()
            .into()
    }

    /// Subscribes given function to be called, whenever a subdocuments are being added, removed
    /// or loaded as children of a current document.
    ///
    /// Returns an observer, which can be freed in order to unsubscribe this callback.
    #[wasm_bindgen(js_name = onSubdocs)]
    pub fn on_subdocs(self, f: js_sys::Function) -> YSubdocsObserver {
        self.deref()
            .observe_subdocs(move |_, e| {
                let arg: JsValue = YSubdocsEvent::new(e).into();
                f.call1(&JsValue::UNDEFINED, &arg).unwrap();
            })
            .unwrap()
            .into()
    }

    /// Subscribes given function to be called, whenever current document is being destroyed.
    ///
    /// Returns an observer, which can be freed in order to unsubscribe this callback.
    #[wasm_bindgen(js_name = onDestroy)]
    pub fn on_destroy(self, f: js_sys::Function) -> YDestroyObserver {
        self.deref()
            .observe_destroy(move |_, e| {
                let arg: JsValue = YDoc::from(e.clone()).into_js();
                f.call1(&JsValue::UNDEFINED, &arg).unwrap();
            })
            .unwrap()
            .into()
    }

    /// Notify the parent document that you request to load data into this subdocument
    /// (if it is a subdocument).
    #[wasm_bindgen(js_name = load)]
    pub fn load(self, parent_txn: &mut Transaction) {
        self.0.load(parent_txn)
    }

    /// Emit `onDestroy` event and unregister all event handlers.
    #[wasm_bindgen(js_name = destroy)]
    pub fn destroy(mut self, parent_txn: &mut Transaction) {
        self.0.destroy(parent_txn)
    }

    /// Returns a list of sub-documents existings within the scope of this document.
    #[wasm_bindgen(js_name = getSubdocs)]
    pub fn subdocs(self, txn: &Transaction) -> js_sys::Array {
        let buf = js_sys::Array::new();
        for doc in txn.subdocs() {
            let doc = YDoc::from(doc.clone());
            buf.push(&doc.into_js());
        }
        buf
    }

    /// Returns a list of unique identifiers of the sub-documents existings within the scope of
    /// this document.
    #[wasm_bindgen(js_name = getSubdocGuids)]
    pub fn subdoc_guids(self, txn: &Transaction) -> js_sys::Set {
        let buf = js_sys::Set::new(&js_sys::Array::new());
        for uid in txn.subdoc_guids() {
            let str = uid.to_string();
            buf.add(&str.into());
        }
        buf
    }
}

#[wasm_bindgen]
pub struct YSubdocsEvent {
    added: JsValue,
    removed: JsValue,
    loaded: JsValue,
}

#[wasm_bindgen]
impl YSubdocsEvent {
    fn new(e: &SubdocsEvent) -> Self {
        fn to_array(iter: SubdocsEventIter) -> JsValue {
            let mut buf = js_sys::Array::new();
            let values = iter.map(|d| {
                let doc = YDoc::from(d.clone());
                let js = doc.into_js();
                js
            });
            buf.extend(values);
            buf.into()
        }

        let added = to_array(e.added());
        let removed = to_array(e.removed());
        let loaded = to_array(e.loaded());
        YSubdocsEvent {
            added,
            removed,
            loaded,
        }
    }

    #[wasm_bindgen(getter)]
    pub fn added(&self) -> JsValue {
        self.added.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn removed(&self) -> JsValue {
        self.removed.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn loaded(&self) -> JsValue {
        self.loaded.clone()
    }
}

#[wasm_bindgen]
pub struct YSubdocsObserver(SubdocsSubscription);

impl From<SubdocsSubscription> for YSubdocsObserver {
    fn from(o: SubdocsSubscription) -> Self {
        YSubdocsObserver(o)
    }
}

#[wasm_bindgen]
pub struct YDestroyObserver(DestroySubscription);

impl From<DestroySubscription> for YDestroyObserver {
    fn from(o: DestroySubscription) -> Self {
        YDestroyObserver(o)
    }
}

#[wasm_bindgen]
pub struct YAfterTransactionEvent {
    before_state: js_sys::Map,
    after_state: js_sys::Map,
    delete_set: js_sys::Map,
}

#[wasm_bindgen]
impl YAfterTransactionEvent {
    /// Returns a state vector - a map of entries (clientId, clock) - that represents logical
    /// time descriptor at the moment when transaction was originally created, prior to any changes
    /// made in scope of this transaction.
    #[wasm_bindgen(getter, js_name = beforeState)]
    pub fn before_state(self) -> js_sys::Map {
        self.before_state.clone()
    }

    /// Returns a state vector - a map of entries (clientId, clock) - that represents logical
    /// time descriptor at the moment when transaction was comitted.
    #[wasm_bindgen(getter, js_name = afterState)]
    pub fn after_state(self) -> js_sys::Map {
        self.after_state.clone()
    }

    /// Returns a delete set - a map of entries (clientId, (clock, len)[]) - that represents a range
    /// of all blocks deleted as part of current transaction.
    #[wasm_bindgen(getter, js_name = deleteSet)]
    pub fn delete_set(self) -> js_sys::Map {
        self.delete_set.clone()
    }

    fn new(e: &TransactionCleanupEvent) -> Self {
        YAfterTransactionEvent {
            before_state: e.before_state.clone().into_js(),
            after_state: e.after_state.clone().into_js(),
            delete_set: e.delete_set.clone().into_js(),
        }
    }
}

#[wasm_bindgen]
pub struct YAfterTransactionObserver(TransactionCleanupSubscription);

impl From<TransactionCleanupSubscription> for YAfterTransactionObserver {
    fn from(o: TransactionCleanupSubscription) -> Self {
        YAfterTransactionObserver(o)
    }
}

#[wasm_bindgen]
pub struct YUpdateObserver(UpdateSubscription);

impl From<UpdateSubscription> for YUpdateObserver {
    fn from(o: UpdateSubscription) -> Self {
        YUpdateObserver(o)
    }
}
