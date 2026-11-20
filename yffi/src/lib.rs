#![allow(unsafe_op_in_unsafe_fn)]

use serde::Serialize;
use serde::de::DeserializeOwned;
use std::ffi::{CStr, CString, c_char, c_void};
use std::fmt::Write;
use std::ops::Bound;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::{null, null_mut};
use std::sync::Arc;
use yrs::lib0::{Decode, Encode};
use yrs::node::{Attrs, NodePtr, PathSegment};
use yrs::{
    Any, Assoc, AttrOp, Delta, DeltaOptions, In, JsonPath, JsonPathEval, LinkSource, NodeID,
    NodeRef, Op, Options, Origin, Snapshot, StateVector, StickyIndex, Update,
};

/// A Yrs document. All nodes live within a document and are read and written via transactions.
pub type Doc = yrs::Doc;

/// A collaborative node. Owned by its document.
pub type Node = yrs::node::Node;

/// Error code: couldn't read data from input stream.
pub const ERR_CODE_IO: u8 = 1;

/// Error code: decoded variable integer outside of the expected integer size bounds.
pub const ERR_CODE_VAR_INT: u8 = 2;

/// Error code: end of stream found when more data was expected.
pub const ERR_CODE_EOS: u8 = 3;

/// Error code: decoded enum tag value was not among known cases.
pub const ERR_CODE_UNEXPECTED_VALUE: u8 = 4;

/// Error code: failure when trying to decode JSON content.
pub const ERR_CODE_INVALID_JSON: u8 = 5;

/// Error code: other error type than the one specified.
pub const ERR_CODE_OTHER: u8 = 6;

/// Error code: not enough memory to perform an operation.
pub const ERR_NOT_ENOUGH_MEMORY: u8 = 7;

/// Error code: conversion attempt to specific Rust type was not possible.
pub const ERR_TYPE_MISMATCH: u8 = 8;

/// Error code: miscellaneous error coming from serde, not covered by other error codes.
pub const ERR_CUSTOM: u8 = 9;

/// Error code: malformed update.
pub const ERR_MALFORMED: u8 = 10;

/// Error code: update block assigned to parent that is not a valid shared ref of deleted block.
pub const ERR_INVALID_PARENT: u8 = 9;

/// Error code: `YDelta` couldn't be converted (invalid tag, JSON or UTF-8 string).
pub const ERR_DELTA_CONVERSION: u8 = 11;

/// Error code: delta was converted, but applying it onto a node failed.
pub const ERR_DELTA_APPLY: u8 = 12;

/// Error code: operation requires a read-write transaction.
pub const ERR_READ_ONLY: u8 = 13;

fn lib0_err(e: yrs::lib0::Error) -> u8 {
    use yrs::lib0::Error as E;
    match e {
        E::InvalidVarInt | E::IntOutOfBounds => ERR_CODE_VAR_INT,
        E::EndOfBuffer(_) => ERR_CODE_EOS,
        E::UnexpectedValue | E::InvalidKey => ERR_CODE_UNEXPECTED_VALUE,
        E::InvalidJSON(_) => ERR_CODE_INVALID_JSON,
        E::NotEnoughMemory(_) => ERR_NOT_ENOUGH_MEMORY,
        E::TypeMismatch(_) => ERR_TYPE_MISMATCH,
        E::Custom(_) => ERR_CUSTOM,
        E::Malformed(_) => ERR_MALFORMED,
    }
}

fn yrs_err(e: yrs::Error) -> u8 {
    match e {
        yrs::Error::Lib0(e) => lib0_err(e),
        yrs::Error::InvalidParent { .. } => ERR_INVALID_PARENT,
        _ => ERR_CODE_OTHER,
    }
}

/// Transaction is a scope in which a document can be read or written.
pub struct Transaction(TxnInner);

enum TxnInner {
    Read(yrs::Transaction<&'static Doc>),
    Write(yrs::Transaction<&'static mut Doc>),
    /// Read-only transaction borrowed for the duration of a callback.
    Ref(&'static yrs::Transaction<&'static Doc>),
    /// Read-write transaction borrowed for the duration of a callback.
    Mut(&'static mut yrs::Transaction<&'static mut Doc>),
}

impl Transaction {
    fn read(&self) -> &yrs::Transaction<&'static Doc> {
        match &self.0 {
            TxnInner::Read(txn) => txn,
            TxnInner::Write(txn) => txn.as_readonly(),
            TxnInner::Ref(txn) => txn,
            TxnInner::Mut(txn) => txn.as_readonly(),
        }
    }

    fn write(&mut self) -> Option<&mut yrs::Transaction<&'static mut Doc>> {
        match &mut self.0 {
            TxnInner::Write(txn) => Some(txn),
            TxnInner::Mut(txn) => Some(txn),
            _ => None,
        }
    }

    /// Wraps a transaction borrowed by a callback.
    unsafe fn borrowed(txn: &yrs::Transaction<&Doc>) -> Self {
        Transaction(TxnInner::Ref(std::mem::transmute(txn)))
    }
}

/// Byte string. Inputs are owned by the caller; outputs are released with `ybinary_destroy`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct YBinary {
    pub ptr: *mut u8,
    pub len: u32,
}

