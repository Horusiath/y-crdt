use lib0::decoding::{Cursor, Read};
use std::time::Instant;
use yrs::Doc;

enum TextOp {
    Insert(u32, String),
    Delete(u32, u32),
}

fn read_input(fpath: &str) -> Vec<TextOp> {
    use std::fs::File;
    use yrs::updates::decoder::DecoderV1;

    let mut f = File::open(fpath).unwrap();
    let mut buf = Vec::new();
    std::io::Read::read_to_end(&mut f, &mut buf).unwrap();
    let mut decoder = DecoderV1::new(Cursor::new(buf.as_slice()));
    let len: usize = decoder.read_uvar();
    let mut result = Vec::with_capacity(len);
    for _ in 0..len {
        let op = {
            match decoder.read_uvar() {
                1u32 => {
                    let idx = decoder.read_uvar();
                    let chunk = decoder.read_string();
                    TextOp::Insert(idx, chunk.to_string())
                }
                2u32 => {
                    let idx = decoder.read_uvar();
                    let len = decoder.read_uvar();
                    TextOp::Delete(idx, len)
                }
                other => panic!("unrecognized TextOp tag type: {}", other),
            }
        };
        result.push(op);
    }
    result
}

fn main() {
    let doc = Doc::new();
    let txt = {
        let mut txn = doc.transact();
        txn.get_text("text")
    };
    let input = read_input("./yrs/benches/input/b4-editing-trace.bin");

    println!("read input of {} operations", input.len());
    let mut c = 0;
    let mut last = Instant::now();
    for i in input.into_iter().take(160000) {
        let mut txn = doc.transact();
        match i {
            TextOp::Insert(idx, chunk) => txt.insert(&mut txn, idx, &chunk),
            TextOp::Delete(idx, len) => txt.remove_range(&mut txn, idx, len),
        }
        c += 1;
        if c % 10_000 == 0 {
            let curr = Instant::now();
            let passed = curr - last;
            last = curr;
            println!("applied {} operations in {}ms", c, passed.as_millis());
        }
    }

    println!("finished applying all operations");
    //let str = {
    //    let txn = doc.transact();
    //    txt.to_string(&txn)
    //};
    //println!("{}", str);
}
