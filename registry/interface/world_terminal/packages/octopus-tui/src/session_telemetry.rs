use std::fmt;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RuntimeMetricTotals {
    pub count: u64,
    pub duration_ms: u64,
}

impl RuntimeMetricTotals {
    pub fn is_empty(self) -> bool {
        self.count == 0 && self.duration_ms == 0
    }

    pub fn merge(&mut self, other: Self) {
        self.count = self.count.saturating_add(other.count);
        self.duration_ms = self.duration_ms.saturating_add(other.duration_ms);
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RuntimeMetricsSummary {
    pub tool_calls: RuntimeMetricTotals,
    pub api_calls: RuntimeMetricTotals,
    pub streaming_events: RuntimeMetricTotals,
    pub websocket_calls: RuntimeMetricTotals,
    pub websocket_events: RuntimeMetricTotals,
    pub responses_api_overhead_ms: u64,
    pub responses_api_inference_time_ms: u64,
    pub responses_api_engine_iapi_ttft_ms: u64,
    pub responses_api_engine_service_ttft_ms: u64,
    pub responses_api_engine_iapi_tbt_ms: u64,
    pub responses_api_engine_service_tbt_ms: u64,
    pub turn_ttft_ms: u64,
    pub turn_ttfm_ms: u64,
}

impl RuntimeMetricsSummary {
    pub fn is_empty(self) -> bool {
        self.tool_calls.is_empty()
            && self.api_calls.is_empty()
            && self.streaming_events.is_empty()
            && self.websocket_calls.is_empty()
            && self.websocket_events.is_empty()
            && self.responses_api_overhead_ms == 0
            && self.responses_api_inference_time_ms == 0
            && self.responses_api_engine_iapi_ttft_ms == 0
            && self.responses_api_engine_service_ttft_ms == 0
            && self.responses_api_engine_iapi_tbt_ms == 0
            && self.responses_api_engine_service_tbt_ms == 0
            && self.turn_ttft_ms == 0
            && self.turn_ttfm_ms == 0
    }

    pub fn merge(&mut self, other: Self) {
        self.tool_calls.merge(other.tool_calls);
        self.api_calls.merge(other.api_calls);
        self.streaming_events.merge(other.streaming_events);
        self.websocket_calls.merge(other.websocket_calls);
        self.websocket_events.merge(other.websocket_events);
        if other.responses_api_overhead_ms > 0 {
            self.responses_api_overhead_ms = other.responses_api_overhead_ms;
        }
        if other.responses_api_inference_time_ms > 0 {
            self.responses_api_inference_time_ms = other.responses_api_inference_time_ms;
        }
        if other.responses_api_engine_iapi_ttft_ms > 0 {
            self.responses_api_engine_iapi_ttft_ms = other.responses_api_engine_iapi_ttft_ms;
        }
        if other.responses_api_engine_service_ttft_ms > 0 {
            self.responses_api_engine_service_ttft_ms = other.responses_api_engine_service_ttft_ms;
        }
        if other.responses_api_engine_iapi_tbt_ms > 0 {
            self.responses_api_engine_iapi_tbt_ms = other.responses_api_engine_iapi_tbt_ms;
        }
        if other.responses_api_engine_service_tbt_ms > 0 {
            self.responses_api_engine_service_tbt_ms = other.responses_api_engine_service_tbt_ms;
        }
        if other.turn_ttft_ms > 0 {
            self.turn_ttft_ms = other.turn_ttft_ms;
        }
        if other.turn_ttfm_ms > 0 {
            self.turn_ttfm_ms = other.turn_ttfm_ms;
        }
    }

    pub fn responses_api_summary(&self) -> RuntimeMetricsSummary {
        Self {
            responses_api_overhead_ms: self.responses_api_overhead_ms,
            responses_api_inference_time_ms: self.responses_api_inference_time_ms,
            responses_api_engine_iapi_ttft_ms: self.responses_api_engine_iapi_ttft_ms,
            responses_api_engine_service_ttft_ms: self.responses_api_engine_service_ttft_ms,
            responses_api_engine_iapi_tbt_ms: self.responses_api_engine_iapi_tbt_ms,
            responses_api_engine_service_tbt_ms: self.responses_api_engine_service_tbt_ms,
            ..RuntimeMetricsSummary::default()
        }
    }
}

pub trait SessionMetricsSink: Send + Sync {
    fn counter(&self, name: &str, inc: i64, tags: &[(&str, &str)]);
    fn record_duration(&self, name: &str, duration: Duration, tags: &[(&str, &str)]);
    fn runtime_metrics_summary(&self) -> Option<RuntimeMetricsSummary>;
    fn reset_runtime_metrics(&self);
}

#[derive(Clone, Default)]
pub struct SessionTelemetry {
    sink: Option<Arc<dyn SessionMetricsSink>>,
}

impl fmt::Debug for SessionTelemetry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionTelemetry")
            .field("enabled", &self.sink.is_some())
            .finish()
    }
}

