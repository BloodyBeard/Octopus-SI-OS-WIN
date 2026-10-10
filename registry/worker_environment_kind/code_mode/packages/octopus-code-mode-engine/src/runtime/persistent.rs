use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;
use std::sync::{Arc, Mutex, mpsc as std_mpsc};
use std::thread;

use serde_json::{Value, json};
use tokio::sync::mpsc;
use v8::inspector::{
    Channel, ChannelImpl, StringBuffer, StringView, V8Inspector, V8InspectorClient,
    V8InspectorClientImpl, V8InspectorClientTrustLevel, V8InspectorSession,
};

use super::{
    PendingRuntimeMode, RuntimeCommand, RuntimeControlCommand, RuntimeEvent, RuntimeState, globals,
    initialize_v8, module_loader, next_runtime_command, send_result, timers,
};
use crate::{ExecuteRequest, enabled_tool_metadata};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersistentContextConfig {
    pub module_source: String,
    pub exports: Vec<String>,
    pub max_source_bytes: usize,
    pub max_output_bytes: usize,
}

impl PersistentContextConfig {
    fn validate(&self) -> Result<(), String> {
        if !(1..=16 * 1024 * 1024).contains(&self.max_source_bytes)
            || !(1024..=64 * 1024 * 1024).contains(&self.max_output_bytes)
        {
            return Err("invalid persistent code mode byte limits".to_string());
        }
        if self.module_source.len() > self.max_source_bytes || self.module_source.trim().is_empty()
        {
            return Err(
                "persistent code mode prelude must be non-empty and fit the source limit"
                    .to_string(),
            );
        }
        let mut names = BTreeSet::new();
        for name in &self.exports {
            if name.is_empty() || name.trim() != name || name.contains('\0') || !names.insert(name)
            {
                return Err(
                    "persistent code mode prelude exports must be unique non-empty names"
                        .to_string(),
                );
            }
        }
        Ok(())
    }
}

pub(crate) struct ContextLifetime {
    active_cell: Option<u64>,
    active_control: Option<std_mpsc::Sender<RuntimeControlCommand>>,
    active_commands: Option<std_mpsc::Sender<RuntimeCommand>>,
    next_cell: u64,
    retired: bool,
}

pub(crate) enum RuntimeTerminationHandle {
    Isolated(v8::IsolateHandle),
    Persistent {
        isolate: v8::IsolateHandle,
        lifetime: Arc<Mutex<ContextLifetime>>,
        cell: u64,
    },
}

impl RuntimeTerminationHandle {
    pub(crate) fn terminate_execution(&self) -> bool {
        match self {
            Self::Isolated(isolate) => isolate.terminate_execution(),
            Self::Persistent {
                isolate,
                lifetime,
                cell,
            } => match lifetime.lock() {
                Ok(mut state) if state.active_cell == Some(*cell) => {
                    state.retired = true;
                    isolate.terminate_execution()
                }
                Ok(_) => false,
                Err(_) => isolate.terminate_execution(),
            },
        }
    }
}

struct ContextCell {
    id: u64,
    stored_values: HashMap<String, Value>,
    request: ExecuteRequest,
    events: mpsc::UnboundedSender<RuntimeEvent>,
    command_tx: std_mpsc::Sender<RuntimeCommand>,
    commands: std_mpsc::Receiver<RuntimeCommand>,
    control: std_mpsc::Receiver<RuntimeControlCommand>,
}

enum ContextCommand {
    Execute(ContextCell),
    Shutdown,
}

pub(crate) struct PersistentRuntime {
    config: PersistentContextConfig,
    commands: std_mpsc::Sender<ContextCommand>,
    lifetime: Arc<Mutex<ContextLifetime>>,
    isolate: v8::IsolateHandle,
}

