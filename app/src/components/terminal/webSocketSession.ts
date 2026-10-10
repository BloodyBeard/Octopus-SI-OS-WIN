import type { OctopusTerminalSession, OctopusTerminalSessionHandlers } from "./OctopusTerminal";
import type {
  OctopusProcessViewerActionRequest,
  OctopusProcessViewerEvent,
  OctopusProcessViewerState,
} from "../../electron";

export type WebSocketTerminalState = {
  detail: string;
  state: "connecting" | "connected" | "disconnected" | "failed";
};

export function createWebSocketTerminalSession(
  socketUrl: string,
  onState: (state: WebSocketTerminalState) => void,
): OctopusTerminalSession {
  let socket: WebSocket | null = null;
  let handlers: OctopusTerminalSessionHandlers | null = null;
  let disposed = false;
  let opened = false;
  let failed = false;

  const update = (state: WebSocketTerminalState) => {
    if (!disposed) onState(state);
  };

  const fail = (detail: string) => {
    failed = true;
    update({ detail, state: "failed" });
  };

  return {
    open() {
      if (disposed) return Promise.reject(new Error("The terminal session is closed."));
      if (socket) return Promise.reject(new Error("The terminal session is already open."));
      update({ detail: "Opening the projected terminal socket…", state: "connecting" });
      return new Promise<void>((resolve, reject) => {
        let settled = false;
        try {
          socket = new WebSocket(socketUrl);
        } catch (error) {
          const detail = error instanceof Error ? error.message : String(error);
          fail(detail);
          reject(error);
          return;
        }
        socket.binaryType = "arraybuffer";
        socket.addEventListener("open", () => {
          opened = true;
          settled = true;
          update({ detail: "Terminal connected", state: "connected" });
          resolve();
        });
        socket.addEventListener("message", (event) => {
          if (typeof event.data === "string") handlers?.onData(event.data);
          else if (event.data instanceof ArrayBuffer) handlers?.onData(new TextDecoder().decode(event.data));
        });
        socket.addEventListener("error", () => {
          const detail = "The projected terminal socket could not be reached.";
          fail(detail);
          try { socket?.close(); } catch {}
          if (!settled) {
            settled = true;
            reject(new Error(detail));
          }
        });
        socket.addEventListener("close", (event) => {
          if (disposed || failed) return;
          const detail = opened
            ? event.reason || `The terminal socket closed${event.code ? ` (code ${event.code})` : ""}.`
            : "The projected terminal socket closed before it connected.";
          update({ detail, state: "disconnected" });
          if (!settled) {
            settled = true;
            reject(new Error(detail));
          }
        });
      });
    },
    write(data) {
      if (socket?.readyState === WebSocket.OPEN) socket.send(data);
    },
    resize() {},
    subscribe(next) {
      handlers = next;
      return () => {
        if (handlers === next) handlers = null;
        disposed = true;
        if (socket && socket.readyState < WebSocket.CLOSING) socket.close(1000, "surface closed");
        socket = null;
      };
    },
  };
}

export function createTranscriptTerminalSession(content: string): OctopusTerminalSession {
  let handlers: OctopusTerminalSessionHandlers | null = null;
  return {
    async open() {
      handlers?.onInitLog(content || "\u001b[2mNo output yet\u001b[0m");
    },
    write() {},
    resize() {},
    subscribe(next) {
      handlers = next;
      return () => {
        if (handlers === next) handlers = null;
      };
    },
  };
}

export type ProcessViewerTerminalState = {
  sessionId: string;
  state: OctopusProcessViewerState;
};

export type ProcessViewerTerminalSession = OctopusTerminalSession & {
  interrupt(): void;
  terminate(): void;
  restart(): void;
  startNewShell(): void;
};

type ProcessViewerCommand =
  | { action: "write"; data: string }
  | { action: "resize"; cols: number; rows: number }
  | { action: "interrupt" | "terminate" | "restart" | "start_new_shell" };