impl SessionTelemetry {
    pub fn disabled() -> Self {
        Self { sink: None }
    }

    pub fn from_sink(sink: Arc<dyn SessionMetricsSink>) -> Self {
        Self { sink: Some(sink) }
    }

    pub fn counter(&self, name: &str, inc: i64, tags: &[(&str, &str)]) {
        if let Some(sink) = &self.sink {
            sink.counter(name, inc, tags);
        }
    }

    pub fn record_duration(&self, name: &str, duration: Duration, tags: &[(&str, &str)]) {
        if let Some(sink) = &self.sink {
            sink.record_duration(name, duration, tags);
        }
    }

    pub fn runtime_metrics_summary(&self) -> Option<RuntimeMetricsSummary> {
        self.sink.as_ref()?.runtime_metrics_summary()
    }

    pub fn reset_runtime_metrics(&self) {
        if let Some(sink) = &self.sink {
            sink.reset_runtime_metrics();
        }
    }
}

pub struct TelemetrySessionInit {
    pub run_id: String,
    pub model: String,
    pub account_email: Option<String>,
    pub originator: String,
    pub terminal_user_agent: String,
}

pub type TelemetrySessionFactory =
    Arc<dyn Fn(TelemetrySessionInit) -> SessionTelemetry + Send + Sync>;

#[derive(Clone)]
pub struct TelemetryLauncher {
    pub originator: String,
    pub session_factory: Option<TelemetrySessionFactory>,
}

impl TelemetryLauncher {
    pub fn new_session(&self, init: TelemetrySessionInit) -> SessionTelemetry {
        match &self.session_factory {
            Some(factory) => factory(init),
            None => SessionTelemetry::disabled(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_port_is_a_no_op_and_reads_none() {
        let telemetry = SessionTelemetry::disabled();
        telemetry.counter("codex.status_line", 1, &[]);
        telemetry.record_duration("codex.turn", Duration::from_millis(5), &[]);
        telemetry.reset_runtime_metrics();
        assert_eq!(telemetry.runtime_metrics_summary(), None);
    }

    #[test]
    fn summary_merge_adds_counters_and_adopts_latest_timings() {
        let mut acc = RuntimeMetricsSummary {
            tool_calls: RuntimeMetricTotals {
                count: 2,
                duration_ms: 100,
            },
            turn_ttft_ms: 40,
            ..RuntimeMetricsSummary::default()
        };
        acc.merge(RuntimeMetricsSummary {
            tool_calls: RuntimeMetricTotals {
                count: 3,
                duration_ms: 50,
            },
            turn_ttft_ms: 70,
            responses_api_overhead_ms: 9,
            ..RuntimeMetricsSummary::default()
        });
        assert_eq!(
            acc.tool_calls,
            RuntimeMetricTotals {
                count: 5,
                duration_ms: 150
            }
        );
        assert_eq!(acc.turn_ttft_ms, 70);
        assert_eq!(acc.responses_api_overhead_ms, 9);
        assert!(!acc.is_empty());
        assert!(acc.responses_api_summary().tool_calls.is_empty());
    }
}
