/**
 * The MIT License (MIT)
 *
 *  Copyright (c) 2020
 *    - Bartosz Sypytkowski <b.sypytkowski@gmail.com>
 *    - Kevin Jahns <kevin.jahns@pm.me>.
 *
 *  Permission is hereby granted, free of charge, to any person obtaining a copy
 *  of this software and associated documentation files (the "Software"), to deal
 *  in the Software without restriction, including without limitation the rights
 *  to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 *  copies of the Software, and to permit persons to whom the Software is
 *  furnished to do so, subject to the following conditions:
 *
 *  The above copyright notice and this permission notice shall be included in all
 *  copies or substantial portions of the Software.
 *
 *  THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 *  IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 *  FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 *  AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 *  LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 *  OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
 *  SOFTWARE.
 */

#ifndef YRS_FFI_H
#define YRS_FFI_H

/**
 * A Yrs document. All nodes live within a document and are read and written via transactions.
 */
typedef struct YDoc YDoc;

/**
 * A collaborative node. Owned by its document.
 */
typedef struct YNode YNode;


#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>

/**
 * Error code: couldn't read data from input stream.
 */
#define ERR_CODE_IO 1

/**
 * Error code: decoded variable integer outside of the expected integer size bounds.
 */
#define ERR_CODE_VAR_INT 2

/**
 * Error code: end of stream found when more data was expected.
 */
#define ERR_CODE_EOS 3

/**
 * Error code: decoded enum tag value was not among known cases.
 */
#define ERR_CODE_UNEXPECTED_VALUE 4

/**
 * Error code: failure when trying to decode JSON content.
 */
#define ERR_CODE_INVALID_JSON 5

/**
 * Error code: other error type than the one specified.
 */
#define ERR_CODE_OTHER 6

/**
 * Error code: not enough memory to perform an operation.
 */
#define ERR_NOT_ENOUGH_MEMORY 7

/**
 * Error code: conversion attempt to specific Rust type was not possible.
 */
#define ERR_TYPE_MISMATCH 8

/**
 * Error code: miscellaneous error coming from serde, not covered by other error codes.
 */
#define ERR_CUSTOM 9

/**
 * Error code: malformed update.
 */
#define ERR_MALFORMED 10

/**
 * Error code: update block assigned to parent that is not a valid shared ref of deleted block.
 */
#define ERR_INVALID_PARENT 9

/**
 * Error code: `YDelta` couldn't be converted (invalid tag, JSON or UTF-8 string).
 */
#define ERR_DELTA_CONVERSION 11

/**
 * Error code: delta was converted, but applying it onto a node failed.
 */
#define ERR_DELTA_APPLY 12

/**
 * Error code: operation requires a read-write transaction.
 */
#define ERR_READ_ONLY 13

/**
 * `YIn` tag: `yrs::Any` value as JSON.
 */
#define Y_IN_JSON 0

/**
 * `YIn` tag: nested node, its delta is the initial content (`NULL` = empty node).
 */
#define Y_IN_NODE 1

/**
 * `YIn` tag: subdocument. The `Doc` pointer is consumed by a successful conversion.
 */
#define Y_IN_DOC 2

/**
 * `YDeltaOp` tag: skip `len` children, optionally formatting them.
 */
#define Y_OP_RETAIN 0

/**
 * `YDeltaOp` tag: insert `text`.
 */
#define Y_OP_INSERT_TEXT 1

/**
 * `YDeltaOp` tag: insert `items`.
 */
#define Y_OP_INSERT 2

/**
 * `YDeltaOp` tag: remove `len` children.
 */
#define Y_OP_REMOVE 3

/**
 * `YDeltaOp` tag: apply `delta` to a nested child node.
 */
#define Y_OP_MODIFY 4

/**
 * `YAttrOp` tag: set attribute `value`.
 */
#define Y_ATTR_UPDATE 0

/**
 * `YAttrOp` tag: remove attribute.
 */
#define Y_ATTR_REMOVE 1

/**
 * `YAttrOp` tag: apply `delta` to a nested node stored in attribute.
 */