export function createProcessViewerTerminalSession(
  workerEnvironmentId: string,
  surfaceId: string,
  onState: (state: ProcessViewerTerminalState | null, error?: string) => void,
): ProcessViewerTerminalSession {
  let handlers: OctopusTerminalSessionHandlers | null = null;
  let snapshot: ProcessViewerTerminalState | null = null;
  const bufferedOutput: string[] = [];
  const earlyEvents: OctopusProcessViewerEvent[] = [];
  let lastDimensions = { cols: 80, rows: 24 };
  let disposed = false;
  let unsubscribe: (() => void) | null = null;

  const perform = async (request: ProcessViewerCommand): Promise<boolean> => {
    if (!snapshot || disposed || !window.octopusTransport) return false;
    try {
      const outcome = await window.octopusTransport.processViewerAction({
        ...request,
        sessionId: snapshot.sessionId,
      } as OctopusProcessViewerActionRequest);
      if (!disposed && snapshot) {
        snapshot = { ...snapshot, state: outcome.state };
        onState(snapshot);
      }
      return true;
    } catch (error) {
      onState(snapshot, error instanceof Error ? error.message : String(error));
      return false;
    }
  };

  return {
    async open(dimensions) {
      lastDimensions = { ...dimensions };
      const transport = window.octopusTransport;
      if (!transport) throw new Error("Process viewer bridge is unavailable.");
      unsubscribe = transport.onProcessViewerEvent((message) => {
        if (disposed) return;
        if (!snapshot) {
          earlyEvents.push(message);
          return;
        }
        if (message.sessionId !== snapshot.sessionId) return;
        deliverProcessViewerEvent(message, handlers, bufferedOutput, snapshot, (next) => {
          snapshot = next;
          onState(next);
        }, (error) => onState(snapshot, error));
      });
      const opened = await transport.openProcessViewer({ surfaceId, workerEnvironmentId });
      if (disposed) {
        await transport.closeProcessViewer({ sessionId: opened.sessionId }).catch(() => undefined);
        return;
      }
      snapshot = opened;
      onState(snapshot);
      handlers?.onInitLog("");
      for (const message of earlyEvents.splice(0)) {
        if (message.sessionId !== opened.sessionId) continue;
        deliverProcessViewerEvent(message, handlers, bufferedOutput, snapshot, (next) => {
          snapshot = next;
          onState(next);
        }, (error) => onState(snapshot, error));
      }
      if (snapshot.state.tty) void perform({ action: "resize", ...dimensions });
    },
    write(data) {
      void perform({ action: "write", data });
    },
    resize(dimensions) {
      lastDimensions = { ...dimensions };
      if (snapshot?.state.tty) void perform({ action: "resize", ...dimensions });
    },
    interrupt() {
      void perform({ action: "interrupt" });
    },
    terminate() {
      void perform({ action: "terminate" });
    },
    restart() {
      void perform({ action: "restart" }).then((started) => {
        if (started && snapshot?.state.tty) void perform({ action: "resize", ...lastDimensions });
      });
    },
    startNewShell() {
      void perform({ action: "start_new_shell" }).then((started) => {
        if (started && snapshot?.state.tty) void perform({ action: "resize", ...lastDimensions });
      });
    },
    subscribe(next) {
      handlers = next;
      if (bufferedOutput.length) {
        next.onData(bufferedOutput.splice(0).join(""));
      }
      return () => {
        if (handlers === next) handlers = null;
        disposed = true;
        unsubscribe?.();
        unsubscribe = null;
        if (snapshot) void window.octopusTransport?.closeProcessViewer({ sessionId: snapshot.sessionId });
        snapshot = null;
      };
    },
  };
}

function deliverProcessViewerEvent(
  message: OctopusProcessViewerEvent,
  handlers: OctopusTerminalSessionHandlers | null,
  bufferedOutput: string[],
  snapshot: ProcessViewerTerminalState,
  update: (state: ProcessViewerTerminalState) => void,
  fail: (error: string) => void,
) {
  if (message.event.type === "output") {
    if (handlers) handlers.onData(message.event.data);
    else bufferedOutput.push(message.event.data);
  }
  if (message.event.type === "history_truncated") {
    const notice = "\r\n\u001b[2m… Earlier process output is no longer retained.\u001b[0m\r\n";
    if (handlers) handlers.onData(notice);
    else bufferedOutput.push(notice);
  }
  if (message.event.type === "reset") handlers?.onInitLog("");
  if (message.event.type === "state") {
    update({ ...snapshot, state: message.event.state });
  }
  if (message.event.type === "failure") fail(message.event.message);
}
