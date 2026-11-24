#define DOCTEST_CONFIG_IMPLEMENT_WITH_MAIN

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "include/doctest.h"

extern "C" {
#include "include/libyrs.h"
};

// Checks a string returned by yffi against `expected` and releases it.
#define CHECK_STR(actual, expected)                                                                \
    do {                                                                                           \
        char *s_ = (actual);                                                                       \
        REQUIRE(s_ != NULL);                                                                       \
        CHECK(strcmp(s_, (expected)) == 0);                                                        \
        ystring_destroy(s_);                                                                       \
    } while (0)

// Checks that a string returned by yffi contains `needle` and releases it.
#define CHECK_CONTAINS(actual, needle)                                                             \
    do {                                                                                           \
        char *s_ = (actual);                                                                       \
        REQUIRE(s_ != NULL);                                                                       \
        INFO(s_);                                                                                  \
        CHECK(strstr(s_, (needle)) != NULL);                                                       \
        ystring_destroy(s_);                                                                       \
    } while (0)

static YBinary bin(const char *str) {
    return YBinary{(uint8_t *) str, (uint32_t) strlen(str)};
}

static YBinary none() {
    return YBinary{NULL, 0};
}

// Copies a byte string into a malloc'ed buffer.
static YBinary copy(YBinary src) {
    uint8_t *ptr = (uint8_t *) malloc(src.len + 1);
    memcpy(ptr, src.ptr, src.len);
    ptr[src.len] = 0;
    return YBinary{ptr, src.len};
}

// Bounded list of byte strings captured by callbacks.
typedef struct Captured {
    uint32_t len;
    YBinary items[4];
} Captured;

static void captured_push(Captured *c, YBinary item) {
    REQUIRE(c->len < 4);
    c->items[c->len++] = copy(item);
}

static void captured_free(Captured *c) {
    for (uint32_t i = 0; i < c->len; i++) {
        free(c->items[i].ptr);
    }
    c->len = 0;
}

// Extracts `"id"` value from a `{"type":"delta","id":...}` node JSON and releases it.
static void node_id(char *json, char *out, size_t cap) {
    REQUIRE(json != NULL);
    const char *start = strstr(json, "\"id\":");
    REQUIRE(start != NULL);
    start += 5;
    size_t len = strlen(start) - 1; // drop closing brace
    REQUIRE(len < cap);
    memcpy(out, start, len);
    out[len] = 0;
    ystring_destroy(json);
}

static void insert_text(YDoc *doc, const char *root, uint32_t index, const char *text,
                        const YReporterVTable *vt = NULL, void *state = NULL) {
    YTransaction *txn = ydoc_write_transaction(doc, none());
    YNode *node = ynode(txn, root);
    YDeltaOp ops[] = {ydelta_retain(index, NULL), ydelta_insert_text(text, NULL)};
    YDelta ops_delta = ydelta(NULL, ops, 2, NULL, 0);
    REQUIRE(ynode_apply_delta(node, txn, &ops_delta) == 0);
    ytransaction_commit(txn, vt, state);
}

// Returns text of a root node, released by the caller.
static char *read_string(YDoc *doc, const char *root) {
    YTransaction *txn = ydoc_read_transaction(doc);
    YNode *node = ynode(txn, root);
    REQUIRE(node != NULL);
    char *result = ynode_string(node, txn);
    ytransaction_commit(txn, NULL, NULL);
    return result;
}

static void check_same_text(YDoc *d1, YDoc *d2, const char *root) {
    char *s1 = read_string(d1, root);
    char *s2 = read_string(d2, root);
    CHECK(strcmp(s1, s2) == 0);
    ystring_destroy(s1);
    ystring_destroy(s2);
}

// Sends all changes of `from` missing in `to`.
static void sync(YDoc *from, YDoc *to) {
    YTransaction *t_to = ydoc_read_transaction(to);
    YBinary sv = ytransaction_state_vector_v1(t_to);
    ytransaction_commit(t_to, NULL, NULL);

    YTransaction *t_from = ydoc_read_transaction(from);
    YBinary diff = ytransaction_state_diff_v1(t_from, sv);
    ytransaction_commit(t_from, NULL, NULL);
    ybinary_destroy(sv);

    t_to = ydoc_write_transaction(to, bin("remote"));
    REQUIRE(ytransaction_apply(t_to, diff) == 0);
    ytransaction_commit(t_to, NULL, NULL);
    ybinary_destroy(diff);
}

