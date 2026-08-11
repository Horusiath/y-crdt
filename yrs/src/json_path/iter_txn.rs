use crate::any::AnyArrayIter;
use crate::json_path::JsonPathToken;
use crate::{Any, Doc, JsonPath, JsonPathEval, NodeID, Out, Transaction};
use std::ops::Deref;

impl<D> JsonPathEval for Transaction<D>
where
    D: Deref<Target = Doc>,
{
    type Iter<'a>
        = JsonPathIter<'a, D>
    where
        D: 'a;

    /// Evaluate JSON path on the current transaction, starting from current transaction [Doc] as
    /// its root.
    ///
    /// # Example
    /// ```rust
    /// use yrs::{any, Array, ArrayPrelim, Doc, In, JsonPath, JsonPathEval, Map, MapPrelim, Out};
    ///
    /// let mut doc = Doc::new();
    /// let mut txn = doc.transact_mut();
    /// let users = txn.get_or_insert_array("users");
    ///
    /// // populate the document with some data to query
    /// users.insert(&mut txn, 0, MapPrelim::from([
    ///     ("name".to_string(), In::Any(any!("Alice"))),
    ///     ("surname".into(), In::Any(any!("Smith"))),
    ///     ("age".into(), In::Any(any!(25))),
    ///     (
    ///         "friends".into(),
    ///         In::from(ArrayPrelim::from([
    ///             any!({ "name": "Bob", "nick": "boreas" }),
    ///             any!({ "nick": "crocodile91" }),
    ///         ])),
    ///     ),
    /// ]));
    ///
    /// let query = JsonPath::parse("$.users..friends.*.nick").unwrap();
    /// let values: Vec<Out> = txn.json_path(&query).collect();
    /// assert_eq!(values, vec![Out::Any(any!("boreas")), Out::Any(any!("crocodile91"))]);
    /// ```
    fn json_path<'a>(&'a self, path: &'a JsonPath<'a>) -> Self::Iter<'a> {
        JsonPathIter::new(self, path.as_ref())
    }
}

fn slice_iter<'a, D: Deref<Target = Doc>>(
    txn: &'a Transaction<D>,
    value: Option<Out>,
    from: usize,
    to: usize,
    by: usize,
) -> Option<Box<dyn Iterator<Item = Out> + 'a>> {
    match value {
        Some(Out::Any(Any::Array(array))) => {
            let iter = AnyArrayIter::new(array);
            Some(Box::new(
                iter.skip(from).take(to - from).step_by(by).map(Out::Any),
            ))
        }
        Some(Out::Node(id)) => {
            let iter = txn.node(id)?.iter();
            Some(Box::new(iter.skip(from).take(to - from).step_by(by)))
        }
        _ => None,
    }
}

fn any_iter<'a, D: Deref<Target = Doc>>(
    txn: &'a Transaction<D>,
    out: Option<Out>,
) -> Option<Box<dyn Iterator<Item = Out> + 'a>> {
    #[inline]
    fn dyn_iter<'a, I: Iterator<Item = Out> + 'a>(iter: I) -> Box<dyn Iterator<Item = Out> + 'a> {
        Box::new(iter)
    }

    match out {
        None => Some(dyn_iter(txn.root_refs().map(|(_, out)| out))),
        Some(Out::Any(any)) => {
            let iter = any.try_into_iter();
            iter.map(|iter| dyn_iter(iter.map(|(_, v)| Out::Any(v))))
        }
        Some(Out::Node(id)) => {
            let node = txn.node(id)?;
            // a node acts both as an indexed sequence and as a map: if its list component is
            // empty, fall back onto its attributes
            if node.len() != 0 {
                Some(dyn_iter(node.iter()))
            } else {
                Some(dyn_iter(node.attr_values().collect::<Vec<_>>().into_iter()))
            }
        }
        _ => None,
    }
}

fn member_union_iter<'a, D: Deref<Target = Doc>>(
    txn: &'a Transaction<D>,
    value: Option<Out>,
    members: &'a [&'a str],
) -> Option<Box<dyn Iterator<Item = Out> + 'a>> {
    match value {
        Some(Out::Any(Any::Map(map))) => {
            let iter = members
                .into_iter()
                .flat_map(move |key| map.get(*key).cloned().map(Out::Any));
            Some(Box::new(iter))
        }
        Some(Out::Node(id)) => {
            let node = txn.node(id)?;
            let iter = members.into_iter().flat_map(move |key| node.attr(*key));
            Some(Box::new(iter))
        }
        None => {
            let iter = members
                .into_iter()
                .map(|key| Out::Node(NodeID::root(*key)));
            Some(Box::new(iter))
        }
        _ => None,
    }
}

