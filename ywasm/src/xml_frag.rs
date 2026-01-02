use crate::collection::SharedCollection;
use crate::js::convert::origin_into_js;
use crate::js::{Callback, Js, OptionDisposed, Shared};
use std::iter::FromIterator;
use wasm_bindgen::prelude::wasm_bindgen;
use wasm_bindgen::JsValue;
use yrs::types::xml::XmlEvent;
use yrs::types::TYPE_REFS_XML_FRAGMENT;
use yrs::{DeepObservable, GetString, Observable, XmlFragment as _, XmlFragmentRef};

/// Represents a list of `YXmlElement` and `YXmlText` types.
/// A `YXmlFragment` is similar to a `YXmlElement`, but it does not have a
/// nodeName, and it does not have attributes. Though it can be bound to a DOM
/// element - in this case the attributes and the nodeName are not shared
#[wasm_bindgen(js_name = "XmlFragment")]
pub struct WasmXmlFragment(pub(crate) SharedCollection<Vec<JsValue>, XmlFragmentRef>);

#[wasm_bindgen(js_class = "XmlFragment")]
impl WasmXmlFragment {
    #[wasm_bindgen(constructor)]
    pub fn new(children: Vec<JsValue>) -> crate::Result<WasmXmlFragment> {
        let mut nodes = Vec::with_capacity(children.len());
        for xml_node in children {
            Js::assert_xml_prelim(&xml_node)?;
            nodes.push(xml_node);
        }
        Ok(WasmXmlFragment(SharedCollection::prelim(nodes)))
    }

    #[wasm_bindgen(getter, js_name = type)]
    pub fn get_type(&self) -> u8 {
        TYPE_REFS_XML_FRAGMENT
    }

    /// Gets unique logical identifier of this type, shared across peers collaborating on the same
    /// document.
    #[wasm_bindgen(getter, js_name = id)]
    pub fn id(&self) -> crate::Result<JsValue> {
        self.0.id()
    }

    /// Returns true if this is a preliminary instance of `YXmlFragment`.
    ///
    /// Preliminary instances can be nested into other shared data types.
    /// Once a preliminary instance has been inserted this way, it becomes integrated into ywasm
    /// document store and cannot be nested again: attempt to do so will result in an exception.
    #[wasm_bindgen(getter)]
    pub fn prelim(&self) -> bool {
        self.0.is_prelim()
    }

    /// Checks if current shared type reference is alive and has not been deleted by its parent collection.
    /// This method only works on already integrated shared types and will return false is current
    /// type is preliminary (has not been integrated into document).
    #[wasm_bindgen(js_name = alive)]
    pub fn alive(&self) -> bool {
        self.0.is_alive()
    }

    /// Returns a number of child XML nodes stored within this `YXMlElement` instance.
    #[wasm_bindgen(js_name = length)]
    pub fn length(&self) -> crate::Result<u32> {
        match &self.0 {
            SharedCollection::Prelim(c) => Ok(c.len() as u32),
            SharedCollection::Integrated(c) => c.transact(|c, txn| Ok(c.len(txn))),
        }
    }

    #[wasm_bindgen(js_name = insert)]
    pub fn insert(&mut self, index: u32, xml_node: JsValue) -> crate::Result<()> {
        Js::assert_xml_prelim(&xml_node)?;
        match &mut self.0 {
            SharedCollection::Prelim(c) => {
                c.insert(index as usize, xml_node);
                Ok(())
            }
            SharedCollection::Integrated(c) => c.transact(|c, txn| {
                c.insert(txn, index, Js::new(xml_node));
                Ok(())
            }),
        }
    }

    #[wasm_bindgen(js_name = push)]
    pub fn push(&mut self, xml_node: JsValue) -> crate::Result<()> {
        Js::assert_xml_prelim(&xml_node)?;
        match &mut self.0 {
            SharedCollection::Prelim(c) => {
                c.push(xml_node);
                Ok(())
            }
            SharedCollection::Integrated(c) => c.transact(|c, txn| {
                c.push_back(txn, Js::new(xml_node));
                Ok(())
            }),
        }
    }

    #[wasm_bindgen(js_name = delete)]
    pub fn delete(&mut self, index: u32, length: Option<u32>) -> crate::Result<()> {
        let length = length.unwrap_or(1);
        match &mut self.0 {
            SharedCollection::Prelim(c) => {
                c.drain((index as usize)..((index + length) as usize));
                Ok(())
            }
            SharedCollection::Integrated(c) => c.transact(|c, txn| {
                c.remove_range(txn, index, length);
                Ok(())
            }),
        }
    }