TEST_CASE("Doc options") {
    YDoc *doc = ydoc("{\"client_id\":1,\"guid\":\"doc-guid\",\"skip_gc\":true}");
    REQUIRE(doc != NULL);
    char *json = ydoc_options_json(doc);
    REQUIRE(json != NULL);
    CHECK(strstr(json, "\"client_id\":1") != NULL);
    CHECK(strstr(json, "\"guid\":\"doc-guid\"") != NULL);
    CHECK(strstr(json, "\"skip_gc\":true") != NULL);
    ystring_destroy(json);
    ydoc_destroy(doc);

    CHECK(ydoc("{invalid") == NULL);
    YDoc *defaults = ydoc(NULL);
    REQUIRE(defaults != NULL);
    ydoc_destroy(defaults);
}

TEST_CASE("Text insert with format") {
    YDoc *doc = ydoc("{\"client_id\":1}");
    YTransaction *txn = ydoc_write_transaction(doc, none());
    CHECK(ytransaction_writeable(txn) == 1);
    YNode *text = ynode(txn, "\"text\"");
    REQUIRE(text != NULL);

    YDeltaOp ops[] = {
        ydelta_insert_text("hello ", NULL),
        ydelta_insert_text("world", "{\"bold\":true}"),
    };
    YDelta ops_delta = ydelta(NULL, ops, 2, NULL, 0);
    REQUIRE(ynode_apply_delta(text, txn, &ops_delta) == 0);
    CHECK_STR(ynode_string(text, txn), "hello world");
    CHECK(ynode_len(text, txn) == 11);
    CHECK_STR(ynode_json(text, txn, 0, 0),
              "{\"children\":[{\"type\":\"insert\",\"insert\":\"hello \"},"
              "{\"type\":\"insert\",\"insert\":\"world\",\"format\":{\"bold\":true}}]}");
    CHECK_STR(ynode_id(text), "\"text\"");
    CHECK(ynode_parent(text, txn) == NULL);

    YDeltaOp remove[] = {ydelta_retain(5, NULL), ydelta_remove(6)};
    YDelta remove_delta = ydelta(NULL, remove, 2, NULL, 0);
    REQUIRE(ynode_apply_delta(text, txn, &remove_delta) == 0);
    CHECK_STR(ynode_string(text, txn), "hello");
    ytransaction_commit(txn, NULL, NULL);

    txn = ydoc_read_transaction(doc);
    CHECK(ytransaction_writeable(txn) == 0);
    CHECK(ynode_apply_delta(text, txn, &ops_delta) == ERR_READ_ONLY);
    CHECK_STR(ytransaction_root_refs(txn), "[\"text\"]");
    ytransaction_commit(txn, NULL, NULL);
    ydoc_destroy(doc);
}

TEST_CASE("Array insert JSON and nested node") {
    YDoc *doc = ydoc("{\"client_id\":1}");
    YTransaction *txn = ydoc_write_transaction(doc, none());
    YNode *array = ynode(txn, "\"array\"");

    YDeltaOp nested_ops[] = {ydelta_insert_text("nested", NULL)};
    YDelta nested = ydelta(NULL, nested_ops, 1, NULL, 0);
    YIn items[] = {yin_json("1"), yin_json("{\"a\":\"b\"}"), yin_node(&nested)};
    YDeltaOp ops[] = {ydelta_insert(items, 3, NULL)};
    YDelta ops_delta = ydelta(NULL, ops, 1, NULL, 0);
    REQUIRE(ynode_apply_delta(array, txn, &ops_delta) == 0);

    CHECK(ynode_len(array, txn) == 3);
    CHECK_STR(ynode_get_json(array, txn, 0), "1");
    CHECK_STR(ynode_get_json(array, txn, 1), "{\"a\":\"b\"}");
    CHECK_CONTAINS(ynode_get_json(array, txn, 2), "{\"type\":\"delta\",\"id\":{");
    CHECK(ynode_get_json(array, txn, 3) == NULL);

    char id[64];
    node_id(ynode_get_json(array, txn, 2), id, sizeof(id));
    YNode *child = ynode(txn, id);
    REQUIRE(child != NULL);
    CHECK_STR(ynode_string(child, txn), "nested");
    CHECK(ynode_parent(child, txn) == array);

    CHECK_CONTAINS(ynode_json(array, txn, 1, 0),
                   "\"children\":[{\"type\":\"insert\",\"insert\":\"nested\"}]");
    CHECK_STR(ytransaction_json_path(txn, "$.array[1].a"), "[\"b\"]");
    CHECK(ytransaction_json_path(txn, "$[") == NULL);
    ytransaction_commit(txn, NULL, NULL);
    ydoc_destroy(doc);
}