impl YBinary {
    fn empty() -> Self {
        YBinary {
            ptr: null_mut(),
            len: 0,
        }
    }

    fn borrowed(bytes: &[u8]) -> Self {
        YBinary {
            len: bytes.len() as u32,
            ptr: bytes.as_ptr() as *mut u8,
        }
    }

    unsafe fn as_slice<'a>(&self) -> &'a [u8] {
        slice(self.ptr, self.len)
    }
}

impl From<Vec<u8>> for YBinary {
    fn from(bytes: Vec<u8>) -> Self {
        if bytes.is_empty() {
            return YBinary::empty();
        }
        let len = bytes.len() as u32;
        let ptr = Box::into_raw(bytes.into_boxed_slice()) as *mut u8;
        YBinary { len, ptr }
    }
}

unsafe fn slice<'a, T>(ptr: *const T, len: u32) -> &'a [T] {
    if ptr.is_null() || len == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(ptr, len as usize)
    }
}

unsafe fn to_str<'a>(str: *const c_char) -> Option<&'a str> {
    if str.is_null() {
        None
    } else {
        CStr::from_ptr(str).to_str().ok()
    }
}

unsafe fn parse<T: DeserializeOwned>(json: *const c_char) -> Option<T> {
    serde_json::from_str(to_str(json)?).ok()
}

unsafe fn origin(key: YBinary) -> Origin {
    Origin::from(key.as_slice())
}

fn into_c_str(str: impl Into<Vec<u8>>) -> *mut c_char {
    CString::new(str).map_or(null_mut(), CString::into_raw)
}

fn to_json<T: Serialize + ?Sized>(value: &T) -> *mut c_char {
    serde_json::to_vec(value).map_or(null_mut(), into_c_str)
}

/// Serializes an iterator as JSON array without collecting it first.
fn json_seq<I>(iter: I) -> *mut c_char
where
    I: IntoIterator,
    I::Item: Serialize,
{
    use serde::Serializer;
    let mut buf = Vec::new();
    match serde_json::Serializer::new(&mut buf).collect_seq(iter) {
        Ok(()) => into_c_str(buf),
        Err(_) => null_mut(),
    }
}

fn raw_node(node: &Node) -> *mut Node {
    node as *const Node as *mut Node
}

unsafe fn node_ptr(node: *const Node) -> NodePtr {
    NodePtr::from(&*node)
}

unsafe fn read_node<'a>(
    node: *const Node,
    txn: *const Transaction,
) -> NodeRef<&'a yrs::Transaction<&'static Doc>> {
    NodeRef::new(node_ptr(node), (*txn).read())
}

fn assoc(assoc: i8) -> Assoc {
    if assoc >= 0 {
        Assoc::After
    } else {
        Assoc::Before
    }
}

/// Releases a string returned by yffi.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ystring_destroy(str: *mut c_char) {
    if !str.is_null() {
        drop(CString::from_raw(str));
    }
}

/// Releases a binary returned by yffi.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ybinary_destroy(binary: YBinary) {
    if !binary.ptr.is_null() {
        let slice = std::ptr::slice_from_raw_parts_mut(binary.ptr, binary.len as usize);
        drop(Box::from_raw(slice));
    }
}

/// Creates a new document from JSON options (`NULL` = defaults). Returns `NULL` on invalid JSON.
/// Release it with `ydoc_destroy`.
///
/// Options shape (every field is optional):
/// ```json
/// {
///   "client_id": 1,
///   "guid": "unique document id",
///   "collection_id": "provider collection",
///   "offset_kind": "Bytes",
///   "skip_gc": false,
///   "auto_load": false,
///   "should_load": true,
///   "cleanup_formatting": true
/// }
/// ```
/// `client_id` defaults to a random value and `guid` to a random UUID v4. `collection_id`
/// defaults to `null`. `offset_kind` is `"Bytes"` (UTF-8 byte offsets, default) or `"Utf16"`.
/// `skip_gc` keeps deleted content (required by snapshots). `auto_load` and `should_load` control
/// subdocument loading. `cleanup_formatting` removes redundant formatting blocks on commit.
/// `cleanup_formatting` defaults to `true`. `ydoc_options_json` returns the same shape.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ydoc(options: *const c_char) -> *mut Doc {
    let options = if options.is_null() {
        Options::default()
    } else {
        match parse::<Options>(options) {
            Some(options) => options,
            None => return null_mut(),
        }
    };
    Box::into_raw(Box::new(Doc::with_options(options)))
}

/// Releases a document. Must not be called on subdocuments borrowed from `ytransaction_subdoc`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ydoc_destroy(doc: *mut Doc) {
    if !doc.is_null() {
        drop(Box::from_raw(doc));
    }
}

/// Returns document options as JSON.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ydoc_options_json(doc: *const Doc) -> *mut c_char {
    to_json((*doc).options())
}

/// Starts a read-only transaction. Release it with `ytransaction_commit`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ydoc_read_transaction(doc: *mut Doc) -> *mut Transaction {
    let doc: &'static Doc = &*doc;
    Box::into_raw(Box::new(Transaction(TxnInner::Read(doc.transact()))))
}

