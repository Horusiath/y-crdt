use crate::event::EventHandler;
use crate::id_set::IdSet;
use crate::store::Store;
use crate::types::BranchRef;
use crate::{DeleteSet, Subscription, SubscriptionId, Transaction};
use std::collections::HashSet;
use std::time::Duration;

pub struct UndoManager {
    events: EventHandler<Event>,
    scope: HashSet<BranchRef>,
    options: Options,
    undo_stack: Vec<StackItem>,
    redo_stack: Vec<StackItem>,
    undoing: bool,
    redoing: bool,
    last_change: usize,
}

impl UndoManager {
    pub(crate) fn with_options(
        store: &mut Store,
        scope: HashSet<BranchRef>,
        options: Options,
    ) -> Self {
        UndoManager {
            scope,
            options,
            events: EventHandler::new(),
            undo_stack: Vec::default(),
            redo_stack: Vec::default(),
            undoing: false,
            redoing: false,
            last_change: 0,
        }
    }

    pub fn subscribe<F>(&mut self, f: F) -> Subscription<Event>
    where
        F: Fn(&Transaction, &Event) -> () + 'static,
    {
        self.events.subscribe(f)
    }

    pub fn unsubscribe(&mut self, subscription_id: SubscriptionId) {
        self.events.unsubscribe(subscription_id)
    }

    pub fn clear(&mut self, txn: &mut Transaction) {
        fn clear_item(txn: &mut Transaction, item: StackItem) {
            //        iterateDeletedStructs(transaction, stackItem.deletions, item => {
            //            if (item instanceof Item && this.scope.some(type => isParentOf(type, item))) {
            //            keepItem(item, false)
            //            }
            //        })
            todo!()
        }
        for item in self.undo_stack.drain(..) {
            clear_item(txn, item);
        }
        for item in self.redo_stack.drain(..) {
            clear_item(txn, item);
        }
    }

    pub fn stop(&mut self) {
        self.last_change = 0;
    }

    pub fn undo(&mut self, txn: &mut Transaction) -> Option<&StackItem> {
        self.undoing = true;
        let result = Self::pop(
            txn,
            &self.scope,
            &mut self.events,
            &mut self.undo_stack,
            true,
        );
        self.undoing = false;
        result
    }

    pub fn redo(&mut self, txn: &mut Transaction) -> Option<&StackItem> {
        self.redoing = true;
        let result = Self::pop(
            txn,
            &self.scope,
            &mut self.events,
            &mut self.redo_stack,
            false,
        );
        self.redoing = false;
        result
    }

    fn after_transaction(&mut self, txn: &Transaction) {
        //// Only track certain transactions
        //if (!this.scope.some(type => transaction.changedParentTypes.has(type)) || (!this.trackedOrigins.has(transaction.origin) && (!transaction.origin || !this.trackedOrigins.has(transaction.origin.constructor)))) {
        //    return
        //}
        //const undoing = this.undoing
        //const redoing = this.redoing
        //const stack = undoing ? this.redoStack : this.undoStack
        //if (undoing) {
        //    this.stopCapturing() // next undo should not be appended to last stack item
        //} else if (!redoing) {
        //    // neither undoing nor redoing: delete redoStack
        //    this.redoStack = []
        //}
        //const insertions = new DeleteSet()
        //transaction.afterState.forEach((endClock, client) => {
        //    const startClock = transaction.beforeState.get(client) || 0
        //    const len = endClock - startClock
        //    if (len > 0) {
        //        addToDeleteSet(insertions, client, startClock, len)
        //    }
        //})
        //const now = time.getUnixTime()
        //if (now - this.lastChange < captureTimeout && stack.length > 0 && !undoing && !redoing) {
        //    // append change to last stack op
        //    const lastOp = stack[stack.length - 1]
        //    lastOp.deletions = mergeDeleteSets([lastOp.deletions, transaction.deleteSet])
        //    lastOp.insertions = mergeDeleteSets([lastOp.insertions, insertions])
        //} else {
        //    // create a new stack op
        //    stack.push(new StackItem(transaction.deleteSet, insertions))
        //}
        //if (!undoing && !redoing) {
        //    this.lastChange = now
        //}
        //// make sure that deleted structs are not gc'd
        //iterateDeletedStructs(transaction, transaction.deleteSet, /** @param {Item|GC} item */ item => {
        //    if (item instanceof Item && this.scope.some(type => isParentOf(type, item))) {
        //    keepItem(item, true)
        //    }
        //})
        //this.emit('stack-item-added', [{ stackItem: stack[stack.length - 1], origin: transaction.origin, type: undoing ? 'redo' : 'undo', changedParentTypes: transaction.changedParentTypes }, this])
    }