TEST_CASE("Invalid delta is not applied") {
    YDoc *doc = ydoc(NULL);
    YTransaction *txn = ydoc_write_transaction(doc, none());
    YNode *array = ynode(txn, "\"array\"");

    YIn items[] = {yin_json("1"), yin_json("{invalid")};
    YDeltaOp ops[] = {ydelta_insert(items, 2, NULL)};
    YDelta ops_delta = ydelta(NULL, ops, 1, NULL, 0);
    CHECK(ynode_apply_delta(array, txn, &ops_delta) == ERR_DELTA_CONVERSION);
    YDeltaOp bad_format[] = {ydelta_insert_text("a", "[1]")};
    YDelta bad_format_delta = ydelta(NULL, bad_format, 1, NULL, 0);
    CHECK(ynode_apply_delta(array, txn, &bad_format_delta) == ERR_DELTA_CONVERSION);
    YDeltaOp bad_tag[] = {ydelta_remove(1)};
    bad_tag[0].tag = 99;
    YDelta bad_tag_delta = ydelta(NULL, bad_tag, 1, NULL, 0);
    CHECK(ynode_apply_delta(array, txn, &bad_tag_delta) == ERR_DELTA_CONVERSION);
    CHECK(ynode_len(array, txn) == 0);
    ytransaction_commit(txn, NULL, NULL);
    ydoc_destroy(doc);
}

TEST_CASE("Map attributes") {
    YDoc *doc = ydoc("{\"client_id\":1}");
    YTransaction *txn = ydoc_write_transaction(doc, none());
    YNode *map = ynode(txn, "\"map\"");

    YAttrOp attrs[] = {
        yattr_update("a", yin_json("\"x\"")),
        yattr_update("b", yin_json("true")),
        yattr_update("inner", yin_node(NULL)),
    };
    YDelta attrs_delta = ydelta(NULL, NULL, 0, attrs, 3);
    REQUIRE(ynode_apply_delta(map, txn, &attrs_delta) == 0);
    CHECK(ynode_attr_len(map, txn) == 3);
    CHECK_STR(ynode_attr_json(map, txn, "a"), "\"x\"");
    CHECK_STR(ynode_attr_json(map, txn, "b"), "true");
    CHECK(ynode_attr_json(map, txn, "missing") == NULL);

    YAttrOp remove[] = {yattr_remove("a")};
    YDelta remove_delta = ydelta(NULL, NULL, 0, remove, 1);
    REQUIRE(ynode_apply_delta(map, txn, &remove_delta) == 0);
    CHECK(ynode_attr_json(map, txn, "a") == NULL);
    CHECK(ynode_attr_len(map, txn) == 2);

    YAttrOp inner_ops[] = {yattr_update("k", yin_json("\"v\""))};
    YDelta inner = ydelta(NULL, NULL, 0, inner_ops, 1);
    YAttrOp modify[] = {yattr_modify("inner", &inner)};
    YDelta modify_delta = ydelta(NULL, NULL, 0, modify, 1);
    REQUIRE(ynode_apply_delta(map, txn, &modify_delta) == 0);
    char id[64];
    node_id(ynode_attr_json(map, txn, "inner"), id, sizeof(id));
    YNode *inner_node = ynode(txn, id);
    REQUIRE(inner_node != NULL);
    CHECK_STR(ynode_attr_json(inner_node, txn, "k"), "\"v\"");
    CHECK_STR(ytransaction_json_path(txn, "$.map.inner.k"), "[\"v\"]");
    ytransaction_commit(txn, NULL, NULL);
    ydoc_destroy(doc);
}

TEST_CASE("XML element") {
    YDoc *doc = ydoc("{\"client_id\":1}");
    YTransaction *txn = ydoc_write_transaction(doc, none());
    YNode *root = ynode(txn, "\"xml\"");

    YDeltaOp text_ops[] = {ydelta_insert_text("hi", NULL)};
    YAttrOp attrs[] = {yattr_update("class", yin_json("\"bold\""))};
    YDelta p = ydelta("p", text_ops, 1, attrs, 1);
    YIn items[] = {yin_node(&p)};
    YDeltaOp ops[] = {ydelta_insert(items, 1, NULL)};
    YDelta ops_delta = ydelta(NULL, ops, 1, NULL, 0);
    REQUIRE(ynode_apply_delta(root, txn, &ops_delta) == 0);

    char *json = ynode_json(root, txn, 1, 0);
    REQUIRE(json != NULL);
    CHECK(strstr(json, "\"name\":\"p\"") != NULL);
    CHECK(strstr(json, "\"class\":{\"type\":\"insert\",\"value\":\"bold\"}") != NULL);
    ystring_destroy(json);
    ytransaction_commit(txn, NULL, NULL);
    ydoc_destroy(doc);
}