impl PersistentRuntime {
    pub(crate) fn new(config: PersistentContextConfig) -> Result<Self, String> {
        config.validate()?;
        initialize_v8()?;
        let (commands, receiver) = std_mpsc::channel();
        let (ready_tx, ready_rx) = std_mpsc::sync_channel(1);
        let lifetime = Arc::new(Mutex::new(ContextLifetime {
            active_cell: None,
            active_control: None,
            active_commands: None,
            next_cell: 1,
            retired: false,
        }));
        let thread_lifetime = Arc::clone(&lifetime);
        let thread_config = config.clone();
        thread::Builder::new()
            .name("code-mode-context".to_string())
            .spawn(move || {
                let _retirement = RetireOnExit(Arc::clone(&thread_lifetime));
                run_context(thread_config, receiver, ready_tx, &thread_lifetime);
            })
            .map_err(|error| format!("failed to create persistent code mode context: {error}"))?;
        let isolate = ready_rx
            .recv()
            .map_err(|_| "failed to initialize persistent code mode context".to_string())?;
        Ok(Self {
            config,
            commands,
            lifetime,
            isolate,
        })
    }

    pub(crate) fn validate_source(&self, source: &str) -> Result<(), String> {
        if !self.is_alive() {
            return Err(
                "code mode persistent context is retired; a new formation is required".to_string(),
            );
        }
        if self
            .config
            .module_source
            .len()
            .checked_add(source.len())
            .is_none_or(|length| length > self.config.max_source_bytes)
        {
            return Err(
                "code mode source and prelude exceed the configured byte limit".to_string(),
            );
        }
        Ok(())
    }

    pub(crate) fn is_alive(&self) -> bool {
        self.lifetime.lock().is_ok_and(|state| !state.retired)
    }

    pub(crate) fn start(
        &self,
        stored_values: HashMap<String, Value>,
        request: ExecuteRequest,
        events: mpsc::UnboundedSender<RuntimeEvent>,
    ) -> Result<
        (
            std_mpsc::Sender<RuntimeCommand>,
            std_mpsc::Sender<RuntimeControlCommand>,
            RuntimeTerminationHandle,
        ),
        String,
    > {
        self.validate_source(&request.source)?;
        let mut lifetime = self
            .lifetime
            .lock()
            .map_err(|_| "persistent code mode lifetime is corrupt".to_string())?;
        if lifetime.retired || lifetime.active_cell.is_some() {
            return Err("persistent code mode context is unavailable".to_string());
        }
        let id = lifetime.next_cell;
        lifetime.next_cell = id
            .checked_add(1)
            .ok_or_else(|| "persistent code mode cell identity exhausted".to_string())?;
        let (command_tx, command_rx) = std_mpsc::channel();
        let (control_tx, control_rx) = std_mpsc::channel();
        lifetime.active_cell = Some(id);
        lifetime.active_control = Some(control_tx.clone());
        lifetime.active_commands = Some(command_tx.clone());
        if self
            .commands
            .send(ContextCommand::Execute(ContextCell {
                id,
                stored_values,
                request,
                events,
                command_tx: command_tx.clone(),
                commands: command_rx,
                control: control_rx,
            }))
            .is_err()
        {
            lifetime.retired = true;
            return Err("persistent code mode context ended".to_string());
        }
        Ok((
            command_tx,
            control_tx,
            RuntimeTerminationHandle::Persistent {
                isolate: self.isolate.clone(),
                lifetime: Arc::clone(&self.lifetime),
                cell: id,
            },
        ))
    }

    pub(crate) fn shutdown(&self) {
        if let Ok(mut state) = self.lifetime.lock() {
            state.retired = true;
            if let Some(control) = &state.active_control {
                let _ = control.send(RuntimeControlCommand::Terminate);
            }
            if let Some(commands) = &state.active_commands {
                let _ = commands.send(RuntimeCommand::Terminate);
            }
            if state.active_cell.is_some() {
                self.isolate.terminate_execution();
            }
        } else {
            self.isolate.terminate_execution();
        }
        let _ = self.commands.send(ContextCommand::Shutdown);
    }
}

