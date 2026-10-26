const assert = require("node:assert/strict");
const { test } = require("node:test");
const { pipeName, localSocketAddress } = require("../electron/localSocket.cjs");

// The same vector is checked by crates/octopus-local-socket (pipe_name) and the
// browser host's native/local-socket.cjs.
test("Windows pipe names match octopus-local-socket", () => {
  const expected = "\\\\.\\pipe\\octopus-bd8b7f095da0d32b198bed7bb31f63c4";
  for (const socketPath of [
    "C:\\Octopus\\Home\\app-server-control\\abc.sock",
    "\\\\?\\C:\\Octopus\\Home\\app-server-control\\abc.sock",
    "c:/octopus/home/app-server-control/abc.sock",
  ]) assert.equal(pipeName(socketPath), expected);
  assert.equal(localSocketAddress("/tmp/a.sock", "darwin"), "/tmp/a.sock");
  assert.equal(localSocketAddress("C:\\a.sock", "win32"), pipeName("C:\\a.sock"));
});
