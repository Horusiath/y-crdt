use crate::array::Array;
use crate::collection::SharedCollection;
use crate::js::{Callback, Js};
use crate::map::Map;
use crate::text::Text;
use crate::xml_frag::XmlFragment;
use crate::Result;
use serde::Deserialize;
use std::iter::FromIterator;
use std::rc::Rc;
use std::sync::Arc;
use wasm_bindgen::__rt::WasmRefCell;
use wasm_bindgen::prelude::wasm_bindgen;
use wasm_bindgen::JsValue;
use yrs::doc::{DocLike, SubDocHook};
use yrs::transaction::Transaction as YTransaction;
use yrs::types::TYPE_REFS_DOC;
use yrs::{DocId, JsonPath, JsonPathEval, MutProvider, OffsetKind, Options};

/// Internal state of a ywasm document, wrapped in Rc<WasmRefCell> for sharing.
pub struct DocState {
    pub(crate) doc: yrs::Doc,
    pub(crate) current_transaction: Option<crate::Transaction>,
    pub(crate) parent_doc: Option<Doc>,
}

/// A ywasm document type. Documents are most important units of collaborative resources management.
/// All shared collections live within a scope of their corresponding documents. All updates are
/// generated on per-document basis (rather than individual shared type). All operations on shared
/// collections happen via [Transaction], which lifetime is also bound to a document.
///
/// Document manages so-called root types, which are top-level shared types definitions (as opposed
/// to recursively nested types).
///
/// A basic workflow sample:
///
/// ```javascript
/// import YDoc from 'ywasm'
///
/// const doc = new YDoc()
/// const txn = doc.beginTransaction()
/// try {
///     const text = txn.getText('name')
///     text.push(txn, 'hello world')
///     const output = text.toString(txn)
///     console.log(output)
/// } finally {
///     txn.free()
/// }
/// ```
#[wasm_bindgen]
#[derive(Clone)]
pub struct Doc {
    pub(crate) state: Rc<WasmRefCell<DocState>>,
}

impl From<yrs::Doc> for Doc {
    fn from(doc: yrs::Doc) -> Self {
        Doc {
            state: Rc::new(WasmRefCell::new(DocState {
                doc,
                current_transaction: None,
                parent_doc: None,
            })),
        }
    }
}

impl Doc {
    pub(crate) fn current_origin(&self) -> JsValue {
        let state = self.state.borrow();
        match &state.current_transaction {
            Some(tx) => tx.origin(),
            None => JsValue::UNDEFINED,
        }
    }

    pub fn from_subdoc(subdoc: &SubDocHook, parent: Doc) -> Doc {
        let doc_ref = subdoc.borrow();
        let doc_ref: &dyn DocLike = &**doc_ref;
        let doc_ref: &dyn std::any::Any = doc_ref;
        let doc_ref: &Js = doc_ref.downcast_ref().unwrap();
        let doc = doc_ref.clone().into_doc();
        doc.state.borrow_mut().parent_doc = Some(parent);
        Doc {
            state: doc.state.clone(),
        }
    }

    pub(crate) fn transact<F, T>(&self, origin: JsValue, f: F) -> T
    where
        F: FnOnce(&mut YTransaction<Js>) -> T,
    {
        let mut state = self.state.borrow_mut();
        match &mut state.current_transaction {
            None => {
                let js = Js::new(JsValue::from(self.clone()));
                state.current_transaction = Some(crate::Transaction::new(js, origin));
                let tx = state.current_transaction.as_mut().unwrap();
                let result = f(tx);
                state.current_transaction = None;
                result
            }
            Some(tx) => f(tx),
        }
    }
}