impl Drop for PersistentRuntime {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct RetireOnExit(Arc<Mutex<ContextLifetime>>);

impl Drop for RetireOnExit {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.lock() {
            state.retired = true;
            state.active_cell = None;
            state.active_control = None;
            state.active_commands = None;
        }
    }
}

struct InspectorClient;
impl V8InspectorClientImpl for InspectorClient {}

struct PromiseOwnership {
    key: v8::Global<v8::Private>,
    cell: u64,
    executing: Option<u64>,
    stack: Vec<Option<u64>>,
    corrupt: bool,
}

pub(super) fn callbacks_belong_to_cell(scope: &v8::PinScope<'_, '_>) -> bool {
    scope.get_slot::<PromiseOwnership>().is_none_or(|state| {
        !state.corrupt && state.cell != 0 && state.executing.is_none_or(|owner| owner == state.cell)
    })
}

unsafe extern "C" fn promise_hook(
    kind: v8::PromiseHookType,
    promise: v8::Local<v8::Promise>,
    _parent: v8::Local<v8::Value>,
) {
    v8::callback_scope!(unsafe scope, promise);
    let Some(context) = promise.get_creation_context(scope) else {
        if let Some(state) = scope.get_slot_mut::<PromiseOwnership>() {
            state.corrupt = true;
        }
        return;
    };
    let scope = &mut v8::ContextScope::new(scope, context);
    let Some(state) = scope.get_slot::<PromiseOwnership>() else {
        return;
    };
    let key = state.key.clone();
    let key = v8::Local::new(scope, key);
    match kind {
        v8::PromiseHookType::Init => {
            let owner = scope
                .get_slot::<PromiseOwnership>()
                .map(|state| state.executing.unwrap_or(state.cell))
                .unwrap_or(0);
            let owner = v8::BigInt::new_from_u64(scope, owner);
            if promise.set_private(scope, key, owner.into()) != Some(true) {
                if let Some(state) = scope.get_slot_mut::<PromiseOwnership>() {
                    state.corrupt = true;
                }
            }
        }
        v8::PromiseHookType::Before => {
            let owner = promise
                .get_private(scope, key)
                .and_then(|value| v8::Local::<v8::BigInt>::try_from(value).ok())
                .map(|value| value.u64_value())
                .filter(|(_, lossless)| *lossless)
                .map(|(value, _)| value);
            if let Some(state) = scope.get_slot_mut::<PromiseOwnership>() {
                state.stack.push(state.executing);
                state.executing = owner;
                state.corrupt |= owner.is_none();
            }
        }
        v8::PromiseHookType::After => {
            if let Some(state) = scope.get_slot_mut::<PromiseOwnership>() {
                match state.stack.pop() {
                    Some(previous) => state.executing = previous,
                    None => state.corrupt = true,
                }
            }
        }
        v8::PromiseHookType::Resolve => {}
    }
}

#[derive(Default)]
struct InspectorReplies {
    expected: Option<i32>,
    response: Option<Value>,
    context_id: Option<i32>,
    failure: Option<String>,
}

struct InspectorChannel {
    replies: Rc<RefCell<InspectorReplies>>,
    max_bytes: usize,
}

impl InspectorChannel {
    fn decode(&self, message: v8::UniquePtr<StringBuffer>) -> Result<Value, String> {
        let message = message
            .as_ref()
            .ok_or_else(|| "empty persistent code mode evaluation response".to_string())?;
        let view = message.string();
        if view.len() > self.max_bytes {
            return Err("persistent code mode evaluation response exceeds byte limit".to_string());
        }
        let text = view.to_string();
        if text.len() > self.max_bytes {
            return Err("persistent code mode evaluation response exceeds byte limit".to_string());
        }
        serde_json::from_str(&text)
            .map_err(|error| format!("invalid persistent code mode evaluation response: {error}"))
    }
}

