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

### Native Session IDs And Resume

Waypoint stores its own session metadata in `~/.waypoint/sessions/<session-id>/meta.json`. For agents with native resume support, it also records a `nativeSessionRef` with the provider, native id, optional project, resume command, and discovery time. When a historical session is reactivated, the backend refreshes this native reference first, then builds the agent-specific resume command.

Agent-specific native id behavior:

```text
Claude Code:
  Waypoint injects its own session id as Claude's --session-id on new sessions.
  If the launch args already contain --resume/-r/--session-id, it does not inject another id.
  meta.nativeSessionRef.id = the Waypoint session id.
  Before resume, Waypoint checks ~/.claude/projects/<workspace-as-claude-project>/<id>.jsonl.
  If that path is missing, it searches ~/.claude/projects for a matching <id>.jsonl filename.
  Resume command: claude --resume <id>.

Codex:
  Waypoint does not force a native id when creating a Codex session.
  If meta already has a native id, resume uses codex resume <id>.
  If no native id is known, resume falls back to codex resume --last.
  For native transcript lookup, Waypoint searches ~/.codex/sessions and
  ~/.codex/archived_sessions by native id. Without a native id, it selects the
  newest transcript matching the workspace cwd and session creation time.

Antigravity CLI (agy):
  agy does not support caller-supplied conversation ids.
  On the first real user submission in an agy PTY session, Waypoint sends:
    <!-- waypoint_session_id: <waypoint-session-id> -->
  This marker is persisted in agy's native transcript.
  On natural exit, kill/stop, resume, and handover construction, Waypoint scans:
    ~/.gemini/antigravity-cli/brain/*/.system_generated/logs/transcript.jsonl
  The brain directory containing the waypoint_session_id marker is the agy conversation id.
  meta.nativeSessionRef.id = the agy conversation id.
  If Waypoint's own terminal transcript includes agy's "Resume in the same project" line,
  Waypoint parses --project=<project> as supplemental resume metadata.
  Resume command: agy --conversation=<conversation-id> [--project=<project>].

GitHub Copilot:
  Waypoint injects its own session id as --session-id=<id> on new sessions.
  If launch args already contain --continue/--resume/-r/--session-id, it does not inject another id.
  For gh copilot, Waypoint inserts -- when needed before Copilot-specific args.
  meta.nativeSessionRef.id = the Waypoint session id.
  Resume command: copilot --resume=<id>, or gh copilot -- --resume=<id>.

Shell:
  Plain shells do not have an agent-native session id. Waypoint keeps only its own PTY transcript and replay.
```

### Handover File Generation

Handover does not push the full context through a target agent's command line. Waypoint writes a file first, then asks the target agent to read that exact file.

File layout and mode selection:

```text
Main file:
  ~/.waypoint/<workspace-name>/handover-<uuid>.md

Full evidence file for Compact mode:
  ~/.waypoint/<workspace-name>/handover-<uuid>-full-evidence.md

workspace-name:
  The final path segment of the workspace directory, or workspace as a fallback.

Mode:
  Recommended uses Compact when the estimated context exceeds 32,000 characters.
  Otherwise Recommended uses Full.
  Users can explicitly choose Compact or Full.
```

The handover file includes:

```text
1. Source and target agent, command, and workspace.
2. The user's note from the Continue dialog.
3. git branch and git status --short.
4. Unstaged and staged diff stat, file lists, and diff previews.
5. Recent source terminal context.
6. Recent user inputs.
7. Inherited context from the previous handover hop.
8. Exact attachment paths, MIME types, and sizes.
9. agy markdown artifacts from ~/.gemini/antigravity-cli/brain/<conversation-id>/*.md.
```

Source context priority:

```text
Claude Code:
  Prefer ~/.claude/projects/.../<native-id>.jsonl.
  Fall back to Waypoint's terminal/chat buffer.

Codex:
  Prefer ~/.codex/sessions or archived_sessions transcripts matching the native id.
  Without a native id, choose the newest transcript matching workspace and creation time.
  Fall back to Waypoint's terminal/chat buffer.

Antigravity CLI:
  Resolve the conversation id through the waypoint_session_id marker first.
  Then read ~/.gemini/antigravity-cli/brain/<conversation-id>/.system_generated/logs/transcript.jsonl.
  Fall back to Waypoint's terminal/chat buffer.

GitHub Copilot / Shell:
  Use Waypoint's terminal/chat buffer and input ring.
```

Full versus Compact:

```text
Full:
  The main handover file contains the structured context, recent conversation,
  user inputs, git state, diff stat, file lists, and bounded diff previews.

Compact:
  The main handover file keeps shorter context, user input, git status, diff stat,
  and changed file lists. It omits inline full diff previews.
  Waypoint also writes *-full-evidence.md with complete evidence, git diff, and staged diff.
  The Compact handover references the evidence file path for exact follow-up reading.
```

### Agent Handover Launch Strategy

```text
Claude Code:
  New Session writes the handover file first.
  Launch shape: claude "<startup prompt>".
  The startup prompt asks Claude to read only the handover file and includes a new waypoint_session_id marker.

Codex:
  The default command includes --no-alt-screen for xterm stability.
  New Session grants the handover directory with --add-dir.
  The startup prompt points to the handover file and includes a new waypoint_session_id marker.
  Waypoint waits longer before injection to reduce writes before Codex is ready.

Antigravity CLI:
  New Session uses agy --prompt-interactive "<startup prompt>".
  Waypoint grants the handover directory with --add-dir.
  The startup prompt only carries the handover path and new waypoint_session_id marker,
  avoiding large diff/context payloads in agy's TUI.

GitHub Copilot:
  New Session uses copilot -i "<startup prompt>".
  The handover directory is passed with --add-dir; gh copilot gets -- before Copilot args.

Existing Session / Forward:
  Waypoint writes the handover file, then bracketed-pastes a short instruction into the target PTY.
  The instruction tells the target to read only that file, acknowledge context loaded, and wait.
  Before injection, Waypoint checks whether the target process already exited.

Create Handover File:
  The topbar handover-file action only writes the file; it does not start or inject into an agent.
  The target is recorded as Manual handover so the file path can be used with external tools.
```

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