#[wasm_bindgen]
impl Doc {
    /// Creates a new ywasm document. If `id` parameter was passed it will be used as this document
    /// globally unique identifier (it's up to caller to ensure that requirement). Otherwise, it will
    /// be assigned a randomly generated number.
    #[wasm_bindgen(constructor)]
    pub fn new(options: Option<JsValue>) -> Result<Doc> {
        use gloo_utils::format::JsValueSerdeExt;
        let js_options = match options {
            None => None,
            Some(options) => options
                .into_serde::<Option<DocOptions>>()
                .map_err(|_| JsValue::from_str("invalid document options"))?,
        };
        let mut options = Options::default();
        options.offset_kind = OffsetKind::Utf16;
        if let Some(o) = js_options {
            o.fill(&mut options);
        }
        let doc = yrs::Doc::with_options(options);
        Ok(Self::from(doc))
    }

    #[wasm_bindgen(getter, js_name = type)]
    pub fn get_type(&self) -> u8 {
        TYPE_REFS_DOC
    }

    /// Checks if a document is a preliminary type. It returns false, if current document
    /// is already a sub-document of another document.
    #[wasm_bindgen(getter)]
    pub fn prelim(&self) -> bool {
        self.state.borrow().parent_doc.is_none()
    }

    /// Returns a parent document of this document or null if current document is not sub-document.
    #[wasm_bindgen(getter, js_name = parentDoc)]
    pub fn parent_doc(&self) -> JsValue {
        match &self.state.borrow().parent_doc {
            None => JsValue::NULL,
            Some(parent) => JsValue::from(parent.clone()),
        }
    }

    /// Gets unique peer identifier of this `YDoc` instance.
    #[wasm_bindgen(getter)]
    pub fn id(&self) -> f64 {
        self.state.borrow().doc.client_id() as f64
    }

    /// Gets globally unique identifier of this `YDoc` instance.
    #[wasm_bindgen(getter)]
    pub fn guid(&self) -> String {
        self.state.borrow().doc.guid().to_string()
    }

    #[wasm_bindgen(getter, js_name = shouldLoad)]
    pub fn should_load(&self) -> bool {
        self.state.borrow().doc.should_load()
    }

    #[wasm_bindgen(getter, js_name = autoLoad)]
    pub fn auto_load(&self) -> bool {
        self.state.borrow().doc.auto_load()
    }

    /// Returns a `YText` shared data type, that's accessible for subsequent accesses using given
    /// `name`.
    ///
    /// If there was no instance with this name before, it will be created and then returned.
    ///
    /// If there was an instance with this name, but it was of different type, it will be projected
    /// onto `YText` instance.
    #[wasm_bindgen(js_name = getText)]
    pub fn get_text(&self, name: &str) -> Text {
        let doc = self.clone();
        self.transact(JsValue::UNDEFINED, |tx| {
            let shared_ref = tx.get_or_insert_text(name);
            Text(SharedCollection::integrated(shared_ref, doc.clone()))
        })
    }

    /// Returns a `YArray` shared data type, that's accessible for subsequent accesses using given
    /// `name`.
    ///
    /// If there was no instance with this name before, it will be created and then returned.
    ///
    /// If there was an instance with this name, but it was of different type, it will be projected
    /// onto `YArray` instance.
    #[wasm_bindgen(js_name = getArray)]
    pub fn get_array(&self, name: &str) -> Array {
        let doc = self.clone();
        self.transact(JsValue::UNDEFINED, |tx| {
            let shared_ref = tx.get_or_insert_array(name);
            Array(SharedCollection::integrated(shared_ref, doc.clone()))
        })
    }

    /// Returns a `YMap` shared data type, that's accessible for subsequent accesses using given
    /// `name`.
    ///
    /// If there was no instance with this name before, it will be created and then returned.
    ///
    /// If there was an instance with this name, but it was of different type, it will be projected
    /// onto `YMap` instance.
    #[wasm_bindgen(js_name = getMap)]
    pub fn get_map(&self, name: &str) -> Map {
        let doc = self.clone();
        self.transact(JsValue::UNDEFINED, |tx| {
            let shared_ref = tx.get_or_insert_map(name);
            Map(SharedCollection::integrated(shared_ref, doc.clone()))
        })
    }

