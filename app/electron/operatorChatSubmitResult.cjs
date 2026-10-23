const OPERATOR_CHAT_SUBMIT_RESULT = "octopus.operator-chat.submit.v1";
const OPERATOR_CHAT_SUBMIT_ERROR_MESSAGE = "Couldn't send your message. Try again.";

function operatorChatSubmitSuccess(value) {
  return {
    contract: OPERATOR_CHAT_SUBMIT_RESULT,
    ok: true,
    value,
  };
}

function operatorChatSubmitFailure(error, { submissionStarted = true } = {}) {
  const contractMismatch = error?.code === "operator_chat_client_contract_mismatch";
  return {
    contract: OPERATOR_CHAT_SUBMIT_RESULT,
    ok: false,
    error: {
      code: contractMismatch ? "chat_package_repair_required" : "message_send_failed",
      message: contractMismatch
        ? "Octopus Home chat package needs repair before sending."
        : OPERATOR_CHAT_SUBMIT_ERROR_MESSAGE,
      retryable: !contractMismatch,
      deliveryState: !submissionStarted || error?.deliveryState === "not_sent" ? "not_sent" : "unknown",
    },
  };
}

function unwrapOperatorChatSubmitResult(result) {
  if (result?.contract === OPERATOR_CHAT_SUBMIT_RESULT && result.ok === true) {
    return result.value;
  }
  const message = result?.contract === OPERATOR_CHAT_SUBMIT_RESULT
    && result.ok === false
    && typeof result.error?.message === "string"
    && result.error.message.trim()
    ? result.error.message
    : OPERATOR_CHAT_SUBMIT_ERROR_MESSAGE;
  const error = new Error(message);
  error.name = "OctopusChatSubmitError";
  error.code = result?.contract === OPERATOR_CHAT_SUBMIT_RESULT
    && result.ok === false
    && typeof result.error?.code === "string"
    ? result.error.code
    : "message_send_failed";
  error.retryable = result?.contract === OPERATOR_CHAT_SUBMIT_RESULT
    && result.ok === false
    && result.error?.retryable === true;
  error.deliveryState = result?.contract === OPERATOR_CHAT_SUBMIT_RESULT
    && result.ok === false
    && result.error?.deliveryState === "not_sent" ? "not_sent" : "unknown";
  throw error;
}

function operatorChatSubmitDiagnostic(error) {
  return {
    code: typeof error?.code === "string" ? error.code.slice(0, 160) : "",
    message: String(error?.message || error || "Unknown operator chat submit failure").slice(0, 8_000),
    name: typeof error?.name === "string" ? error.name.slice(0, 160) : "Error",
    stack: typeof error?.stack === "string" ? error.stack.slice(0, 8_000) : "",
  };
}

module.exports = {
  OPERATOR_CHAT_SUBMIT_ERROR_MESSAGE,
  operatorChatSubmitDiagnostic,
  operatorChatSubmitFailure,
  operatorChatSubmitSuccess,
  unwrapOperatorChatSubmitResult,
};