/// Starts a read-write transaction with an optional `origin` (`len == 0` = none).
/// Commit and release it with `ytransaction_commit`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ydoc_write_transaction(
    doc: *mut Doc,
    origin: YBinary,
) -> *mut Transaction {
    let doc: &'static mut Doc = &mut *doc;
    let txn = if origin.len == 0 {
        doc.transact_mut()
    } else {
        doc.transact_mut_with(origin.as_slice())
    };
    Box::into_raw(Box::new(Transaction(TxnInner::Write(txn))))
}

/// Requests a subdocument to be loaded. No-op if `parent_txn` is read-only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ydoc_load(doc: *mut Doc, parent_txn: *mut Transaction) {
    if let Some(txn) = (*parent_txn).write() {
        (*doc).load(txn);
    }
}

/// Destroys a document: emits destroy event and clears all callbacks. `parent_txn` (read-write) is
/// required for subdocuments and may be `NULL` otherwise.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ydoc_clear(doc: *mut Doc, parent_txn: *mut Transaction) {
    let txn = parent_txn.as_mut().and_then(Transaction::write);
    (*doc).destroy(txn);
}

/// Subscribes `cb` under `key` to lib0 v1 updates. The update is valid only inside the callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ydoc_observe_updates_v1(
    doc: *mut Doc,
    key: YBinary,
    state: *mut c_void,
    cb: extern "C" fn(*mut c_void, YBinary),
) {
    (*doc).observe_update_v1(origin(key), move |_, e| {
        cb(state, YBinary::borrowed(&e.update))
    });
}

/// Unsubscribes a callback registered by `ydoc_observe_updates_v1`. Returns 1 if removed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ydoc_unobserve_updates_v1(doc: *mut Doc, key: YBinary) -> u8 {
    (*doc).unobserve_update_v1(origin(key)) as u8
}

/// Subscribes `cb` under `key` to lib0 v2 updates. The update is valid only inside the callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ydoc_observe_updates_v2(
    doc: *mut Doc,
    key: YBinary,
    state: *mut c_void,
    cb: extern "C" fn(*mut c_void, YBinary),
) {
    (*doc).observe_update_v2(origin(key), move |_, e| {
        cb(state, YBinary::borrowed(&e.update))
    });
}

/// Unsubscribes a callback registered by `ydoc_observe_updates_v2`. Returns 1 if removed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ydoc_unobserve_updates_v2(doc: *mut Doc, key: YBinary) -> u8 {
    (*doc).unobserve_update_v2(origin(key)) as u8
}

/// Subscribes `cb` under `key` to committed transactions. The transaction is read-write and valid
/// only inside the callback: it must not be committed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ydoc_observe_after_transaction(
    doc: *mut Doc,
    key: YBinary,
    state: *mut c_void,
    cb: extern "C" fn(*mut c_void, *mut Transaction),
) {
    (*doc).observe_after_transaction(origin(key), move |txn| {
        let mut txn = Transaction(TxnInner::Mut(std::mem::transmute(txn)));
        cb(state, &mut txn)
    });
}

/// Unsubscribes a callback registered by `ydoc_observe_after_transaction`. Returns 1 if removed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ydoc_unobserve_after_transaction(doc: *mut Doc, key: YBinary) -> u8 {
    (*doc).unobserve_after_transaction(origin(key)) as u8
}

/// Subscribes `cb` under `key` to subdocument changes, passed as JSON
/// `{"added":[guid],"removed":[guid],"loaded":[guid]}` valid only inside the callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ydoc_observe_subdocs(
    doc: *mut Doc,
    key: YBinary,
    state: *mut c_void,
    cb: extern "C" fn(*mut c_void, *const c_char),
) {
    (*doc).observe_subdocs(origin(key), move |_, e| {
        if let Ok(json) = serde_json::to_vec(e).map(CString::new) {
            let json = json.unwrap();
            cb(state, json.as_ptr())
        }
    });
}

/// Unsubscribes a callback registered by `ydoc_observe_subdocs`. Returns 1 if removed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ydoc_unobserve_subdocs(doc: *mut Doc, key: YBinary) -> u8 {
    (*doc).unobserve_subdocs(origin(key)) as u8
}

/// Subscribes `cb` under `key` to document destruction (see `ydoc_clear`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ydoc_observe_destroy(
    doc: *mut Doc,
    key: YBinary,
    state: *mut c_void,
    cb: extern "C" fn(*mut c_void, *mut Doc),
) {
    (*doc).observe_destroy(origin(key), move |_, doc| {
        cb(state, doc as *const Doc as *mut Doc)
    });
}

/// Unsubscribes a callback registered by `ydoc_observe_destroy`. Returns 1 if removed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ydoc_unobserve_destroy(doc: *mut Doc, key: YBinary) -> u8 {
    (*doc).unobserve_destroy(origin(key)) as u8
}

/// Commits (if read-write) and releases a transaction.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ytransaction_commit(txn: *mut Transaction) {
    drop(Box::from_raw(txn)); // committed on drop
}

/// Returns 1 if transaction is read-write, 0 otherwise.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ytransaction_writeable(txn: *mut Transaction) -> u8 {
    (*txn).write().is_some() as u8
}

