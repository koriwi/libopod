use super::{hash, word::Word};

const OUTPUT_BYTES: usize = 190;

pub(super) fn expand(middle_ciphertext: &[u8; 24], leading_ciphertext: &[u8; 24]) -> [u8; 190] {
    let mut work = [0_u8; 64];
    for (target, source) in work[..24].iter_mut().zip(middle_ciphertext) {
        *target = source.wrapping_mul(0x6f);
    }
    for (target, source) in work[24..44].iter_mut().zip(&leading_ciphertext[..20]) {
        *target = source.wrapping_mul(0x6f);
    }
    work[48..52].copy_from_slice(&12_u32.to_le_bytes());

    let mut output = [0_u8; OUTPUT_BYTES];
    for iteration in 0..6_u32 {
        let offset = usize::try_from(iteration).unwrap_or(0) * 32;
        work[44..48].copy_from_slice(&iteration.to_le_bytes());
        let digest = hash::hash(&mut work);
        let chunk = transform_hash_chunk(&digest);
        let copy_length = (OUTPUT_BYTES - offset).min(chunk.len());
        output[offset..offset + copy_length].copy_from_slice(&chunk[..copy_length]);
    }
    output
}

fn transform_hash_chunk(hash: &[u8; 32]) -> [u8; 32] {
    let mut output = [0_u8; 32];
    for (input, target) in hash.chunks_exact(4).zip(output.chunks_exact_mut(4)) {
        let word = Word::new(u32::from_le_bytes(
            input.try_into().expect("four-byte digest word"),
        ));
        let product = word * Word::new(0xc818_0aff);
        let transformed = u32::from((word * Word::new(0x6b)).low_byte())
            | (u32::from(((product >> 8) * Word::new(0x95)).low_byte()) << 8)
            | (u32::from(((product >> 16) * Word::new(0x95)).low_byte()) << 16)
            | (u32::from(((product >> 24) * Word::new(0x95)).low_byte()) << 24);
        target.copy_from_slice(&transformed.to_le_bytes());
    }
    output
}