impl ChannelImpl for InspectorChannel {
    fn send_response(&self, call_id: i32, message: v8::UniquePtr<StringBuffer>) {
        let decoded = self.decode(message);
        let mut state = self.replies.borrow_mut();
        if state.expected != Some(call_id) || state.response.is_some() {
            state.failure = Some("unexpected persistent code mode evaluation identity".to_string());
            return;
        }
        match decoded {
            Ok(value) if value.get("id").and_then(Value::as_i64) == Some(i64::from(call_id)) => {
                state.response = Some(value);
            }
            Ok(_) => {
                state.failure =
                    Some("persistent code mode evaluation identity mismatch".to_string())
            }
            Err(error) => state.failure = Some(error),
        }
    }

    fn send_notification(&self, message: v8::UniquePtr<StringBuffer>) {
        match self.decode(message) {
            Ok(value)
                if value.get("method").and_then(Value::as_str)
                    == Some("Runtime.executionContextCreated") =>
            {
                let mut state = self.replies.borrow_mut();
                let id = value
                    .pointer("/params/context/id")
                    .and_then(Value::as_i64)
                    .and_then(|id| i32::try_from(id).ok())
                    .filter(|id| *id > 0);
                if id.is_none()
                    || state
                        .context_id
                        .is_some_and(|previous| Some(previous) != id)
                {
                    state.failure =
                        Some("ambiguous persistent code mode evaluation context".to_string());
                } else {
                    state.context_id = id;
                }
            }
            Ok(_) => {}
            Err(error) => self.replies.borrow_mut().failure = Some(error),
        }
    }

    fn flush_protocol_notifications(&self) {}
}

fn dispatch(session: &V8InspectorSession, replies: &Rc<RefCell<InspectorReplies>>, command: Value) {
    {
        let mut state = replies.borrow_mut();
        state.expected = command
            .get("id")
            .and_then(Value::as_i64)
            .and_then(|id| i32::try_from(id).ok());
        state.response = None;
    }
    let encoded: Vec<u16> = command.to_string().encode_utf16().collect();
    session.dispatch_protocol_message(StringView::from(encoded.as_slice()));
}

fn response(replies: &Rc<RefCell<InspectorReplies>>) -> Result<Option<Value>, String> {
    let mut state = replies.borrow_mut();
    if let Some(error) = &state.failure {
        return Err(error.clone());
    }
    let Some(value) = state.response.take() else {
        return Ok(None);
    };
    state.expected = None;
    if let Some(error) = value.get("error") {
        return Err(format!("persistent code mode evaluation failed: {error}"));
    }
    value
        .get("result")
        .filter(|value| value.is_object())
        .cloned()
        .map(Some)
        .ok_or_else(|| "persistent code mode evaluation result is missing".to_string())
}

