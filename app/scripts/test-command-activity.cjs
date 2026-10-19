const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const ts = require("typescript");
const React = require("react");
const { renderToStaticMarkup } = require("react-dom/server");
const root = path.resolve(__dirname, "..");
const read = (file) => fs.readFileSync(path.join(root, file), "utf8");
const record = (value) => value && typeof value === "object" && !Array.isArray(value) ? value : {};
const text = (value) => typeof value === "string" ? value : "";

function evaluate(source, dependencies = {}, filename = "probe.tsx") {
  const compiled = ts.transpileModule(source, { fileName: filename, reportDiagnostics: true,
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX } });
  assert.deepEqual(compiled.diagnostics, []);
  const module = { exports: {} };
  Function("require", "module", "exports", ...Object.keys(dependencies), compiled.outputText)(
    require, module, module.exports, ...Object.values(dependencies));
  return module.exports;
}

function functions(source, names, dependencies) {
  const ast = ts.createSourceFile("source.tsx", source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const found = new Map();
  function visit(node) {
    if (ts.isFunctionDeclaration(node) && names.includes(node.name?.text)) found.set(node.name.text, node.getText(ast));
    ts.forEachChild(node, visit);
  }
  visit(ast);
  assert.equal(found.size, names.length, names.join(", "));
  return evaluate(`${[...found.values()].join("\n")}\nmodule.exports = {${names.join(",")}};`, dependencies);
}

function constants(source, prefix) {
  const rows = [...source.matchAll(new RegExp(`^const (${prefix}[A-Z_]+) = ([^;]+);`, "gm"))];
  return evaluate(`${rows.map((row) => row[0]).join("\n")}\nmodule.exports = {${rows.map((row) => row[1]).join(",")}};`);
}

const model = evaluate(read("src/components/chat/commandActivityModel.ts"));
const observationSource = read("src/components/chat/chatTurnObservation.ts");
const { isChatReplyTerminal } = evaluate(read("src/components/chat/chatTurnControl.ts"));
const observations = evaluate(observationSource.replace(/^import .* from "[^"\n]+";$/gm, ""), {
  readOperatorChatArtifacts: async () => { throw new Error("connection closed before the next result"); },
});
const projectionSource = read("src/components/chat/chatActivityProjection.ts");
const projections = functions(projectionSource, ["mergeRenderedMessages", "projectedMessageIdentity", "messageTextIdentity", "projectedMessageIncludes", "coalesceRenderedStreams", "assistantItemCompleted", "streamFamily", "streamIdentity", "streamDiscriminator", "eventString", "eventNumber", "eventRecord"], {});
const activitySource = read("src/components/chat/TurnActivity.tsx");
const activities = functions(activitySource, ["activityIsActive", "activityTone", "entryIsActive"], { ...model, record, text, activityFamily: () => "tool" });
const toolPresentation = evaluate(read("src/components/chat/toolPresentation.ts").replace(/^import .* from "[^"\n]+";$/gm, ""), {
  catalogRows: JSON.parse(read("src/components/chat/toolActivityPresentationCatalog.json")),
});
const toolActivities = functions(activitySource, ["completedGroupSummary", "pushPluralSegment", "activityDetailMessages", "dynamicToolActivityLabel", "activeActivityLabel"], {
  record, text, ...toolPresentation,
});
const progress = functions(activitySource, ["activityProgressSurface"], { record });
const listSource = read("src/components/chat/ChatMessageList.tsx");
const turns = functions(listSource, ["groupIntoTurns"], { eventRecord: record, eventString: text });
{
  const user = { id: "first", role: "user" };
  const queued = { id: "queued", role: "user" };
  const started = { id: "call", role: "assistant", runId: "run", interfaceId: "client",
    eventKind: "dynamic_tool", eventData: { tool: "code_mode.run", call_id: "exact-call", status: "started",
      thread_id: "thread", turn_id: "native-turn",
      _activity_origin: { interface_id: "provider" }, arguments: { script: "broken();" } } };
  const failed = { ...started, id: "result", eventData: { ...started.eventData, status: "failed", ok: false,
    arguments: undefined, error: { message: "recorded native failure" } } };
  assert.deepEqual(turns.groupIntoTurns([user, started, queued, failed]), [[user, started, failed], [queued]]);
  const entry = { family: "dynamic_tool", messages: [started, failed] };
  assert.equal(activities.entryIsActive(entry), false);
  assert.deepEqual(toolActivities.activityDetailMessages(entry)[0].eventData.arguments, started.eventData.arguments);
  assert.deepEqual(turns.groupIntoTurns([user, started, queued, failed], false), [[user, started], [queued, failed]]);
  const closure = { ...failed, id: "closed", eventKind: "interrupted_result", terminal: true,
    eventData: { _activity_origin: started.eventData._activity_origin, correlation_id: "exact-call",
      thread_id: "thread", turn_id: "native-turn", returned_to_model: false, turn_interrupted: true } };
  assert.deepEqual(turns.groupIntoTurns([user, started, queued, failed, closure]), [[user, started, failed, closure], [queued]]);
  const foreignClosure = { ...closure, eventData: { ...closure.eventData, turn_id: "other" } };
  assert.deepEqual(turns.groupIntoTurns([user, started, queued, foreignClosure]), [[user, started], [queued, foreignClosure]]);
  for (const foreign of [{ ...failed, runId: "other" }, { ...failed, interfaceId: "other" },
    { ...failed, eventData: { ...failed.eventData, _activity_origin: { interface_id: "other" } } },
    { ...failed, eventData: { ...failed.eventData, call_id: "other" } },
    { ...failed, eventData: { ...failed.eventData, tool: "other" } }]) {
    assert.deepEqual(turns.groupIntoTurns([user, started, queued, foreign]), [[user, started], [queued, foreign]]);
  }
}
const uiDependencies = {
  ...React, ...model, useShouldReduceMotion: () => true,
  motion: { div: ({ children, className, ...props }) => React.createElement("div", { className, "aria-hidden": props["aria-hidden"] }, children) },
  Glyph: () => null, CadencedShimmerText: ({ children }) => children,
  TerminalAnsiText: ({ text }) => React.createElement("span", null, text),
  ShellCopyButton: ({ label, value }) => React.createElement("button", { "aria-label": label, "data-copy": value }, label),
  eventArray: (value) => Array.isArray(value) ? value : [], eventString: text,
};
const ui = functions(listSource, ["CommandActivity", "normalizeTerminalControl", "formatActivityElapsed"], uiDependencies);
const commandUi = functions(listSource, ["ExecActivity", "DynamicToolExecActivity", "dynamicToolExecMessage", "suppliedShellCommand", "shellCommandText", "shellArgument", "activityPathLabel"], {
  ...uiDependencies, ...toolPresentation, eventRecord: record, CommandActivity: ui.CommandActivity,
  MessageTooltip: ({ children }) => children,
});
const turnTree = ts.createSourceFile("turn.tsx", activitySource, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
const turnUi = evaluate(turnTree.statements.filter((node) => !ts.isImportDeclaration(node)).map((node) => node.getText(turnTree)).join("\n"), {
  ...uiDependencies, ...toolPresentation, ...evaluate(read("src/components/chat/subagentActivity.ts")),
  ...evaluate(read("src/components/chat/reasoningSummary.ts")),
  ...evaluate(read("src/components/chat/conversationTrace.ts").replace(/^import .* from "[^"\n]+";$/gm, ""), toolPresentation),
});
const dynamicUi = functions(listSource, ["AssistantEventContent", "WebSearchActivity", "StructuredActivity", "activityLifecycle", "activityLifecycleFromStatus", "eventJson", "dynamicToolResult"], {
  ...uiDependencies, ...toolPresentation, ...commandUi, eventRecord: record, ActivityDisclosure: turnUi.ActivityDisclosure,
  Glyph: ({ name }) => React.createElement("span", { "data-glyph": name }),
});
const renderTurn = (activities, turnInProgress = true, response) => renderToStaticMarkup(React.createElement(turnUi.TurnActivity, {
  activities, turnInProgress, response, presentation: "conversation", startedAtMs: 1, toolPresentations: {},
  renderActivity: (message) => React.createElement("span", null, message.text),
}));
const native = (output, status = "completed") => ({ id: "command", runId: "run-a", role: "assistant", text: "arbitrary executable", createdAtMs: 1, eventKind: "tool", eventData: { status, output } });
const dynamic = (body, extra = {}) => ({ ...native(undefined), eventKind: "dynamic_tool", eventData: { tool: "unrelated.custom.execute", ok: true, status: "completed", body, ...extra } });
const render = (message) => renderToStaticMarkup(React.createElement(ui.CommandActivity, { command: "arbitrary executable", message }));

(async () => {
  for (const eventData of [
    { command: "printf 'hello world'" },
    { arguments: { cmd: "printf 'hello world'" } },
    { arguments: { argv: ["printf", "hello world"] } },
    { parsed_cmd: { cmd: "printf 'hello world'" } },
  ]) {
    const mapped = commandUi.dynamicToolExecMessage({ ...dynamic({}), text: "Run", eventData });
    assert.equal(mapped.eventData.command, "printf 'hello world'");
    const html = renderToStaticMarkup(React.createElement(commandUi.ExecActivity, { message: mapped }));
    assert.match(html, /hello world/);
    assert.doesNotMatch(html, /\$ Run/);
  }
  const absent = commandUi.dynamicToolExecMessage({ ...dynamic({}), text: "Run" });
  assert.equal(absent.eventData.command, "");
  const absentHtml = renderToStaticMarkup(React.createElement(commandUi.ExecActivity, { message: absent }));
  assert.match(absentHtml, /Command unavailable/);
  assert.doesNotMatch(absentHtml, /Copy command|\$ Run/);
  const script = "const d = await prelude.cua.documentation(); return d;";
  const codeMessage = commandUi.dynamicToolExecMessage({ ...dynamic({}), text: "Run", eventData: {
    tool: "code_mode.run__42450f1bf38856e1fb9743bf91929c06", status: "started", arguments: { script, yield_time_ms: 1000 },
  } });
  assert.equal(codeMessage.eventData.command, script);
  const codeHtml = renderToStaticMarkup(React.createElement(commandUi.ExecActivity, { message: codeMessage }));
  assert.match(codeHtml, /Running code/);
  assert.match(codeHtml, /prelude.cua.documentation/);
  assert.match(codeHtml, /Copy code/);
  assert.doesNotMatch(codeHtml, />Shell<|\$ Run|\$ const/);
  assert.equal(toolPresentation.dynamicToolPresentation(codeMessage, {}).target, script);
  assert.equal(toolActivities.activeActivityLabel({ family: "dynamic_tool", messages: [codeMessage] }, {}), `Running ${script}`);
  for (const errorText of ["SyntaxError: Illegal return statement", "TypeError: prelude.cua.documentation is not a function"]) {
    const result = commandUi.dynamicToolExecMessage({ ...codeMessage, eventData: { ...codeMessage.eventData,
      status: "completed", ok: true, body: { cell_id: "1", state: "completed", output: [], error_text: errorText } } });
    assert.equal(model.commandActivityModel(result).phase, "failed");
    assert.equal(activities.activityTone([result]), "danger");
    assert.equal(activities.activityIsActive(result), false);
    const html = renderToStaticMarkup(React.createElement(commandUi.ExecActivity, { message: result }));
    assert.match(html, /Failed/);
    assert.ok(html.includes(errorText));
    assert.doesNotMatch(html, /Outcome unknown|Running code/);
  }
  const codeResult = dynamic({ cell_id: "2", state: "completed", output: [{ type: "text", text: "NEXT_CODE_OK" }] });
  assert.equal(model.commandActivityModel(codeResult).phase, "exited");
  assert.deepEqual(model.commandActivityModel(codeResult).sections, [{ stream: "", text: "NEXT_CODE_OK" }]);
  const codeYield = dynamic({ cell_id: "3", state: "yielded", output: [] });
  assert.equal(model.commandActivityModel(codeYield).phase, "running");
  assert.equal(model.commandActivityModel(codeYield).yielded, true);
  assert.equal(activities.activityIsActive(codeYield), true);
  const begin = { id: "start", eventKind: "dynamic_tool", eventData: {
    call_id: "command-call", status: "started", tool: "workspace.build__0123456789abcdef0123456789abcdef",
    arguments: { command: ["/bin/sh", "-c", "pwd"] },
  } };
  const complete = { ...begin, id: "end", eventData: {
    tool: begin.eventData.tool, call_id: "command-call", status: "completed", ok: true,
    body: { output: { exit_code: 0, stdout: { text: "/work" } } },
  } };
  const commandEntry = { id: "command-call", family: "dynamic_tool", messages: [begin, complete] };
  assert.equal(toolActivities.activeActivityLabel(commandEntry, {}), "Running /bin/sh -c pwd");
  const detail = toolActivities.activityDetailMessages(commandEntry)[0];
  assert.deepEqual(detail.eventData.arguments, begin.eventData.arguments);
  assert.deepEqual(detail.eventData.body, complete.eventData.body);
  for (const suffix of ["0123456789abcdef01234567", "0123456789abcdef0123456789abcdef"]) {
    for (const [tool, args, label] of [
      ["workspace.read_file", { path: "/work/README.md" }, "Read README.md"],
      ["workspace.list_dir", { path: "/work" }, "Listed files in /work"],
      ["artifact.read", { id: "docs.README.md:db13113ae69d4a945ec05fd595cbf83a41dd9decd3e96ca6899bc2f6e5149a6c" }, "Read artifact docs.README.md"],
      ["artifact.list", {}, "Listed artifacts"],
    ]) {
      const started = { ...begin, eventData: { ...begin.eventData, tool: `${tool}__${suffix}`, arguments: args } };
      const completed = { ...complete, eventData: { ...complete.eventData, tool: started.eventData.tool, body: { result: "read" } } };
      const [message] = toolActivities.activityDetailMessages({ ...commandEntry, messages: [started, completed] });
      const presentation = toolPresentation.dynamicToolPresentation(message, {});
      assert.equal(toolPresentation.dynamicToolReadLabel(presentation, false), label);
      if (tool.startsWith("artifact.")) {
        assert.equal(presentation.path, "");
        if (args.id) assert.equal(presentation.target, args.id);
      }
    }
  }
  assert.equal(toolPresentation.dynamicToolPresentation({ eventData: { tool: "artifact.read", arguments: { id: "opaque:artifact-id" } } }, {}).pathLabel, "opaque:artifact-id");
  const listedRoot = toolPresentation.dynamicToolPresentation({ eventData: { tool: "workspace.list_dir", arguments: { limit: 50 }, body: { path: "." } } }, {});
  assert.equal(toolPresentation.dynamicToolReadLabel(listedRoot, false), "Listed files in .");
  assert.equal(toolPresentation.dynamicToolPresentation({ eventData: { tool: "unknown_tool", body: { path: "/unrelated" } } }, {}).target, "");
  const processList = { eventKind: "dynamic_tool", eventData: { tool: "process.list", arguments: {}, status: "completed", body: { process_id: null, exit_code: null, status: "idle" } } };
  assert.equal(toolPresentation.dynamicToolPresentation(processList, {}).family, "tool");
  assert.doesNotMatch(renderToStaticMarkup(React.createElement(dynamicUi.AssistantEventContent, { message: processList, toolPresentations: {} })), /octopus-exec-shell|data-glyph="terminal"/);
  const continued = { eventData: { namespace: "formed_interfaces", tool: "run_continue", status: "completed",
    arguments: { interface: "peer-interface", text: "Continue the assigned task" },
    body: { admitted: true, recipient_action_confirmed: false, work_completed: false } } };
  const continuation = toolPresentation.dynamicToolPresentation(continued, {});
  assert.equal(continuation.family, "tool");
  assert.equal(continuation.target, "peer-interface");
  assert.equal(toolPresentation.dynamicToolOperationLabel(continuation), "Requested run continuation");
  assert.deepEqual(continuation.operation.details.slice(2, 5), [["Admission", "Accepted"], ["Recipient action", "Not confirmed"], ["Work", "Not completed"]]);
  const refusedMessage = { eventKind: "dynamic_tool", eventData: { ...continued.eventData, status: "failed", error: { message: "Request failed" }, body: { ok: false, error: "Peer did not admit continuation" } } };
  const refused = toolPresentation.nativeRunOperationPresentation(refusedMessage);
  assert.equal(refused.label, "Continuation request failed");
  assert.deepEqual(refused.details.at(-1), ["Error", "Request failed\nPeer did not admit continuation"]);
  assert.equal(toolPresentation.nativeRunOperationPresentation({ eventData: { ...continued.eventData, namespace: "unrelated" } }), null);
  for (const [action, query, expected] of [
    [null, "", ""], [{ kind: "other" }, "", ""],
    [{ kind: "search", queries: ["", "contract site:example.org", "second query"] }, "stale", "contract | example.org ..."],
    [{ kind: "open_page", url: "https://example.org/page" }, "stale", "https://example.org/page"],
    [{ kind: "find_in_page", url: "https://example.org/page", pattern: "term" }, "stale", "'term' in https://example.org/page"],
  ]) assert.equal(toolPresentation.webSearchActivityDetail({ text: "turn.web_search", eventData: { action, query } }), expected);
  for (const tool of ["web.search", "web.search__0123456789abcdef0123456789abcdef"]) {
    const query = "requirements site:example.org";
    const started = { ...begin, eventData: { ...begin.eventData, tool, arguments: { query } } };
    const ended = { ...complete, eventData: { ...complete.eventData, tool, body: { results: [] } } };
    const entry = { ...commandEntry, messages: [started, ended] };
    const [message] = toolActivities.activityDetailMessages(entry);
    const presentation = toolPresentation.dynamicToolPresentation(message, {});
    assert.equal(presentation.family, "web");
    assert.equal(presentation.path, "");
    assert.equal(toolActivities.activeActivityLabel(entry, {}), "Searching the web for requirements | example.org");
    assert.deepEqual(toolActivities.completedGroupSummary([entry], {}), { icon: "search", label: "Searched the web" });
    for (const item of [started, message]) {
      const native = { ...item, eventKind: "web_search", eventData: { status: item.eventData.status, action: { kind: "search", query } } };
      const renderSearch = (message) => renderToStaticMarkup(React.createElement(dynamicUi.AssistantEventContent, { message, toolPresentations: {} }));
      assert.equal(renderSearch(item), renderSearch(native));
      assert.equal(renderSearch(item).split("requirements").length - 1, 1);
    }
  }
  const runEntry = { id: "create-call", family: "dynamic_tool", messages: [{
    id: "create", eventKind: "dynamic_tool", eventData: { tool: "run_create", status: "failed" },
  }] };
  const summary = toolActivities.completedGroupSummary([runEntry, commandEntry], {});
  assert.equal(summary.label, "Ran a command, run create");
  assert.equal(toolActivities.activeActivityLabel(runEntry, {}), "Run create");
  assert.equal(toolPresentation.dynamicToolPresentation({ eventData: { tool: "custom_tool", arguments: { command: "pwd" } } }, {}).family, "tool");
  const runList = { eventData: { namespace: "formed_interfaces", tool: "run_list", arguments: {} } };
  assert.equal(toolPresentation.dynamicToolPresentation(runList, {}).family, "tool");
  assert.equal(toolPresentation.dynamicToolGlyph(toolPresentation.dynamicToolPresentation(runList, {})), "octopus");
  assert.equal(toolActivities.completedGroupSummary([runEntry], {}).icon, "octopus");
  const commentary = { id: "commentary", kind: "standalone", entry: { family: "commentary", active: false, messages: [] } };
  const group = { id: "commands", kind: "group", entries: [{ family: "dynamic_tool", active: false, messages: [] }] };
  for (const units of [[], [commentary], [group, commentary]]) {
    assert.deepEqual(progress.activityProgressSurface(units, true), { activeGroupId: null, showThinkingFallback: true });
    assert.deepEqual(progress.activityProgressSurface(units, false), { activeGroupId: null, showThinkingFallback: false });
  }
  assert.deepEqual(progress.activityProgressSurface([group], true), { activeGroupId: "commands", showThinkingFallback: false });
  assert.equal(progress.activityProgressSurface([{ ...commentary, entry: { ...commentary.entry, family: "reasoning", active: true } }], true).showThinkingFallback, false);
  assert.equal(progress.activityProgressSurface([{ ...commentary, entry: { ...commentary.entry, active: true } }], true).showThinkingFallback, false);
  assert.deepEqual(progress.activityProgressSurface([{ ...group, entries: [{ family: "exec", active: true, messages: [] }] }, commentary], true), { activeGroupId: "commands", showThinkingFallback: false });
  const request = { ...commentary, entry: { ...commentary.entry, family: "operator", messages: [{ eventKind: "approval", eventData: {} }] } };
  assert.equal(progress.activityProgressSurface([request], true).showThinkingFallback, false);
  request.entry.messages[0].eventData.resolved = true;
  assert.equal(progress.activityProgressSurface([request], true).showThinkingFallback, true);
  const operationElement = dynamicUi.AssistantEventContent({
    message: { ...runList, eventKind: "dynamic_tool", eventData: { ...runList.eventData, status: "completed", body: { source_run: { run_id: "run-a" }, declared_peer_interfaces: ["peer-interface"] } } }, toolPresentations: {},
  });
  const operationHtml = renderToStaticMarkup(operationElement);
  assert.match(operationHtml, /Inspected run/);
  assert.match(operationHtml, /aria-expanded="false"/);
  assert.doesNotMatch(operationHtml, /peer-interface/);
  const disclosure = dynamicUi.StructuredActivity(operationElement.props);
  const expandedOperationHtml = renderToStaticMarkup(React.cloneElement(disclosure, { defaultExpanded: true }));
  assert.match(expandedOperationHtml, /aria-expanded="true"/);
  assert.match(expandedOperationHtml, /peer-interface/);
  assert.doesNotMatch(operationHtml, /data-glyph="(?:tool|terminal)"|Ran a command|Success/);
  for (const message of [{ ...continued, eventKind: "dynamic_tool" }, refusedMessage]) {
    const element = dynamicUi.AssistantEventContent({ message, toolPresentations: {} });
    const html = renderToStaticMarkup(React.cloneElement(dynamicUi.StructuredActivity(element.props), { defaultExpanded: true }));
    assert.doesNotMatch(html, /data-glyph="terminal"|octopus-exec-shell|multi-agent|Arguments|Operation|work_completed/);
    assert.match(html, message === refusedMessage ? /Peer did not admit continuation/ : /Admission[\s\S]*Accepted[\s\S]*Recipient action[\s\S]*Not confirmed[\s\S]*Work[\s\S]*Not completed/);
  }
  const completedCommentary = { id: "commentary", role: "assistant", eventKind: "commentary", text: "Progress explained", createdAtMs: 2, eventData: { status: "completed" } };
  assert.match(renderTurn([completedCommentary]), /Progress explained[\s\S]*octopus-thinking-fallback/);
  assert.doesNotMatch(renderTurn([completedCommentary], false), /octopus-thinking-fallback/);
  assert.match(renderTurn([completedCommentary], true, { id: "pending", role: "assistant", eventKind: "message_delta" }), /octopus-thinking-fallback/);
  assert.doesNotMatch(renderTurn([completedCommentary], true, { id: "final", role: "assistant", eventKind: "message_delta", eventData: { phase: "final_answer" } }), /octopus-thinking-fallback/);
  const completedReasoning = { ...completedCommentary, eventKind: "reasoning", eventData: { status: "completed", summary: ["**Next step**"] } };
  assert.match(renderTurn([completedCommentary, completedReasoning]), /octopus-thinking-fallback[\s\S]*Next step/);

  const success = native({ exit_code: 0, aggregated_output: "exact output\n", duration_ms: 1200 });
  assert.equal(model.commandActivityModel(success).phase, "exited");
  const successfulHtml = render(success);
  assert.match(successfulHtml, /Ran arbitrary executable/);
  assert.match(successfulHtml, />Success</);
  assert.match(successfulHtml, /data-copy="exact output\n"/);
  for (const message of [success, detail]) {
    const html = renderToStaticMarkup(React.createElement(dynamicUi.AssistantEventContent, { message, toolPresentations: {} }));
    assert.match(html, /data-command-state="exited"/);
    assert.match(html, />Shell</);
    assert.match(html, />Success</);
    assert.doesNotMatch(html, /structured-activity|multi-agent/);
  }

  const nonzero = dynamic({ completed: true, output: { exit_code: 7, stdout: { text: "prefix\n" } } });
  assert.match(render(nonzero), />Exit code 7</);
  assert.match(render(nonzero), /Ran arbitrary executable/);
  assert.equal(activities.activityTone([nonzero]), "danger");
  assert.equal(activities.entryIsActive({ family: "exec", messages: [success, { ...success, eventKind: "tool_output_delta", eventData: {} }] }), false);

  const yielded = dynamic({ process_id: "p1", completed: false, continuation_available: true,
    output: { running: true, exited: false, closed: false, exit_code: null, chunks: [{ stream: "stdout", content: { text: "  prefix\n" } }] } });
  const running = model.commandActivityModel(yielded);
  assert.equal(running.invocationStatus, "completed");
  assert.equal(running.phase, "running");
  assert.equal(running.yielded, true);
  assert.equal(activities.activityIsActive(yielded), true);
  assert.match(render(yielded), /Command still running/);
  assert.doesNotMatch(render(yielded), /Exit code|>Success</);
  const controlled = dynamic({ ...yielded.eventData.body, control: { action: "interrupt", delivered: true } });
  assert.equal(model.commandActivityModel(controlled).phase, "running");
  assert.match(render(controlled), /Process control delivered: interrupt/);

  const timedOut = dynamic({ completed: false, output: { timed_out: true, exit_code: -1, stdout: { text: "retained" }, stdout_truncated: true, stdout_omitted_bytes: 17, stdout_retention: "head_tail" } });
  assert.equal(model.commandActivityModel(timedOut).exitCode, null);
  assert.match(render(timedOut), /Timed out/);
  assert.match(render(timedOut), /17 bytes omitted/);
  assert.doesNotMatch(render(timedOut), /Exit code -1|>Success</);
  const exitedWithOutput = dynamic({ completed: false, continuation_available: true, output: { exited: true, closed: false, running: false, exit_code: 0 } });
  assert.equal(model.commandActivityModel(exitedWithOutput).phase, "exited");
  assert.match(render(exitedWithOutput), /More output is available/);
  assert.equal(model.commandActivityModel(native({ aggregated_output: "partial" }, "interrupted")).phase, "interrupted");
  assert.equal(model.commandActivityModel(dynamic({ completed: false, output: { closed: true, running: false } })).phase, "unknown");
  assert.match(render(native({ exit_code: 7 }, "failed")), /Failed · exit code 7/);
  assert.match(render(dynamic({})), /Outcome unknown/);

  const failed = dynamic(undefined, { ok: false, status: "failed", error: { code: "tool_execution_failed", message: "output reader failed", details: {
    process_id: "p1", completed: false, output: { running: true, failure: "output reader failed", chunks: [
      { stream: "stdout", source_seq: 1, content: { text: " first\n" } },
      { stream: "stderr", source_seq: 2, content: { encoding: "base64", retained_bytes: 3, base64: "/wph" }, preview: { text: "�\na", lossy: true } },
    ] },
  } } });
  const failedModel = model.commandActivityModel(failed);
  assert.equal(failedModel.phase, "failed");
  assert.deepEqual(failedModel.sections.map(({ stream, text }) => [stream, text]), [["stdout", " first\n"], ["stderr", "�\na"]]);
  assert.match(render(failed), /output reader failed/);
  assert.match(render(failed), /process is still running/);
  assert.match(render(failed), /replacement characters/);
  assert.equal(model.commandActivityModel({ ...yielded, eventData: { ...yielded.eventData, tool: "renamed.tool", arguments: { command: "completely different" } } }).phase, "running");
  assert.equal(activities.activityIsActive({ ...native(undefined, "started"), eventKind: "dynamic_tool", eventData: { status: "started", body: { unrelated: true } } }), true);

  const compactSource = read("electron/projection.cjs");
  const markers = functions(compactSource, ["clientEventHasProcessOutput"], { objectOrEmpty: record });
  assert.equal(markers.clientEventHasProcessOutput(failed.eventData), true);
  assert.equal(markers.clientEventHasProcessOutput({ body: { unrelated: "value" } }), false);
  const historical = functions(compactSource, ["clientEventDataProjection", "clientEventHasProcessOutput", "clientEventHasUnifiedDiff", "clientReasoningSummaryProjection", "compactClientEventValue"], {
    ...constants(compactSource, "CLIENT_"), objectOrEmpty: record, arrayValue: (value) => Array.isArray(value) ? value : [], stringValue: text,
  }).clientEventDataProjection;
  const liveSource = read("../registry/interface/world_operator_chat/client.cjs");
  const live = functions(liveSource, ["compactOperatorChatLiveEventData", "compactOperatorChatLiveEventValue"], {
    ...constants(liveSource, "LIVE_EVENT_DATA_"), isObject: (value) => Boolean(value && typeof value === "object" && !Array.isArray(value)),
  }).compactOperatorChatLiveEventData;
  const wrappers = [
    (output) => ({ body: { completed: false, output } }),
    (output) => ({ result: { completed: false, output } }),
    (output) => ({ output }),
    (output) => ({ error: { code: "capture_failed", message: "partial", details: { completed: false, output } } }),
  ];
  for (const wrap of wrappers) {
    const small = wrap({ stdout: { text: "exact" }, running: true, exit_code: null });
    for (const compact of [live, historical]) assert.deepEqual(compact(small), small);
    for (const output of [{ stdout: { text: "x".repeat(12_000) } }, { stderr: "x".repeat(12_000) }, { chunks: [{ stream: "stdout", content: { text: "x".repeat(12_000) } }] }]) {
      for (const overflow of [false, true]) {
        const payload = { ...wrap(output), ...(overflow ? { padding: Object.fromEntries(Array.from({ length: 12 }, (_, i) => [`field${i}`, "y".repeat(1500)])) } : {}) };
        const [l, h] = [live(payload), historical(payload)];
        assert.deepEqual(l._payload_projection, h._payload_projection);
        assert.equal(l._payload_projection.contains_process_output, true);
        if (overflow) assert.equal(l.padding, undefined);
      }
    }
    for (const output of [{}, { stdout: "", stderr: {}, chunks: [] }, { unrelated: "x".repeat(12_000) }]) {
      const payload = { ...wrap(output), padding: "z".repeat(12_000) };
      for (const compact of [live, historical]) assert.equal(compact(payload)._payload_projection.contains_process_output, undefined);
    }
  }
  const preview = { ...success, eventData: { ...success.eventData, _payload_projection: { truncated: true, contains_process_output: true } } };
  assert.match(render(preview), /partial preview/);
  assert.doesNotMatch(render({ ...preview, eventData: { ...preview.eventData, _payload_projection: { truncated: true, resolved: true } } }), /partial preview/);

  let observedFailure;
  for (const accessProfile of ["live", "lab"]) {
    let reads = 0;
    const failed = { source: "movement", direction: "out", movementSeq: 11, terminal: true, error: true,
      event_kind: "turn_control", event_data: { action: "stop", status: "failed" }, eventKind: "turn_control", eventData: { action: "stop", status: "failed" } };
    const completed = { ...failed, movementSeq: 12, artifact_id: "answer", artifactId: "answer", error: false,
      event_kind: "message", event_data: {}, eventKind: "message", eventData: {} };
    const reader = evaluate(observationSource.replace(/^import .* from "[^"\n]+";$/gm, ""), {
      isChatReplyTerminal,
      readOperatorChatArtifacts: async () => ({ cursor: {}, artifacts: [++reads === 1 ? failed : completed] }),
      loadChatRunMessages: async () => ++reads === 1 ? [failed] : [failed, completed],
      currentProjectionGeneration: () => 1, nextProjectionChange: async () => {},
    });
    const result = await reader.observeChatReply({ accessProfile, knownRunId: "run-a", interfaceId: "interface-a", afterSeq: 10,
      cursor: {}, isCurrent: () => true, onArtifacts: () => {} });
    assert.equal(reads, 2); assert.equal(result.terminal.artifactId, "answer");
  }
  await assert.rejects(async () => {
    try { await observations.observeChatReply({ accessProfile: "live", knownRunId: "run-a", interfaceId: "interface-a", afterSeq: 10, cursor: {}, isCurrent: () => true, onArtifacts: () => assert.fail("no receipt was delivered") }); }
    catch (failure) { observedFailure = failure; throw failure; }
  }, /connection closed/);
  for (const accessProfile of ["live", "lab"]) for (const reject of [false, true]) {
    let current = true, reads = 0;
    const stopped = evaluate(observationSource.replace(/^import .* from "[^"\n]+";$/gm, ""), {
      readOperatorChatArtifacts: async () => { reads++; current = false; if (reject) throw new Error("retired reader"); return { cursor: {}, artifacts: [{ source: "movement", terminal: true }] }; },
      loadChatRunMessages: async () => { reads++; current = false; if (reject) throw new Error("retired reader"); return [{ direction: "out", movementSeq: 11, terminal: true }]; },
      currentProjectionGeneration: () => 1,
    });
    const result = await stopped.observeChatReply({ accessProfile, knownRunId: "run-a", interfaceId: "interface-a", afterSeq: 10,
      cursor: {}, isCurrent: () => current, onArtifacts: () => assert.fail("retired view must not consume receipts") });
    assert.equal(result, null); assert.equal(reads, 1);
  }
  {
    let current = true, reads = 0, waits = 0, failures = 0, deliveries = 0;
    const recovering = evaluate(observationSource.replace(/^import .* from "[^"\n]+";$/gm, ""), {
      readOperatorChatArtifacts: async (input) => {
        reads++; assert.equal(input.afterSeq, 10); assert.equal(input.cursor, undefined);
        if (reads === 1) throw new Error("read disconnected");
        return { cursor: { movement_seq: 11 }, artifacts: [{ source: "movement", movement_seq: 11 }] };
      },
      currentProjectionGeneration: () => 1,
      nextProjectionChange: async () => { waits++; },
    });
    await recovering.observeChatArtifacts({ runId: "run-a", interfaceId: "interface-a", afterSeq: 10,
      isCurrent: () => current, onFailure: () => { failures++; }, onRecovered: () => { current = false; },
      onArtifacts: () => { deliveries++; if (deliveries === 1) throw new Error("receipt not consumed"); },
    });
    assert.deepEqual({ reads, waits, failures, deliveries }, { reads: 3, waits: 2, failures: 2, deliveries: 2 });
  }
  const workspace = read("src/components/chat/useChatWorkspace.ts");
  const ast = ts.createSourceFile("workspace.ts", workspace, ts.ScriptTarget.Latest, true);
  let catchBlock;
  function visit(node) { if (ts.isCatchClause(node) && node.block.getText(ast).includes("const durableTerminalObserved")) catchBlock = node.block.getText(ast); ts.forEachChild(node, visit); }
  visit(ast); assert.ok(catchBlock);
  let pending = [], targets = { "run-a": { runId: "run-a", interfaceId: "interface-a" } };
  let scopeCurrent = true, switchDuringRead = false, readResult = [];
  const catchProbe = evaluate(`async function probe(failure) { let current; while (true) ${catchBlock.slice(0, -1)} break; } } module.exports = probe;`, {
    ...observations, isChatReplyTerminal, formatRuntimeError: (e) => e.message, turn: { queueId: "q1", runKey: "run-a", runId: "run-a" },
    queueId: "q1", owner: "view1", submittedToRuntime: true, lockedRunKey: "run-a", effectiveRunId: "run-a", messageId: "m1",
    queueStore: { settle: () => assert.fail("accepted turn must not be resubmitted or reclassified") },
    replacePausedQueueRunKeys: (update) => update([]), deliveryScopeCurrent: () => scopeCurrent,
    chatDraftKey: (id) => id, refreshRuns: async () => {}, refreshMessages: async () => { if (switchDuringRead) scopeCurrent = false; return readResult; },
    submittedAfterSeq: 10, terminalErrorReceiptObserved: false, messageCacheRef: { current: new Map() }, userBubble: { id: "user", role: "user", text: "question" },
    setInFlightTurnControlTargets: (update) => { targets = update(targets); }, replaceRunPending: (_key, messages) => { pending = messages; },
  });
  await catchProbe(observedFailure);
  assert.equal(pending[1].terminal, false);
  assert.equal(pending[1].eventData._observation_error, true);
  assert.ok(targets["run-a"]);
  const newer = { ...native({}), id: "receipt", eventData: { _movement: { seq: 11 } } };
  assert.equal(projections.mergeRenderedMessages([newer], pending, [], true).some((m) => m.eventData?._observation_error), false);
  assert.equal(projections.mergeRenderedMessages([{ ...newer, runId: "other" }], pending, [], true).some((m) => m.eventData?._observation_error), true);
  readResult = [{ ...newer, direction: "out", terminal: true, error: false, movementSeq: 11 }];
  await catchProbe(observedFailure);
  assert.deepEqual(pending, []);
  assert.deepEqual(targets, {});
  switchDuringRead = true; pending = ["new Home state"]; targets = { "new-run": {} };
  await catchProbe(observedFailure);
  assert.deepEqual(pending, ["new Home state"]);
  assert.deepEqual(targets, { "new-run": {} });

  if (process.argv[2]) {
    const baseline = JSON.parse(fs.readFileSync(process.argv[2], "utf8"));
    const original = baseline["app/src/components/chat/ChatMessageList.tsx"].source;
    const old = functions(original, ["CommandActivity", "normalizeTerminalControl", "formatActivityElapsed"], uiDependencies);
    const oldHtml = renderToStaticMarkup(React.createElement(old.CommandActivity, { command: "arbitrary executable", lifecycle: "complete", message: success, output: "exact output\n", outputRecord: success.eventData.output }));
    for (const pattern of [/Ran arbitrary executable/, />Success</, /exact output/]) { assert.match(oldHtml, pattern); assert.match(successfulHtml, pattern); }
    const oldLifecycle = functions(original, ["activityLifecycleFromStatus"], {});
    assert.equal(oldLifecycle.activityLifecycleFromStatus(yielded.eventData.status), "complete");
    console.log("PASS source baseline: successful command retained; yielded completion gap corrected");
  }
  console.log("PASS command lifecycle, named file/artifact reads, native operation receipts, typed web detail, lazy disclosure, live/historical compaction parity, observer rejection and workspace catch");
})().catch((error) => { console.error(error); process.exitCode = 1; });