/// Garbage collects all deleted blocks, even with `skip_gc` set. No-op on read-only transactions.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ytransaction_gc(txn: *mut Transaction) {
    if let Some(txn) = (*txn).write() {
        txn.gc(None);
    }
}

/// Returns a node by its JSON id: root name (`"\"text\""`) or `{"client":n,"clock":n}`.
/// Read-write transactions create missing root nodes. Returns `NULL` if missing or deleted.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ynode(txn: *mut Transaction, id: *const c_char) -> *mut Node {
    let Some(id) = parse::<NodeID>(id) else {
        return null_mut();
    };
    let txn = &mut *txn;
    let node = match txn.write() {
        Some(txn) => txn.node_mut(id).map(|node| raw_node(node.as_ref())),
        None => txn.read().node(id).map(|node| raw_node(node.as_ref())),
    };
    node.unwrap_or(null_mut())
}

/// Returns JSON array of root node names.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ytransaction_root_refs(txn: *const Transaction) -> *mut c_char {
    json_seq((*txn).read().root_refs().map(|(name, _)| name))
}

/// Returns a subdocument by its guid or `NULL`. It's owned by its parent: don't destroy it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ytransaction_subdoc(
    txn: *const Transaction,
    guid: *const c_char,
) -> *mut Doc {
    let Some(guid) = to_str(guid) else {
        return null_mut();
    };
    match (*txn).read().subdoc(&Arc::from(guid)) {
        Some(doc) => doc as *const Doc as *mut Doc,
        None => null_mut(),
    }
}

/// Returns JSON array of subdocument guids.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ytransaction_subdocs(txn: *const Transaction) -> *mut c_char {
    json_seq((*txn).read().subdoc_guids())
}

/// Returns lib0 v1 encoded state vector of the document.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ytransaction_state_vector_v1(txn: *const Transaction) -> YBinary {
    (*txn).read().state_vector().encode_v1().into()
}

unsafe fn state_vector(sv: YBinary) -> Option<StateVector> {
    if sv.len == 0 {
        Some(StateVector::default())
    } else {
        StateVector::decode_v1(sv.as_slice()).ok()
    }
}

/// Returns lib0 v1 update with changes missing from state vector `sv` (`len == 0` = all).
/// Returns empty binary if `sv` is malformed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ytransaction_state_diff_v1(
    txn: *const Transaction,
    sv: YBinary,
) -> YBinary {
    match state_vector(sv) {
        Some(sv) => (*txn).read().encode_diff_v1(&sv).into(),
        None => YBinary::empty(),
    }
}

/// Returns lib0 v2 update with changes missing from state vector `sv` (`len == 0` = all).
/// Returns empty binary if `sv` is malformed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ytransaction_state_diff_v2(
    txn: *const Transaction,
    sv: YBinary,
) -> YBinary {
    match state_vector(sv) {
        Some(sv) => (*txn).read().encode_diff_v2(&sv).into(),
        None => YBinary::empty(),
    }
}

unsafe fn apply(txn: *mut Transaction, update: Result<Update, yrs::lib0::Error>) -> u8 {
    let Some(txn) = (*txn).write() else {
        return ERR_READ_ONLY;
    };
    match update {
        Ok(update) => match txn.apply_update(update) {
            Ok(()) => 0,
            Err(e) => yrs_err(*e),
        },
        Err(e) => lib0_err(e),
    }
}

/// Applies lib0 v1 update. Returns 0 on success or an error code.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ytransaction_apply(txn: *mut Transaction, update: YBinary) -> u8 {
    apply(txn, Update::decode_v1(update.as_slice()))
}

/// Applies lib0 v2 update. Returns 0 on success or an error code.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ytransaction_apply_v2(txn: *mut Transaction, update: YBinary) -> u8 {
    apply(txn, Update::decode_v2(update.as_slice()))
}

/// Returns lib0 v1 encoded snapshot of the document.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ytransaction_snapshot(txn: *const Transaction) -> YBinary {
    (*txn).read().snapshot().encode_v1().into()
}

unsafe fn state_from_snapshot<E: yrs::lib0::Encoder>(
    txn: *const Transaction,
    snapshot: YBinary,
    mut encoder: E,
) -> YBinary {
    let Ok(snapshot) = Snapshot::decode_v1(snapshot.as_slice()) else {
        return YBinary::empty();
    };
    match (*txn)
        .read()
        .encode_state_from_snapshot(&snapshot, &mut encoder)
    {
        Ok(()) => encoder.to_vec().into(),
        Err(_) => YBinary::empty(),
    }
}

/// Encodes document state at `snapshot` as lib0 v1 update. Requires `skip_gc`.
/// Returns empty binary on failure.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ytransaction_encode_state_from_snapshot_v1(
    txn: *const Transaction,
    snapshot: YBinary,
) -> YBinary {
    state_from_snapshot(txn, snapshot, yrs::lib0::EncoderV1::new())
}

/// Encodes document state at `snapshot` as lib0 v2 update. Requires `skip_gc`.
/// Returns empty binary on failure.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ytransaction_encode_state_from_snapshot_v2(
    txn: *const Transaction,
    snapshot: YBinary,
) -> YBinary {
    state_from_snapshot(txn, snapshot, yrs::lib0::EncoderV2::new())
}