    /// Returns a `YXmlFragment` shared data type, that's accessible for subsequent accesses using
    /// given `name`.
    ///
    /// If there was no instance with this name before, it will be created and then returned.
    ///
    /// If there was an instance with this name, but it was of different type, it will be projected
    /// onto `YXmlFragment` instance.
    #[wasm_bindgen(js_name = getXmlFragment)]
    pub fn get_xml_fragment(&self, name: &str) -> XmlFragment {
        let doc = self.clone();
        self.transact(JsValue::UNDEFINED, |tx| {
            let shared_ref = tx.get_or_insert_xml_fragment(name);
            XmlFragment(SharedCollection::integrated(shared_ref, doc.clone()))
        })
    }

    #[wasm_bindgen(js_name = on)]
    pub fn on(&mut self, event: &str, callback: js_sys::Function) -> Result<()> {
        let abi = callback.subscription_key();
        let mut state = self.state.borrow_mut();
        match event {
            "update" => state.doc.observe_update_v1_with(abi, move |txn, e| {
                let update = js_sys::Uint8Array::from(e.update.as_slice());
                callback.call1(&JsValue::UNDEFINED, &update).unwrap();
            }),
            "updateV2" => state.doc.observe_update_v2_with(abi, move |txn, e| {
                let update = js_sys::Uint8Array::from(e.update.as_slice());
                callback.call1(&JsValue::UNDEFINED, &update).unwrap();
            }),
            "subdocs" => {
                let doc = self.clone();
                state.doc.observe_subdocs_with(abi, move |e| {
                    let event: JsValue = YSubdocsEvent::new(e, &doc).into();
                    callback.call1(&JsValue::UNDEFINED, &event).unwrap();
                })
            }
            "destroy" => state.doc.observe_destroy_with(abi, move |_| {
                callback.call0(&JsValue::UNDEFINED).unwrap();
            }),
            "afterTransaction" => state.doc.observe_after_transaction_with(abi, move |txn| {
                callback.call0(&JsValue::UNDEFINED).unwrap();
            }),
            "cleanup" => state
                .doc
                .observe_transaction_cleanup_with(abi, move |txn, _| {
                    callback.call0(&JsValue::UNDEFINED).unwrap();
                }),
            other => {
                return Err(JsValue::from_str(&format!("unknown event: '{}'", other)).into());
            }
        };
        Ok(())
    }

    #[wasm_bindgen(js_name = off)]
    pub fn off(&mut self, event: &str, callback: js_sys::Function) -> Result<bool> {
        let abi = callback.subscription_key();
        let mut state = self.state.borrow_mut();
        let unsubscribed = match event {
            "update" => state.doc.unobserve_update_v1(abi),
            "updateV2" => state.doc.unobserve_update_v2(abi),
            "subdocs" => state.doc.unobserve_subdocs(abi),
            "destroy" => state.doc.unobserve_destroy(abi),
            "afterTransaction" => state.doc.unobserve_after_transaction(abi),
            "cleanup" => state.doc.unobserve_transaction_cleanup(abi),
            other => {
                return Err(JsValue::from_str(&format!("unknown event: '{}'", other)).into());
            }
        };
        Ok(unsubscribed)
    }

    /// Notify the parent document that you request to load data into this subdocument
    /// (if it is a subdocument).
    #[wasm_bindgen(js_name = load)]
    pub fn load(&self) -> Result<()> {
        let parent_doc = self.state.borrow().parent_doc.clone();
        match parent_doc {
            Some(parent_doc) => parent_doc.transact(JsValue::UNDEFINED, |parent_txn| {
                let parent_scope = parent_txn.subdoc_scope();
                let mut state = self.state.borrow_mut();
                let mut child_doc = state.doc.get_mut();
                child_doc.load(parent_scope);
                Ok(())
            }),
            None => Err(JsValue::from_str("not a subdocument").into()),
        }
    }

