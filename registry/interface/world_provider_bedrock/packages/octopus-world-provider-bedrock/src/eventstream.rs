use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub(crate) struct EventStreamMessage {
    pub headers: BTreeMap<String, HeaderValue>,
    pub payload: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum HeaderValue {
    Bool(bool),
    Byte(i8),
    Int16(i16),
    Int32(i32),
    Int64(i64),
    Bytes(Vec<u8>),
    String(String),
    Timestamp(i64),
    Uuid([u8; 16]),
}

impl EventStreamMessage {
    pub(crate) fn string_header(&self, name: &str) -> Result<Option<&str>, String> {
        match self.headers.get(name) {
            None => Ok(None),
            Some(HeaderValue::String(value)) => Ok(Some(value)),
            Some(_) => Err(format!("AWS EventStream header '{name}' is not a string")),
        }
    }
}

pub(crate) fn decode_messages(
    bytes: &[u8],
    maximum_frame_bytes: usize,
) -> Result<Vec<EventStreamMessage>, String> {
    if maximum_frame_bytes < 16 {
        return Err("AWS EventStream maximum_frame_bytes must be at least 16".to_string());
    }
    let mut cursor = 0usize;
    let mut messages = Vec::new();
    while cursor < bytes.len() {
        let remaining = &bytes[cursor..];
        if remaining.len() < 16 {
            return Err(format!(
                "AWS EventStream ends with an incomplete {} byte frame",
                remaining.len()
            ));
        }
        let total_length = read_u32(&remaining[0..4])? as usize;
        let headers_length = read_u32(&remaining[4..8])? as usize;
        if total_length < 16 || total_length > maximum_frame_bytes {
            return Err(format!(
                "AWS EventStream frame length {total_length} is outside 16..={maximum_frame_bytes}"
            ));
        }
        if total_length > remaining.len() {
            return Err(format!(
                "AWS EventStream frame declares {total_length} bytes but only {} remain",
                remaining.len()
            ));
        }
        if headers_length > total_length - 16 {
            return Err(format!(
                "AWS EventStream headers length {headers_length} exceeds frame body"
            ));
        }
        let expected_prelude_crc = read_u32(&remaining[8..12])?;
        let actual_prelude_crc = crc32fast::hash(&remaining[..8]);
        if expected_prelude_crc != actual_prelude_crc {
            return Err(format!(
                "AWS EventStream prelude CRC mismatch: expected {expected_prelude_crc:08x}, computed {actual_prelude_crc:08x}"
            ));
        }
        let frame = &remaining[..total_length];
        let expected_message_crc = read_u32(&frame[total_length - 4..])?;
        let actual_message_crc = crc32fast::hash(&frame[..total_length - 4]);
        if expected_message_crc != actual_message_crc {
            return Err(format!(
                "AWS EventStream message CRC mismatch: expected {expected_message_crc:08x}, computed {actual_message_crc:08x}"
            ));
        }
        let header_start = 12;
        let header_end = header_start + headers_length;
        let headers = decode_headers(&frame[header_start..header_end])?;
        let payload = frame[header_end..total_length - 4].to_vec();
        messages.push(EventStreamMessage { headers, payload });
        cursor = cursor
            .checked_add(total_length)
            .ok_or_else(|| "AWS EventStream cursor overflowed".to_string())?;
    }
    if messages.is_empty() {
        return Err("AWS EventStream response contains no frames".to_string());
    }
    Ok(messages)
}

fn decode_headers(bytes: &[u8]) -> Result<BTreeMap<String, HeaderValue>, String> {
    let mut cursor = 0usize;
    let mut headers = BTreeMap::new();
    while cursor < bytes.len() {
        let name_length = *bytes
            .get(cursor)
            .ok_or_else(|| "AWS EventStream header has no name length".to_string())?
            as usize;
        cursor += 1;
        if name_length == 0 {
            return Err("AWS EventStream header name is empty".to_string());
        }
        let name_end = checked_end(cursor, name_length, bytes.len(), "header name")?;
        let name = std::str::from_utf8(&bytes[cursor..name_end])
            .map_err(|error| format!("AWS EventStream header name is not UTF-8: {error}"))?
            .to_string();
        cursor = name_end;
        let value_type = *bytes
            .get(cursor)
            .ok_or_else(|| format!("AWS EventStream header '{name}' has no value type"))?;
        cursor += 1;
        let value = match value_type {
            0 => HeaderValue::Bool(true),
            1 => HeaderValue::Bool(false),
            2 => {
                let value = *bytes
                    .get(cursor)
                    .ok_or_else(|| format!("AWS EventStream byte header '{name}' is truncated"))?;
                cursor += 1;
                HeaderValue::Byte(value as i8)
            }
            3 => {
                let end = checked_end(cursor, 2, bytes.len(), "int16 header")?;
                let value = i16::from_be_bytes(bytes[cursor..end].try_into().unwrap());
                cursor = end;
                HeaderValue::Int16(value)
            }
            4 => {
                let end = checked_end(cursor, 4, bytes.len(), "int32 header")?;
                let value = i32::from_be_bytes(bytes[cursor..end].try_into().unwrap());
                cursor = end;
                HeaderValue::Int32(value)
            }
            5 => {
                let end = checked_end(cursor, 8, bytes.len(), "int64 header")?;
                let value = i64::from_be_bytes(bytes[cursor..end].try_into().unwrap());
                cursor = end;
                HeaderValue::Int64(value)
            }
            6 => {
                let length_end = checked_end(cursor, 2, bytes.len(), "bytes header length")?;
                let length =
                    u16::from_be_bytes(bytes[cursor..length_end].try_into().unwrap()) as usize;
                cursor = length_end;
                let value_end = checked_end(cursor, length, bytes.len(), "bytes header value")?;
                let value = bytes[cursor..value_end].to_vec();
                cursor = value_end;
                HeaderValue::Bytes(value)
            }
            7 => {
                let length_end = checked_end(cursor, 2, bytes.len(), "string header length")?;
                let length =
                    u16::from_be_bytes(bytes[cursor..length_end].try_into().unwrap()) as usize;
                cursor = length_end;
                let value_end = checked_end(cursor, length, bytes.len(), "string header value")?;
                let value = std::str::from_utf8(&bytes[cursor..value_end])
                    .map_err(|error| {
                        format!("AWS EventStream string header '{name}' is not UTF-8: {error}")
                    })?
                    .to_string();
                cursor = value_end;
                HeaderValue::String(value)
            }
            8 => {
                let end = checked_end(cursor, 8, bytes.len(), "timestamp header")?;
                let value = i64::from_be_bytes(bytes[cursor..end].try_into().unwrap());
                cursor = end;
                HeaderValue::Timestamp(value)
            }
            9 => {
                let end = checked_end(cursor, 16, bytes.len(), "UUID header")?;
                let value: [u8; 16] = bytes[cursor..end].try_into().unwrap();
                cursor = end;
                HeaderValue::Uuid(value)
            }
            other => {
                return Err(format!(
                    "AWS EventStream header '{name}' has unknown value type {other}"
                ));
            }
        };
        if headers.insert(name.clone(), value).is_some() {
            return Err(format!("AWS EventStream repeats header '{name}'"));
        }
    }
    Ok(headers)
}

fn checked_end(
    start: usize,
    length: usize,
    maximum: usize,
    surface: &str,
) -> Result<usize, String> {
    let end = start
        .checked_add(length)
        .ok_or_else(|| format!("AWS EventStream {surface} length overflowed"))?;
    if end > maximum {
        return Err(format!("AWS EventStream {surface} is truncated"));
    }
    Ok(end)
}

fn read_u32(bytes: &[u8]) -> Result<u32, String> {
    let raw: [u8; 4] = bytes
        .try_into()
        .map_err(|_| "AWS EventStream u32 is truncated".to_string())?;
    Ok(u32::from_be_bytes(raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(event_type: &str, payload: &[u8]) -> Vec<u8> {
        let mut headers = Vec::new();
        for (name, value) in [
            (":message-type", "event"),
            (":event-type", event_type),
            (":content-type", "application/json"),
        ] {
            headers.push(name.len() as u8);
            headers.extend_from_slice(name.as_bytes());
            headers.push(7);
            headers.extend_from_slice(&(value.len() as u16).to_be_bytes());
            headers.extend_from_slice(value.as_bytes());
        }
        let total = 16 + headers.len() + payload.len();
        let mut message = Vec::with_capacity(total);
        message.extend_from_slice(&(total as u32).to_be_bytes());
        message.extend_from_slice(&(headers.len() as u32).to_be_bytes());
        message.extend_from_slice(&crc32fast::hash(&message[..8]).to_be_bytes());
        message.extend_from_slice(&headers);
        message.extend_from_slice(payload);
        let crc = crc32fast::hash(&message);
        message.extend_from_slice(&crc.to_be_bytes());
        message
    }

    #[test]
    fn decodes_multiple_crc_checked_messages() {
        let mut bytes = frame("messageStart", br#"{"role":"assistant"}"#);
        bytes.extend(frame(
            "contentBlockDelta",
            br#"{"contentBlockIndex":0,"delta":{"text":"hello"}}"#,
        ));
        let messages = decode_messages(&bytes, 1024).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages[1].string_header(":event-type").unwrap(),
            Some("contentBlockDelta")
        );
    }

    #[test]
    fn rejects_corrupted_message_crc() {
        let mut bytes = frame("messageStart", br#"{"role":"assistant"}"#);
        let index = bytes.len() - 5;
        bytes[index] ^= 1;
        assert!(decode_messages(&bytes, 1024)
            .unwrap_err()
            .contains("CRC mismatch"));
    }
}