    fn pop<'a>(
        txn: &mut Transaction,
        node: &HashSet<BranchRef>,
        even_handler: &mut EventHandler<Event>,
        stack: &mut Vec<StackItem>,
        is_undo: bool,
    ) -> Option<&'a StackItem> {
        ///**
        // * Whether a change happened
        // * @type {StackItem?}
        // */
        //let result = null
        ///**
        // * Keep a reference to the transaction so we can fire the event with the changedParentTypes
        // * @type {any}
        // */
        //let _tr = null
        //const doc = undoManager.doc
        //const scope = undoManager.scope
        //transact(doc, transaction => {
        //    while (stack.length > 0 && result === null) {
        //        const store = doc.store
        //        const stackItem = /** @type {StackItem} */ (stack.pop())
        //        /**
        //         * @type {Set<Item>}
        //         */
        //        const itemsToRedo = new Set()
        //        /**
        //         * @type {Array<Item>}
        //         */
        //        const itemsToDelete = []
        //        let performedChange = false
        //        iterateDeletedStructs(transaction, stackItem.insertions, struct => {
        //            if (struct instanceof Item) {
        //                if (struct.redone !== null) {
        //                    let { item, diff } = followRedone(store, struct.id)
        //                    if (diff > 0) {
        //                        item = getItemCleanStart(transaction, createID(item.id.client, item.id.clock + diff))
        //                    }
        //                    struct = item
        //                }
        //                if (!struct.deleted && scope.some(type => isParentOf(type, /** @type {Item} */ (struct)))) {
        //                itemsToDelete.push(struct)
        //                }
        //            }
        //        })
        //        iterateDeletedStructs(transaction, stackItem.deletions, struct => {
        //            if (
        //            struct instanceof Item &&
        //                scope.some(type => isParentOf(type, struct)) &&
        //                // Never redo structs in stackItem.insertions because they were created and deleted in the same capture interval.
        //                !isDeleted(stackItem.insertions, struct.id)
        //            ) {
        //            itemsToRedo.add(struct)
        //            }
        //        })
        //        itemsToRedo.forEach(struct => {
        //            performedChange = redoItem(transaction, struct, itemsToRedo, itemsToDelete) !== null || performedChange
        //        })
        //        // We want to delete in reverse order so that children are deleted before
        //        // parents, so we have more information available when items are filtered.
        //        for (let i = itemsToDelete.length - 1; i >= 0; i--) {
        //            const item = itemsToDelete[i]
        //            if (undoManager.deleteFilter(item)) {
        //                item.delete(transaction)
        //                performedChange = true
        //            }
        //        }
        //        result = performedChange ? stackItem : null
        //    }
        //    transaction.changed.forEach((subProps, type) => {
        //    // destroy search marker if necessary
        //    if (subProps.has(null) && type._searchMarker) {
        //    type._searchMarker.length = 0
        //    }
        //    })
        //    _tr = transaction
        //}, undoManager)
        //if (result != null) {
        //    const changedParentTypes = _tr.changedParentTypes
        //    undoManager.emit('stack-item-popped', [{ stackItem: result, type: eventType, changedParentTypes }, undoManager])
        //}
        //return result
        todo!()
    }
}

pub struct Options {
    pub capture_timeout: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            capture_timeout: Duration::from_millis(500),
        }
    }
}

pub struct StackItem {
    pub deletions: DeleteSet,
    pub insertions: IdSet,
    // meta: HashMap
}