    /// Returns a first child of this XML node.
    /// It can be either `YXmlElement`, `YXmlText` or `undefined` if current node has not children.
    #[wasm_bindgen(js_name = firstChild)]
    pub fn first_child(&self) -> crate::Result<JsValue> {
        match &self.0 {
            SharedCollection::Prelim(c) => Ok(c.first().cloned().unwrap_or(JsValue::UNDEFINED)),
            SharedCollection::Integrated(c) => {
                let doc = c.doc.clone();
                c.transact(|c, txn| match c.first_child() {
                    None => Ok(JsValue::UNDEFINED),
                    Some(xml) => Ok(Js::from_xml(xml, doc).into()),
                })
            }
        }
    }

    /// Returns a string representation of this XML node.
    #[wasm_bindgen(js_name = toString)]
    pub fn to_string(&self) -> crate::Result<String> {
        match &self.0 {
            SharedCollection::Prelim(c) => {
                let mut str = String::new();
                for js in c.iter() {
                    let res = match Shared::from_ref(js)? {
                        Shared::XmlText(c) => c.to_string(),
                        Shared::XmlElement(c) => c.to_string(),
                        Shared::XmlFragment(c) => c.to_string(),
                        _ => return Err(JsValue::from_str(crate::js::errors::NOT_XML_TYPE)),
                    };
                    str.push_str(&res?);
                }
                Ok(str)
            }
            SharedCollection::Integrated(c) => c.transact(|c, txn| Ok(c.get_string(txn))),
        }
    }

    /// Returns an iterator that enables a deep traversal of this XML node - starting from first
    /// child over this XML node successors using depth-first strategy.
    #[wasm_bindgen(js_name = treeWalker)]
    pub fn tree_walker(&self) -> crate::Result<js_sys::Array> {
        match &self.0 {
            SharedCollection::Prelim(_) => {
                Err(JsValue::from_str(crate::js::errors::INVALID_PRELIM_OP))
            }
            SharedCollection::Integrated(c) => {
                let doc = c.doc.clone();
                c.transact(|c, txn| {
                    let walker = c.successors(txn).map(|n| {
                        let js: JsValue = Js::from_xml(n, doc.clone()).into();
                        js
                    });
                    let array = js_sys::Array::from_iter(walker);
                    Ok(array.into())
                })
            }
        }
    }

    /// Subscribes to all operations happening over this instance of `YXmlFragment`. All changes are
    /// batched and eventually triggered during transaction commit phase.
    #[wasm_bindgen(js_name = observe)]
    pub fn observe(&self, callback: js_sys::Function) -> crate::Result<()> {
        match &self.0 {
            SharedCollection::Prelim(_) => {
                Err(JsValue::from_str(crate::js::errors::INVALID_PRELIM_OP))
            }
            SharedCollection::Integrated(c) => {
                let abi = callback.subscription_key();
                c.doc.transact(None, |tx| {
                    let target = c.hook.get(tx).ok_or_disposed()?;
                    let doc = c.doc.clone();
                    target.observe_with(abi, move |tx, e| {
                        let origin = origin_into_js(tx.origin());
                        let e = WasmXmlEvent::new(e, &doc, &origin);
                        callback.call1(&JsValue::UNDEFINED, &e.into()).unwrap();
                    });
                    Ok(())
                })
            }
        }
    }

    /// Unsubscribes a callback previously subscribed with `observe` method.
    #[wasm_bindgen(js_name = unobserve)]
    pub fn unobserve(&mut self, callback: js_sys::Function) -> crate::Result<bool> {
        match &self.0 {
            SharedCollection::Prelim(_) => {
                Err(JsValue::from_str(crate::js::errors::INVALID_PRELIM_OP))
            }
            SharedCollection::Integrated(c) => {
                let abi = callback.subscription_key();
                c.transact(|array, _| Ok(array.unobserve(abi)))
            }
        }
    }

