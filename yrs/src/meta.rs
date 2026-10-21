use crate::formatter::AttrValue;
use crate::id_map::{ContentAttribute, Diff, IdMap};
use crate::id_set::DeleteSet;
use crate::lib0::{Decode, Decoder, Encode, Encoder};
use crate::{Doc, DocHandle, IdSet, Transaction, Update};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::hash::Hash;

/// Ids of inserted and deleted content.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ContentIds {
    pub inserts: IdSet,
    pub deletes: IdSet,
}

impl ContentIds {
    pub fn new(inserts: IdSet, deletes: IdSet) -> Self {
        ContentIds { inserts, deletes }
    }

    /// All content of a `doc`: every inserted id (deleted included) and every deleted id.
    pub fn from_doc(doc: &Doc) -> Self {
        ContentIds {
            inserts: IdSet::from_store(&doc.blocks),
            deletes: IdSet::tombstones_from(&doc.blocks),
        }
    }

    /// Content inserted or deleted in `next` but not in `prev`.
    pub fn doc_diff(prev: &Doc, next: &Doc) -> Self {
        let prev = Self::from_doc(prev);
        let next = Self::from_doc(next);
        next.diff(&prev)
    }

    /// Content inserted (including content deleted in the same update) and deleted by `update`.
    pub fn from_update(update: &Update) -> Self {
        ContentIds {
            inserts: update.insertions(true),
            deletes: update.delete_set().clone(),
        }
    }

    /// Content inserted and deleted by a transaction.
    pub fn from_transaction<D: DocHandle>(txn: &Transaction<D>) -> Self {
        ContentIds {
            inserts: txn.insert_set().clone(),
            deletes: txn.delete_set().clone(),
        }
    }

    pub fn diff(&self, other: &ContentIds) -> Self {
        ContentIds {
            inserts: self.inserts.diff(&other.inserts),
            deletes: self.deletes.diff(&other.deletes),
        }
    }

    pub fn merge(items: &[Self]) -> Self {
        let mut result = ContentIds::default();
        for item in items {
            result.inserts.merge_with(item.inserts.clone());
            result.deletes.merge_with(item.deletes.clone());
        }
        result
    }

    pub fn intersect(&self, other: &ContentIds) -> Self {
        ContentIds {
            inserts: self.inserts.intersect(&other.inserts),
            deletes: self.deletes.intersect(&other.deletes),
        }
    }
}

impl Encode for ContentIds {
    fn encode<E: Encoder>(&self, encoder: &mut E) {
        self.inserts.encode(encoder);
        self.deletes.encode(encoder);
    }
}

impl Decode for ContentIds {
    fn decode<D: Decoder>(decoder: &mut D) -> Result<Self, crate::lib0::Error> {
        let inserts = IdSet::decode(decoder)?;
        let deletes = IdSet::decode(decoder)?;
        Ok(ContentIds { inserts, deletes })
    }
}

/// Attributed inserted and deleted content.
#[derive(Debug, PartialEq)]
pub struct ContentMap<A: Eq + Hash = AttrValue> {
    pub inserts: IdMap<A>,
    pub deletes: IdMap<A>,
}

// attributes are `Arc`s: cloning a map never clones `A`
impl<A: Eq + Hash> Clone for ContentMap<A> {
    fn clone(&self) -> Self {
        ContentMap {
            inserts: self.inserts.clone(),
            deletes: self.deletes.clone(),
        }
    }
}

impl<A: Eq + Hash> Default for ContentMap<A> {
    fn default() -> Self {
        ContentMap {
            inserts: IdMap::new(),
            deletes: IdMap::new(),
        }
    }
}

impl<A: Eq + Hash> ContentMap<A> {
    pub fn new(inserts: IdMap<A>, deletes: IdMap<A>) -> Self {
        ContentMap { inserts, deletes }
    }

    /// Attributes all `ids` with given attributes.
    pub fn from_content_ids(
        ids: &ContentIds,
        insert_attrs: Vec<ContentAttribute<A>>,
        delete_attrs: Vec<ContentAttribute<A>>,
    ) -> Self {
        ContentMap {
            inserts: IdMap::from_set(&ids.inserts, insert_attrs),
            deletes: IdMap::from_set(&ids.deletes, delete_attrs),
        }
    }