pub struct Event {
    pub kind: EventKind,
    pub action: EventAction,
    pub item: StackItem,
}

#[repr(u8)]
pub enum EventKind {
    Added,
    Popped,
}

#[repr(u8)]
pub enum EventAction {
    Undo,
    Redo,
}

#[cfg(test)]
mod test {
    use crate::test_utils::exchange_updates;
    use crate::types::text::Diff;
    use crate::types::{Attrs, Value};
    use crate::undo::EventKind;
    use crate::updates::decoder::Decode;
    use crate::{Doc, Text, Transaction, Update};
    use lib0::any::Any;
    use std::cell::Cell;
    use std::collections::HashMap;

    fn text_run<F, A, B>(doc: &Doc, f: F, a: A, b: B)
    where
        F: Fn(&Text, &mut Transaction, A, B),
    {
        let mut txn = doc.transact();
        let txt = txn.get_text("text");
        f(&txt, &mut txn, a, b);
    }

    #[test]
    fn undo_text() {
        let d1 = Doc::with_client_id(1);
        let txt1 = {
            let mut txn = d1.transact();
            txn.get_text("text")
        };

        let mut mgr = d1.undo_manager([&txt1]);

        // items that are added & deleted in the same transaction won't be undo
        text_run(&d1, Text::insert, 0, "test");
        text_run(&d1, Text::remove_range, 0, 4);
        {
            let mut txn = d1.transact();
            mgr.undo(&mut txn);
            assert_eq!(txt1.to_string(&txn), "");
        }

        // follow redone items
        text_run(&d1, Text::insert, 0, "a");
        mgr.stop();
        text_run(&d1, Text::remove_range, 0, 1);
        mgr.stop();
        {
            let mut txn = d1.transact();
            mgr.undo(&mut txn);
            assert_eq!(txt1.to_string(&txn), "a");
            mgr.undo(&mut txn);
            assert_eq!(txt1.to_string(&txn), "");
        }

        let d2 = Doc::with_client_id(2);
        let txt2 = {
            let mut txn = d2.transact();
            txn.get_text("text")
        };

        text_run(&d1, Text::insert, 0, "abc");
        text_run(&d2, Text::insert, 0, "xyz");
        exchange_updates(&[&d1, &d2]);
        {
            let mut txn = d1.transact();
            mgr.undo(&mut txn);
            assert_eq!(txt1.to_string(&txn), "xyz");
            mgr.redo(&mut txn);
            assert_eq!(txt1.to_string(&txn), "abcxyz");
        }
        exchange_updates(&[&d1, &d2]);
        text_run(&d2, Text::remove_range, 0, 1);
        exchange_updates(&[&d1, &d2]);
        {
            let mut txn = d1.transact();
            mgr.undo(&mut txn);
            assert_eq!(txt1.to_string(&txn), "xyz");
            mgr.redo(&mut txn);
            assert_eq!(txt1.to_string(&txn), "bcxyz");
        }
        // test marks
        let attrs: Attrs = HashMap::from([("bold".into(), Any::Bool(true))]);
        {
            let mut txn = d1.transact();
            txt1.format(&mut txn, 1, 3, attrs.clone());
            assert_eq!(
                txt1.diff(&mut txn),
                vec![
                    Diff::Insert("b".into(), None),
                    Diff::Insert("cxy".into(), Some(Box::new(attrs.clone()))),
                    Diff::Insert("z".into(), None),
                ]
            );
        }
        {
            let mut txn = d1.transact();
            mgr.undo(&mut txn);
            assert_eq!(
                txt1.diff(&mut txn),
                vec![Diff::Insert("bcxyz".into(), None),]
            );
        }
        {
            let mut txn = d1.transact();
            mgr.redo(&mut txn);
            assert_eq!(
                txt1.diff(&mut txn),
                vec![
                    Diff::Insert("b".into(), None),
                    Diff::Insert("cxy".into(), Some(Box::new(attrs.clone()))),
                    Diff::Insert("z".into(), None),
                ]
            );
        }
    }