static void on_subdocs(void *state, const char *json) {
    captured_push((Captured *) state, bin(json));
}

static const YReporterVTable SUBDOCS = {.subdocs = on_subdocs};

TEST_CASE("Subdocuments") {
    YDoc *doc = ydoc("{\"client_id\":1}");
    Captured events = {0};

    YDoc *sub = ydoc("{\"guid\":\"sub-guid\"}");
    YTransaction *txn = ydoc_write_transaction(doc, none());
    YNode *array = ynode(txn, "\"docs\"");
    YIn items[] = {yin_doc(sub)};
    YDeltaOp ops[] = {ydelta_insert(items, 1, NULL)};
    YDelta ops_delta = ydelta(NULL, ops, 1, NULL, 0);
    REQUIRE(ynode_apply_delta(array, txn, &ops_delta) == 0);
    CHECK_STR(ynode_get_json(array, txn, 0), "{\"type\":\"doc\",\"guid\":\"sub-guid\"}");
    CHECK_STR(ytransaction_subdocs(txn), "[\"sub-guid\"]");
    YDoc *live = ytransaction_subdoc(txn, "sub-guid");
    REQUIRE(live != NULL);
    CHECK(ytransaction_subdoc(txn, "missing") == NULL);

    // a failed conversion doesn't consume the doc
    YDoc *other = ydoc(NULL);
    YIn bad[] = {yin_doc(other), yin_json("{invalid")};
    YDeltaOp bad_ops[] = {ydelta_insert(bad, 2, NULL)};
    YDelta bad_ops_delta = ydelta(NULL, bad_ops, 1, NULL, 0);
    CHECK(ynode_apply_delta(array, txn, &bad_ops_delta) == ERR_DELTA_CONVERSION);
    ydoc_destroy(other);
    ytransaction_commit(txn, &SUBDOCS, &events);

    REQUIRE(events.len == 1);
    CHECK(strstr((const char *) events.items[0].ptr, "\"added\":[\"sub-guid\"]") != NULL);
    captured_free(&events);

    insert_text(live, "\"text\"", 0, "inside");
    CHECK_STR(read_string(live, "\"text\""), "inside");
    ydoc_destroy(doc);
}

static void on_update(void *state, YTransaction *txn, YBinary update) {
    captured_push((Captured *) state, update);
}

static const YReporterVTable UPDATES_V1 = {.update_v1 = on_update};
static const YReporterVTable UPDATES_V2 = {.update_v2 = on_update};

TEST_CASE("Sync two docs") {
    YDoc *d1 = ydoc("{\"client_id\":1}");
    YDoc *d2 = ydoc("{\"client_id\":2}");
    insert_text(d1, "\"text\"", 0, "hello");
    insert_text(d2, "\"text\"", 0, "world");

    sync(d1, d2);
    sync(d2, d1);
    check_same_text(d1, d2, "\"text\"");
    char *merged = read_string(d1, "\"text\"");
    CHECK(strlen(merged) == 10);
    ystring_destroy(merged);

    // incremental update reported by commit, applied with v2
    Captured updates = {0};
    insert_text(d1, "\"text\"", 0, "!", &UPDATES_V2, &updates);
    REQUIRE(updates.len == 1);
    YTransaction *txn = ydoc_write_transaction(d2, none());
    CHECK(ytransaction_apply_v2(txn, updates.items[0]) == 0);
    CHECK(ytransaction_apply(txn, bin("\xff\xff")) != 0);
    ytransaction_commit(txn, NULL, NULL);
    check_same_text(d1, d2, "\"text\"");
    captured_free(&updates);

    ydoc_destroy(d1);
    ydoc_destroy(d2);
}