    pub fn diff(&self, other: &ContentIds) -> Self {
        let mut result = self.clone();
        result.inserts.diff_with(&other.inserts);
        result.deletes.diff_with(&other.deletes);
        result
    }

    pub fn merge(items: &[Self]) -> Self {
        let mut result = ContentMap::default();
        for item in items {
            result.inserts.merge_with(item.inserts.clone());
            result.deletes.merge_with(item.deletes.clone());
        }
        result
    }

    pub fn intersect(&self, other: &ContentMap<A>) -> Self {
        let mut result = self.clone();
        result.inserts.intersect_with(&other.inserts);
        result.deletes.intersect_with(&other.deletes);
        result
    }

    /// Keeps only the ranges whose attributes match the corresponding predicate.
    pub fn filter<I, D>(&self, inserts: I, deletes: D) -> Self
    where
        I: Fn(&[ContentAttribute<A>]) -> bool,
        D: Fn(&[ContentAttribute<A>]) -> bool,
    {
        ContentMap {
            inserts: self.inserts.filter(inserts),
            deletes: self.deletes.filter(deletes),
        }
    }
}

impl From<ContentMap> for ContentIds {
    fn from(value: ContentMap) -> Self {
        ContentIds {
            inserts: value.inserts.as_id_set(),
            deletes: value.deletes.as_id_set(),
        }
    }
}

impl<A: Serialize + Eq + Hash> Encode for ContentMap<A> {
    fn encode<E: Encoder>(&self, encoder: &mut E) {
        self.inserts.encode(encoder);
        self.deletes.encode(encoder);
    }
}

impl<A: DeserializeOwned + Eq + Hash> Decode for ContentMap<A> {
    fn decode<D: Decoder>(decoder: &mut D) -> Result<Self, crate::lib0::Error> {
        let inserts = IdMap::decode(decoder)?;
        let deletes = IdMap::decode(decoder)?;
        Ok(ContentMap { inserts, deletes })
    }
}

#[cfg(test)]
mod test {
    use super::{ContentIds, ContentMap};
    use crate::formatter::AttrValue;
    use crate::id_map::ContentAttribute;
    use crate::lib0::{Decode, Encode};
    use crate::test_utils::clone_doc;
    use crate::{ClientID, Doc, IdSet};

    #[test]
    fn content_ids_encoding() {
        let client = ClientID::new(1);
        let ids = ContentIds::new(
            IdSet::from_iter([(client, [0..3, 5..6])]),
            IdSet::from_iter([(client, [1..2])]),
        );
        assert_eq!(ContentIds::decode_v1(&ids.encode_v1()).unwrap(), ids);
    }

    #[test]
    fn content_map_encoding() {
        let client = ClientID::new(1);
        let ids = ContentIds::new(
            IdSet::from_iter([(client, [0..3])]),
            IdSet::from_iter([(client, [5..7])]),
        );
        let map: ContentMap<AttrValue> = ContentMap::from_content_ids(
            &ids,
            vec![
                ContentAttribute::insert("bob"),
                ContentAttribute::insert_at(42),
            ],
            vec![],
        );
        assert_eq!(ContentMap::decode_v1(&map.encode_v1()).unwrap(), map);
        assert_eq!(ContentIds::from(map), ids);
    }

    #[test]
    fn content_ids_from_doc_diff() {
        let mut doc = Doc::with_client_id(1);
        doc.transact_mut()
            .node_mut("t")
            .unwrap()
            .insert_text(0, "abc");
        let prev = clone_doc(&doc, None);
        doc.transact_mut().node_mut("t").unwrap().remove(1, 1);
        doc.transact_mut()
            .node_mut("t")
            .unwrap()
            .insert_text(2, "x");
        let diff = ContentIds::doc_diff(&prev, &doc);
        let client = ClientID::new(1);
        assert_eq!(diff.inserts, IdSet::from_iter([(client, [3..4])]));
        assert_eq!(diff.deletes, IdSet::from_iter([(client, [1..2])]));
    }
}
