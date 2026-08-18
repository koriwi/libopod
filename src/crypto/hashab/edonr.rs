use super::word::Word;

const INITIAL_STATE: [u32; 16] = [
    0x4041_4243,
    0x4445_4647,
    0x4849_4a4b,
    0x4c4d_4e4f,
    0x5051_5253,
    0x5455_5657,
    0x5859_5a5b,
    0x5c5d_5e5f,
    0x6061_6263,
    0x6465_6667,
    0x6869_6a6b,
    0x6c6d_6e6f,
    0x7071_7273,
    0x7475_7677,
    0x7879_7a7b,
    0x7c7d_7e7f,
];

pub(super) fn initial_state() -> [Word; 16] {
    INITIAL_STATE.map(Word::new)
}

pub(super) fn compress(state: &mut [Word; 16], block: &[Word; 16]) {
    let mut p16 = q256(
        [
            block[15], block[14], block[13], block[12], block[11], block[10], block[9], block[8],
        ],
        block[..8].try_into().expect("fixed slice"),
    );
    let mut p24 = q256(p16, block[8..].try_into().expect("fixed slice"));

    p16 = q256(state[8..].try_into().expect("fixed slice"), p16);
    p24 = q256(p16, p24);

    p16 = q256(p16, state[..8].try_into().expect("fixed slice"));
    p24 = q256(p24, p16);

    p16 = q256(
        [
            block[7], block[6], block[5], block[4], block[3], block[2], block[1], block[0],
        ],
        p16,
    );
    p24 = q256(p16, p24);

    for index in 0..8 {
        state[index] ^= block[index + 8] ^ p16[index];
        state[index + 8] ^= block[index] ^ p24[index];
    }
}

#[allow(clippy::many_single_char_names)]
fn q256(x: [Word; 8], y: [Word; 8]) -> [Word; 8] {
    let t8 = x[0] + x[4];
    let t9 = x[1] + x[7];
    let t12 = t8 + t9;
    let t10 = x[2] + x[3];
    let t11 = x[5] + x[6];
    let t13 = t10 + t11;
    let t0 = Word::new(0xaaaa_aaaa) + t12 + x[2];
    let t1 = (t12 + x[3]).rotate_left(4);
    let t2 = (t12 + x[6]).rotate_left(8);
    let t3 = (t13 + x[7]).rotate_left(13);
    let t4 = (x[1] + t13).rotate_left(17);
    let t5 = (t8 + t10 + x[5]).rotate_left(22);
    let t6 = (x[0] + t9 + t11).rotate_left(24);
    let t7 = (t13 + x[4]).rotate_left(29);
    let t16 = t0 ^ t4;
    let t17 = t1 ^ t7;
    let t18 = t2 ^ t3;
    let t19 = t5 ^ t6;
    let a5 = t3 ^ t19;
    let a6 = t2 ^ t19;
    let a7 = t18 ^ t5;
    let a0 = t16 ^ t1;
    let a1 = t16 ^ t7;
    let a2 = t17 ^ t6;
    let a3 = t18 ^ t4;
    let a4 = t0 ^ t17;

    let t16 = y[0] + y[1];
    let t17 = y[2] + y[5];
    let t20 = t16 + t17;
    let t18 = y[3] + y[4];
    let t22 = t16 + t18;
    let t19 = y[6] + y[7];
    let t21 = t18 + t19;
    let t23 = t17 + t19;
    let t0 = Word::new(0x5555_5555) + t20 + y[7];
    let t1 = (t22 + y[6]).rotate_left(5);
    let t2 = (t20 + y[3]).rotate_left(9);
    let t3 = (y[2] + t21).rotate_left(11);
    let t4 = (t22 + y[5]).rotate_left(15);
    let t5 = (t23 + y[4]).rotate_left(20);
    let t6 = (y[1] + t23).rotate_left(25);
    let t7 = (y[0] + t21).rotate_left(27);
    let t16 = t0 ^ t1;
    let t17 = t2 ^ t5;
    let t18 = t3 ^ t4;
    let t19 = t6 ^ t7;

    [
        a0 + (t16 ^ t5),
        a1 + (t2 ^ t19),
        a2 + (t16 ^ t3),
        a3 + (t0 ^ t18),
        a4 + (t1 ^ t17),
        a5 + (t18 ^ t6),
        a6 + (t17 ^ t7),
        a7 + (t4 ^ t19),
    ]
}