/// Returns lib0 v1 encoded update waiting for missing dependencies, or empty binary if none.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ytransaction_pending_update(txn: *const Transaction) -> YBinary {
    match (*txn).read().doc().pending_update() {
        Some(pending) => pending.update.encode_v1().into(),
        None => YBinary::empty(),
    }
}

/// Returns lib0 v1 encoded delete set waiting for missing dependencies, or empty binary if none.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ytransaction_pending_ds(txn: *const Transaction) -> YBinary {
    match (*txn).read().doc().pending_ds() {
        Some(ds) => ds.encode_v1().into(),
        None => YBinary::empty(),
    }
}

/// Evaluates JSON path query and returns JSON array of matching values, or `NULL` on invalid query.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ytransaction_json_path(
    txn: *const Transaction,
    path: *const c_char,
) -> *mut c_char {
    let Some(query) = to_str(path).and_then(|path| JsonPath::parse(path).ok()) else {
        return null_mut();
    };
    json_seq((*txn).read().json_path(&query))
}

/// Returns node id as JSON.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ynode_id(node: *const Node) -> *mut c_char {
    to_json(&(*node).id())
}

/// Returns parent node, or `NULL` for root nodes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ynode_parent(node: *const Node, txn: *const Transaction) -> *const Node {
    let txn = (*txn).read();
    let parent = NodeRef::new(node_ptr(node), txn).parent();
    match parent.and_then(|id| txn.node(id)) {
        Some(parent) => raw_node(parent.as_ref()),
        None => null(),
    }
}

/// Returns number of node children.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ynode_len(node: *const Node, txn: *const Transaction) -> u32 {
    read_node(node, txn).len()
}

/// Returns number of node attributes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ynode_attr_len(node: *const Node, txn: *const Transaction) -> u32 {
    read_node(node, txn).attr_len()
}

/// Returns child at `index` as JSON, or `NULL` if out of bounds.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ynode_get_json(
    node: *const Node,
    txn: *const Transaction,
    index: u32,
) -> *mut c_char {
    match read_node(node, txn).get(index) {
        Some(value) => to_json(&value),
        None => null_mut(),
    }
}

/// Returns attribute `name` as JSON, or `NULL` if missing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ynode_attr_json(
    node: *const Node,
    txn: *const Transaction,
    name: *const c_char,
) -> *mut c_char {
    let Some(name) = to_str(name) else {
        return null_mut();
    };
    match read_node(node, txn).attr(name) {
        Some(value) => to_json(&value),
        None => null_mut(),
    }
}

/// Returns text content of a node.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ynode_string(node: *const Node, txn: *const Transaction) -> *mut c_char {
    into_c_str(read_node(node, txn).to_string())
}

/// Returns node content as delta JSON. `deep` renders nested nodes, `link_depth` follows weak links.
///
/// Shape (fields are omitted when empty):
/// ```json
/// {
///   "name": "div",
///   "children": [
///     {"type": "insert", "insert": "text", "format": {"bold": true}},
///     {"type": "insert", "insert": [1, {"type": "delta", "id": {"client": 1, "clock": 2}, ...},
///                                   {"type": "doc", "guid": "..."}]}
///   ],
///   "attrs": {"key": {"type": "insert", "value": 1}},
///   "link": {...}
/// }
/// ```
/// Nested nodes carry their own `children`/`attrs` only when `deep` is set. Change deltas (see
/// `yevent_delta`) may also contain `{"type":"retain","retain":n}`, `{"type":"delete","delete":n}`,
/// `{"type":"modify","value":{...}}` children and `{"type":"delete"}`/`{"type":"modify"}` attrs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ynode_json(
    node: *const Node,
    txn: *const Transaction,
    deep: u8,
    link_depth: u32,
) -> *mut c_char {
    let options = DeltaOptions {
        deep: deep != 0,
        link_depth,
        ..DeltaOptions::default()
    };
    to_json(&read_node(node, txn).delta(&options))
}

/// Returns JSON link source to attribute `key`, or `NULL` if missing. Insert it as a node with
/// `ydelta_link`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ynode_link(
    node: *const Node,
    txn: *const Transaction,
    key: *const c_char,
) -> *mut c_char {
    let Some(key) = to_str(key) else {
        return null_mut();
    };
    match read_node(node, txn).link(key) {
        Some(link) => to_json(&link),
        None => null_mut(),
    }
}

/// Returns JSON link source quoting children range `start..end`, or `NULL` if out of bounds.
/// Exclusive bounds include concurrent inserts at their edges. Insert it with `ydelta_link`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ynode_quote(
    node: *const Node,
    txn: *const Transaction,
    start: u32,
    end: u32,
    start_exclusive: u8,
    end_exclusive: u8,
) -> *mut c_char {
    let bound = |i, exclusive| match exclusive {
        0 => Bound::Included(i),
        _ => Bound::Excluded(i),
    };
    let range = (bound(start, start_exclusive), bound(end, end_exclusive));
    match read_node(node, txn).quote(range) {
        Ok(link) => to_json(&link),
        Err(_) => null_mut(),
    }
}

