const path = require("node:path");
const { randomUUID } = require("node:crypto");
const { commitClientAuthoringTransaction } = require("./workspaceSetup.cjs");

function commitProjectionClientAuthoring(octopusHome, registryRoot, request) {
  const writes = Array.isArray(request?.writes) ? request.writes : [];
  const removals = Array.isArray(request?.removals) ? request.removals : [];
  const publishBarrierPath = path.resolve(request?.publishBarrierPath ?? "");
  if (!writes.some((write) => path.resolve(write?.file ?? "") === publishBarrierPath)) {
    throw new Error("Client authoring publication barrier is absent from its write set");
  }
  return commitClientAuthoringTransaction({
    octopusHomeRoot: octopusHome,
    registryRoot,
    plan: {
      operationId: randomUUID(),
      label: String(request?.label ?? ""),
      writes: writes.map((write) => ({
        destination: path.resolve(write?.file ?? ""),
        expectedBeforeDigest: write?.beforeDigest ?? null,
        content: Object.hasOwn(write, "content")
          ? write.content
          : write?.next === null ? null : `${JSON.stringify(write?.next, null, 2)}\n`,
        tag: String(write?.label ?? write?.file ?? "client authoring write"),
        publishBarrier: path.resolve(write?.file ?? "") === publishBarrierPath,
      })),
      removals: removals.map((removal) => ({
        destination: path.resolve(removal?.path ?? ""),
        type: removal?.type,
        expectedDev: removal?.dev,
        expectedIno: removal?.ino,
        tag: String(removal?.label ?? removal?.path ?? "client authoring removal"),
      })),
    },
  });
}

module.exports = { commitProjectionClientAuthoring };
