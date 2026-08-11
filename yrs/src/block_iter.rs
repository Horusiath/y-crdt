use crate::block::{Item, ItemContent, ItemPtr};
use crate::node::{NodePtr, TypePtr};
use crate::transaction::TransactionMut;
use crate::{Doc, ID, In, Out};

/// Struct used for iterating over the sequence of item's values with respect to a potential
/// [Move] markers that may change their order.
#[derive(Debug, Clone)]
pub(crate) struct BlockIter {
    branch: NodePtr,
    index: u32,
    rel: u32,
    next_item: Option<ItemPtr>,
    reached_end: bool,
}

impl BlockIter {
    pub fn new(branch: NodePtr) -> Self {
        let next_item = branch.start;
        let reached_end = branch.start.is_none();
        BlockIter {
            branch,
            next_item,
            reached_end,
            index: 0,
            rel: 0,
        }
    }

    #[inline]
    pub fn rel(&self) -> u32 {
        self.rel
    }

    #[inline]
    pub fn finished(&self) -> bool {
        self.reached_end || self.index == self.branch.content_len
    }

    #[inline]
    pub fn next_item(&self) -> Option<ItemPtr> {
        self.next_item
    }

    pub fn left(&self) -> Option<ItemPtr> {
        if self.reached_end {
            self.next_item
        } else if let Some(item) = self.next_item.as_deref() {
            item.left
        } else {
            None
        }
    }

    pub fn right(&self) -> Option<ItemPtr> {
        if self.reached_end {
            None
        } else {
            self.next_item
        }
    }

    fn can_forward(&self, ptr: Option<ItemPtr>, len: u32) -> bool {
        if !self.reached_end {
            if len > 0 {
                return true;
            } else if let Some(item) = ptr.as_deref() {
                return !item.is_countable() || item.is_deleted() || self.reached_end;
            }
        }

        false
    }

    pub fn forward(&mut self, doc: &Doc, len: u32) {
        if !self.try_forward(doc, len) {
            panic!("Length exceeded")
        }
    }

    pub fn try_forward(&mut self, doc: &Doc, mut len: u32) -> bool {
        if len == 0 && self.next_item.is_none() {
            return true;
        }

        if self.index + len > self.branch.content_len() || self.next_item.is_none() {
            return false;
        }

        let mut item = self.next_item;
        self.index += len;
        if self.rel != 0 {
            len += self.rel;
            self.rel = 0;
        }

        let encoding = doc.options.offset_kind;
        while self.can_forward(item, len) {
            if item.is_none() {
                return false;
            } else if let Some(i) = item.as_deref() {
                if i.is_countable() && !i.is_deleted() && len > 0 {
                    let item_len = i.content_len(encoding);
                    if item_len > len {
                        self.rel = len;
                        len = 0;
                        break;
                    } else {
                        len -= item_len;
                    }
                }
            }

            if self.reached_end {
                return false;
            }

            match item.as_deref() {
                Some(i) if i.right.is_some() => item = i.right,
                _ => self.reached_end = true, //TODO: we need to ensure to iterate further if this.currMoveEnd === null
            }
        }

        self.index -= len;
        self.next_item = item;
        true
    }

    pub fn backward(&mut self, doc: &Doc, mut len: u32) {
        if self.index < len {
            panic!("Length exceeded");
        }
        self.index -= len;
        let encoding = doc.options.offset_kind;
        if self.reached_end {
            if let Some(next_item) = self.next_item.as_deref() {
                self.rel = if next_item.is_countable() && !next_item.is_deleted() {
                    next_item.content_len(encoding)
                } else {
                    0
                };
            }
        }
        if self.rel >= len {
            self.rel -= len;
            return;
        }
        let mut item = self.next_item;
        if let Some(i) = item.as_deref() {
            len += if i.is_countable() && !i.is_deleted() {
                i.content_len(encoding)
            } else {
                0
            };
            len -= self.rel;
        }
        self.rel = 0;
        while let Some(i) = item.as_deref() {
            if len == 0 {
                break;
            }

            if i.is_countable() && !i.is_deleted() {
                let item_len = i.content_len(encoding);
                if len < item_len {
                    self.rel = item_len - len;
                    len = 0;
                } else {
                    len -= item_len;
                }
                if len == 0 {
                    break;
                }
            }

            item = if let Some(i) = item.as_deref() {
                i.left
            } else {
                None
            };
        }
        self.next_item = item;
    }