#define Y_ATTR_MODIFY 2

/**
 * Transaction is a scope in which a document can be read or written.
 */
typedef struct YTransaction YTransaction;

/**
 * Byte string. Inputs are owned by the caller; outputs are released with `ybinary_destroy`.
 */
typedef struct YBinary {
  uint8_t *ptr;
  uint32_t len;
} YBinary;

/**
 * Callbacks called by `ytransaction_commit` in field order, each with the `state` passed there.
 * `NULL` fields are skipped. Meant to be declared as `static const`.
 *
 * Pointers passed to callbacks (transaction, update, JSON) are valid only during the callback.
 * The transaction is read-only (writes return `ERR_READ_ONLY`) and must never be committed.
 * Callbacks must not open a transaction on the same document.
 */
typedef struct YReporterVTable {
  /**
   * Called before GC. Deltas (`ytransaction_delta`) must be read here: after GC they render
   * deleted content as spurious deletes.
   */
  void (*committing)(void *state, struct YTransaction *txn);
  /**
   * Receives lib0 v1 update of the transaction. Skipped if the transaction changed nothing.
   */
  void (*update_v1)(void *state, struct YTransaction *txn, struct YBinary update);
  /**
   * Receives lib0 v2 update of the transaction. Skipped if the transaction changed nothing.
   */
  void (*update_v2)(void *state, struct YTransaction *txn, struct YBinary update);
  /**
   * Receives JSON `{"added":[guid],"removed":[guid],"loaded":[guid]}`. Skipped if all are empty.
   */
  void (*subdocs)(void *state, const char *json);
  /**
   * Called last.
   */
  void (*committed)(void *state, struct YTransaction *txn);
} YReporterVTable;

typedef union YInValue {
  const char *json;
  const struct YDelta *node;
  YDoc *doc;
} YInValue;

/**
 * Value inserted into a node. Owned by the caller, except `Y_IN_DOC` pointers.
 */
typedef struct YIn {
  uint8_t tag;
  union YInValue value;
} YIn;

typedef struct YItems {
  const struct YIn *items;
  uint32_t len;
} YItems;

typedef union YDeltaOpValue {
  uint32_t len;
  const char *text;
  struct YItems items;
  const struct YDelta *delta;
} YDeltaOpValue;

/**
 * Operation on node children. `format` is an optional JSON object.
 */
typedef struct YDeltaOp {
  uint8_t tag;
  const char *format;
  union YDeltaOpValue value;
} YDeltaOp;

typedef union YAttrOpValue {
  struct YIn value;
  const struct YDelta *delta;
} YAttrOpValue;

/**
 * Operation on a node attribute `key`.
 */
typedef struct YAttrOp {
  const char *key;
  uint8_t tag;
  union YAttrOpValue value;
} YAttrOp;

/**
 * Changes to a node. `name` is an optional XML tag, `link` an optional JSON link source (see
 * `ynode_link`/`ynode_quote`) turning an inserted node into a weak link.
 */
typedef struct YDelta {
  const char *name;
  const struct YDeltaOp *children;
  uint32_t children_len;
  const struct YAttrOp *attrs;
  uint32_t attrs_len;
  const char *link;
} YDelta;

/**
 * Releases a string returned by yffi.
 */
void ystring_destroy(char *str);

/**
 * Releases a binary returned by yffi.
 */
void ybinary_destroy(struct YBinary binary);

/**
 * Creates a new document from JSON options (`NULL` = defaults). Returns `NULL` on invalid JSON.
 * Release it with `ydoc_destroy`.
 *
 * Options shape (every field is optional):
 * ```json
 * {
 *   "client_id": 1,
 *   "guid": "unique document id",
 *   "collection_id": "provider collection",
 *   "offset_kind": "Bytes",
 *   "skip_gc": false,
 *   "auto_load": false,
 *   "should_load": true,
 *   "cleanup_formatting": true
 * }
 * ```
 * `client_id` defaults to a random value and `guid` to a random UUID v4. `collection_id`
 * defaults to `null`. `offset_kind` is `"Bytes"` (UTF-8 byte offsets, default) or `"Utf16"`.
 * `skip_gc` keeps deleted content (required by snapshots). `auto_load` and `should_load` control
 * subdocument loading. `cleanup_formatting` removes redundant formatting blocks on commit.
 * `cleanup_formatting` defaults to `true`. `ydoc_options_json` returns the same shape.
 */
