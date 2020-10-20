use criterion::{criterion_group, criterion_main, Criterion};
use yrs::*;

const ITERATIONS: u32 = 1000000;

fn ytext_insert() {
    let doc = Doc::new();
    // let tr = doc.transact();
    let t = doc.get_type("");
    for _ in 0..ITERATIONS {
        t.insert(0, 'a')
    }
    // tr.end();
}

const MULT_STRUCT_SIZE: u32 = 7;

fn gen_vec_perf_optimal () {
    let mut vec: Vec<u64> = Vec::new();
    for i in 0..((ITERATIONS * MULT_STRUCT_SIZE) as u64) {
        vec.push(i);
    }
}

fn gen_vec_perf_pred_optimal () {
    let mut vec: Vec<u64> = Vec::with_capacity((ITERATIONS * MULT_STRUCT_SIZE) as usize);
    for i in 0..((ITERATIONS * MULT_STRUCT_SIZE) as u64) {
        vec.push(i);
    }
}

fn criterion_benchmark(c: &mut Criterion) {
    c.bench_function("ytext insert", |b| b.iter(|| ytext_insert()));
    c.bench_function("gen vec perf optimal", |b| b.iter(|| gen_vec_perf_optimal()));
    c.bench_function("gen vec perf pred optimal", |b| b.iter(|| gen_vec_perf_pred_optimal()));
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);