TEST_CASE("Pending update") {
    YDoc *d1 = ydoc("{\"client_id\":1}");
    YDoc *d2 = ydoc("{\"client_id\":2}");
    Captured updates = {0};
    insert_text(d1, "\"text\"", 0, "a", &UPDATES_V1, &updates);
    insert_text(d1, "\"text\"", 1, "b", &UPDATES_V1, &updates);
    REQUIRE(updates.len == 2);

    YTransaction *txn = ydoc_write_transaction(d2, none());
    CHECK(ytransaction_apply(txn, updates.items[1]) == 0);
    YBinary pending = ytransaction_pending_update(txn);
    CHECK(pending.len > 0);
    ybinary_destroy(pending);
    ytransaction_commit(txn, NULL, NULL);
    captured_free(&updates);

    ydoc_destroy(d1);
    ydoc_destroy(d2);
}

TEST_CASE("Snapshot") {
    YDoc *doc = ydoc("{\"client_id\":1,\"skip_gc\":true}");
    insert_text(doc, "\"text\"", 0, "hello");
    YTransaction *txn = ydoc_read_transaction(doc);
    YBinary snapshot = ytransaction_snapshot(txn);
    ytransaction_commit(txn, NULL, NULL);
    insert_text(doc, "\"text\"", 5, " world");

    txn = ydoc_read_transaction(doc);
    YBinary update = ytransaction_encode_state_from_snapshot_v1(txn, snapshot);
    ytransaction_commit(txn, NULL, NULL);

    YDoc *past = ydoc(NULL);
    txn = ydoc_write_transaction(past, none());
    CHECK(ytransaction_apply(txn, update) == 0);
    ytransaction_commit(txn, NULL, NULL);
    CHECK_STR(read_string(past, "\"text\""), "hello");

    ybinary_destroy(update);
    ybinary_destroy(snapshot);
    ydoc_destroy(past);
    ydoc_destroy(doc);
}

// Changes read in `committing`, strings owned until `event_free`.
typedef struct Event {
    YNode *node;
    int calls;
    char *delta;
    char *keys;
    char *path;
    char *within;
    char *all;
    uint8_t writeable;
} Event;

static void event_free(Event *e) {
    ystring_destroy(e->delta);
    ystring_destroy(e->keys);
    ystring_destroy(e->path);
    ystring_destroy(e->within);
    ystring_destroy(e->all);
    e->delta = e->keys = e->path = e->within = e->all = NULL;
}

// Captures changes of `target`.
static void capture(Event *e, YTransaction *txn, YNode *target) {
    e->calls++;
    e->delta = ytransaction_delta(txn, target, 0);
    e->keys = ytransaction_keys_changed(txn, target);
    e->path = ynode_path(target, txn);
    e->writeable = ytransaction_writeable(txn);
}

static void on_node(void *state, YTransaction *txn) {
    Event *e = (Event *) state;
    event_free(e);
    capture(e, txn, e->node);
}

// Captures nodes modified within `e->node` and changes of the first one.
static void on_deep(void *state, YTransaction *txn) {
    Event *e = (Event *) state;
    event_free(e);
    e->all = ytransaction_modified(txn, NULL);
    e->within = ytransaction_modified(txn, e->node);
    char id[64];
    size_t len = strlen(e->within) - 2; // strip [ ]
    if (len >= sizeof(id)) {
        return;
    }
    memcpy(id, e->within + 1, len);
    id[len] = 0;
    YNode *target = ynode(txn, id);
    if (target != NULL) {
        capture(e, txn, target);
    }
}

static const YReporterVTable NODE = {.committing = on_node};
static const YReporterVTable DEEP = {.committing = on_deep};

TEST_CASE("Observe node") {
    YDoc *doc = ydoc("{\"client_id\":1}");
    YTransaction *txn = ydoc_write_transaction(doc, none());
    Event c = {ynode(txn, "\"text\"")};
    ytransaction_commit(txn, NULL, NULL);

    insert_text(doc, "\"text\"", 0, "abc", &NODE, &c);
    CHECK(c.calls == 1);
    CHECK(strcmp(c.delta, "{\"children\":[{\"type\":\"insert\",\"insert\":\"abc\"}]}") == 0);
    CHECK(strcmp(c.keys, "[null]") == 0);
    CHECK(strcmp(c.path, "$.text") == 0);
    CHECK(c.writeable == 0);

    // deleted content still renders before GC
    txn = ydoc_write_transaction(doc, none());
    YDeltaOp remove[] = {ydelta_retain(1, NULL), ydelta_remove(2)};
    YDelta remove_delta = ydelta(NULL, remove, 2, NULL, 0);
    REQUIRE(ynode_apply_delta(c.node, txn, &remove_delta) == 0);
    ytransaction_commit(txn, &NODE, &c);
    CHECK(c.calls == 2);
    CHECK(strcmp(c.delta, "{\"children\":[{\"type\":\"retain\",\"retain\":1},"
                          "{\"type\":\"delete\",\"delete\":2}]}") == 0);
    event_free(&c);
    ydoc_destroy(doc);
}

