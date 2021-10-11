use crate::block::{Block, Skip};
use crate::id_set::DeleteSet;
use crate::update::{Blocks, Update};
use crate::updates::decoder::{Decode, Decoder, DecoderV1};
use crate::updates::encoder::{Encode, Encoder, EncoderV1};
use crate::{ID, StateVector};
use lib0::decoding::Cursor;


pub fn merge_updates (updates: &[&[u8]]) -> Vec<u8> {
    let x: Vec<Update> = updates.iter().map(|buf | Update::decode_v1(buf)).into_iter().collect();
    Update::merge_updates(x).encode_v1()
}

// Computes the state vector from a document update
pub fn encode_state_vector_from_update(update: &[u8]) -> Vec<u8> {
    let update = Update::decode_v1(update);
    update.state_vector().encode_v1()
}

// Encode the missing differences to another document update.
pub fn diff_updates(update: &[u8], state_vector: &[u8]) -> Vec<u8> {
    let sv = StateVector::decode_v1(state_vector);
    let cursor = Cursor::new(update);
    let mut decoder = DecoderV1::new(cursor);
    let update = Update::decode(&mut decoder);
    let mut encoder = EncoderV1::new();
    update.encode_diff(&sv, &mut encoder);
    // for delete set, don't decode/encode it - just copy the remaining part from the decoder
    encoder.to_vec()
}

#[cfg(test)]
mod test {
    use crate::{diff_updates, encode_state_vector_from_update, merge_updates};

    #[test]
    fn merge_updates_compatibility() {
        let a = &[
            1, 1, 220, 240, 237, 172, 15, 0, 4, 1, 4, 116, 101, 115, 116, 3, 97, 98, 99, 0,
        ];
        let b = &[
            1, 1, 201, 139, 250, 201, 1, 0, 4, 1, 4, 116, 101, 115, 116, 2, 100, 101, 0,
        ];
        let expected = &[
            2, 1, 220, 240, 237, 172, 15, 0, 4, 1, 4, 116, 101, 115, 116, 3, 97, 98, 99, 1, 201,
            139, 250, 201, 1, 0, 4, 1, 4, 116, 101, 115, 116, 2, 100, 101, 0,
        ];

        let actual = merge_updates(&[a, b]);
        assert_eq!(actual, expected);
    }

    #[test]
    fn merge_updates_compatibility_2() {
        let a = &[
            1, 1, 129, 231, 135, 164, 7, 0, 4, 1, 4, 49, 50, 51, 52, 1, 97, 0
        ];
        let b = &[
            1, 1, 129, 231, 135, 164, 7, 1, 68, 129, 231, 135, 164, 7, 0, 1, 98, 0
        ];
        let expected = &[
            1, 2, 129, 231, 135, 164, 7, 0, 4, 1, 4, 49, 50, 51, 52, 1, 97, 68, 129, 231, 135, 164, 7, 0, 1, 98, 0
        ];

        let actual = merge_updates(&[a, b]);
        assert_eq!(actual, expected);
    }


    #[test]
    fn encode_state_vector_compatibility() {
        let update = &[
            2, 1, 220, 240, 237, 172, 15, 0, 4, 1, 4, 116, 101, 115, 116, 3, 97, 98, 99, 1, 201,
            139, 250, 201, 1, 0, 4, 1, 4, 116, 101, 115, 116, 2, 100, 101, 0,
        ];
        let expected = &[2, 220, 240, 237, 172, 15, 3, 201, 139, 250, 201, 1, 2];
        let actual = encode_state_vector_from_update(update);
        assert_eq!(actual, expected);
    }

    #[test]
    fn diff_updates_compatibility() {
        let state_vector = &[1, 148, 189, 145, 162, 9, 3];
        let update = &[
            1, 2, 148, 189, 145, 162, 9, 0, 4, 1, 4, 116, 101, 115, 116, 3, 97, 98, 99, 68, 148,
            189, 145, 162, 9, 0, 2, 100, 101, 0,
        ];
        let expected = &[
            1, 1, 148, 189, 145, 162, 9, 3, 68, 148, 189, 145, 162, 9, 0, 2, 100, 101, 0,
        ];
        let actual = diff_updates(update, state_vector);
        assert_eq!(actual, expected);
    }
}