    pub fn delete(&mut self, txn: &mut TransactionMut, mut len: u32) {
        let mut item = self.next_item;
        if self.index + len > self.branch.content_len() {
            panic!("Length exceeded");
        }

        let encoding = txn.doc().options.offset_kind;
        let mut i: &Item;
        while len > 0 {
            while let Some(block) = item.as_deref() {
                i = block;
                if !i.is_deleted() && i.is_countable() && !self.reached_end && len > 0 {
                    if self.rel > 0 {
                        let mut id = i.id.clone();
                        id.clock += self.rel;
                        let store = &mut *txn.doc;
                        item = store
                            .blocks
                            .get_item_clean_start(&id)
                            .map(|s| store.materialize(s));
                        i = item.as_deref().unwrap();
                        self.rel = 0;
                    }
                    if len < i.content_len(encoding) {
                        let mut id = i.id.clone();
                        id.clock += len;
                        let store = &mut *txn.doc;
                        store
                            .blocks
                            .get_item_clean_start(&id)
                            .map(|s| store.materialize(s));
                    }
                    len -= i.content_len(encoding);
                    txn.delete(item.unwrap());
                    if i.right.is_some() {
                        item = i.right;
                    } else {
                        self.reached_end = true;
                    }
                } else {
                    break;
                }
            }
            if len > 0 {
                self.next_item = item;
                if self.try_forward(txn.doc(), 0) {
                    item = self.next_item;
                } else {
                    panic!("Block iter couldn't move forward");
                }
            }
        }
        self.next_item = item;
    }

    pub(crate) fn slice(&mut self, doc: &Doc, buf: &mut [Out]) -> u32 {
        let mut len = buf.len() as u32;
        if self.index + len > self.branch.content_len() {
            return 0;
        }
        self.index += len;
        let mut next_item = self.next_item;
        let encoding = doc.options.offset_kind;
        let mut read = 0u32;
        while len > 0 {
            if !self.reached_end {
                while let Some(item) = next_item {
                    if item.is_countable() && !self.reached_end && len > 0 {
                        if !item.is_deleted() {
                            // we're iterating inside of a block
                            let r = item
                                .content
                                .read(self.rel as usize, &mut buf[read as usize..])
                                as u32;
                            read += r;
                            len -= r;
                            if self.rel + r == item.content_len(encoding) {
                                self.rel = 0;
                            } else {
                                self.rel += r;
                                continue; // do not iterate to item.right
                            }
                        }

                        if item.right.is_some() {
                            next_item = item.right;
                        } else {
                            self.reached_end = true;
                        }
                    } else {
                        break;
                    }
                }
                if !self.reached_end && len > 0 {
                    // always set nextItem before any method call
                    self.next_item = next_item;
                    if !self.try_forward(doc, 0) || self.next_item.is_none() {
                        return read;
                    }
                    next_item = self.next_item;
                }
            } else {
                // reached end and move stack is empty
                next_item = None;
                break;
            }
        }
        self.next_item = next_item;
        self.index -= len;
        read
    }

    fn split_rel(&mut self, txn: &mut TransactionMut) {
        if self.rel > 0 {
            if let Some(ptr) = self.next_item {
                let mut item_id = ptr.id().clone();
                item_id.clock += self.rel;
                let store = &mut *txn.doc;
                self.next_item = store
                    .blocks
                    .get_item_clean_start(&item_id)
                    .map(|s| store.materialize(s));
                self.rel = 0;
            }
        }
    }

    pub(crate) fn read_value(&mut self, doc: &Doc) -> Option<Out> {
        let mut buf = [Out::default()];
        if self.slice(doc, &mut buf) != 0 {
            Some(std::mem::replace(&mut buf[0], Out::default()))
        } else {
            None
        }
    }

    pub fn values<'a, 'doc>(&'a mut self, doc: &'doc Doc) -> Values<'a, 'doc> {
        Values::new(self, doc)
    }
}

pub struct Values<'a, 'doc> {
    iter: &'a mut BlockIter,
    doc: &'doc Doc,
}

impl<'a, 'doc> Values<'a, 'doc> {
    fn new(iter: &'a mut BlockIter, doc: &'doc Doc) -> Self {
        Values { iter, doc }
    }
}

impl<'a, 'doc> Iterator for Values<'a, 'doc> {
    type Item = Out;

    fn next(&mut self) -> Option<Self::Item> {
        if self.iter.reached_end || self.iter.index == self.iter.branch.content_len() {
            None
        } else {
            let mut buf = [Out::default()];
            if self.iter.slice(self.doc, &mut buf) != 0 {
                Some(std::mem::replace(&mut buf[0], Out::default()))
            } else {
                None
            }
        }
    }
}
