use super::*;
use std::io::{BufRead, Read, Write};

const FRAME_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Default)]
pub struct NativeAdoptionBatch {
    scope: Option<Value>,
    runs: BTreeSet<String>,
    ledger: Option<result_receiving::AdoptionLedgerView>,
    failed: bool,
    metrics: PlanningMetrics,
}

#[derive(Default)]
pub(super) struct PlanningMetrics {
    #[cfg(test)]
    pub phases: BTreeMap<&'static str, std::time::Duration>,
}

impl PlanningMetrics {
    pub(super) fn measure<T>(&mut self, _phase: &'static str, operation: impl FnOnce() -> T) -> T {
        #[cfg(test)]
        let start = std::time::Instant::now();
        let result = operation();
        #[cfg(test)]
        {
            *self.phases.entry(_phase).or_default() += start.elapsed();
        }
        result
    }
}

impl NativeAdoptionBatch {
    pub fn plan(
        &mut self,
        request: &NativeRunAdoptionRequest,
    ) -> Result<NativeRunAdoptionPlan, String> {
        if self.failed {
            return Err("native adoption batch has already failed".into());
        }
        self.failed = true;
        let scope = json!({"roots":request.roots,"ledger_root":request.ledger_root,
            "client_template_interface":request.client_template_interface});
        if self.scope.as_ref().is_some_and(|prior| prior != &scope) {
            return Err("native adoption batch changed its exact roots or client owner".into());
        }
        if !self.runs.insert(request.run_id.clone()) {
            return Err("native adoption batch repeats a run".into());
        }
        self.scope = Some(scope);
        self.metrics.measure("ledger_revalidation", || {
            self.ledger
                .as_ref()
                .map(|ledger| ledger.validate())
                .transpose()
        })?;
        let result =
            plan_existing_adoption_with_ledger(request, &mut self.ledger, &mut self.metrics)?;
        self.failed = false;
        Ok(result)
    }

    fn finish(&self) -> Result<(), String> {
        if self.failed {
            return Err("native adoption batch has already failed".into());
        }
        self.ledger
            .as_ref()
            .map(|ledger| ledger.validate())
            .transpose()?;
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    Plan {
        request_id: u64,
        request: NativeRunAdoptionRequest,
    },
    Finish {
        request_id: u64,
    },
}

fn serve(input: &mut impl BufRead, output: &mut impl Write) -> Result<(), String> {
    let mut batch = NativeAdoptionBatch::default();
    let mut sequence = 1_u64;
    loop {
        let mut line = Vec::new();
        input
            .take(FRAME_BYTES + 1)
            .read_until(b'\n', &mut line)
            .map_err(|e| e.to_string())?;
        if line.len() as u64 > FRAME_BYTES || !line.ends_with(b"\n") {
            return Err("native adoption batch requires a complete bounded request frame".into());
        }
        let command: Command = serde_json::from_slice(&line).map_err(|e| e.to_string())?;
        let request_id = match &command {
            Command::Plan { request_id, .. } | Command::Finish { request_id } => *request_id,
        };
        if request_id != sequence {
            return Err("native adoption batch request sequence differs".into());
        }
        let (response, finished) = match command {
            Command::Plan { request, .. } => (
                json!({"schema_version":1,"request_id":request_id,"plan":batch.plan(&request)?}),
                false,
            ),
            Command::Finish { .. } => {
                batch.finish()?;
                (
                    json!({"schema_version":1,"request_id":request_id,"finished":true}),
                    true,
                )
            }
        };
        let bytes = serde_json::to_vec(&response).map_err(|e| e.to_string())?;
        if bytes.len() as u64 + 1 > FRAME_BYTES {
            return Err("native adoption batch response exceeds its frame bound".into());
        }
        output
            .write_all(&bytes)
            .and_then(|_| output.write_all(b"\n"))
            .and_then(|_| output.flush())
            .map_err(|e| e.to_string())?;
        if finished {
            return Ok(());
        }
        sequence = sequence
            .checked_add(1)
            .ok_or("native adoption batch sequence overflow")?;
    }
}

pub fn serve_native_adoption_batch() -> Result<(), String> {
    serve(&mut std::io::stdin().lock(), &mut std::io::stdout().lock())
}

#[cfg(test)]
#[path = "../../tests/support/adoption_batch.rs"]
mod tests;
