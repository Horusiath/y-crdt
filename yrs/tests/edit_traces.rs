#![allow(dead_code)]
/*
   ORIGINAL CODE AUTHORED BY Seph Gentle
   available at: https://github.com/josephg/editing-traces
*/

use flate2::bufread::GzDecoder;
use serde::Deserialize;
use std::fs::File;
use std::io::{BufReader, Read};
use std::time::Instant;
use yrs::{Doc, OffsetKind, Options};

/// This file contains some simple helpers for loading test data. Its used by benchmarking and
/// testing code.

/// (position, delete length, insert content).
#[derive(Debug, Clone, Deserialize, Eq, PartialEq)]
pub struct TestPatch(pub usize, pub usize, pub String);

#[derive(Debug, Clone, Deserialize, Eq, PartialEq)]
pub struct TestTxn {
    // time: String, // ISO String. Unused.
    pub patches: Vec<TestPatch>,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq)]
pub struct TestData {
    #[serde(default)]
    pub using_byte_positions: bool,

    #[serde(rename = "startContent")]
    pub start_content: String,
    #[serde(rename = "endContent")]
    pub end_content: String,

    pub txns: Vec<TestTxn>,
}

impl TestData {
    pub fn len(&self) -> usize {
        self.txns.iter().map(|txn| txn.patches.len()).sum::<usize>()
    }

    pub fn is_empty(&self) -> bool {
        !self.txns.iter().any(|txn| !txn.patches.is_empty())
    }

    /// This method returns a clone of the testing data using byte offsets instead of codepoint
    /// indexes.
    pub fn chars_to_bytes(&self) -> Self {
        assert_eq!(false, self.using_byte_positions);

        let mut r = ropey::Rope::new();

        Self {
            using_byte_positions: true,
            start_content: self.start_content.clone(),
            end_content: self.end_content.clone(),
            txns: self
                .txns
                .iter()
                .map(|txn| TestTxn {
                    patches: txn
                        .patches
                        .iter()
                        .map(|TestPatch(pos_chars, del_chars, ins)| {
                            let pos_bytes = r.char_to_byte(*pos_chars);
                            let del_bytes = if *del_chars > 0 {
                                let del_end_bytes = r.char_to_byte(pos_chars + *del_chars);
                                r.remove(*pos_chars..*pos_chars + *del_chars);
                                del_end_bytes - pos_bytes
                            } else {
                                0
                            };
                            if !ins.is_empty() {
                                r.insert(*pos_chars, ins);
                            }

                            TestPatch(pos_bytes, del_bytes, ins.clone())
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    pub fn patches(&self) -> impl Iterator<Item = &TestPatch> {
        self.txns.iter().flat_map(|txn| txn.patches.iter())
    }
}

/// Load the testing data at the specified file. If the filename ends in .gz, it will be
/// transparently uncompressed.
///
/// This method panics if the file does not exist, or is corrupt. It'd be better to have a try_
/// variant of this method, but given this is mostly for benchmarking and testing, I haven't felt
/// the need to write that code.
pub fn load_testing_data(filename: &str) -> TestData {
    // let start = SystemTime::now();
    // let mut file = File::open("benchmark_data/automerge-paper.json.gz").unwrap();
    let file = File::open(filename).unwrap();

    let mut reader = BufReader::new(file);
    // We could pass the GzDecoder straight to serde, but it makes it way slower to parse for
    // some reason.
    let mut raw_json = vec![];

    if filename.ends_with(".gz") {
        let mut reader = GzDecoder::new(reader);
        reader.read_to_end(&mut raw_json).unwrap();
    } else {
        reader.read_to_end(&mut raw_json).unwrap();
    }

    let data: TestData = serde_json::from_reader(raw_json.as_slice()).unwrap();
    data
}

#[test]
fn edit_trace_automerge() {
    test_editing_trace("../assets/editing-traces/sequential_traces/automerge-paper.json.gz");
}

#[test]
fn edit_trace_friendsforever() {
    test_editing_trace("../assets/editing-traces/sequential_traces/friendsforever_flat.json.gz");
}

#[test]
fn edit_trace_sephblog1() {
    test_editing_trace("../assets/editing-traces/sequential_traces/seph-blog1.json.gz");
}

#[test]
fn edit_trace_sveltecomponent() {
    test_editing_trace("../assets/editing-traces/sequential_traces/sveltecomponent.json.gz");
}

#[test]
fn edit_trace_rustcode() {
    test_editing_trace("../assets/editing-traces/sequential_traces/rustcode.json.gz");
}

fn test_editing_trace(fpath: &str) {
    let data = load_testing_data(fpath);
    let mut doc = Doc::with_options(Options {
        offset_kind: if data.using_byte_positions {
            OffsetKind::Bytes
        } else {
            OffsetKind::Utf16
        },
        ..Options::default()
    });
    let start = Instant::now();
    for t in data.txns {
        let mut txn = doc.transact_mut();
        let mut txt = txn.node_mut("text").unwrap();
        for patch in t.patches {
            let at = patch.0;
            let delete = patch.1;
            let content = patch.2;

            if delete != 0 {
                txt.remove(at as u32, delete as u32);
            }
            if !content.is_empty() {
                txt.insert_text(at as u32, &content);
            }
        }
    }
    let finish = Instant::now();
    println!("elapsed: {}ms", (finish - start).as_millis());
    let actual = doc.transact().node("text").unwrap().to_string();
    assert_eq!(actual, data.end_content);
}
