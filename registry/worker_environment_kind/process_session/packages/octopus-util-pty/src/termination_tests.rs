use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

struct DeniedOnce(Arc<AtomicUsize>);

impl ChildTerminator for DeniedOnce {
    fn signal(&mut self, signal: ProcessSignal) -> io::Result<()> {
        Err(unsupported_signal(signal))
    }

    fn kill(&mut self) -> io::Result<()> {
        if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(io::Error::from_raw_os_error(1))
        } else {
            Ok(())
        }
    }

    fn termination_scope_has_exited(&self) -> io::Result<bool> {
        Ok(false)
    }
}

#[tokio::test]
async fn denied_termination_keeps_control_and_io_for_retry() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let (writer_tx, _writer_rx) = mpsc::channel(1);
    let reader = tokio::spawn(std::future::pending());
    let reader_abort = reader.abort_handle();
    let waiter = tokio::spawn(std::future::pending());
    let waiter_abort = waiter.abort_handle();
    let session = ProcessHandle::new(
        writer_tx, Box::new(DeniedOnce(Arc::clone(&attempts))), reader, Vec::new(),
        tokio::spawn(std::future::pending()), waiter, Arc::new(AtomicBool::new(false)),
        Arc::new(StdMutex::new(None)), None, None,
    );

    assert_eq!(session.terminate().unwrap_err().raw_os_error(), Some(1));
    assert!(!session.has_exited());
    assert!(!session.termination_scope_has_exited().unwrap());
    assert!(!reader_abort.is_finished());
    assert!(!session.writer_sender().is_closed());
    session.terminate().unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert!(!session.has_exited());
    waiter_abort.abort();
}