TEST_CASE("Observe deep") {
    YDoc *doc = ydoc("{\"client_id\":1}");
    YTransaction *txn = ydoc_write_transaction(doc, none());
    YNode *map = ynode(txn, "\"map\"");
    YAttrOp attrs[] = {yattr_update("inner", yin_node(NULL))};
    YDelta attrs_delta = ydelta(NULL, NULL, 0, attrs, 1);
    REQUIRE(ynode_apply_delta(map, txn, &attrs_delta) == 0);
    ytransaction_commit(txn, NULL, NULL);

    Event c = {map};
    txn = ydoc_write_transaction(doc, none());
    YAttrOp inner_ops[] = {yattr_update("k", yin_json("1"))};
    YDelta inner = ydelta(NULL, NULL, 0, inner_ops, 1);
    YAttrOp modify[] = {yattr_modify("inner", &inner)};
    YDelta modify_delta = ydelta(NULL, NULL, 0, modify, 1);
    REQUIRE(ynode_apply_delta(map, txn, &modify_delta) == 0);
    YNode *other = ynode(txn, "\"other\"");
    REQUIRE(ynode_apply_delta(other, txn, &inner) == 0);
    ytransaction_commit(txn, &DEEP, &c);

    CHECK(c.calls == 1);
    CHECK(strstr(c.all, "\"other\"") != NULL);
    CHECK(strstr(c.within, "\"other\"") == NULL);
    CHECK(strcmp(c.path, "$.map.inner") == 0);
    txn = ydoc_read_transaction(doc);
    CHECK_CONTAINS(ytransaction_json_path(txn, c.path), "[{\"type\":\"delta\",\"id\":{");
    ytransaction_commit(txn, NULL, NULL);
    CHECK(strcmp(c.keys, "[\"k\"]") == 0);
    CHECK(strcmp(c.delta, "{\"attrs\":{\"k\":{\"type\":\"insert\",\"value\":1}}}") == 0);
    event_free(&c);
    ydoc_destroy(doc);
}

// Order of reporter callbacks, one letter per call.
typedef struct Calls {
    char order[16];
    uint32_t len;
} Calls;

static void push_call(void *state, char c) {
    Calls *calls = (Calls *) state;
    if (calls->len < sizeof(calls->order) - 1) {
        calls->order[calls->len++] = c;
    }
}

static void call_committing(void *state, YTransaction *txn) {
    push_call(state, 'c');
}

static void call_update_v1(void *state, YTransaction *txn, YBinary update) {
    push_call(state, '1');
}

static void call_update_v2(void *state, YTransaction *txn, YBinary update) {
    push_call(state, '2');
}

static void call_subdocs(void *state, const char *json) {
    push_call(state, 's');
}

static void call_committed(void *state, YTransaction *txn) {
    push_call(state, 'C');
}

static const YReporterVTable CALLS = {
    call_committing, call_update_v1, call_update_v2, call_subdocs, call_committed,
};

TEST_CASE("Partial vtable") {
    YDoc *doc = ydoc(NULL);
    static const YReporterVTable partial = {.update_v1 = call_update_v1,
                                            .committed = call_committed};
    Calls calls = {0};
    insert_text(doc, "\"text\"", 0, "abc", &partial, &calls);
    CHECK(strcmp(calls.order, "1C") == 0);
    ydoc_destroy(doc);
}

TEST_CASE("Read-only commit ignores vtable") {
    YDoc *doc = ydoc(NULL);
    Calls calls = {0};
    ytransaction_commit(ydoc_read_transaction(doc), &CALLS, &calls);
    CHECK(calls.len == 0);
    ydoc_destroy(doc);
}

TEST_CASE("Empty commit skips updates") {
    YDoc *doc = ydoc(NULL);
    Calls calls = {0};
    ytransaction_commit(ydoc_write_transaction(doc, bin("origin")), &CALLS, &calls);
    CHECK(strcmp(calls.order, "cC") == 0);
    ydoc_destroy(doc);
}

// Transaction origin and locality seen by an update callback.
typedef struct Meta {
    char origin[16];
    uint8_t local;
} Meta;