    #[test]
    fn double_undo_text() {
        let doc = Doc::new();
        let txt = {
            let mut txn = doc.transact();
            txn.get_text("text")
        };
        text_run(&doc, Text::insert, 0, "1221");

        let mut mgr = doc.undo_manager([&txt]);

        text_run(&doc, Text::insert, 2, "3");
        text_run(&doc, Text::insert, 3, "3");

        {
            let mut txn = doc.transact();
            mgr.undo(&mut txn);
            mgr.undo(&mut txn);

            txt.insert(&mut txn, 2, "3");

            assert_eq!(txt.to_string(&txn), "12321");
        }
    }

    #[test]
    fn undo_map() {
        let d1 = Doc::with_client_id(1);
        let m1 = {
            let mut txn = d1.transact();
            let m = txn.get_map("map");
            m.insert(&mut txn, "a", 0u32);
            m
        };

        let d2 = Doc::with_client_id(1);
        let m2 = {
            let mut txn = d1.transact();
            txn.get_map("map")
        };

        let mut mgr = d1.undo_manager([&m1]);
        m1.insert(&mut d1.transact(), "a", 1u32);
        {
            let mut txn = d1.transact();
            mgr.undo(&mut txn);
            assert_eq!(m1.get(&mut txn, "a"), Some(0u32.into()));
            mgr.redo(&mut txn);
            assert_eq!(m1.get(&mut txn, "a"), Some(0u32.into()));
        }

        // testing sub-types and if it can restore a whole type
        let expected = Any::Map(Box::new(HashMap::from([(
            "a".into(),
            Any::Map(Box::new(HashMap::from([("x".into(), 42.into())]))),
        )])));
        {
            let mut txn = d1.transact();
            m1.insert(&mut txn, "a", HashMap::<String, Any>::default());
            let sub_type = if let Some(Value::YMap(m)) = m1.get(&txn, "a") {
                m
            } else {
                panic!("should not happen")
            };
            sub_type.insert(&mut txn, "x", 42);
            assert_eq!(&m1.to_json(&txn), &expected);
        }
        {
            let mut txn = d1.transact();
            mgr.undo(&mut txn);
            assert_eq!(m1.get(&txn, "a"), Some(1.into()));
            mgr.redo(&mut txn);
            assert_eq!(&m1.to_json(&txn), &expected);
        }
        exchange_updates(&[&d1, &d2]);

        // if content is overwritten by another user, undo operations should be skipped
        m2.insert(&mut d2.transact(), "a", 44);
        exchange_updates(&[&d1, &d2]);
        {
            let mut txn = d1.transact();
            mgr.undo(&mut txn);
            assert_eq!(m1.get(&txn, "a"), Some(44.into()));
            mgr.redo(&mut txn);
            assert_eq!(m1.get(&txn, "a"), Some(44.into()));
        }

        // test setting value multiple times
        m1.insert(&mut d1.transact(), "b", "initial");
        mgr.stop();
        m1.insert(&mut d1.transact(), "b", "val1");
        m1.insert(&mut d1.transact(), "b", "val2");
        mgr.stop();
        {
            let mut txn = d1.transact();
            mgr.undo(&mut txn);
            assert_eq!(m1.get(&txn, "b"), Some("initial".into()));
        }
    }

    #[test]
    fn undo_events() {
        let doc = Doc::with_client_id(1);
        let txt = {
            let mut txn = doc.transact();
            txn.get_text("text")
        };

        let mut mgr = doc.undo_manager([&txt]);
        let mut a = Cell::new(0);
        let mut b = Cell::new(0);
        let ac = a.clone();
        let bc = b.clone();
        let _sub = mgr.subscribe(move |txn, e| match e.kind {
            EventKind::Added => ac.set(ac.get() + 1),
            EventKind::Popped => bc.set(bc.get() + 1),
        });
        txt.insert(&mut doc.transact(), 0, "abc");
        {
            let mut txn = doc.transact();
            mgr.undo(&mut txn);
            assert_eq!(a.get(), 1);
            assert_eq!(b.get(), 0);
            mgr.redo(&mut txn);
            assert_eq!(a.get(), 1);
            assert_eq!(a.get(), 2);
        }
    }
}
