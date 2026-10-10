pub(crate) mod executor_stream;
mod harness;
mod message_framing;
mod ordered_ciphertext;

use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

use crate::ExecServerError;

pub(crate) use harness::NoiseHarnessConnectionArgs;
pub(crate) use harness::noise_harness_connection_from_websocket;

pub(crate) const NOISE_RELAY_RESET_REASON: &str = "noise_relay_protocol_error";

const MAX_NOISE_RELAY_WEBSOCKET_MESSAGE_SIZE: usize = 256 * 1024;

pub(crate) fn noise_relay_websocket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_frame_size(Some(MAX_NOISE_RELAY_WEBSOCKET_MESSAGE_SIZE))
        .max_message_size(Some(MAX_NOISE_RELAY_WEBSOCKET_MESSAGE_SIZE))
}

fn take_next_sequence(next_seq: &mut u32) -> Result<u32, ExecServerError> {
    let seq = *next_seq;
    *next_seq = next_seq.checked_add(1).ok_or_else(|| {
        ExecServerError::Protocol("Noise relay sequence number exhausted".to_string())
    })?;
    Ok(seq)
}