YDoc *ydoc(const char *options);

/**
 * Releases a document. Must not be called on subdocuments borrowed from `ytransaction_subdoc`.
 */
void ydoc_destroy(YDoc *doc);

/**
 * Returns document options as JSON.
 */
char *ydoc_options_json(const YDoc *doc);

/**
 * Starts a read-only transaction. Release it with `ytransaction_commit`.
 */
struct YTransaction *ydoc_read_transaction(YDoc *doc);

/**
 * Starts a read-write transaction with an optional `origin` (`len == 0` = none).
 * Commit and release it with `ytransaction_commit`.
 */
struct YTransaction *ydoc_write_transaction(YDoc *doc, struct YBinary origin);

/**
 * Requests a subdocument to be loaded. No-op if `parent_txn` is read-only.
 */
void ydoc_load(YDoc *doc, struct YTransaction *parent_txn);

/**
 * Destroys a document. `parent_txn` (read-write) is required for subdocuments and may be `NULL`
 * otherwise: destroyed subdocument is reported as `removed` by the parent's commit `subdocs`.
 */
void ydoc_clear(YDoc *doc, struct YTransaction *parent_txn);

/**
 * Commits (if read-write) and releases a transaction, reporting the commit to `vtable` callbacks
 * called with `state`. `NULL` vtable reports nothing. Read-only transactions ignore the vtable.
 */
void ytransaction_commit(struct YTransaction *txn,
                         const struct YReporterVTable *vtable,
                         void *state);

/**
 * Returns JSON array of ids of nodes modified by the transaction: all of them if `root` is `NULL`,
 * otherwise only `root` and nodes nested in it.
 */
char *ytransaction_modified(const struct YTransaction *txn, const YNode *root);

/**
 * Returns the transaction origin, or empty binary if none. It's borrowed: don't destroy it.
 */
struct YBinary ytransaction_origin(const struct YTransaction *txn);

/**
 * Returns 1 if the transaction is local, 0 if it applies a remote update.
 */
uint8_t ytransaction_local(const struct YTransaction *txn);

/**
 * Returns 1 if transaction is read-write, 0 otherwise.
 */
uint8_t ytransaction_writeable(struct YTransaction *txn);

/**
 * Garbage collects all deleted blocks, even with `skip_gc` set. No-op on read-only transactions.
 */
void ytransaction_gc(struct YTransaction *txn);

/**
 * Returns a node by its JSON id: root name (`"\"text\""`) or `{"client":n,"clock":n}`.
 * Read-write transactions create missing root nodes. Returns `NULL` if missing or deleted.
 */
YNode *ynode(struct YTransaction *txn, const char *id);

/**
 * Returns JSON array of root node names.
 */
char *ytransaction_root_refs(const struct YTransaction *txn);

/**
 * Returns a subdocument by its guid or `NULL`. It's owned by its parent: don't destroy it.
 */
YDoc *ytransaction_subdoc(const struct YTransaction *txn, const char *guid);

/**
 * Returns JSON array of subdocument guids.
 */
char *ytransaction_subdocs(const struct YTransaction *txn);

/**
 * Returns lib0 v1 encoded state vector of the document.
 */
struct YBinary ytransaction_state_vector_v1(const struct YTransaction *txn);

/**
 * Returns lib0 v1 update with changes missing from state vector `sv` (`len == 0` = all).
 * Returns empty binary if `sv` is malformed.
 */
struct YBinary ytransaction_state_diff_v1(const struct YTransaction *txn, struct YBinary sv);

/**
 * Returns lib0 v2 update with changes missing from state vector `sv` (`len == 0` = all).
 * Returns empty binary if `sv` is malformed.
 */
