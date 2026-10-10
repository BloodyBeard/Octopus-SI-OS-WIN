# Octopus desktop client

The React and Electron client presents Home, tasks, applications, drivers, and
runtime evidence. Navigation, windows, onboarding, and local file preparation
belong to the client. Autonomous model and tool execution crosses declared
Octopus interfaces under policy.

- [Build and first setup](../docs/getting-started.md)
- [Using the desktop client](../docs/client.md)
- [Architecture](../docs/architecture.md)
- [Development and checks](../docs/development.md)

From the repository root, install client dependencies with `npm ci --prefix app`.
For a renderer preview, run `npm --prefix app run dev`. Desktop setup and runtime
flows require the packaged app described in the build guide.

Source lives in `src/`; host operations live in `electron/`; `distribution/`
declares the baseline and local application packs. Build outputs in `dist/` and
`build/` are generated and ignored.
