use super::{tables, word::Word};

const INPUT_BYTES: usize = 190;
const OUTPUT_BYTES: usize = 16;

const fn w(value: u32) -> Word {
    Word::new(value)
}

fn rotate_left_or_ones(value: Word, count: Word) -> Word {
    let count = count.0 & 31;
    if count == 0 {
        w(u32::MAX)
    } else {
        value.rotate_left(count)
    }
}

fn rotate_right_or_ones(value: Word, count: Word) -> Word {
    let count = count.0 & 31;
    if count == 0 {
        w(u32::MAX)
    } else {
        value.rotate_right(count)
    }
}

fn choose(mask: Word, when_set: Word, when_clear: Word) -> Word {
    (mask & when_set) | (!mask & when_clear)
}

fn majority(a: Word, b: Word, c: Word) -> Word {
    (a & b) | (a & c) | (b & c)
}

fn fold_word(word: Word) -> u8 {
    let bytes = word.0.to_le_bytes();
    (bytes[0].wrapping_add(bytes[1]) ^ bytes[2]).wrapping_add(bytes[3])
}

#[allow(clippy::too_many_lines, clippy::many_single_char_names)]
pub(super) fn reduce(input: &[u8; INPUT_BYTES]) -> [u8; OUTPUT_BYTES] {
    let mut key = tables::KEY_SEED.map(Word::new);
    let mut state = tables::STATE_SEED.map(Word::new);
    let table = tables::REDUCE_TABLE.map(Word::new);
    let mut work = [Word::default(); 128];

    for (index, target) in work.iter_mut().enumerate() {
        let mut assembled = 0_u32;
        for byte in 0..4 {
            let value = input[(4 * index + byte) % INPUT_BYTES].wrapping_mul(0xbd);
            assembled |= u32::from(value) << (24 - 8 * byte);
        }
        *target = w(assembled);
    }

    for _ in 0..4 {
        for index in 0..128 {
            work[index] = (work[index] ^ work[(index.wrapping_sub(13)) & 127].rotate_left(3))
                + work[(index + 71) & 127].rotate_left(5)
                - work[(index + 101) & 127].rotate_left(23);
        }
    }

    let s7 = (work[22] / w(5)) * w(0xc345_6f96);
    let s8 = majority(state[60], table[63], w(0x908d_4d25)) / w(3) + w(0xef96_95dd);
    key[42] += ((table[work[33].index(63)] & w(0x3cba_9069)) + w(0x4344_0114)) / w(5);
    let s9 = choose(state[26], state[work[87].index(63)], !w(0x3cba_9069));
    let rotation = rotate_left_or_ones(w(0x6f72_b2da), -state[work[55].index(63)] & w(31));
    let k = key[s7.index(63)] / w(5);
    let selected = choose(state[42], !w(0x3047_874d), !w(0xb0de_a603));
    let s10 = rotation ^ w(0x908d_4d25);
    key[63] -= majority(selected, k, s7);
    state[25] += key[s7.index(63)];
    state[60] = choose(key[21], s8, !w(0x1fb7_d8d3)) * work[122];
    work[5] += choose(s7, !w(0xd36d_6fb6), table[work[120].index(63)]);

    key[35] += w(0xb1a7_e76f);
    let s18 = key[s10.index(63)] ^ w(0x908d_4d25);
    state[44] ^= w(3) * choose(w(0xc345_6f96), table[29], table[work[18].index(63)]);
    work[107] += state[work[44].index(63)] + w(1);
    let inverted_rotation = !rotate_right_or_ones(w(0xb0de_a603), work[71] & w(31));
    key[54] -= majority(state[29], inverted_rotation, !w(0x6f72_b2da));
    work[62] ^= w(0xdbd3_d289);
    key[19] += state[s7.index(63)] / w(5)
        - majority(state[45], table[work[45].index(63)], !w(0x4cf5_0fa3));
    let squared = state[s18.index(63)] * state[s18.index(63)];
    key[49] = table[squared.index(63)];
    key[21] ^= table[work[27].index(63)];
    let s19 = w(0x908d_4d24) - rotate_left_or_ones(w(0x98b9_9bd8), (state[60] * state[41]) & w(31));
    state[10] = work[73];
    key[20] ^= rotate_left_or_ones(!state[work[98].index(63)], key[60] & w(31));
    key[49] += choose(state[56], key[41], table[s19.index(63)]) >> 1;
    let table_word = table[work[10].index(63)];
    state[24] ^= choose(key[39], state[37], table_word * table_word * table_word);
    let mut s6 = table[work[43].index(63)] & !w(0x4505_9986);
    s6 = (s6 | w(0x4105_0082)) ^ w(0x3cba_9069);
    let rotated = rotate_right_or_ones(!s18, table[work[31].index(63)] & w(31));
    state[13] ^= (key[work[50].index(63)] & !w(0x147f_3886)) | !(rotated | w(0xeb80_c779));

    key[28] -= choose(
        key[35],
        majority(key[58], table[s9.index(63)], w(0x908d_4d25)),
        !key[29],
    );
    state[23] += !majority(s8, state[work[106].index(63)], w(0x2725_fe3b));
    state[24] ^= work[118] * work[118];
    key[35] ^= majority(
        state[54],
        w(2) * work[108],
        state[41] * state[41] * state[41],
    );
    work[34] ^= choose(
        s18,
        state[work[39].index(63)] ^ state[37],
        table[29] + w(0x02e5_a7e3),
    );
    state[42] ^= choose(state[40], state[work[35].index(63)], work[78] * work[78]);
    work[49] += w(3) * key[work[39].index(63)] + w(0x6211_6be6);
    key[48] = s7;
    state[62] = key[s19.index(63)] - state[work[93].index(63)] - w(0xef96_95dd);
    key[55] += key[60] / w(3) - state[work[112].index(63)] / w(3);
    let s21 = majority(s6, table[60] + w(0x4d48_5456), w(0x41d4_4d69)) + w(0x3cba_9069);
    let s25 = s9 ^ key[37];
    state[55] += w(0x1d0f_1b76);
    work[77] -= table[((state[60] & w(127)) >> 1).index(63)];
    state[29] = key[work[68].index(63)] / w(15);
    state[46] ^= table[table[table[37].index(63)].index(63)];
    let s22 = w(0x4f21_59fc) - majority(state[59], table[29], table[60] / w(5));
    let s24 = choose(
        w(0xbd06_dd88),
        majority(w(0x5908_9be9), table[work[16].index(63)], state[60]),
        table[60],
    ) + w(0x4f21_59fc);
    work[10] = w(0xde28_49dd);
    work[8] += majority(
        choose(state[52], w(0x908d_4d25), w(0x5987_b420)),
        work[48] ^ s22,
        w(0x5112_7d0f),
    );
    state[20] += table[work[1].index(63)];
    work[32] += w(1) + rotate_right_or_ones(w(0x0c7a_d237), (s22 * s22 * s22) & w(31));
    state[4] -= choose(key[35], w(0x4f21_59fc), w(0xc054_0978))
        | !rotate_right_or_ones(w(0x46ef_99f4), key[41] & w(31));
    state[60] = !rotate_left_or_ones(
        !(work[62] / w(3)),
        (work[22] + key[work[93].index(63)]) & w(31),
    );
    key[30] = w(0x3cba_9069);
    key[10] = !(table[work[122].index(63)] + w(0x972d_a190));
    state[46] += majority(
        state[8],
        table[work[35].index(63)] | table[41],
        w(0xf3d3_9067),
    );
    let s26 = w(0x3cba_9069)
        - ((table[work[67].index(63)] | table[work[120].index(63)])
            - table[work[98].index(63)]
            - w(1));
    let selected_state = state[work[38].index(63)] >> 1;
    state[17] ^= selected_state * selected_state * selected_state;
    state[44] += table[62];
    key[26] -= table[s6.index(63)];
    let first = choose(w(0x313e_c15d), key[s6.index(63)], state[60]);
    let second = (table[work[79].index(63)] & w(0x7fbb_d9fd)) | w(0x0c20_1068);
    key[35] -= majority(key[34], first, second);
    key[16] ^= majority(state[43], work[27] >> 1, state[41] | state[37]);
    work[33] = w(0x1c3e_9665);
    key[58] -= work[118];
    state[21] += key[60];
    state[10] -= state[60];

    let initial = [
        (w(0xba) + s25).low_byte(),
        0x97,
        0x97,
        (w(0x9b) + s24).low_byte(),
        (w(0xba) + s8).low_byte(),
        0x97,
        (w(0xd3) + s22).low_byte(),
        0x97,
        (w(0xbe) + s18 + s6).low_byte(),
        (w(0x27) + s7).low_byte(),
        0x4c,
        (w(0x27) + s19).low_byte(),
        (w(0xbe) + s10 + s21).low_byte(),
        (w(0xe3) + s26).low_byte(),
        0x4c,
        (w(0x27) + work[92]).low_byte(),
    ];

    let mut output = [0_u8; OUTPUT_BYTES];
    for index in 0..OUTPUT_BYTES {
        let mut reduced = initial[index];
        for slot in (index..64).step_by(OUTPUT_BYTES) {
            reduced ^= key[slot].low_byte() ^ state[slot].low_byte();
        }
        for slot in (index..128).step_by(OUTPUT_BYTES) {
            reduced ^= fold_word(work[slot]);
        }
        output[index] = 0xa5_u8.wrapping_mul(reduced);
    }
    output
}
