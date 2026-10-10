#![deny(clippy::print_stdout, clippy::print_stderr)]

pub mod application_consent_contract;
pub mod control_socket;
pub use control_socket::interface_control_socket_path_from_root;

pub mod input_artifact;
pub use input_artifact::{
    INTERFACE_INPUT_SOURCE_ARTIFACT_FIELD, InterfaceInputArtifactBody,
    store_interface_input_artifact, store_interface_input_artifact_body, store_json_artifact,
    write_interface_input_artifact_reference,
};

pub mod projection;
pub use projection::{
    ACCOUNT_RATE_LIMITS_LABEL, ACCOUNT_TOKEN_USAGE_LABEL, AGENT_MESSAGE_DELTA_LABEL,
    APP_SERVER_INTERFACE_DRIVER_REF, AppServerInterfaceDriver, COMMAND_EXECUTION_COMPLETED_LABEL,
    COMMAND_EXECUTION_STARTED_LABEL, GOAL_STATUS_LABEL, HOOK_CATALOG_LABEL, HOOK_RESULT_LABEL,
    INTERFACE_ARTIFACT_OUTPUT_LABEL, InterfaceArtifactEnvelope, InterfaceArtifactEnvelopeError,
    OPERATOR_APPROVAL_REQUEST_LABEL, OPERATOR_APPROVAL_RESPONSE_LABEL,
    OPERATOR_ELICITATION_REQUEST_LABEL, OPERATOR_ELICITATION_RESPONSE_LABEL,
    OPERATOR_USER_INPUT_REQUEST_LABEL, OPERATOR_USER_INPUT_RESPONSE_LABEL, TOOL_CALL_LABEL,
    OperatorPermissionChoice, OperatorPermissionDecision, OperatorPermissionOffer,
    OperatorPermissionPersistence, OperatorPermissionResponse,
    TURN_ERROR_LABEL, TURN_EXEC_COMMAND_LABEL, TURN_PLAN_UPDATE_LABEL, TURN_REASONING_LABEL,
    TURN_TOKEN_USAGE_LABEL, TURN_USER_MESSAGE_LABEL, TURN_WEB_SEARCH_LABEL,
    method_is_command_execution,
};

pub mod reply_stream;
pub use reply_stream::{
    ACT_OBSERVATION_REPLY_LABEL, AGENT_MESSAGE_DELTA_METHOD, CommandExecutionPhase, LiveLogReader,
    ReplyNotification, ReplyStep, ReplyStream, ReplyStreamError, classify_movement,
    live_reply_notifications, project_movements_as_notifications,
};
