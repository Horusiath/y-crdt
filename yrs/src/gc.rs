use crate::block::{Block, ClientID};
use crate::transaction::ensure_state;
use crate::{Doc, IdSet, TransactionMut, ID};
use std::collections::HashMap;

#[derive(Default)]
pub(crate) struct GCCollector {
    marked: HashMap<ClientID, Vec<u32>>,
}

impl GCCollector {
    /// Garbage collect all blocks deleted within current transaction scope.
    pub fn collect(txn: &mut TransactionMut) {
        let mut gc = Self::default();
        let state = txn.state.as_ref().unwrap();
        gc.mark_in_scope(&mut txn.doc, None, &state.delete_set);
        gc.collect_marked(txn);
    }

    /// Garbage collect all deleted blocks from current transaction's document store.
    pub fn collect_all(txn: &mut TransactionMut, delete_set: Option<&IdSet>) {
        let mut gc = Self::default();
        match delete_set {
            None => gc.mark_all(txn),
            Some(ds) => {
                let state = ensure_state(&mut txn.state);
                gc.mark_in_scope(&mut txn.doc, Some(&mut state.merge_blocks), ds);
            }
        }
        gc.collect_marked(txn);
    }

    /// Mark deleted items based on a provided delete set.
    fn mark_in_scope(
        &mut self,
        store: &mut Doc,
        mut merge_blocks: Option<&mut Vec<ID>>,
        delete_set: &IdSet,
    ) {
        for (client, range) in delete_set.iter() {
            if let Some(blocks) = store.blocks.get_client_mut(client) {
                for delete_item in range.iter().rev() {
                    let mut start = delete_item.start;
                    if let Some(mut i) = blocks.find_index(start) {
                        while i < blocks.len() {
                            let mut block = unsafe { blocks.get(i).unwrap_unchecked() };
                            let block = block.as_mut();
                            let len = block.len();
                            start += len;
                            if start > delete_item.end {
                                break;
                            } else {
                                if let Block::Item(item) = block {
                                    item.gc(self, false);
                                    if let Some(merge_blocks) = merge_blocks.as_deref_mut() {
                                        merge_blocks.push(item.id);
                                    }
                                }
                                i += 1;
                            }
                        }
                    }
                }
            }
        }
    }

    fn mark_all(&mut self, txn: &mut TransactionMut) {
        for (_, client_blocks) in txn.doc.blocks.iter_mut() {
            for mut block in client_blocks.iter() {
                if let Block::Item(item) = block.as_mut() {
                    if item.is_deleted() {
                        item.gc(self, false);
                        ensure_state(&mut txn.state).merge_blocks.push(item.id);
                    }
                }
            }
        }
    }

    /// Marks item with a given [ID] as a candidate for being GCed.
    pub(crate) fn mark(&mut self, id: &ID) {
        let client = self.marked.entry(id.client).or_default();
        client.push(id.clock);
    }

    /// Garbage collects all items marked for GC.
    fn collect_marked(self, txn: &mut TransactionMut) {
        for (client_id, clocks) in self.marked.into_iter() {
            let client = txn.doc.blocks.get_client_blocks_mut(client_id);
            for clock in clocks {
                if let Some(index) = client.find_index(clock) {
                    let block = unsafe { client.get(index).unwrap_unchecked() }.as_mut();
                    if let Block::Item(item) = block {
                        if item.is_deleted() && !item.info.is_keep() {
                            let gc = Block::GC(item.block_range());
                            *block = gc;
                        }
                    }
                }
            }
        }
    }
}