    /// Emit `onDestroy` event and unregister all event handlers.
    #[wasm_bindgen(js_name = destroy)]
    pub fn destroy(&self) -> Result<()> {
        let parent_doc = self.state.borrow().parent_doc.clone();
        match parent_doc {
            Some(parent_doc) => parent_doc.transact(JsValue::UNDEFINED, |parent_txn| {
                let parent_scope = parent_txn.subdoc_scope();
                let mut state = self.state.borrow_mut();
                let mut child_doc = state.doc.get_mut();
                child_doc.destroy(parent_scope);
                Ok(())
            }),
            None => Err(JsValue::from_str("not a subdocument").into()),
        }
    }

    /// Returns a list of sub-documents existings within the scope of this document.
    #[wasm_bindgen(js_name = getSubdocs)]
    pub fn subdocs(&self) -> Result<js_sys::Array> {
        let res = js_sys::Array::new();
        self.transact(JsValue::UNDEFINED, |txn| {
            for subdoc in txn.subdoc_refs() {
                todo!()
            }
        });
        Ok(res)
    }

    /// Returns a list of unique identifiers of the sub-documents existings within the scope of
    /// this document.
    #[wasm_bindgen(js_name = getSubdocGuids)]
    pub fn subdoc_guids(&self) -> js_sys::Set {
        let set = js_sys::Array::new();
        for guid in self.state.borrow().doc.subdoc_guids() {
            set.push(&JsValue::from_str(&guid.to_string()));
        }
        js_sys::Set::new(&set)
    }

    /// Returns a list of all root-level replicated collections, together with their types.
    /// These collections can then be accessed via `getMap`/`getText` etc. methods.
    ///
    /// Example:
    /// ```js
    /// import * as Y from 'ywasm'
    ///
    /// const doc = new Y.YDoc()
    /// const ymap = doc.getMap('a')
    /// const yarray = doc.getArray('b')
    /// const ytext = doc.getText('c')
    /// const yxml = doc.getXmlFragment('d')
    ///
    /// const roots = doc.roots() // [['a',ymap], ['b',yarray], ['c',ytext], ['d',yxml]]
    /// ```
    #[wasm_bindgen(js_name = roots)]
    pub fn roots(&self) -> js_sys::Map {
        let doc = self.clone();
        let result = js_sys::Map::new();
        for (key, value) in self.state.borrow().doc.root_refs() {
            let value = Js::from_value(&value, doc.clone());
            result.set(&JsValue::from_str(&key), &value);
        }
        result
    }

    /// Evaluates a JSON path expression (see: https://en.wikipedia.org/wiki/JSONPath) on
    /// the document and returns an array of values matching that query.
    ///
    /// Currently, this method supports the following syntax:
    /// - `$` - root object
    /// - `@` - current object
    /// - `.field` or `['field']` - member accessor
    /// - `[1]` - array index (also supports negative indices)
    /// - `.*` or `[*]` - wildcard (matches all members of an object or array)
    /// - `..` - recursive descent (matches all descendants not only direct children)
    /// - `[start:end:step]` - array slice operator (requires positive integer arguments)
    /// - `['a', 'b', 'c']` - union operator (returns an array of values for each query)
    /// - `[1, -1, 3]` - multiple indices operator (returns an array of values for each index)
    ///
    /// At the moment, JSON Path does not support filter predicates.
    #[wasm_bindgen(js_name = selectAll)]
    pub fn select_all(&self, json_path: &str) -> Result<js_sys::Array> {
        let jpath = JsonPath::parse(json_path).map_err(|e| JsValue::from_str(&e.to_string()))?;
        let doc = self.clone();
        let result: Vec<_> =
            self.transact(JsValue::UNDEFINED, |txn| txn.json_path(&jpath).collect());
        let array = js_sys::Array::new();
        for res in result {
            array.push(&Js::from_value(&res, doc.clone()).into());
        }
        Ok(array)
    }

