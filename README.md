# Waypoint

<p align="center">
  <img src="src-tauri/icons/icon.png" alt="Waypoint logo" width="96" height="96">
</p>

<p align="center">
  <strong>A local desktop router for long-running AI agent CLI sessions.</strong>
</p>

<p align="center">
  <a href="README.zh-CN.md">中文文档</a>
</p>

![Waypoint desktop interface](docs/assets/waypoint-screenshot.png)

Waypoint is a Tauri desktop app for managing multiple local AI agent CLI sessions in one place. It keeps PTY-backed sessions alive, lets you switch between workspaces and agents, and provides a handover flow for passing context from one agent session to another.

It is built for tools like Claude Code, Codex, Antigravity CLI, GitHub Copilot CLI, and your regular shell.

## Highlights

- **Local PTY session hosting**: run agent CLIs in real terminal sessions managed by the desktop app.
- **Multi-agent workspace routing**: pin workspace folders and launch available agents from each folder.
- **Persistent session switching**: switch between sessions without killing the underlying process.
- **Agent handover**: continue work by forwarding terminal context, git status, diffs, and a note to another agent session.
- **Native desktop shell**: Tauri v2 + Rust backend with a React and xterm.js interface.
- **Automatic CLI detection**: resolves agent commands through the user login shell so the app sees a PATH close to the terminal environment.

## Supported Agents

Waypoint currently detects these presets:

```text
Claude Code:
  claude

Codex:
  codex

Antigravity CLI:
  agy

GitHub Copilot:
  copilot
  gh copilot

Shell:
  $SHELL
```

If an agent is available in your terminal but appears as missing in Waypoint, verify it with:

```bash
command -v claude
command -v codex
command -v agy
command -v copilot
command -v gh
```

## Tech Stack

- **Desktop shell**: Tauri v2 + Rust
- **Frontend**: React + TypeScript + Vite
- **Terminal UI**: `@xterm/xterm` + `@xterm/addon-fit`
- **PTY hosting**: `portable-pty`

## Prerequisites

Waypoint requires Node.js, npm, and a Rust toolchain.

On macOS, make sure Xcode Command Line Tools are installed:

```bash
xcode-select -p
```

If they are missing:

```bash
xcode-select --install
```

Install Rust with `rustup`:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

Verify the environment:

```bash
node --version
npm --version
rustc --version
cargo --version
```

## Development

Install dependencies:

```bash
npm install
```

Run the desktop app in development mode:

```bash
npm run tauri:dev
```

This starts the Vite dev server and opens the Tauri desktop shell.

> [!IMPORTANT]
> PTY management runs in the Tauri desktop process. Opening `http://127.0.0.1:1420/` in a regular browser is useful for UI preview only; creating and using real PTY sessions requires the Tauri desktop window.

Build the frontend:

```bash
npm run build
```

Build the Tauri app without bundling installers:

```bash
npm run tauri -- build --debug --no-bundle --ci
```

## Manual Acceptance Flow

1. Start the desktop app with `npm run tauri:dev`.
2. Confirm the left-side agent environment list shows available and missing local agents.
3. Pin or select a local workspace folder.
4. Start an available agent from that workspace.
5. Confirm the terminal launches in the selected workspace.
6. Start a second session and switch back to the first one.
7. Confirm the first PTY process is still alive.

## Continue / Handover

The Continue flow passes context from the current session to another agent.

For a new target session:

1. Open a source session.
2. Click **Continue**.
3. Choose **New Session**.
4. Select the target agent and workspace.
5. Add an optional note describing what the next agent should focus on.
6. Click **Create & Continue**.

Waypoint collects recent terminal context, workspace git status, git diff, and staged diff. It writes a handover file under `~/.waypoint/<workspace-name>/handover-*.md`, launches the target session, and injects a short instruction pointing the target agent to the handover file.

Existing sessions can also receive a handover through **Existing Session** mode.

## Troubleshooting

### `cargo` or `rustc` command not found

Load Cargo into the current shell:

```bash
source "$HOME/.cargo/env"
```

Then check:

```bash
rustc --version
cargo --version
```

### `npm run tauri:dev` says Rust is not installed

Check what Tauri can detect:

```bash
npm run tauri -- info
```

If `rustc` is not detected, reload your shell environment or add Cargo to your shell profile.

### `Tauri runtime unavailable` in the browser

This is expected outside the desktop shell. Tauri APIs for PTY sessions, local file access, and native commands only exist inside the Tauri app.

### Continue fails with `failed to write handover`

The target agent may have exited before Waypoint could inject the handover prompt, or it may be waiting on login/configuration.

Try starting the target agent directly first. If it immediately exits, resolve its own authentication or CLI setup before using it as a handover target.

## Documentation

- [Technical design](AGENTRELAY_TECHNICAL_DESIGN.md)
- [Architecture summary](AGENTRELAY_ARCHITECTURE_SUMMARY.md)
