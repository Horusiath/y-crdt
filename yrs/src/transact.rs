use crate::{Doc, Origin, Store, Transaction, TransactionMut};
use std::future::Future;

impl Doc {
    /// Creates and returns a read-write capable transaction. This transaction can be used to
    /// mutate the contents of underlying document store and upon dropping or committing it may
    /// subscription callbacks.
    ///
    /// # Panics
    ///
    /// Only one read-write transaction can be active at the same time. If any other transaction -
    /// be it a read-write or read-only one - is active at the same time, this method will panic.
    pub fn transact_mut(&mut self) -> TransactionMut {
        TransactionMut::new(self, None)
    }

    /// Creates and returns a read-write capable transaction with an `origin` classifier attached.
    /// This transaction can be used to mutate the contents of underlying document store and upon
    /// dropping or committing it may subscription callbacks.
    ///
    /// An `origin` may be used to identify context of operations made (example updates performed
    /// locally vs. incoming from remote replicas) and it's used i.e. by [`UndoManager`][crate::undo::UndoManager].
    ///
    /// # Errors
    ///
    /// Only one read-write transaction can be active at the same time. If any other transaction -
    /// be it a read-write or read-only one - is active at the same time, this method will panic.
    pub fn transact_mut_with<T>(&mut self, origin: T) -> TransactionMut
    where
        T: Into<Origin>,
    {
        TransactionMut::new(self, Some(origin.into()))
    }

    /// Creates and returns a lightweight read-only transaction.
    ///
    /// # Panics
    ///
    /// While it's possible to have multiple read-only transactions active at the same time,
    /// this method will panic whenever called while a read-write transaction
    /// (see: [Self::transact_mut]) is active at the same time.
    pub fn transact(&self) -> Transaction {
        Transaction::new(&self)
    }
}

#[cfg(test)]
mod test {
    use crate::{Doc, GetString, Text};
    use rand::random;
    use std::sync::{Arc, Barrier};
    use std::time::{Duration, Instant};

    #[test]
    fn multi_thread_transact_mut() {
        let doc = Doc::new();
        let txt = doc.get_or_insert_text("text");

        const N: usize = 3;
        let barrier = Arc::new(Barrier::new(N + 1));

        let start = Instant::now();
        for _ in 0..N {
            let d = doc.clone();
            let t = txt.clone();
            let b = barrier.clone();
            std::thread::spawn(move || {
                // let mut txn = d.try_transact_mut().unwrap(); // this will hang forever
                let mut txn = d.transact_mut();
                let n = random::<u64>() % 5;
                std::thread::sleep(Duration::from_millis(n * 100));
                t.insert(&mut txn, 0, "a");
                drop(txn);
                b.wait();
            });
        }

        barrier.wait();
        println!("{} threads executed in {:?}", N, Instant::now() - start);

        let expected: String = (0..N).map(|_| 'a').collect();
        let txn = doc.transact();
        let str = txt.get_string(&txn);
        assert_eq!(str, expected);
    }
}
