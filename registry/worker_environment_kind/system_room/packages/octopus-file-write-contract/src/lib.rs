use std::io::Write;

use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JsonFormat {
    #[default]
    Pretty,
    Compact,
}

impl JsonFormat {
    pub fn encode(self, value: &serde_json::Value) -> Result<Vec<u8>, String> {
        let mut bytes = match self {
            Self::Pretty => serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?,
            Self::Compact => octopus_descriptors::canonical_bytes(value),
        };
        bytes.push(b'\n');
        Ok(bytes)
    }
}

pub fn optional_json_format<'de, D: serde::Deserializer<'de>>(
    decoder: D,
) -> Result<Option<JsonFormat>, D::Error> {
    JsonFormat::deserialize(decoder).map(Some)
}

pub struct DecodeBudget {
    remaining: u64,
}

impl DecodeBudget {
    pub fn new(maximum: u64) -> Result<Self, String> {
        if maximum == 0 || usize::try_from(maximum).is_err() {
            return Err("file.write decoded-byte budget must fit a positive host limit".into());
        }
        Ok(Self { remaining: maximum })
    }

    pub fn charge(&mut self, bytes: u64) -> Result<(), String> {
        self.remaining = self
            .remaining
            .checked_sub(bytes)
            .ok_or("file.write decoded preimages and postimages exceed the host payload budget")?;
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Preimage {
    Text(String),
    Encoded(EncodedPreimage),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EncodedPreimage {
    encoding: PreimageEncoding,
    byte_count: u64,
    data: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum PreimageEncoding {
    ZlibBase64,
}

impl Preimage {
    pub fn encode(bytes: &[u8]) -> Result<Self, String> {
        std::str::from_utf8(bytes).map_err(|error| error.to_string())?;
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(3));
        encoder
            .write_all(bytes)
            .map_err(|error| error.to_string())?;
        let compressed = encoder.finish().map_err(|error| error.to_string())?;
        Ok(Self::Encoded(EncodedPreimage {
            encoding: PreimageEncoding::ZlibBase64,
            byte_count: bytes.len() as u64,
            data: STANDARD.encode(compressed),
        }))
    }

    pub fn decode(&self, expected_hash: &str, budget: &mut DecodeBudget) -> Result<String, String> {
        let bytes = match self {
            Self::Text(text) => {
                budget.charge(text.len() as u64)?;
                text.as_bytes().to_vec()
            }
            Self::Encoded(encoded) => {
                budget.charge(encoded.byte_count)?;
                let compressed = STANDARD
                    .decode(&encoded.data)
                    .map_err(|error| error.to_string())?;
                if STANDARD.encode(&compressed) != encoded.data {
                    return Err("file.write preimage has noncanonical base64".into());
                }
                let mut decoder = flate2::Decompress::new(true);
                let capacity = usize::try_from(encoded.byte_count)
                    .map_err(|error| error.to_string())?
                    .checked_add(1)
                    .ok_or("preimage length overflow")?;
                let mut bytes = vec![0; capacity];
                let status = decoder
                    .decompress(&compressed, &mut bytes, flate2::FlushDecompress::Finish)
                    .map_err(|error| error.to_string())?;
                if status != flate2::Status::StreamEnd
                    || decoder.total_out() != encoded.byte_count
                    || decoder.total_in() != compressed.len() as u64
                {
                    return Err("file.write preimage has a wrong length, incomplete stream or trailing data".into());
                }
                bytes.truncate(encoded.byte_count as usize);
                bytes
            }
        };
        if blake3::hash(&bytes).to_hex().as_str() != expected_hash {
            return Err("file.write preimage hash differs from its exact decoded bytes".into());
        }
        String::from_utf8(bytes).map_err(|error| error.to_string())
    }
}
