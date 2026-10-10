use std::sync::{Arc, Mutex};

use octopus_ledger::{Movement, MovementDraft, MovementLog, Seq};
use octopus_ledger_index::LedgerSqliteIndex;

struct LedgerWriterInner {
    log: Box<dyn MovementLog + Send>,
    spotlight: Option<LedgerSqliteIndex>,
}

#[derive(Clone)]
pub struct LedgerWriter {
    inner: Arc<Mutex<LedgerWriterInner>>,
}

impl LedgerWriter {
    pub fn new<L: MovementLog + Send + 'static>(
        log: L,
        spotlight: Option<LedgerSqliteIndex>,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(LedgerWriterInner {
                log: Box::new(log),
                spotlight,
            })),
        }
    }
}

impl MovementLog for LedgerWriter {
    fn append(&mut self, draft: MovementDraft) -> Movement {
        let mut guard = self
            .inner
            .lock()
            .expect("ledger writer mutex is not poisoned");
        let inner: &mut LedgerWriterInner = &mut guard;
        let movement = inner.log.append(draft);
        let ingest_error = match inner.spotlight.as_mut() {
            Some(spotlight) => spotlight.ingest_from(inner.log.as_ref()).err(),
            None => None,
        };
        if let Some(error) = ingest_error {
            eprintln!(
                "run_daemon: ledger spotlight ingest failed (cache goes stale; ledger untouched; \
                 next boot rebuilds): {error}"
            );
            inner.spotlight = None;
        }
        movement
    }

    fn read_from(&self, from: Seq) -> Vec<Movement> {
        self.inner
            .lock()
            .expect("ledger writer mutex is not poisoned")
            .log
            .read_from(from)
    }

    fn read_at(&self, seq: Seq) -> Option<Movement> {
        self.inner
            .lock()
            .expect("ledger writer mutex is not poisoned")
            .log
            .read_at(seq)
    }

    fn len(&self) -> usize {
        self.inner
            .lock()
            .expect("ledger writer mutex is not poisoned")
            .log
            .len()
    }

    fn durable_end_offset(&self) -> Option<u64> {
        self.inner
            .lock()
            .expect("ledger writer mutex is not poisoned")
            .log
            .durable_end_offset()
    }

    fn durable_row_byte_range(&self, seq: Seq) -> Option<(u64, u64)> {
        self.inner
            .lock()
            .expect("ledger writer mutex is not poisoned")
            .log
            .durable_row_byte_range(seq)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use octopus_descriptors::InterfaceId;
    use octopus_ledger::{Endpoint, InMemoryLog, MovementKind, TraceId};

    fn moved(run: &str) -> MovementDraft {
        MovementDraft {
            trace: TraceId("t".into()),
            kind: MovementKind::ArtifactMoved,
            from: Endpoint::Interface(InterfaceId::new(run)),
            to: Endpoint::Interface(InterfaceId::new(run)),
            artifact: None,
            invocation_metadata: None,
            causal_floor: None,
        }
    }

    #[test]
    fn clones_share_one_serialized_append_order() {
        let writer = LedgerWriter::new(InMemoryLog::new(), None);
        let mut a = writer.clone();
        let mut b = writer.clone();
        let m0 = a.append(moved("a"));
        let m1 = b.append(moved("b"));
        let m2 = a.append(moved("a"));
        assert_eq!(m0.seq, Seq(0));
        assert_eq!(m1.seq, Seq(1));
        assert_eq!(m2.seq, Seq(2));
        assert_eq!(
            writer.len(),
            3,
            "every clone appends into the one shared log"
        );
        let rows = writer.read_from(Seq(0));
        assert_eq!(rows.len(), 3);
        assert!(
            rows.windows(2).all(|w| w[0].seq.0 + 1 == w[1].seq.0),
            "the single writer keeps a strict append-only seq order (movement is holy)"
        );
    }

    #[test]
    fn sparse_replay_reads_only_the_requested_movement() {
        use std::ops::ControlFlow;
        struct StreamingLog(InMemoryLog, Arc<std::sync::atomic::AtomicUsize>);
        impl MovementLog for StreamingLog {
            fn append(&mut self, draft: MovementDraft) -> Movement {
                self.0.append(draft)
            }
            fn read_from(&self, _: Seq) -> Vec<Movement> {
                panic!("sparse replay must stream")
            }
            fn len(&self) -> usize {
                self.0.len()
            }
            fn scan_from(&self, from: Seq, visit: &mut dyn FnMut(Movement) -> ControlFlow<()>) {
                self.0.scan_from(from, &mut |movement| {
                    self.1.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    visit(movement)
                });
            }
        }
        let visited = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut writer = LedgerWriter::new(StreamingLog(InMemoryLog::new(), visited.clone()), None);
        for _ in 0..1000 {
            writer.append(moved("a"));
        }
        for expected in [Seq(0), Seq(400), Seq(999)] {
            assert_eq!(writer.read_at(expected).unwrap().seq, expected);
        }
        assert_eq!(visited.load(std::sync::atomic::Ordering::Relaxed), 3);
    }
}
