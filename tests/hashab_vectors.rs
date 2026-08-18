use libopod::crypto::hashab::{calculate, CBK_RANDOM, SIGNATURE_BYTES};
use serde::Deserialize;

#[derive(Deserialize)]
struct Vector {
    sha1: String,
    uuid: String,
    target: String,
}

#[test]
fn matches_all_public_hashab_vectors() {
    let vectors: Vec<Vector> =
        serde_json::from_str(include_str!("fixtures/hashab-vectors.json")).unwrap();
    assert_eq!(vectors.len(), 100);

    for (index, vector) in vectors.iter().enumerate() {
        let sha1: [u8; 20] = decode_hex(&vector.sha1).try_into().unwrap();
        let guid: [u8; 8] = decode_hex(&vector.uuid).try_into().unwrap();
        let expected: [u8; SIGNATURE_BYTES] = decode_hex(&vector.target).try_into().unwrap();
        assert_eq!(
            calculate(&sha1, &guid, &CBK_RANDOM),
            expected,
            "HASHAB vector {index}"
        );
    }
}

fn decode_hex(value: &str) -> Vec<u8> {
    assert_eq!(value.len() % 2, 0);
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let text = std::str::from_utf8(pair).unwrap();
            u8::from_str_radix(text, 16).unwrap()
        })
        .collect()
}
