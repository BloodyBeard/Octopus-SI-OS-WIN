# world_mcp_example

A live example of an external MCP server as an ordinary World interface
package.

The folder contains:

- `interface.json` with the transport config;
- `life.json` with a package-local MCP life process and a generic tool-door
  process;
- `packages/octopus-mcp-client` with the executable code;
- `registry/tool/mcp.example.search/tool.json` with a separate process
  declaration for the server tool.

MCP is not a runtime primitive. The life reads its own movements, speaks the
MCP wire, and returns resources and results as ordinary artifacts. The tool
process receives only the environment variable explicitly selected by the
transport config, network authority, a private var root, and the stdin/stdout
intake. The runtime does not know the MCP payload, the server's tools, OAuth,
or the transport dialect.

A new server instance is a new interface folder with its own id and config, and
an explicit `reference: world_mcp_example` if it reuses this `life.json`. A new
server tool is a new tool folder with a package process and
`handler_config.mcp_tool_name`. Neither step changes the runtime, app
bootstrap, or a compiled dispatch table. A missing executable, a malformed
descriptor, or a runtime refusal remains a typed fail-closed result; there is
no placeholder success.
