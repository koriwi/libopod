use super::{tables, word::Word};

const BLOCK_BYTES: usize = 16;
const ROUNDS: usize = 10;

struct Profile {
    input_map: &'static [u8; 256],
    final_map: &'static [u8; 256],
    output_map: &'static [u8; 256],
    round_base: &'static [u8; 256],
    round_affine: &'static [u8; 135],
    schedule: &'static [u8; 176],
}

const STAGE1: Profile = Profile {
    input_map: &tables::STAGE1_INPUT_MAP,
    final_map: &tables::STAGE1_FINAL_MAP,
    output_map: &tables::STAGE1_OUTPUT_MAP,
    round_base: &tables::STAGE1_ROUND_BASE,
    round_affine: &tables::STAGE1_ROUND_AFFINE,
    schedule: &tables::STAGE1_SCHEDULE,
};

const STAGE2: Profile = Profile {
    input_map: &tables::STAGE2_INPUT_MAP,
    final_map: &tables::STAGE2_FINAL_MAP,
    output_map: &tables::STAGE2_OUTPUT_MAP,
    round_base: &tables::STAGE2_ROUND_BASE,
    round_affine: &tables::STAGE2_ROUND_AFFINE,
    schedule: &tables::STAGE2_SCHEDULE,
};

pub(super) fn stage1(input: &[u8; 80]) -> [u8; 80] {
    let mut output = [0_u8; 80];
    cipher_cbc(&STAGE1, &mut output, input, &tables::STAGE1_INITIAL_CHAIN);
    output
}

pub(super) fn stage2(input: &[u8; 32], initialization_vector: &[u8; 16]) -> [u8; 32] {
    let mut initial_chain = [0_u8; BLOCK_BYTES];
    for (target, value) in initial_chain.iter_mut().zip(initialization_vector) {
        *target = inverse_map_byte(STAGE2.output_map, *value);
    }
    let mut output = [0_u8; 32];
    cipher_cbc(&STAGE2, &mut output, input, &initial_chain);
    output
}

fn affine_byte(parameters: &[u8], value: u8) -> u8 {
    let mut result = parameters[0];
    for bit in 0..8 {
        if value & (1 << bit) != 0 {
            result ^= parameters[bit + 1];
        }
    }
    result
}

fn profile_word(profile: &Profile, table_number: usize, index: u8) -> Word {
    let base = profile.round_base[usize::from(index)];
    let mut bytes = [0_u8; 4];
    for (byte, target) in bytes.iter_mut().enumerate() {
        let lane = table_number * 4 + byte;
        *target = if lane == 0 {
            base
        } else {
            let start = (lane - 1) * 9;
            affine_byte(&profile.round_affine[start..start + 9], base)
        };
    }
    Word::new(u32::from_le_bytes(bytes))
}

fn table_round(profile: &Profile, input: &[u8; 16], round_key: &[u8]) -> [u8; 16] {
    let mut output = [0_u8; 16];
    for word in 0..4 {
        let key_offset = word * 4;
        let mut value = Word::new(u32::from_le_bytes(
            round_key[key_offset..key_offset + 4]
                .try_into()
                .expect("four-byte round-key word"),
        ));
        for table in 0..4 {
            value ^= profile_word(profile, table, input[(word * 4 + table * 5) & 15]);
        }
        output[key_offset..key_offset + 4].copy_from_slice(&value.0.to_le_bytes());
    }
    output
}

fn final_round(profile: &Profile, input: &[u8; 16], round_key: &[u8]) -> [u8; 16] {
    let mut output = [0_u8; 16];
    for position in 0..BLOCK_BYTES {
        let input_position = ((position & 0x0c) + (position & 0x03) * 5) & 0x0f;
        let table_value = profile.final_map[usize::from(input[input_position])];
        output[position] = table_value.wrapping_mul(0x81).wrapping_add(0x7a) ^ round_key[position];
    }
    output
}

fn inverse_map_byte(map: &[u8; 256], value: u8) -> u8 {
    map.iter()
        .position(|candidate| *candidate == value)
        .and_then(|index| u8::try_from(index).ok())
        .unwrap_or(0)
}

fn cipher_cbc(profile: &Profile, output: &mut [u8], input: &[u8], initial_chain: &[u8; 16]) {
    debug_assert_eq!(input.len(), output.len());
    debug_assert!(!input.is_empty());
    debug_assert_eq!(input.len() % BLOCK_BYTES, 0);

    let mut chaining = *initial_chain;
    for (input_block, output_block) in input
        .chunks_exact(BLOCK_BYTES)
        .zip(output.chunks_exact_mut(BLOCK_BYTES))
    {
        let mut state = [0_u8; BLOCK_BYTES];
        for position in 0..BLOCK_BYTES {
            state[position] = profile.input_map[usize::from(input_block[position])]
                ^ chaining[position]
                ^ profile.schedule[position];
        }
        for round in 1..ROUNDS {
            let key_start = round * BLOCK_BYTES;
            state = table_round(
                profile,
                &state,
                &profile.schedule[key_start..key_start + BLOCK_BYTES],
            );
        }
        let key_start = ROUNDS * BLOCK_BYTES;
        let encrypted = final_round(
            profile,
            &state,
            &profile.schedule[key_start..key_start + BLOCK_BYTES],
        );
        for position in 0..BLOCK_BYTES {
            chaining[position] = encrypted[position];
            output_block[position] = profile.output_map[usize::from(encrypted[position])];
        }
    }
}
