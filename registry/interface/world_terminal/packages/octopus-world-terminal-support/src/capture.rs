use std::collections::VecDeque;
use std::io;
use std::io::Write;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;

use tracing_subscriber::fmt::MakeWriter;

const CAPTURE_CAPACITY_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone)]
pub struct SupportLogCapture {
    bytes: Arc<Mutex<VecDeque<u8>>>,
}

impl SupportLogCapture {
    pub fn global() -> Self {
        static GLOBAL: OnceLock<SupportLogCapture> = OnceLock::new();
        GLOBAL
            .get_or_init(|| Self {
                bytes: Arc::new(Mutex::new(VecDeque::with_capacity(CAPTURE_CAPACITY_BYTES))),
            })
            .clone()
    }

    pub fn snapshot_tail(&self, limit: usize) -> Vec<u8> {
        let bytes = self
            .bytes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let keep = limit.min(bytes.len());
        bytes.iter().skip(bytes.len() - keep).copied().collect()
    }
}

pub struct SupportLogWriter {
    bytes: Arc<Mutex<VecDeque<u8>>>,
}

impl Write for SupportLogWriter {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        let mut bytes = self
            .bytes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if input.len() >= CAPTURE_CAPACITY_BYTES {
            bytes.clear();
            bytes.extend(
                input[input.len() - CAPTURE_CAPACITY_BYTES..]
                    .iter()
                    .copied(),
            );
            return Ok(input.len());
        }
        let overflow = bytes
            .len()
            .saturating_add(input.len())
            .saturating_sub(CAPTURE_CAPACITY_BYTES);
        bytes.drain(..overflow);
        bytes.extend(input.iter().copied());
        Ok(input.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for SupportLogCapture {
    type Writer = SupportLogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        SupportLogWriter {
            bytes: self.bytes.clone(),
        }
    }
}