    /// Subscribes to all operations happening over this Y shared type, as well as events in
    /// shared types stored within this one. All changes are batched and eventually triggered
    /// during transaction commit phase.
    #[wasm_bindgen(js_name = observeDeep)]
    pub fn observe_deep(&self, callback: js_sys::Function) -> crate::Result<()> {
        match &self.0 {
            SharedCollection::Prelim(_) => {
                Err(JsValue::from_str(crate::js::errors::INVALID_PRELIM_OP))
            }
            SharedCollection::Integrated(c) => {
                let abi = callback.subscription_key();
                c.doc.transact(None, |tx| {
                    let target = c.hook.get(tx).ok_or_disposed()?;
                    let doc = c.doc.clone();
                    let origin = origin_into_js(tx.origin());
                    target.observe_deep_with(abi, move |_, e| {
                        let e = crate::js::convert::events_into_js(e, &doc, &origin);
                        callback.call1(&JsValue::UNDEFINED, &e).unwrap();
                    });
                    Ok(())
                })
            }
        }
    }

    /// Unsubscribes a callback previously subscribed with `observeDeep` method.
    #[wasm_bindgen(js_name = unobserveDeep)]
    pub fn unobserve_deep(&mut self, callback: js_sys::Function) -> crate::Result<bool> {
        match &self.0 {
            SharedCollection::Prelim(_) => {
                Err(JsValue::from_str(crate::js::errors::INVALID_PRELIM_OP))
            }
            SharedCollection::Integrated(c) => {
                let abi = callback.subscription_key();
                c.transact(|array, _| Ok(array.unobserve_deep(abi)))
            }
        }
    }
}

/// Event generated by `YXmlElement.observe` method. Emitted during transaction commit phase.
#[wasm_bindgen(js_name = "XmlEvent")]
pub struct WasmXmlEvent {
    inner: &'static XmlEvent,
    doc: crate::WasmDoc,
    target: Option<JsValue>,
    keys: Option<JsValue>,
    delta: Option<JsValue>,
    origin: JsValue,
}

#[wasm_bindgen(js_class = "XmlEvent")]
impl WasmXmlEvent {
    pub(crate) fn new<'doc>(event: &XmlEvent, doc: &crate::WasmDoc, origin: &JsValue) -> Self {
        let inner: &'static XmlEvent = unsafe { std::mem::transmute(event) };
        WasmXmlEvent {
            inner,
            origin: origin.clone(),
            doc: doc.clone(),
            target: None,
            delta: None,
            keys: None,
        }
    }

    /// Returns an array of keys and indexes creating a path from root type down to current instance
    /// of shared type (accessible via `target` getter).
    #[wasm_bindgen]
    pub fn path(&self) -> JsValue {
        crate::js::convert::path_into_js(self.inner.path())
    }

    /// Returns a current shared type instance, that current event changes refer to.
    #[wasm_bindgen(getter)]
    pub fn target(&mut self) -> JsValue {
        let target = self.inner.target();
        let doc = self.doc.clone();
        let js = self
            .target
            .get_or_insert_with(|| Js::from_xml(target.clone(), doc).into());
        js.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn origin(&mut self) -> JsValue {
        self.origin.clone()
    }

    /// Returns a list of attribute changes made over corresponding `YXmlText` collection within
    /// bounds of current transaction. These changes follow a format:
    ///
    /// - { action: 'add'|'update'|'delete', oldValue: string|undefined, newValue: string|undefined }
    #[wasm_bindgen(getter)]
    pub fn keys(&mut self) -> crate::Result<JsValue> {
        if let Some(keys) = &self.keys {
            Ok(keys.clone())
        } else {
            let keys = self.inner.keys();
            let result = js_sys::Object::new();
            for (key, value) in keys.iter() {
                let key = JsValue::from(key.as_ref());
                let value = crate::js::convert::entry_change_into_js(value, self.doc.clone())?;
                js_sys::Reflect::set(&result, &key, &value)?;
            }
            let keys: JsValue = result.into();
            self.keys = Some(keys.clone());
            Ok(keys)
        }
    }

    /// Returns a list of XML child node changes made over corresponding `YXmlElement` collection
    /// within bounds of current transaction. These changes follow a format:
    ///
    /// - { insert: (YXmlText|YXmlElement)[] }
    /// - { delete: number }
    /// - { retain: number }
    #[wasm_bindgen(getter)]
    pub fn delta(&mut self) -> JsValue {
        if let Some(delta) = &self.delta {
            delta.clone()
        } else {
            let inner = &self.inner;
            let doc = self.doc.clone();
            let delta = self.delta.get_or_insert_with(|| {
                let delta = inner
                    .delta()
                    .into_iter()
                    .map(|change| crate::js::convert::change_into_js(change, &doc));
                let mut result = js_sys::Array::new();
                result.extend(delta);
                result.into()
            });
            delta.clone()
        }
    }
}