    /// Evaluates a JSON path expression (see: https://en.wikipedia.org/wiki/JSONPath) on
    /// the document and returns first value matching that query.
    ///
    /// Currently, this method supports the following syntax:
    /// - `$` - root object
    /// - `@` - current object
    /// - `.field` or `['field']` - member accessor
    /// - `[1]` - array index (also supports negative indices)
    /// - `.*` or `[*]` - wildcard (matches all members of an object or array)
    /// - `..` - recursive descent (matches all descendants not only direct children)
    /// - `[start:end:step]` - array slice operator (requires positive integer arguments)
    /// - `['a', 'b', 'c']` - union operator (returns an array of values for each query)
    /// - `[1, -1, 3]` - multiple indices operator (returns an array of values for each index)
    ///
    /// At the moment, JSON Path does not support filter predicates.
    #[wasm_bindgen(js_name = selectOne)]
    pub fn select_one(&self, json_path: &str) -> Result<JsValue> {
        let jpath = JsonPath::parse(json_path).map_err(|e| JsValue::from_str(&e.to_string()))?;
        let doc = self.clone();
        let result = self.transact(JsValue::UNDEFINED, |txn| txn.json_path(&jpath).next());
        match result {
            Some(value) => Ok(Js::from_value(&value, doc).into()),
            None => Ok(JsValue::UNDEFINED),
        }
    }
}

#[wasm_bindgen]
pub struct YSubdocsEvent {
    added: js_sys::Array,
    removed: js_sys::Array,
    loaded: js_sys::Array,
}

#[wasm_bindgen]
impl YSubdocsEvent {
    fn new(e: &yrs::SubdocsEvent, parent_doc: &Doc) -> Self {
        let added = js_sys::Array::from_iter(
            e.added()
                .into_iter()
                .map(|subdoc| JsValue::from(crate::Doc::from_subdoc(subdoc, parent_doc.clone()))),
        );
        let removed = js_sys::Array::from_iter(
            e.removed()
                .into_iter()
                .map(|subdoc| JsValue::from(crate::Doc::from_subdoc(subdoc, parent_doc.clone()))),
        );
        let loaded = js_sys::Array::from_iter(
            e.loaded()
                .into_iter()
                .map(|subdoc| JsValue::from(crate::Doc::from_subdoc(subdoc, parent_doc.clone()))),
        );
        YSubdocsEvent {
            added,
            removed,
            loaded,
        }
    }

    #[wasm_bindgen(getter)]
    pub fn added(&self) -> js_sys::Array {
        self.added.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn removed(&self) -> js_sys::Array {
        self.removed.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn loaded(&self) -> js_sys::Array {
        self.loaded.clone()
    }
}

#[derive(Deserialize)]
pub struct DocOptions {
    #[serde(alias = "clientID", default)]
    pub client_id: Option<u64>,

    #[serde(alias = "guid", default)]
    pub guid: Option<String>,

    #[serde(alias = "collectionid", default)]
    pub collection_id: Option<String>,

    #[serde(alias = "gc", default)]
    pub gc: Option<bool>,

    #[serde(alias = "autoLoad", default)]
    pub auto_load: Option<bool>,

    #[serde(alias = "shouldLoad", default)]
    pub should_load: Option<bool>,
}

impl DocOptions {
    fn fill(self, options: &mut Options) {
        if let Some(value) = self.client_id {
            options.client_id = value;
        }
        if let Some(value) = self.guid {
            options.guid = DocId::from(Arc::from(value));
        }
        if let Some(value) = self.collection_id {
            options.collection_id = Some(value.into());
        }
        if let Some(value) = self.gc {
            options.skip_gc = !value;
        }
        if let Some(value) = self.auto_load {
            options.auto_load = value;
        }
        if let Some(value) = self.should_load {
            options.should_load = value;
        }
    }
}
