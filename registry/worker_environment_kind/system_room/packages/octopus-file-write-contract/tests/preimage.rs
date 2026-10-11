use base64::{Engine, engine::general_purpose::STANDARD};
use octopus_file_write_contract::{DecodeBudget, JsonFormat, Preimage};
use serde_json::json;

#[test]
fn exact_preimages_require_bounded_complete_canonical_streams() {
    let bytes = b"{\n  \"manual\": [1, 2, 3], \"text\": \"exact whitespace\"\n}\n";
    let hash = blake3::hash(bytes).to_hex().to_string();
    let encoded = serde_json::to_value(Preimage::encode(bytes).unwrap()).unwrap();
    let decode = |value, maximum| {
        serde_json::from_value::<Preimage>(value)
            .map_err(|error| error.to_string())?
            .decode(&hash, &mut DecodeBudget::new(maximum)?)
    };
    assert_eq!(
        decode(encoded.clone(), bytes.len() as u64)
            .unwrap()
            .as_bytes(),
        bytes
    );
    let text = json!(std::str::from_utf8(bytes).unwrap());
    assert_eq!(
        serde_json::to_value(serde_json::from_value::<Preimage>(text.clone()).unwrap()).unwrap(),
        text
    );
    assert_eq!(decode(text, bytes.len() as u64).unwrap().as_bytes(), bytes);
    assert!(decode(encoded.clone(), bytes.len() as u64 - 1).is_err());
    let compressed = STANDARD.decode(encoded["data"].as_str().unwrap()).unwrap();
    for length in 0..compressed.len() {
        let mut truncated = encoded.clone();
        truncated["data"] = json!(STANDARD.encode(&compressed[..length]));
        assert!(decode(truncated, 4096).is_err(), "truncated at {length}");
    }
    let mut trailing = compressed.clone();
    trailing.push(0);
    let mut concatenated = compressed.clone();
    concatenated.extend_from_slice(&compressed);
    for (field, invalid) in [
        ("encoding", json!("gzip_base64")),
        ("byte_count", json!(1)),
        ("byte_count", json!(u64::MAX)),
        ("byte_count", json!(bytes.len() + 1)),
        ("data", json!(STANDARD.encode(trailing))),
        ("data", json!(STANDARD.encode(concatenated))),
        (
            "data",
            json!(format!("{}\n", encoded["data"].as_str().unwrap())),
        ),
        ("foreign", json!(true)),
    ] {
        let mut changed = encoded.clone();
        changed[field] = invalid;
        assert!(decode(changed, 4096).is_err(), "{field}");
    }
    let mut budget = DecodeBudget::new(2 * bytes.len() as u64 - 1).unwrap();
    let preimage: Preimage = serde_json::from_value(encoded).unwrap();
    preimage.decode(&hash, &mut budget).unwrap();
    assert!(preimage.decode(&hash, &mut budget).is_err());
    assert!(
        preimage
            .decode(&"0".repeat(64), &mut DecodeBudget::new(4096).unwrap())
            .is_err()
    );
    let value = serde_json::from_slice(bytes).unwrap();
    assert_ne!(
        JsonFormat::Pretty.encode(&value).unwrap(),
        JsonFormat::Compact.encode(&value).unwrap()
    );
    for format in [JsonFormat::Pretty, JsonFormat::Compact] {
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&format.encode(&value).unwrap()).unwrap(),
            value
        );
    }
}
