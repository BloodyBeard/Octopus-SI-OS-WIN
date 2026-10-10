const MAX_DATA_BYTES = 256 * 1024;
const MAX_DOCUMENT_BYTES = 640 * 1024;
const MAX_ACTION_PAYLOAD_BYTES = 16 * 1024;
const MAX_NODES = 512;
const MAX_DEPTH = 16;
const DEFAULT_COLLECTION_LIMIT = 50;
const MAX_COLLECTION_LIMIT = 100;
const EMBEDDED_IMAGE_PATTERN = /^data:image\/(?:avif|gif|jpeg|png|webp);base64,[a-z0-9+/]+={0,2}$/i;

export function clientSurfaceDocumentSemanticInspection(document, options = {}) {
  const issues = [];
  const readMethods = new Set(options.readMethods ?? []);
  const sourceBytes = options.sourceBytes ?? byteLength(document);
  if (sourceBytes > MAX_DOCUMENT_BYTES) {
    issues.push({
      code: "document.size.exceeded",
      path: "",
      message: `document is ${sourceBytes} bytes; maximum is ${MAX_DOCUMENT_BYTES}`,
    });
  }
  if (options.compiled !== true && document.id.startsWith("builtin.")) {
    issues.push({
      code: "document.id.reserved",
      path: "/id",
      message: "client surface ids must not use the reserved builtin. prefix",
    });
  }
  const sourceIds = new Set();
  for (const [index, source] of (document.sources ?? []).entries()) {
    if (sourceIds.has(source.id)) {
      issues.push({
        code: "source.id.duplicate",
        path: `/sources/${index}/id`,
        message: `projection source id "${source.id}" is duplicated`,
      });
    }
    sourceIds.add(source.id);
    if (!readMethods.has(source.method)) {
      issues.push({
        code: "source.method.unavailable",
        path: `/sources/${index}/method`,
        message: `projection method "${source.method}" is not available to client surfaces`,
      });
    }
    if (source.params !== undefined && !isBoundedJson(source.params, 16 * 1024)) {
      issues.push({
        code: "source.params.invalid",
        path: `/sources/${index}/params`,
        message: "projection parameters must be a bounded JSON value",
      });
    }
  }

  const dataBytes = byteLength(document.data ?? {});
  if (dataBytes > MAX_DATA_BYTES) {
    issues.push({
      code: "document.data.size_exceeded",
      path: "/data",
      message: `document data is ${dataBytes} bytes; maximum is ${MAX_DATA_BYTES}`,
    });
  }

  if (document.target.kind === "voice.widget") {
    const layout = document.target.layout;
    if (layout.min_width > layout.preferred_width || layout.preferred_width > layout.max_width) {
      issues.push({
        code: "target.layout.width_order.invalid",
        path: "/target/layout",
        message: "widget width must satisfy min_width <= preferred_width <= max_width",
      });
    }
    if (layout.min_height > layout.preferred_height || layout.preferred_height > layout.max_height) {
      issues.push({
        code: "target.layout.height_order.invalid",
        path: "/target/layout",
        message: "widget height must satisfy min_height <= preferred_height <= max_height",
      });
    }
  }

  let nodeCount = 0;
  let maxDepth = 0;
  if (document.render.kind === "native") {
    const stats = inspectNode(document.render.root, 1, "/render/root", issues);
    nodeCount = stats.nodeCount;
    maxDepth = stats.maxDepth;
    if (nodeCount > MAX_NODES) {
      issues.push({
        code: "render.nodes.exceeded",
        path: "/render/root",
        message: `native tree has ${nodeCount} nodes; maximum is ${MAX_NODES}`,
      });
    }
    if (maxDepth > MAX_DEPTH) {
      issues.push({
        code: "render.depth.exceeded",
        path: "/render/root",
        message: `native tree depth is ${maxDepth}; maximum is ${MAX_DEPTH}`,
      });
    }
  } else {
    for (const [actionId, action] of Object.entries(document.render.actions ?? {})) {
      inspectAction(action, `/render/actions/${actionId}`, issues);
    }
  }
  return { issues, nodeCount, maxDepth };
}

function inspectNode(node, depth, nodePath, issues) {
  let nodeCount = 1;
  let maxDepth = depth;
  if (node.type === "button") inspectAction(node.action, `${nodePath}/action`, issues);
  if (node.type === "input" && node.action) inspectAction(node.action, `${nodePath}/action`, issues);
  if (node.type === "image" && typeof node.source === "string" && !isSafeImageSource(node.source)) {
    issues.push({
      code: "render.image.source.unavailable",
      path: `${nodePath}/source`,
      message: "native images must be embedded AVIF, GIF, JPEG, PNG, or WebP data URLs",
    });
  }
  if (node.type === "table") {
    const columnKeys = new Set();
    node.columns.forEach((column, index) => {
      if (columnKeys.has(column.key)) {
        issues.push({
          code: "render.table.column.duplicate",
          path: `${nodePath}/columns/${index}/key`,
          message: `table column key "${column.key}" is duplicated`,
        });
      }
      columnKeys.add(column.key);
    });
    nodeCount += collectionLimit(node.rows.limit) * node.columns.length;
  }
  if (node.type === "list") {
    let templateNodeCount = 0;
    node.children.forEach((child, index) => {
      const childStats = inspectNode(child, depth + 1, `${nodePath}/children/${index}`, issues);
      templateNodeCount += childStats.nodeCount;
      maxDepth = Math.max(maxDepth, childStats.maxDepth);
    });
    nodeCount += templateNodeCount * collectionLimit(node.items.limit);
  } else if (node.type === "stack" || node.type === "row" || node.type === "section") {
    node.children.forEach((child, index) => {
      const childStats = inspectNode(child, depth + 1, `${nodePath}/children/${index}`, issues);
      nodeCount += childStats.nodeCount;
      maxDepth = Math.max(maxDepth, childStats.maxDepth);
    });
  }
  return { nodeCount, maxDepth };
}

function collectionLimit(value) {
  return Math.min(MAX_COLLECTION_LIMIT, Math.max(1, Math.floor(value ?? DEFAULT_COLLECTION_LIMIT)));
}

function inspectAction(action, actionPath, issues) {
  const payloadBytes = byteLength(action.payload ?? {});
  if (payloadBytes <= MAX_ACTION_PAYLOAD_BYTES) return;
  issues.push({
    code: "action.payload.size_exceeded",
    path: `${actionPath}/payload`,
    message: `action payload is ${payloadBytes} bytes; maximum is ${MAX_ACTION_PAYLOAD_BYTES}`,
  });
}

function isSafeImageSource(value) {
  return value.length <= 512 * 1024 && EMBEDDED_IMAGE_PATTERN.test(value);
}

function byteLength(value) {
  try {
    return new TextEncoder().encode(typeof value === "string" ? value : JSON.stringify(value)).byteLength;
  } catch {
    return Number.MAX_SAFE_INTEGER;
  }
}

function isBoundedJson(value, maximumBytes) {
  try {
    return byteLength(value) <= maximumBytes && JSON.stringify(value) !== undefined;
  } catch {
    return false;
  }
}
