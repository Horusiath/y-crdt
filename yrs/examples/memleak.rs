use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use yrs::updates::decoder::Decode;
use yrs::{Doc, Map, ReadTxn, StateVector, Transact, Update, WriteTxn};

fn main() {
    let doc = Doc::new();
    let shutdown = Arc::new(AtomicBool::new(false));

    let terminate = shutdown.clone();
    let origin_doc = doc.clone();
    let _t = std::thread::spawn(move || {
        let mut tx = origin_doc.transact_mut();
        let a = tx.get_or_insert_map("a");
        for i in 0..10_000 {
            a.insert(&mut tx, "A", i);
            a.insert(&mut tx, "B", i);
        }
        drop(tx);
        let update = origin_doc
            .transact()
            .encode_state_as_update_v1(&StateVector::default());

        while !terminate.load(Ordering::SeqCst) {
            let doc = Doc::new();
            let update = Update::decode_v1(&update).unwrap();
            doc.transact_mut().apply_update(update).unwrap();
        }
    });
    std::thread::sleep(Duration::from_secs(10));
    shutdown.store(true, Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(100));
    drop(doc);
}