fn index_union_iter<'a, D: Deref<Target = Doc>>(
    txn: &'a Transaction<D>,
    value: Option<Out>,
    indices: &'a [i32],
) -> Option<Box<dyn Iterator<Item = Out> + 'a>> {
    match value {
        Some(Out::Any(Any::Array(array))) => {
            let iter = indices.into_iter().flat_map(move |i| {
                let i = if *i < 0 {
                    (array.len() as i32 + *i) as usize
                } else {
                    *i as usize
                };
                array.get(i).cloned().map(Out::Any)
            });
            Some(Box::new(iter))
        }
        Some(Out::Node(id)) => {
            let node = txn.node(id)?;
            let len = node.len();
            let iter = indices.into_iter().flat_map(move |i| {
                let i = if *i < 0 {
                    (len as i32 + *i) as u32
                } else {
                    *i as u32
                };
                node.get(i)
            });
            Some(Box::new(iter))
        }
        _ => None,
    }
}

pub struct JsonPathIter<'a, D> {
    txn: &'a Transaction<D>,
    pattern: &'a [JsonPathToken<'a>],
    frame: ExecutionFrame<'a>,
}

impl<'a, D> JsonPathIter<'a, D>
where
    D: Deref<Target = Doc>,
{
    fn new(txn: &'a Transaction<D>, path: &'a [JsonPathToken<'a>]) -> Self {
        Self {
            txn,
            pattern: path.as_ref(),
            frame: ExecutionFrame::new(None, 0, None),
        }
    }
}

impl<'a, D> Iterator for JsonPathIter<'a, D>
where
    D: Deref<Target = Doc>,
{
    type Item = Out;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(iter) = &mut self.frame.iter {
            match iter.next() {
                None => {
                    self.frame.iter = None;
                    return if self.frame.ascend() {
                        self.next()
                    } else {
                        None
                    };
                }
                Some(curr) => {
                    self.frame.descend(curr);
                }
            }
        } else if self.frame.index == self.pattern.len() {
            return if self.frame.ascend() {
                self.next()
            } else {
                None
            };
        }
        // early return only works if the loop after has at least one iteration
        let mut early_return = false;
        while self.frame.index < self.pattern.len() {
            let segment = &self.pattern[self.frame.index];
            self.frame.index += 1;
            match segment {
                JsonPathToken::Root => self.frame.current = None,
                JsonPathToken::Current => { /* do nothing */ }
                JsonPathToken::Member(key) => {
                    if let Some(value) = get_member(self.txn, self.frame.current.as_ref(), *key) {
                        self.frame.current = Some(value);
                    } else {
                        early_return = true;
                    }
                }
                JsonPathToken::Index(idx) => {
                    if let Some(value) = get_index(self.txn, self.frame.current.as_ref(), *idx) {
                        self.frame.current = Some(value);
                    } else {
                        early_return = true;
                    }
                }
                JsonPathToken::Wildcard => {
                    if let Some(iter) = any_iter(self.txn, self.frame.current.clone()) {
                        self.frame.iter = Some(iter);
                        return self.next();
                    }
                    early_return = true;
                }
                JsonPathToken::RecursiveDescend => {
                    if let Some(iter) = any_iter(self.txn, self.frame.current.clone()) {
                        self.frame.iter = Some(iter);
                        self.frame.is_descending = true;
                        return self.next();
                    }
                    early_return = true;
                }
                JsonPathToken::Slice(from, to, by) => {
                    if let Some(iter) = slice_iter(
                        self.txn,
                        self.frame.current.clone(),
                        *from as usize,
                        *to as usize,
                        *by as usize,
                    ) {
                        self.frame.iter = Some(iter);
                        return self.next();
                    }
                    early_return = true;
                }
                JsonPathToken::MemberUnion(members) => {
                    if let Some(iter) =
                        member_union_iter(self.txn, self.frame.current.clone(), &members)
                    {
                        self.frame.iter = Some(iter);
                        return self.next();
                    }
                    early_return = true;
                }
                JsonPathToken::IndexUnion(indices) => {
                    if let Some(iter) =
                        index_union_iter(self.txn, self.frame.current.clone(), &indices)
                    {
                        self.frame.iter = Some(iter);
                        return self.next();
                    }
                    early_return = true;
                }
            }

            if early_return {
                break;
            }
        }

        if !early_return {
            return self.frame.current.clone();
        } else if self.frame.is_descending {
            if let Some(iter) = any_iter(self.txn, self.frame.current.clone()) {
                self.frame.iter = Some(iter);
                self.frame.is_descending = true;
                self.frame.index -= 1; // '..' means we're not consuming the segment in this iteration
                return self.next();
            }
        }

        if self.frame.iter.is_none() && !self.frame.ascend() {
            None // we got to the end of the evaluator
        } else {
            self.next()
        }
    }
}