fn run_context(
    config: PersistentContextConfig,
    commands: std_mpsc::Receiver<ContextCommand>,
    ready: std_mpsc::SyncSender<v8::IsolateHandle>,
    lifetime: &Arc<Mutex<ContextLifetime>>,
) {
    let isolate = &mut v8::Isolate::new(v8::CreateParams::default());
    isolate.set_microtasks_policy(v8::MicrotasksPolicy::Explicit);
    isolate.set_promise_hook(promise_hook);
    isolate.set_host_import_module_dynamically_callback(module_loader::dynamic_import_callback);
    if ready.send(isolate.thread_safe_handle()).is_err() {
        return;
    }
    let inspector = V8Inspector::create(isolate, V8InspectorClient::new(Box::new(InspectorClient)));
    v8::scope!(let scope, isolate);
    let context = v8::Context::new(scope, Default::default());
    let scope = &mut v8::ContextScope::new(scope, context);
    let key = v8::Private::new(scope, None);
    let key = v8::Global::new(scope, key);
    scope.set_slot(PromiseOwnership {
        key,
        cell: 0,
        executing: None,
        stack: Vec::new(),
        corrupt: false,
    });
    inspector.context_created(
        context,
        1,
        StringView::from(&b"code-mode"[..]),
        StringView::empty(),
    );
    let replies = Rc::new(RefCell::new(InspectorReplies::default()));
    let session = inspector.connect(
        1,
        Channel::new(Box::new(InspectorChannel {
            replies: Rc::clone(&replies),
            max_bytes: config.max_output_bytes,
        })),
        StringView::empty(),
        V8InspectorClientTrustLevel::Untrusted,
    );
    dispatch(
        &session,
        &replies,
        json!({"id":1,"method":"Runtime.enable"}),
    );
    let setup_error = match response(&replies) {
        Ok(Some(_)) if replies.borrow().context_id.is_some() => None,
        Ok(_) => Some("persistent code mode evaluation context was not initialized".to_string()),
        Err(error) => Some(error),
    };
    let mut prelude = None;
    while let Ok(ContextCommand::Execute(cell)) = commands.recv() {
        if !lifetime
            .lock()
            .is_ok_and(|state| !state.retired && state.active_cell == Some(cell.id))
        {
            send_result(
                &cell.events,
                HashMap::new(),
                Some("persistent code mode context is retired".to_string()),
            );
            break;
        }
        if let Some(state) = scope.get_slot_mut::<PromiseOwnership>() {
            state.cell = cell.id;
        }
        scope.set_slot(RuntimeState {
            event_tx: cell.events.clone(),
            pending_tool_calls: HashMap::new(),
            pending_timeouts: HashMap::new(),
            stored_values: cell.stored_values.clone(),
            stored_value_writes: HashMap::new(),
            enabled_tools: cell
                .request
                .enabled_tools
                .iter()
                .map(enabled_tool_metadata)
                .collect(),
            next_tool_call_id: 1,
            next_timeout_id: 1,
            tool_call_id: cell.request.tool_call_id.clone(),
            runtime_command_tx: cell.command_tx.clone(),
            exit_requested: false,
            callbacks_enabled: false,
            output_bytes_remaining: Some(config.max_output_bytes),
        });
        let _ = cell.events.send(RuntimeEvent::Started);
        let outcome = if let Some(error) = &setup_error {
            Err(error.clone())
        } else {
            (|| {
                if prelude.is_none() {
                    globals::install_globals(scope)?;
                    prelude = Some(module_loader::evaluate_prelude(
                        scope,
                        &config.module_source,
                        &config.exports,
                    )?);
                }
                scope
                    .get_slot_mut::<RuntimeState>()
                    .ok_or_else(|| "persistent runtime state unavailable".to_string())?
                    .callbacks_enabled = true;
                evaluate_cell(scope, &session, &replies, &cell)
            })()
        };
        let mut fatal = outcome.is_err()
            || scope.is_execution_terminating()
            || scope
                .get_slot::<PromiseOwnership>()
                .is_none_or(|state| state.corrupt || !state.stack.is_empty());
        let error_text = match outcome {
            Ok(error) => error,
            Err(error) => Some(error),
        };
        if !fatal && cancel_pending(scope).is_err() {
            fatal = true;
        }
        fatal |= scope
            .get_slot::<PromiseOwnership>()
            .is_none_or(|state| state.corrupt || !state.stack.is_empty());
        let writes = scope
            .get_slot::<RuntimeState>()
            .map(|state| state.stored_value_writes.clone())
            .unwrap_or_default();
        match lifetime.lock() {
            Ok(mut state) => {
                state.retired |= fatal;
                state.active_cell = None;
                state.active_control = None;
                state.active_commands = None;
                fatal |= state.retired;
            }
            Err(_) => fatal = true,
        }
        send_result(
            &cell.events,
            writes,
            error_text
                .or_else(|| fatal.then(|| "persistent code mode context retired".to_string())),
        );
        if fatal {
            break;
        }
    }
    drop(prelude);
    scope.remove_slot::<RuntimeState>();
    scope.remove_slot::<PromiseOwnership>();
    drop(session);
    inspector.context_destroyed(context);
}

