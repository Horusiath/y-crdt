use crate::block::{Block, BlockPtr, Item, ItemContent};
use crate::id_set::{DeleteSet, IdSet};
use crate::store::Store;
use crate::types::TypePtr;
use crate::update::Update;
use crate::updates::decoder::{Decode, DecoderV1};
use crate::updates::encoder::Encode;
use crate::{BlockStore, Doc, ID};
use lib0::any::Any;
use lib0::decoding::Cursor;
use std::cell::Cell;
use std::rc::Rc;

#[test]
fn text_insert_delete() {
    /** Generated via:
        ```js
           const doc = new Y.Doc()
           const ytext = doc.getText('type')
           doc.transact(function () {
               ytext.insert(0, 'def')
               ytext.insert(0, 'abc')
               ytext.insert(6, 'ghi')
               ytext.delete(2, 5)
           })
           const update = Y.encodeStateAsUpdate(doc)
           ytext.toString() // => 'abhi'
        ```
        This way we confirm that we can decode and apply:
        1. blocks without left/right origin consisting of multiple characters
        2. blocks with left/right origin consisting of multiple characters
        3. delete sets
    */
    let update = &[
        1, 5, 152, 234, 173, 126, 0, 1, 1, 4, 116, 121, 112, 101, 3, 68, 152, 234, 173, 126, 0, 2,
        97, 98, 193, 152, 234, 173, 126, 4, 152, 234, 173, 126, 0, 1, 129, 152, 234, 173, 126, 2,
        1, 132, 152, 234, 173, 126, 6, 2, 104, 105, 1, 152, 234, 173, 126, 2, 0, 3, 5, 2,
    ];
    const CLIENT_ID: u64 = 264992024;
    let expected_blocks = vec![
        Block::Item(Item {
            id: ID::new(CLIENT_ID, 0),
            left: None,
            right: None,
            origin: None,
            right_origin: None,
            content: ItemContent::Deleted(3),
            parent: TypePtr::Named("type".to_string()),
            parent_sub: None,
            deleted: false,
        }),
        Block::Item(Item {
            id: ID::new(CLIENT_ID, 3),
            left: None,
            right: None,
            origin: None,
            right_origin: Some(ID::new(CLIENT_ID, 0)),
            content: ItemContent::String("ab".to_string()),
            parent: TypePtr::Id(BlockPtr::new(ID::new(CLIENT_ID, 0), 0)),
            parent_sub: None,
            deleted: false,
        }),
        Block::Item(Item {
            id: ID::new(CLIENT_ID, 5),
            left: None,
            right: None,
            origin: Some(ID::new(CLIENT_ID, 4)),
            right_origin: Some(ID::new(CLIENT_ID, 0)),
            content: ItemContent::Deleted(1),
            parent: TypePtr::Id(BlockPtr::new(ID::new(CLIENT_ID, 4), 4)),
            parent_sub: None,
            deleted: false,
        }),
        Block::Item(Item {
            id: ID::new(CLIENT_ID, 6),
            left: None,
            right: None,
            origin: Some(ID::new(CLIENT_ID, 2)),
            right_origin: None,
            content: ItemContent::Deleted(1),
            parent: TypePtr::Id(BlockPtr::new(ID::new(CLIENT_ID, 2), 2)),
            parent_sub: None,
            deleted: false,
        }),
        Block::Item(Item {
            id: ID::new(CLIENT_ID, 7),
            left: None,
            right: None,
            origin: Some(ID::new(CLIENT_ID, 6)),
            right_origin: None,
            content: ItemContent::String("hi".to_string()),
            parent: TypePtr::Id(BlockPtr::new(ID::new(CLIENT_ID, 6), 6)),
            parent_sub: None,
            deleted: false,
        }),
    ];
    let expected_ds = {
        let mut ds = IdSet::new();
        ds.insert(ID::new(CLIENT_ID, 0), 3);
        ds.insert(ID::new(CLIENT_ID, 5), 2);
        DeleteSet::from(ds)
    };
    let visited = Rc::new(Cell::new(false));
    let setter = visited.clone();

    let mut doc = Doc::new();
    let _sub = doc.on_update(move |e| {
        for (actual, expected) in e.update.blocks().zip(expected_blocks.as_slice()) {
            //println!("{}", actual);
            assert_eq!(actual, expected);
        }
        assert_eq!(&e.delete_set, &expected_ds);
        setter.set(true);
    });
    let mut txn = doc.transact();
    let txt = txn.get_text("type");
    doc.apply_update(&mut txn, update);
    assert_eq!(txt.to_string(&txn), "abhi".to_string());
    assert!(visited.get());
}

#[test]
fn map_set() {
    /* Generated via:
        ```js
           const doc = new Y.Doc()
           const x = doc.getMap('test')
           x.set('k1', 'v1')
           x.set('k2', 'v2')
           const update = Y.encodeStateAsUpdate(doc)
           console.log(update);
        ```
    */
    let original = &[
        1, 2, 183, 229, 212, 163, 3, 0, 40, 1, 4, 116, 101, 115, 116, 2, 107, 49, 1, 119, 2, 118,
        49, 40, 1, 4, 116, 101, 115, 116, 2, 107, 50, 1, 119, 2, 118, 50, 0,
    ];
    const CLIENT_ID: u64 = 880095927;
    let expected = &[
        &Block::Item(Item {
            id: ID::new(CLIENT_ID, 0),
            left: None,
            right: None,
            origin: None,
            right_origin: None,
            content: ItemContent::Any(vec![Any::String("v1".to_string())]),
            parent: TypePtr::Named("test".to_string()),
            parent_sub: Some("k1".to_string()),
            deleted: false,
        }),
        &Block::Item(Item {
            id: ID::new(CLIENT_ID, 1),
            left: None,
            right: None,
            origin: None,
            right_origin: None,
            content: ItemContent::Any(vec![Any::String("v2".to_string())]),
            parent: TypePtr::Named("test".to_string()),
            parent_sub: Some("k2".to_string()),
            deleted: false,
        }),
    ];
    let u = Update::decode_v1(original);
    let blocks: Vec<&Block> = u.blocks().collect();
    assert_eq!(blocks.as_slice(), expected);

    let store: Store = u.into();
    let serialized = store.encode_v1();
    assert_eq!(serialized, original);
}

#[test]
fn array_insert() {
    /* Generated via:
        ```js
           const doc = new Y.Doc()
           const x = doc.getArray('test')
           x.push(['a']);
           x.push(['b']);
           const update = Y.encodeStateAsUpdate(doc)
           console.log(update);
        ```
    */
    let original = &[
        1, 1, 199, 195, 202, 51, 0, 8, 1, 4, 116, 101, 115, 116, 2, 119, 1, 97, 119, 1, 98, 0,
    ];
    const CLIENT_ID: u64 = 108175815;
    let expected = &[&Block::Item(Item {
        id: ID::new(CLIENT_ID, 0),
        left: None,
        right: None,
        origin: None,
        right_origin: None,
        content: ItemContent::Any(vec![
            Any::String("a".to_string()),
            Any::String("b".to_string()),
        ]),
        parent: TypePtr::Named("test".to_string()),
        parent_sub: None,
        deleted: false,
    })];
    let u = Update::decode_v1(original);
    let blocks: Vec<&Block> = u.blocks().collect();
    assert_eq!(blocks.as_slice(), expected);

    let store: Store = u.into();
    let serialized = store.encode_v1();
    assert_eq!(serialized, original);
}