fn get_member<D: Deref<Target = Doc>>(
    txn: &Transaction<D>,
    out: Option<&Out>,
    key: &str,
) -> Option<Out> {
    match out {
        None => Some(Out::Node(NodeID::root(key))),
        Some(Out::Any(Any::Map(map))) => map.get(key).map(|any| Out::Any(any.clone())),
        Some(Out::Node(id)) => txn.node(id.clone())?.attr(key),
        _ => None,
    }
}

fn get_index<D: Deref<Target = Doc>>(
    txn: &Transaction<D>,
    out: Option<&Out>,
    idx: i32,
) -> Option<Out> {
    match out {
        Some(Out::Any(Any::Array(array))) => {
            let idx = if idx < 0 {
                array.len() as i32 + idx
            } else {
                idx
            } as usize;
            array.get(idx).cloned().map(Out::Any)
        }
        Some(Out::Node(id)) => {
            let node = txn.node(id.clone())?;
            let idx = if idx < 0 {
                node.len() as i32 + idx
            } else {
                idx
            } as u32;
            node.get(idx)
        }
        _ => None,
    }
}

/// Scope used for recursive iteration, i.e. wildcard, descent or slice.
struct ExecutionFrame<'a> {
    /// Offset to tokens array, where the current scope starts.
    index: usize,
    /// Whether we're in recursive descent scope.
    is_descending: bool,
    /// Current object this scope is iterating over.
    current: Option<Out>,
    /// Iterator used by this scope.
    iter: Option<ScopeIterator<'a>>,
    /// Scopes can be nested in each other i.e. `$.people.*.friends[*]name`. In such case they
    /// are organized in a linked list, with the first elements being the innermost scopes.
    next: Option<Box<ExecutionFrame<'a>>>,
}

impl<'a> ExecutionFrame<'a> {
    fn new(current: Option<Out>, index: usize, iter: Option<ScopeIterator<'a>>) -> Self {
        Self {
            index,
            is_descending: false,
            current,
            iter,
            next: None,
        }
    }

    /// Descent into given iterator context, moving current frame to the stack and replacing it with
    /// a new one executing in a context of that iterator.
    fn descend(&mut self, current: Out) {
        let new_self = ExecutionFrame {
            index: self.index,
            is_descending: self.is_descending,
            current: Some(current),
            iter: None,
            next: None,
        };
        let old_frame = std::mem::replace(self, new_self);
        self.next = Some(Box::new(old_frame));
    }

    /// Return from current scope, restoring previous iter frame from the stack.
    /// If there are no more iter frames, return false.
    fn ascend(&mut self) -> bool {
        match self.next.take() {
            None => false,
            Some(next) => {
                *self = *next;
                true
            }
        }
    }
}

type ScopeIterator<'a> = Box<dyn Iterator<Item = Out> + 'a>;

#[cfg(test)]
mod test {
    use crate::updates::decoder::Decode;
    use crate::{Array, ArrayPrelim, Doc, In, JsonPath, JsonPathEval, MapPrelim, Out, Update, any};

    fn mixed_sample() -> Doc {
        let mut doc = Doc::new();
        let mut tx = doc.transact_mut();
        let users = tx.get_or_insert_array("users");
        users.insert(
            &mut tx,
            0,
            MapPrelim::from([
                ("name".to_string(), In::Any(any!("Alice"))),
                ("surname".into(), In::Any(any!("Smith"))),
                ("age".into(), In::Any(any!(25))),
                (
                    "friends".into(),
                    In::from(ArrayPrelim::from([
                        any!({ "name": "Bob", "nick": "boreas" }),
                        any!({ "nick": "crocodile91" }),
                    ])),
                ),
            ]),
        );
        users.insert(
            &mut tx,
            1,
            MapPrelim::from([
                ("name".to_string(), In::Any(any!("Bob"))),
                ("nick".into(), In::Any(any!("boreas"))),
                ("age".into(), In::Any(any!(30))),
            ]),
        );
        users.insert(
            &mut tx,
            2,
            MapPrelim::from([
                ("nick".to_string(), In::Any(any!("crocodile91"))),
                ("age".into(), In::Any(any!(35))),
            ]),
        );
        users.insert(
            &mut tx,
            3,
            MapPrelim::from([
                ("name".to_string(), In::Any(any!("Damian"))),
                ("surname".into(), In::Any(any!("Smith"))),
                ("age".into(), In::Any(any!(30))),
            ]),
        );
        users.insert(
            &mut tx,
            4,
            MapPrelim::from([
                ("name".to_string(), In::Any(any!("Elise"))),
                ("age".into(), In::Any(any!(35))),
            ]),
        );
        drop(tx);
        doc
    }