struct YBinary ytransaction_state_diff_v2(const struct YTransaction *txn, struct YBinary sv);

/**
 * Applies lib0 v1 update. Returns 0 on success or an error code.
 */
uint8_t ytransaction_apply(struct YTransaction *txn, struct YBinary update);

/**
 * Applies lib0 v2 update. Returns 0 on success or an error code.
 */
uint8_t ytransaction_apply_v2(struct YTransaction *txn, struct YBinary update);

/**
 * Returns lib0 v1 encoded snapshot of the document.
 */
struct YBinary ytransaction_snapshot(const struct YTransaction *txn);

/**
 * Encodes document state at `snapshot` as lib0 v1 update. Requires `skip_gc`.
 * Returns empty binary on failure.
 */
struct YBinary ytransaction_encode_state_from_snapshot_v1(const struct YTransaction *txn,
                                                          struct YBinary snapshot);

/**
 * Encodes document state at `snapshot` as lib0 v2 update. Requires `skip_gc`.
 * Returns empty binary on failure.
 */
struct YBinary ytransaction_encode_state_from_snapshot_v2(const struct YTransaction *txn,
                                                          struct YBinary snapshot);

/**
 * Returns lib0 v1 encoded update waiting for missing dependencies, or empty binary if none.
 */
struct YBinary ytransaction_pending_update(const struct YTransaction *txn);

/**
 * Returns lib0 v1 encoded delete set waiting for missing dependencies, or empty binary if none.
 */
struct YBinary ytransaction_pending_ds(const struct YTransaction *txn);

/**
 * Evaluates JSON path query and returns JSON array of matching values, or `NULL` on invalid query.
 */
char *ytransaction_json_path(const struct YTransaction *txn, const char *path);

/**
 * Returns node id as JSON.
 */
char *ynode_id(const YNode *node);

/**
 * Returns parent node, or `NULL` for root nodes.
 */
const YNode *ynode_parent(const YNode *node, const struct YTransaction *txn);

/**
 * Returns number of node children.
 */
uint32_t ynode_len(const YNode *node, const struct YTransaction *txn);

/**
 * Returns number of node attributes.
 */
uint32_t ynode_attr_len(const YNode *node, const struct YTransaction *txn);

/**
 * Returns child at `index` as JSON, or `NULL` if out of bounds.
 */
char *ynode_get_json(const YNode *node, const struct YTransaction *txn, uint32_t index);

/**
 * Returns attribute `name` as JSON, or `NULL` if missing.
 */
char *ynode_attr_json(const YNode *node, const struct YTransaction *txn, const char *name);

/**
 * Returns text content of a node.
 */
char *ynode_string(const YNode *node, const struct YTransaction *txn);

/**
 * Returns node content as delta JSON. `deep` renders nested nodes, `link_depth` follows weak links.
 *
 * Shape (fields are omitted when empty):
 * ```json
 * {
 *   "name": "div",
 *   "children": [
 *     {"type": "insert", "insert": "text", "format": {"bold": true}},
 *     {"type": "insert", "insert": [1, {"type": "delta", "id": {"client": 1, "clock": 2}, ...},
 *                                   {"type": "doc", "guid": "..."}]}
 *   ],
 *   "attrs": {"key": {"type": "insert", "value": 1}},
 *   "link": {...}
 * }
 * ```
 * Nested nodes carry their own `children`/`attrs` only when `deep` is set. Change deltas (see
 * `ytransaction_delta`) may also contain `{"type":"retain","retain":n}`,
 * `{"type":"delete","delete":n}`, `{"type":"modify","value":{...}}` children and
 * `{"type":"delete"}`/`{"type":"modify"}` attrs.
 */
char *ynode_json(const YNode *node,
                 const struct YTransaction *txn,
                 uint8_t deep,
                 uint32_t link_depth);

/**
 * Returns JSON link source to attribute `key`, or `NULL` if missing. Insert it as a node with
 * `ydelta_link`.
 */
char *ynode_link(const YNode *node, const struct YTransaction *txn, const char *key);