static void on_meta(void *state, YTransaction *txn, YBinary update) {
    Meta *m = (Meta *) state;
    YBinary origin = ytransaction_origin(txn);
    if (origin.len < sizeof(m->origin)) {
        memcpy(m->origin, origin.ptr, origin.len);
        m->origin[origin.len] = 0;
    }
    m->local = ytransaction_local(txn);
}

static const YReporterVTable META = {.update_v1 = on_meta};

TEST_CASE("Update reads origin") {
    YDoc *d1 = ydoc("{\"client_id\":1}");
    Meta meta = {"-", 0};
    YTransaction *txn = ydoc_write_transaction(d1, bin("my-origin"));
    YDeltaOp ops[] = {ydelta_insert_text("abc", NULL)};
    YDelta ops_delta = ydelta(NULL, ops, 1, NULL, 0);
    REQUIRE(ynode_apply_delta(ynode(txn, "\"text\""), txn, &ops_delta) == 0);
    YBinary update = ytransaction_state_diff_v1(txn, none());
    ytransaction_commit(txn, &META, &meta);
    CHECK(strcmp(meta.origin, "my-origin") == 0);
    CHECK(meta.local == 1);

    YDoc *d2 = ydoc("{\"client_id\":2}");
    txn = ydoc_write_transaction(d2, none());
    REQUIRE(ytransaction_apply(txn, update) == 0);
    ytransaction_commit(txn, &META, &meta);
    CHECK(strcmp(meta.origin, "") == 0);
    CHECK(meta.local == 0);
    ybinary_destroy(update);
    ydoc_destroy(d1);
    ydoc_destroy(d2);
}

TEST_CASE("Destroy") {
    YDoc *doc = ydoc(NULL);
    YTransaction *txn = ydoc_write_transaction(doc, none());
    YIn items[] = {yin_doc(ydoc("{\"guid\":\"sub-guid\"}"))};
    YDeltaOp ops[] = {ydelta_insert(items, 1, NULL)};
    YDelta ops_delta = ydelta(NULL, ops, 1, NULL, 0);
    REQUIRE(ynode_apply_delta(ynode(txn, "\"docs\""), txn, &ops_delta) == 0);
    ytransaction_commit(txn, NULL, NULL);

    Captured events = {0};
    txn = ydoc_write_transaction(doc, none());
    ydoc_clear(ytransaction_subdoc(txn, "sub-guid"), txn);
    ytransaction_commit(txn, &SUBDOCS, &events);
    REQUIRE(events.len == 1);
    CHECK(strstr((const char *) events.items[0].ptr, "\"removed\":[\"sub-guid\"]") != NULL);
    captured_free(&events);
    ydoc_destroy(doc);
}

TEST_CASE("Sticky index") {
    YDoc *doc = ydoc("{\"client_id\":1}");
    insert_text(doc, "\"text\"", 0, "hello world");

    YTransaction *txn = ydoc_read_transaction(doc);
    YNode *text = ynode(txn, "\"text\"");
    YBinary index = ysticky_index_from_index(text, txn, 6, 0);
    REQUIRE(index.len > 0);
    CHECK(ysticky_index_from_index(text, txn, 100, 0).len == 0);
    ytransaction_commit(txn, NULL, NULL);

    insert_text(doc, "\"text\"", 0, "say ");
    txn = ydoc_read_transaction(doc);
    CHECK_STR(ysticky_index_read(index, txn), "{\"id\":\"text\",\"index\":10}");
    CHECK(ysticky_index_read(bin("garbage"), txn) == NULL);
    ytransaction_commit(txn, NULL, NULL);
    ybinary_destroy(index);
    ydoc_destroy(doc);
}

