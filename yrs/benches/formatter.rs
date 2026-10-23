use criterion::*;
use yrs::{
    AttributionsFormatter, ContentIds, ContentMap, DeltaOptions, Doc, Options, SnapshotFormatter,
};

/// `N` random edits, a quarter of them deletions of up to 5 chars, the rest insertions of up to 5.
fn random_text(skip_gc: bool) -> Doc {
    const N: u32 = 10_000;
    let mut rng = fastrand::Rng::with_seed(0xdeadbeef);
    let mut doc = Doc::with_options(Options {
        skip_gc,
        ..Options::default()
    });
    let mut txn = doc.transact_mut();
    let mut text = txn.node_mut("text").unwrap();
    for _ in 0..N {
        let len = text.len();
        if rng.bool() && rng.bool() && len > 0 {
            let index = rng.u32(0..len);
            text.remove(index, rng.u32(0..=(len - index).min(5)));
        } else {
            let index = rng.u32(0..=len);
            let content: String = (0..rng.u32(1..=5)).map(|_| rng.alphanumeric()).collect();
            text.insert_text(index, &content);
        }
    }
    drop(txn);
    doc
}

/// `toString()` vs `toDelta()` with the default formatter.
fn bench_default(c: &mut Criterion) {
    let doc = random_text(false);
    let txn = doc.transact();
    let text = txn.node("text").unwrap();
    let mut g = c.benchmark_group("formatter/default");
    g.bench_function("to_string", |b| b.iter(|| black_box(text.to_string())));
    g.bench_function("delta", |b| {
        b.iter(|| black_box(text.delta(&DeltaOptions::default())))
    });
    g.finish();
}

/// The default fast path vs an `AttributionsFormatter` with no attributions whose rendered content
/// is the full insert set: every deletion is restored and rendered normally, so this measures the
/// formatter's restore path against the generic one.
fn bench_attributions(c: &mut Criterion) {
    let doc = random_text(true);
    let inserts = ContentIds::from_doc(&doc).inserts;
    let empty: ContentMap = ContentMap::default();
    let fmt = AttributionsFormatter::new(&empty).with_rendered_content(inserts);
    let txn = doc.transact();
    let text = txn.node("text").unwrap();
    let opts = DeltaOptions::default();
    let mut g = c.benchmark_group("formatter/attributions");
    g.bench_function("default delta", |b| b.iter(|| black_box(text.delta(&opts))));
    g.bench_function("restoring delta", |b| {
        b.iter(|| black_box(text.formatted(&fmt).delta(&opts)))
    });
    g.finish();
}

/// A `SnapshotFormatter` of the final state against the default render of the same doc.
fn bench_snapshot(c: &mut Criterion) {
    let doc = random_text(true);
    let txn = doc.transact();
    let fmt = SnapshotFormatter::single(txn.snapshot());
    let text = txn.node("text").unwrap();
    let opts = DeltaOptions::default();
    let mut g = c.benchmark_group("formatter/snapshot");
    g.bench_function("default delta", |b| b.iter(|| black_box(text.delta(&opts))));
    g.bench_function("snapshot delta", |b| {
        b.iter(|| black_box(text.formatted(&fmt).delta(&opts)))
    });
    g.finish();
}

criterion_group!(benches, bench_default, bench_attributions, bench_snapshot);
criterion_main!(benches);