/**
 * Returns JSON link source quoting children range `start..end`, or `NULL` if out of bounds.
 * Exclusive bounds include concurrent inserts at their edges. Insert it with `ydelta_link`.
 */
char *ynode_quote(const YNode *node,
                  const struct YTransaction *txn,
                  uint32_t start,
                  uint32_t end,
                  uint8_t start_exclusive,
                  uint8_t end_exclusive);

struct YDelta ydelta(const char *name,
                     const struct YDeltaOp *children,
                     uint32_t children_len,
                     const struct YAttrOp *attrs,
                     uint32_t attrs_len);

/**
 * Delta of a weak link node, inserted via `yin_node`. `link` comes from `ynode_link`/`ynode_quote`.
 */
struct YDelta ydelta_link(const char *link);

struct YIn yin_json(const char *json);

struct YIn yin_node(const struct YDelta *delta);

struct YIn yin_doc(YDoc *doc);

struct YDeltaOp ydelta_retain(uint32_t len, const char *format);

struct YDeltaOp ydelta_insert_text(const char *text, const char *format);

struct YDeltaOp ydelta_insert(const struct YIn *items, uint32_t len, const char *format);

struct YDeltaOp ydelta_remove(uint32_t len);

struct YDeltaOp ydelta_modify(const struct YDelta *delta, const char *format);

struct YAttrOp yattr_update(const char *key, struct YIn value);

struct YAttrOp yattr_remove(const char *key);

struct YAttrOp yattr_modify(const char *key, const struct YDelta *delta);

/**
 * Applies `delta` to a node. Converts the whole delta first: on conversion failure nothing is
 * applied and no `Y_IN_DOC` pointer is consumed. Inserted subdocuments are reachable afterwards
 * via `ytransaction_subdoc`. Returns 0 on success or an error code.
 */
uint8_t ynode_apply_delta(YNode *node, struct YTransaction *txn, const struct YDelta *delta);

/**
 * Returns a JSON path from the document root to the node, e.g. `$.map.inner[2]`, accepted by
 * `ytransaction_json_path`.
 */
char *ynode_path(const YNode *node, const struct YTransaction *txn);

/**
 * Returns changes made to `node` by the transaction as delta JSON. `deep` also renders changes of
 * nested nodes as `modify` operations. Only correct in `YReporterVTable.committing`: after GC
 * deleted content renders as spurious deletes.
 *
 * Shape (fields are omitted when empty):
 * ```json
 * {
 *   "children": [
 *     {"type": "retain", "retain": 3, "format": {"bold": null}},
 *     {"type": "insert", "insert": "text", "format": {"bold": true}},
 *     {"type": "insert", "insert": [1, {"type": "delta", "id": {"client": 1, "clock": 2}}]},
 *     {"type": "delete", "delete": 2},
 *     {"type": "modify", "value": {"children": [...], "attrs": {...}}}
 *   ],
 *   "attrs": {
 *     "a": {"type": "insert", "value": 1},
 *     "b": {"type": "delete"},
 *     "c": {"type": "modify", "value": {"children": [...], "attrs": {...}}}
 *   }
 * }
 * ```
 * `retain` skips unchanged children, carrying a `format` only when formatting changed (`null`
 * clears a key).
 */
char *ytransaction_delta(const struct YTransaction *txn, const YNode *node, uint8_t deep);

/**
 * Returns JSON array of `node` attribute keys changed by the transaction, where `null` marks
 * a change of children.
 */
char *ytransaction_keys_changed(const struct YTransaction *txn, const YNode *node);

/**
 * Returns lib0 v1 encoded sticky index at `index` of a node (`assoc` >= 0: after, < 0: before),
 * or empty binary if out of bounds.
 */
struct YBinary ysticky_index_from_index(const YNode *node,
                                        const struct YTransaction *txn,
                                        uint32_t index,
                                        int8_t assoc_value);

/**
 * Resolves lib0 v1 encoded sticky index into JSON `{"id":<node id>,"index":n}`, or `NULL` if
 * malformed or unresolvable.
 */
char *ysticky_index_read(struct YBinary index, const struct YTransaction *txn);

#endif
