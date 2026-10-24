const { readOperatorChatArtifacts } = require("./client/chatArtifacts.cjs");
const { loadOperatorTaskLifecycle, submitOperatorTurn } = require("./client/operatorTurns.cjs");
const { updateOperatorChatPins } = require("./client/pinnedContext.cjs");
const { loginProviderAccount, refreshProviderModels, validateProviderAccountObservation, verifyProviderAccount } = require("./client/providerAccounts.cjs");
const { loadStoragePublisher, publishOperatorFiles, waitForInterfaceSocket, withOperatorRunAuthoringLock, withOperatorWorkspaceTopologyLock } = require("./client/publication.cjs");
const { assertGenericOperatorChatArtifactAllowed, planOperatorChatSurface, planOperatorChatTaskAmendment, submitOperatorTurnControl } = require("./client/surfacePlanning.cjs");
const { releaseOperatorTaskWorkerEnvironments } = require("./client/taskEnvironments.cjs");
const { interfaceEndpoint } = require("./client/topologyPlan.cjs");

module.exports = {
  loadStoragePublisher,
  publishOperatorFiles,
  loadOperatorTaskLifecycle,
  assertGenericOperatorChatArtifactAllowed,
  loginProviderAccount,
  planOperatorChatSurface,
  planOperatorChatTaskAmendment,
  readOperatorChatArtifacts,
  refreshProviderModels,
  releaseOperatorTaskWorkerEnvironments,
  submitOperatorTurn,
  submitOperatorTurnControl,
  requestComposerPrediction: (options, request) => require("./composer-predictions.cjs").requestComposerPrediction({
    ...options, withRunLock: withOperatorRunAuthoringLock, withTopologyLock: withOperatorWorkspaceTopologyLock,
    waitForInterface: waitForInterfaceSocket,
    handIn: (input) => options.handInArtifact({ ...input, endpoint: interfaceEndpoint(options.octopusHome(), input.interfaceId) }),
  }, request),
  requestTaskTitle: (options, request) => require("./task-titles.cjs").requestTaskTitle({
    ...options, withRunLock: withOperatorRunAuthoringLock, withTopologyLock: withOperatorWorkspaceTopologyLock,
    waitForInterface: waitForInterfaceSocket,
    handIn: (input) => options.handInArtifact({ ...input, endpoint: interfaceEndpoint(options.octopusHome(), input.interfaceId) }),
  }, request),
  updateOperatorChatPins,
  validateProviderAccountObservation,
  verifyProviderAccount,
  withOperatorRunAuthoringLock,
  withOperatorWorkspaceTopologyLock,
};