TEST_CASE("Weak links") {
    YDoc *doc = ydoc("{\"client_id\":1}");
    YTransaction *txn = ydoc_write_transaction(doc, none());
    YNode *map = ynode(txn, "\"map\"");
    YAttrOp attrs[] = {yattr_update("a", yin_json("\"value\""))};
    YDelta attrs_delta = ydelta(NULL, NULL, 0, attrs, 1);
    REQUIRE(ynode_apply_delta(map, txn, &attrs_delta) == 0);
    YNode *text = ynode(txn, "\"text\"");
    YDeltaOp text_ops[] = {ydelta_insert_text("abcdef", NULL)};
    YDelta text_ops_delta = ydelta(NULL, text_ops, 1, NULL, 0);
    REQUIRE(ynode_apply_delta(text, txn, &text_ops_delta) == 0);

    char *link = ynode_link(map, txn, "a");
    char *quote = ynode_quote(text, txn, 1, 3, 0, 1);
    REQUIRE(link != NULL);
    REQUIRE(quote != NULL);
    CHECK(ynode_link(map, txn, "missing") == NULL);
    CHECK(ynode_quote(text, txn, 1, 100, 0, 0) == NULL);

    YNode *array = ynode(txn, "\"array\"");
    YDelta link_delta = ydelta_link(link);
    YDelta quote_delta = ydelta_link(quote);
    YIn items[] = {yin_node(&link_delta), yin_node(&quote_delta)};
    YDeltaOp ops[] = {ydelta_insert(items, 2, NULL)};
    YDelta ops_delta = ydelta(NULL, ops, 1, NULL, 0);
    REQUIRE(ynode_apply_delta(array, txn, &ops_delta) == 0);
    CHECK(ynode_len(array, txn) == 2);
    ytransaction_commit(txn, NULL, NULL);

    txn = ydoc_read_transaction(doc);
    char expected[256];
    snprintf(expected, sizeof(expected), "\"link\":%s", link);
    CHECK_CONTAINS(ynode_json(array, txn, 1, 0), expected);
    snprintf(expected, sizeof(expected), "\"link\":%s", quote);
    CHECK_CONTAINS(ynode_json(array, txn, 1, 0), expected);
    ytransaction_commit(txn, NULL, NULL);
    ystring_destroy(link);
    ystring_destroy(quote);
    ydoc_destroy(doc);
}

static void on_track(void *state, YTransaction *txn) {
    yundo_manager_track((YUndoManager *) state, txn);
}

static const YReporterVTable TRACK = {.committing = on_track};

// Runs undo (or redo) in its own transaction, returning the reverted item.
static YStackItem *undo_redo(YDoc *doc, YUndoManager *mgr, bool undo) {
    YTransaction *txn = ydoc_write_transaction(doc, none());
    YStackItem *item = NULL;
    REQUIRE((undo ? yundo_manager_undo : yundo_manager_redo)(mgr, txn, &item) == 0);
    ytransaction_commit(txn, NULL, NULL);
    REQUIRE(item != NULL);
    return item;
}

TEST_CASE("Undo manager") {
    YDoc *doc = ydoc("{\"client_id\":1,\"guid\":\"undo-doc\"}");
    YUndoManager *mgr = yundo_manager();
    YTransaction *txn = ydoc_write_transaction(doc, none());
    yundo_manager_expand_scope(mgr, txn, ynode(txn, "\"text\""));
    ytransaction_commit(txn, NULL, NULL);

    insert_text(doc, "\"other\"", 0, "x", &TRACK, mgr);
    CHECK(yundo_manager_len(mgr, Y_UNDO_STACK) == 0); // out of scope

    int m1, m2;
    insert_text(doc, "\"text\"", 0, "a", &TRACK, mgr);
    CHECK(yundo_manager_capture(mgr, &m1) == 1);
    insert_text(doc, "\"text\"", 1, "b", &TRACK, mgr);
    CHECK(yundo_manager_capture(mgr, &m2) == 1);
    CHECK(yundo_manager_capture(mgr, &m2) == 0); // nothing tracked since
    CHECK(yundo_manager_len(mgr, Y_UNDO_STACK) == 2);

    YStackItem *item = undo_redo(doc, mgr, true);
    CHECK(item->meta == &m2);
    CHECK(strcmp(item->doc, "undo-doc") == 0);
    ystack_item_destroy(item);
    CHECK_STR(read_string(doc, "\"text\""), "a");
    item = undo_redo(doc, mgr, true);
    CHECK(item->meta == &m1);
    ystack_item_destroy(item);
    CHECK_STR(read_string(doc, "\"text\""), "");
    CHECK(yundo_manager_len(mgr, Y_REDO_STACK) == 2);

    item = undo_redo(doc, mgr, false);
    CHECK(item->meta == NULL); // redo stack items carry no meta
    ystack_item_destroy(item);
    CHECK_STR(read_string(doc, "\"text\""), "a");
    ystack_item_destroy(undo_redo(doc, mgr, false));
    CHECK_STR(read_string(doc, "\"text\""), "ab");

    txn = ydoc_write_transaction(doc, none());
    CHECK(yundo_manager_clear(mgr, txn, Y_UNDO_STACK | Y_REDO_STACK) == 1);
    ytransaction_commit(txn, NULL, NULL);
    yundo_manager_destroy(mgr);
    ydoc_destroy(doc);
}
