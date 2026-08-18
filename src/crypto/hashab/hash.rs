use super::{edonr, tables, word::Word};

const fn w(value: u32) -> Word {
    Word::new(value)
}

fn encoded_rotate_left(value: Word, count: u32) -> Word {
    (value * w(0x8fa4_91df)).rotate_left(count & 31)
}

fn preprocess_word(input: Word) -> Word {
    let input_multiplier = w(0xa272_be3f);
    let combine_multiplier = w(0x7f86_31bf);
    let byte_multiplier = w(0xc83b_ebc7);
    let word_bias = w(0x44e5_7c7e);
    let byte0 = w(u32::from(tables::HASH_BYTE_TABLE_0[input.index(0xff)]));
    let mut byte1 = w(u32::from(
        tables::HASH_BYTE_TABLE_1[(input >> 8).index(0xff)],
    ));
    let byte2 = w(u32::from(
        tables::HASH_BYTE_TABLE_2[(input >> 16).index(0xff)],
    ));
    let byte3 = w(u32::from(
        tables::HASH_BYTE_TABLE_3[(input >> 24).index(0xff)],
    ));

    let mut scaled = (((byte0 * w(0xc9) * w(0x79)) & w(0xff)) << 8) * input_multiplier;

    byte1 = (byte1 * w(0xc9)) & w(0xff);
    let mut high_mask = !(byte1 * w(0x79)) | w(0xff);
    let mut partial = byte1 * byte_multiplier
        + word_bias
        + !high_mask * input_multiplier
        + high_mask * word_bias
        + scaled;
    scaled = partial * combine_multiplier * w(0x100) * input_multiplier;

    high_mask = !(byte2 * w(0x79)) | w(0xff);
    partial = byte2 * byte_multiplier
        + word_bias
        + !high_mask * input_multiplier
        + high_mask * word_bias
        + partial
        + scaled;

    high_mask = !(byte3 * w(0x79)) | w(0xff);
    partial = high_mask * word_bias
        + byte3 * byte_multiplier
        + word_bias
        + !high_mask * input_multiplier
        + partial * combine_multiplier * w(0x100) * input_multiplier
        + partial;

    partial = partial * combine_multiplier * w(0x100) * input_multiplier + partial;
    partial * combine_multiplier
}

fn byte_index(value: Word) -> usize {
    (value * w(0xbf)).index(0xff)
}

fn state_to_word(canonical_state_word: Word) -> Word {
    let state_encode_multiplier = w(0xc2ed_397f);
    let input_multiplier = w(0xa272_be3f);
    let index_decode = w(0x7f86_31bf);
    let index_multiplier = w(0x5d8d_41c1);
    let state_multiplier = w(0xbcb9_5b41);
    let byte_multiplier = w(0xc83b_ebc7);
    let word_bias = w(0x44e5_7c7e);
    let rotate_input = w(0x1e05_1c21);
    let encoded_or_multiplier = w(0x00f3_9c82);
    let state_word = canonical_state_word * state_encode_multiplier;

    let mut mapped = w(u32::from(
        tables::HASH_BYTE_TABLE_0[byte_index(state_word * state_multiplier)],
    ));
    mapped = (mapped * w(0xc9)) & w(0xff);
    let mut high_mask = !(mapped * w(0x79)) | w(0xff);
    let mut combined = state_word * state_multiplier
        + high_mask * word_bias
        + mapped * byte_multiplier
        + word_bias
        + !high_mask * input_multiplier;
    let mut rotated = encoded_rotate_left(combined * rotate_input, 11);
    rotated = rotated * input_multiplier;

    mapped = w(u32::from(tables::HASH_BYTE_TABLE_1[byte_index(rotated)]));
    mapped = (mapped * w(0xc9)) & w(0xff);
    high_mask = !(mapped * w(0x79)) | w(0xff);
    combined = !high_mask * input_multiplier
        + mapped * byte_multiplier
        + word_bias
        + high_mask * word_bias;
    rotated = encoded_rotate_left(
        (((combined * encoded_or_multiplier - w(1)) | (rotated * encoded_or_multiplier - w(1)))
            * input_multiplier
            + rotated
            + combined
            - index_multiplier)
            * rotate_input,
        10,
    );

    mapped = w(u32::from(
        tables::HASH_BYTE_TABLE_2[byte_index(rotated * input_multiplier)],
    ));
    high_mask = !(mapped * w(0x79)) | w(0xff);
    combined = mapped * byte_multiplier
        + word_bias
        + !high_mask * input_multiplier
        + high_mask * word_bias;
    rotated = encoded_rotate_left((rotated * input_multiplier - combined) * rotate_input, 7);
    rotated * input_multiplier * index_decode
}

pub(super) fn hash(work: &mut [u8; 64]) -> [u8; 32] {
    let mut block = [Word::default(); 16];
    for (index, chunk) in work.chunks_exact(4).enumerate() {
        block[index] = preprocess_word(w(u32::from_le_bytes(
            chunk.try_into().expect("four-byte work word"),
        )));
    }

    let mut state = edonr::initial_state();
    for round in 0..4 {
        edonr::compress(&mut state, &block);
        if round != 3 {
            for index in 0..16 {
                block[index] = state_to_word(state[index]);
            }
        }
    }

    for (chunk, word) in work.chunks_exact_mut(4).zip(block) {
        chunk.copy_from_slice(&word.0.to_le_bytes());
    }
    let mut output = [0_u8; 32];
    for index in 0..8 {
        let value = state[8 + index] * w(0xf46e_f4ff);
        output[index * 4..index * 4 + 4].copy_from_slice(&value.0.to_le_bytes());
    }
    output
}