    #[test]
    fn eval_member_partial() {
        let doc = mixed_sample();
        let path = JsonPath::parse("$.users").unwrap();
        let tx = doc.transact();
        let values: Vec<_> = tx.json_path(&path).collect();
        let expected = tx.get("users").unwrap();
        assert_eq!(values, vec![expected]);
    }

    #[test]
    fn eval_member_full() {
        let doc = mixed_sample();
        let path = JsonPath::parse("$.users[0].name").unwrap();
        let tx = doc.transact();
        let values: Vec<_> = tx.json_path(&path).collect();
        assert_eq!(values, vec![Out::Any(any!("Alice"))]);
    }

    #[test]
    fn eval_member_negative_index() {
        let doc = mixed_sample();
        let path = JsonPath::parse("$.users[-1].name").unwrap();
        let tx = doc.transact();
        let values: Vec<_> = tx.json_path(&path).collect();
        assert_eq!(values, vec![Out::Any(any!("Elise"))]);
    }

    #[test]
    fn eval_member_wildcard_array() {
        let doc = mixed_sample();
        let path = JsonPath::parse("$.users[*].name").unwrap();
        let tx = doc.transact();
        let values: Vec<_> = tx.json_path(&path).collect();
        assert_eq!(
            values,
            vec![
                Out::Any(any!("Alice")),
                Out::Any(any!("Bob")),
                Out::Any(any!("Damian")),
                Out::Any(any!("Elise"))
            ]
        );
    }

    #[test]
    fn eval_member_slice() {
        let doc = mixed_sample();
        let path = JsonPath::parse("$.users[1:3].nick").unwrap();
        let tx = doc.transact();
        let values: Vec<_> = tx.json_path(&path).collect();
        assert_eq!(
            values,
            vec![Out::Any(any!("boreas")), Out::Any(any!("crocodile91"))]
        );
    }

    #[test]
    fn eval_index_union() {
        let doc = mixed_sample();
        let path = JsonPath::parse("$.users[1,3].name").unwrap();
        let tx = doc.transact();
        let values: Vec<_> = tx.json_path(&path).collect();
        assert_eq!(
            values,
            vec![Out::Any(any!("Bob")), Out::Any(any!("Damian"))]
        );
    }

    #[test]
    fn eval_member_union() {
        let doc = mixed_sample();
        let path = JsonPath::parse("$.users[0]['name','surname']").unwrap();
        let tx = doc.transact();
        let values: Vec<_> = tx.json_path(&path).collect();
        assert_eq!(
            values,
            vec![Out::Any(any!("Alice")), Out::Any(any!("Smith"))]
        );
    }

    #[test]
    fn eval_descent_flat() {
        let doc = mixed_sample();
        let path = JsonPath::parse("$.users..name").unwrap();
        let tx = doc.transact();
        let values: Vec<_> = tx.json_path(&path).collect();
        assert_eq!(
            values,
            vec![
                Out::Any(any!("Alice")),
                Out::Any(any!("Bob")),
                Out::Any(any!("Damian")),
                Out::Any(any!("Elise"))
            ]
        );
    }
    #[test]
    fn eval_on_fresh_document() {
        let doc_state = mixed_sample()
            .transact()
            .encode_state_as_update_v1(&Default::default());
        let mut doc = Doc::new();
        doc.transact_mut()
            .apply_update(Update::decode_v1(&doc_state).unwrap())
            .unwrap();
        let path = JsonPath::parse("$.users[*].name").unwrap();
        let tx = doc.transact();
        let values: Vec<_> = tx.json_path(&path).collect();
        assert_eq!(
            values,
            vec![
                Out::Any(any!("Alice")),
                Out::Any(any!("Bob")),
                Out::Any(any!("Damian")),
                Out::Any(any!("Elise"))
            ]
        );
    }
}