/// `YIn` tag: `yrs::Any` value as JSON.
pub const Y_IN_JSON: u8 = 0;
/// `YIn` tag: nested node, its delta is the initial content (`NULL` = empty node).
pub const Y_IN_NODE: u8 = 1;
/// `YIn` tag: subdocument. The `Doc` pointer is consumed by a successful conversion.
pub const Y_IN_DOC: u8 = 2;

/// Value inserted into a node. Owned by the caller, except `Y_IN_DOC` pointers.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct YIn {
    pub tag: u8,
    pub value: YInValue,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union YInValue {
    pub json: *const c_char,
    pub node: *const YDelta,
    pub doc: *mut Doc,
}

/// `YDeltaOp` tag: skip `len` children, optionally formatting them.
pub const Y_OP_RETAIN: u8 = 0;
/// `YDeltaOp` tag: insert `text`.
pub const Y_OP_INSERT_TEXT: u8 = 1;
/// `YDeltaOp` tag: insert `items`.
pub const Y_OP_INSERT: u8 = 2;
/// `YDeltaOp` tag: remove `len` children.
pub const Y_OP_REMOVE: u8 = 3;
/// `YDeltaOp` tag: apply `delta` to a nested child node.
pub const Y_OP_MODIFY: u8 = 4;

/// Operation on node children. `format` is an optional JSON object.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct YDeltaOp {
    pub tag: u8,
    pub format: *const c_char,
    pub value: YDeltaOpValue,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union YDeltaOpValue {
    pub len: u32,
    pub text: *const c_char,
    pub items: YItems,
    pub delta: *const YDelta,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct YItems {
    pub items: *const YIn,
    pub len: u32,
}

/// `YAttrOp` tag: set attribute `value`.
pub const Y_ATTR_UPDATE: u8 = 0;
/// `YAttrOp` tag: remove attribute.
pub const Y_ATTR_REMOVE: u8 = 1;
/// `YAttrOp` tag: apply `delta` to a nested node stored in attribute.
pub const Y_ATTR_MODIFY: u8 = 2;

/// Operation on a node attribute `key`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct YAttrOp {
    pub key: *const c_char,
    pub tag: u8,
    pub value: YAttrOpValue,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union YAttrOpValue {
    pub value: YIn,
    pub delta: *const YDelta,
}

/// Changes to a node. `name` is an optional XML tag, `link` an optional JSON link source (see
/// `ynode_link`/`ynode_quote`) turning an inserted node into a weak link.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct YDelta {
    pub name: *const c_char,
    pub children: *const YDeltaOp,
    pub children_len: u32,
    pub attrs: *const YAttrOp,
    pub attrs_len: u32,
    pub link: *const c_char,
}

#[unsafe(no_mangle)]
pub extern "C" fn ydelta(
    name: *const c_char,
    children: *const YDeltaOp,
    children_len: u32,
    attrs: *const YAttrOp,
    attrs_len: u32,
) -> YDelta {
    YDelta {
        name,
        children,
        children_len,
        attrs,
        attrs_len,
        link: null(),
    }
}