fn evaluate_cell(
    scope: &mut v8::PinScope<'_, '_>,
    session: &V8InspectorSession,
    replies: &Rc<RefCell<InspectorReplies>>,
    cell: &ContextCell,
) -> Result<Option<String>, String> {
    let context_id = replies
        .borrow()
        .context_id
        .ok_or_else(|| "persistent code mode context identity missing".to_string())?;
    dispatch(
        session,
        replies,
        json!({
            "id":2,"method":"Runtime.evaluate","params":{
                "expression":format!("{}\n;void 0;",cell.request.source),
                "contextId":context_id,"objectGroup":"code-mode-cell",
                "replMode":true,"includeCommandLineAPI":false,
                "allowUnsafeEvalBlockedByCSP":false,"generatePreview":false,"returnByValue":false
            }
        }),
    );
    loop {
        scope.perform_microtask_checkpoint();
        if scope.is_execution_terminating() {
            return Err("persistent code mode execution terminated".to_string());
        }
        if let Some(result) = response(replies)? {
            let error = result.get("exceptionDetails").map(|exception| {
                exception
                    .pointer("/exception/description")
                    .and_then(Value::as_str)
                    .or_else(|| {
                        exception
                            .pointer("/exception/value")
                            .and_then(Value::as_str)
                    })
                    .or_else(|| exception.get("text").and_then(Value::as_str))
                    .unwrap_or("persistent code mode exception")
                    .to_string()
            });
            let exited = scope
                .get_slot::<RuntimeState>()
                .is_some_and(|state| state.exit_requested)
                && result
                    .pointer("/exceptionDetails/exception/value")
                    .and_then(Value::as_str)
                    == Some(super::EXIT_SENTINEL);
            if error.is_none() && !result.get("result").is_some_and(Value::is_object) {
                return Err("persistent code mode evaluation omitted completion".to_string());
            }
            dispatch(
                session,
                replies,
                json!({"id":3,"method":"Runtime.releaseObjectGroup",
                "params":{"objectGroup":"code-mode-cell"}}),
            );
            if response(replies)?.is_none() {
                return Err("persistent code mode object release did not complete".to_string());
            }
            return Ok(if exited { None } else { error });
        }
        match next_runtime_command(
            &cell.events,
            &cell.commands,
            &cell.control,
            PendingRuntimeMode::PauseUntilResumed,
        ) {
            Some(RuntimeCommand::ToolResponse { id, result }) => {
                module_loader::resolve_tool_response(scope, &id, Ok(result))?
            }
            Some(RuntimeCommand::ToolError { id, error_text }) => {
                module_loader::resolve_tool_response(scope, &id, Err(error_text))?
            }
            Some(RuntimeCommand::TimeoutFired { id }) => {
                timers::invoke_timeout_callback(scope, id)?
            }
            Some(RuntimeCommand::ObservePendingFrontier) => {}
            Some(RuntimeCommand::Terminate) | None => {
                return Err("persistent code mode cell terminated".to_string());
            }
        }
    }
}

fn cancel_pending(scope: &mut v8::PinScope<'_, '_>) -> Result<(), String> {
    let pending = {
        let state = scope
            .get_slot_mut::<RuntimeState>()
            .ok_or_else(|| "persistent runtime state unavailable".to_string())?;
        state.callbacks_enabled = false;
        state.pending_timeouts.clear();
        std::mem::take(&mut state.pending_tool_calls)
    };
    if !pending.is_empty() {
        let error = v8::String::new(scope, "originating code mode cell completed")
            .ok_or_else(|| "failed to allocate pending tool cancellation".to_string())?;
        for resolver in pending.into_values() {
            let resolver = v8::Local::new(scope, resolver);
            if resolver.reject(scope, error.into()).is_none() {
                return Err("failed to retire pending code mode promise".to_string());
            }
        }
        scope.perform_microtask_checkpoint();
    }
    if scope.is_execution_terminating() {
        return Err("persistent code mode context terminated during completion".to_string());
    }
    Ok(())
}