/// Delta of a weak link node, inserted via `yin_node`. `link` comes from `ynode_link`/`ynode_quote`.
#[unsafe(no_mangle)]
pub extern "C" fn ydelta_link(link: *const c_char) -> YDelta {
    YDelta {
        link,
        ..ydelta(null(), null(), 0, null(), 0)
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn yin_json(json: *const c_char) -> YIn {
    YIn {
        tag: Y_IN_JSON,
        value: YInValue { json },
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn yin_node(delta: *const YDelta) -> YIn {
    YIn {
        tag: Y_IN_NODE,
        value: YInValue { node: delta },
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn yin_doc(doc: *mut Doc) -> YIn {
    YIn {
        tag: Y_IN_DOC,
        value: YInValue { doc },
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ydelta_retain(len: u32, format: *const c_char) -> YDeltaOp {
    YDeltaOp {
        tag: Y_OP_RETAIN,
        format,
        value: YDeltaOpValue { len },
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ydelta_insert_text(text: *const c_char, format: *const c_char) -> YDeltaOp {
    YDeltaOp {
        tag: Y_OP_INSERT_TEXT,
        format,
        value: YDeltaOpValue { text },
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ydelta_insert(items: *const YIn, len: u32, format: *const c_char) -> YDeltaOp {
    YDeltaOp {
        tag: Y_OP_INSERT,
        format,
        value: YDeltaOpValue {
            items: YItems { items, len },
        },
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ydelta_remove(len: u32) -> YDeltaOp {
    YDeltaOp {
        tag: Y_OP_REMOVE,
        format: null(),
        value: YDeltaOpValue { len },
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ydelta_modify(delta: *const YDelta, format: *const c_char) -> YDeltaOp {
    YDeltaOp {
        tag: Y_OP_MODIFY,
        format,
        value: YDeltaOpValue { delta },
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn yattr_update(key: *const c_char, value: YIn) -> YAttrOp {
    YAttrOp {
        key,
        tag: Y_ATTR_UPDATE,
        value: YAttrOpValue { value },
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn yattr_remove(key: *const c_char) -> YAttrOp {
    YAttrOp {
        key,
        tag: Y_ATTR_REMOVE,
        value: YAttrOpValue { delta: null() },
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn yattr_modify(key: *const c_char, delta: *const YDelta) -> YAttrOp {
    YAttrOp {
        key,
        tag: Y_ATTR_MODIFY,
        value: YAttrOpValue { delta },
    }
}

/// Subdocuments met during conversion, consumed only once whole delta converted successfully.
type Docs = Vec<*mut Doc>;

unsafe fn to_delta(delta: &YDelta, docs: &mut Docs) -> Option<Delta<In>> {
    let mut result = Delta::new();
    if !delta.name.is_null() {
        result.name = Some(to_str(delta.name)?.into());
    }
    if !delta.link.is_null() {
        result.link = Some(parse::<LinkSource>(delta.link)?);
    }
    let children = slice(delta.children, delta.children_len);
    result.children.reserve_exact(children.len());
    for op in children {
        result.children.push(to_op(op, docs)?);
    }
    let attrs = slice(delta.attrs, delta.attrs_len);
    result.attrs.reserve(attrs.len());
    for attr in attrs {
        result
            .attrs
            .insert(to_str(attr.key)?.into(), to_attr(attr, docs)?);
    }
    Some(result)
}

unsafe fn to_op(op: &YDeltaOp, docs: &mut Docs) -> Option<Op<In>> {
    let format = if op.format.is_null() {
        None
    } else {
        Some(Box::new(parse::<Attrs>(op.format)?))
    };
    let attribution = None;
    Some(match op.tag {
        Y_OP_RETAIN => Op::Retain {
            len: op.value.len,
            format,
            attribution,
        },
        Y_OP_INSERT_TEXT => Op::InsertText {
            text: to_str(op.value.text)?.to_owned(),
            format,
            attribution,
        },
        Y_OP_INSERT => {
            let items = slice(op.value.items.items, op.value.items.len);
            let items = items
                .iter()
                .map(|item| to_in(item, docs))
                .collect::<Option<Vec<_>>>()?;
            Op::Insert {
                items,
                format,
                attribution,
            }
        }
        Y_OP_REMOVE => Op::Remove {
            len: op.value.len,
            prev: None,
        },
        Y_OP_MODIFY => Op::Modify {
            delta: Box::new(to_delta(op.value.delta.as_ref()?, docs)?),
            format,
            attribution,
        },
        _ => return None,
    })
}

unsafe fn to_attr(op: &YAttrOp, docs: &mut Docs) -> Option<AttrOp<In>> {
    Some(match op.tag {
        Y_ATTR_UPDATE => AttrOp::update(to_in(&op.value.value, docs)?, None),
        Y_ATTR_REMOVE => AttrOp::remove(None),
        Y_ATTR_MODIFY => AttrOp::modify(to_delta(op.value.delta.as_ref()?, docs)?),
        _ => return None,
    })
}

unsafe fn to_in(value: &YIn, docs: &mut Docs) -> Option<In> {
    match value.tag {
        Y_IN_JSON => Some(In::Any(parse::<Any>(value.value.json)?)),
        Y_IN_NODE => match value.value.node.as_ref() {
            None => Some(In::Node(Box::new(Delta::new()))),
            Some(delta) => Some(In::Node(Box::new(to_delta(delta, docs)?))),
        },
        Y_IN_DOC => {
            let ptr = value.value.doc;
            let doc = ptr.as_ref()?;
            if !docs.contains(&ptr) {
                docs.push(ptr);
            }
            // only options are used on insert
            Some(In::Doc(Doc::with_options(doc.options().clone())))
        }
        _ => None,
    }
}

/// Applies `delta` to a node. Converts the whole delta first: on conversion failure nothing is
/// applied and no `Y_IN_DOC` pointer is consumed. Inserted subdocuments are reachable afterwards
/// via `ytransaction_subdoc`. Returns 0 on success or an error code.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ynode_apply_delta(
    node: *mut Node,
    txn: *mut Transaction,
    delta: *const YDelta,
) -> u8 {
    let Some(txn) = (*txn).write() else {
        return ERR_READ_ONLY;
    };
    let mut docs = Docs::new();
    let Some(delta) = delta.as_ref().and_then(|d| to_delta(d, &mut docs)) else {
        return ERR_DELTA_CONVERSION;
    };
    for doc in docs {
        drop(Box::from_raw(doc));
    }
    let mut node = NodeRef::new(node_ptr(node), txn);
    match catch_unwind(AssertUnwindSafe(|| node.apply_delta(delta))) {
        Ok(()) => 0,
        Err(_) => ERR_DELTA_APPLY,
    }
}

/* Events */

/// Node change event. Valid only inside the observer callback: read it with `yevent_*` functions.
#[repr(C)]
pub struct YEvent {
    event: *mut c_void,
    txn: *const Transaction,
}

impl YEvent {
    fn new(event: &mut yrs::Event, txn: &Transaction) -> Self {
        YEvent {
            event: event as *mut yrs::Event as *mut c_void,
            txn,
        }
    }

    unsafe fn event<'a>(&self) -> &'a mut yrs::Event<'static> {
        &mut *(self.event as *mut yrs::Event<'static>)
    }
}

/// Subscribes `cb` under `key` to changes of this node.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ynode_observe(
    node: *mut Node,
    key: YBinary,
    state: *mut c_void,
    cb: extern "C" fn(*mut c_void, *const YEvent),
) {
    (*node).observe(origin(key), move |e| {
        let txn = Transaction::borrowed(e.transaction());
        cb(state, &YEvent::new(e, &txn))
    });
}

/// Unsubscribes a callback registered by `ynode_observe`. Returns 1 if removed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ynode_unobserve(node: *mut Node, key: YBinary) -> u8 {
    (*node).unobserve(&origin(key)) as u8
}

/// Subscribes `cb` under `key` to changes of this node and its descendants. Callback receives
/// an array of `len` events.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ynode_observe_deep(
    node: *mut Node,
    key: YBinary,
    state: *mut c_void,
    cb: extern "C" fn(*mut c_void, u32, *const YEvent),
) {
    (*node).observe_deep(origin(key), move |events| {
        let Some(first) = events.first() else {
            return;
        };
        let txn = Transaction::borrowed(first.transaction());
        let events: Vec<_> = events.iter_mut().map(|e| YEvent::new(e, &txn)).collect();
        cb(state, events.len() as u32, events.as_ptr())
    });
}

/// Unsubscribes a callback registered by `ynode_observe_deep`. Returns 1 if removed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ynode_unobserve_deep(node: *mut Node, key: YBinary) -> u8 {
    (*node).unobserve_deep(&origin(key)) as u8
}

/// Returns the node changed by this event.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn yevent_target(event: *const YEvent) -> *mut Node {
    raw_node((*event).event().target().as_ref())
}

/// Returns the read-only transaction which triggered this event.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn yevent_transaction(event: *const YEvent) -> *const Transaction {
    (*event).txn
}

/// Returns a JSON path from the document root to the event target, e.g. `$.map.inner[2]`,
/// accepted by `ytransaction_json_path`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn yevent_path(event: *const YEvent) -> *mut c_char {
    let target = (*event).event().target();
    let mut path = String::from("$");
    for segment in target.path() {
        match segment {
            PathSegment::Key(key) => push_key(&mut path, &key),
            PathSegment::Index(i) => write!(path, "[{}]", i).unwrap(),
        }
    }
    into_c_str(path)
}

fn push_key(path: &mut String, key: &str) {
    let plain = !key.is_empty() && key.chars().all(|c| c.is_alphanumeric() || c == '_');
    if plain {
        write!(path, ".{}", key).unwrap()
    } else {
        write!(path, "['{}']", key).unwrap()
    }
}

/// Returns changes made to the target node as delta JSON. `deep` also renders changes of nested
/// nodes as `modify` operations instead of listing them as separate events.
///
/// Shape (fields are omitted when empty):
/// ```json
/// {
///   "children": [
///     {"type": "retain", "retain": 3, "format": {"bold": null}},
///     {"type": "insert", "insert": "text", "format": {"bold": true}},
///     {"type": "insert", "insert": [1, {"type": "delta", "id": {"client": 1, "clock": 2}}]},
///     {"type": "delete", "delete": 2},
///     {"type": "modify", "value": {"children": [...], "attrs": {...}}}
///   ],
///   "attrs": {
///     "a": {"type": "insert", "value": 1},
///     "b": {"type": "delete"},
///     "c": {"type": "modify", "value": {"children": [...], "attrs": {...}}}
///   }
/// }
/// ```
/// `retain` skips unchanged children, carrying a `format` only when formatting changed (`null`
/// clears a key). Like Yjs, event deltas don't carry previous attribute values.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn yevent_delta(event: *const YEvent, deep: u8) -> *mut c_char {
    to_json(&(*event).event().delta(deep != 0))
}

/// Returns JSON array of changed attribute keys, where `null` marks a change of children.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn yevent_keys_changed(event: *const YEvent) -> *mut c_char {
    let event = (*event).event();
    let children = event.children_changed().then_some(None);
    json_seq(children.into_iter().chain(event.keys_changed().map(Some)))
}

/// Returns lib0 v1 encoded sticky index at `index` of a node (`assoc` >= 0: after, < 0: before),
/// or empty binary if out of bounds.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ysticky_index_from_index(
    node: *const Node,
    txn: *const Transaction,
    index: u32,
    assoc_value: i8,
) -> YBinary {
    match read_node(node, txn).sticky_index(index, assoc(assoc_value)) {
        Some(index) => index.encode_v1().into(),
        None => YBinary::empty(),
    }
}

/// Resolves lib0 v1 encoded sticky index into JSON `{"id":<node id>,"index":n}`, or `NULL` if
/// malformed or unresolvable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ysticky_index_read(
    index: YBinary,
    txn: *const Transaction,
) -> *mut c_char {
    let Ok(index) = StickyIndex::decode_v1(index.as_slice()) else {
        return null_mut();
    };
    match index.get_offset((*txn).read()) {
        Some(offset) => {
            to_json(&serde_json::json!({ "id": offset.branch.id(), "index": offset.index }))
        }
        None => null_mut(),
    }
}
