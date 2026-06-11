import { invoke } from "@tauri-apps/api/core";
import type { AgentPresetInfo, HandoverResult, SessionInfo, SessionSnapshot } from "../types";

declare global {
  interface Window {
    __TAURI_INTERNALS__?: unknown;
  }
}

function isTauriRuntime() {
  return typeof window !== "undefined" && Boolean(window.__TAURI_INTERNALS__);
}

function assertTauriRuntime() {
  if (!isTauriRuntime()) {
    throw new Error("Tauri runtime unavailable. Start the desktop app with npm run tauri:dev.");
  }
}

export function createAgentSession(agentId: string, cwd: string): Promise<SessionInfo> {
  assertTauriRuntime();
  return invoke("create_agent_session", {
    agentId,
    cwd,
    rows: 30,
    cols: 100,
  });
}

export function listAgentPresets(): Promise<AgentPresetInfo[]> {
  if (!isTauriRuntime()) {
    return Promise.resolve([
      {
        id: "claude-code",
        name: "Claude Code",
        description: "Anthropic Claude Code CLI",
        available: false,
        command: "claude",
        resolvedCommand: null,
      },
      {
        id: "codex",
        name: "Codex",
        description: "OpenAI Codex CLI",
        available: false,
        command: "codex",
        resolvedCommand: null,
      },
      {
        id: "gemini",
        name: "Gemini CLI",
        description: "Google Gemini CLI",
        available: false,
        command: "gemini",
        resolvedCommand: null,
      },
      {
        id: "copilot",
        name: "GitHub Copilot",
        description: "GitHub Copilot CLI",
        available: false,
        command: "copilot",
        resolvedCommand: null,
      },
    ]);
  }
  return invoke("list_agent_presets");
}

export function defaultWorkspace(): Promise<string> {
  if (!isTauriRuntime()) {
    return Promise.resolve("");
  }
  return invoke("default_workspace");
}

export function listSessions(): Promise<SessionInfo[]> {
  if (!isTauriRuntime()) {
    return Promise.resolve([]);
  }
  return invoke("list_sessions");
}

export function attachSession(sessionId: string): Promise<SessionSnapshot> {
  assertTauriRuntime();
  return invoke("attach_session", { sessionId });
}

export function reactivateSession(sessionId: string): Promise<SessionInfo> {
  assertTauriRuntime();
  return invoke("reactivate_session", {
    sessionId,
    rows: 30,
    cols: 100,
  });
}

export function detachSession(sessionId: string): Promise<void> {
  assertTauriRuntime();
  return invoke("detach_session", { sessionId });
}

export function writeSession(sessionId: string, data: string): Promise<void> {
  assertTauriRuntime();
  return invoke("write_session", { sessionId, data });
}

export function resizeSession(sessionId: string, rows: number, cols: number): Promise<void> {
  assertTauriRuntime();
  return invoke("resize_session", { sessionId, rows, cols });
}

export function killSession(sessionId: string): Promise<void> {
  assertTauriRuntime();
  return invoke("kill_session", { sessionId });
}

export function deleteSession(sessionId: string): Promise<void> {
  assertTauriRuntime();
  return invoke("delete_session", { sessionId });
}

export function forwardSession(
  sourceSessionId: string,
  targetSessionId: string,
  note: string,
): Promise<HandoverResult> {
  assertTauriRuntime();
  return invoke("forward_session", {
    sourceSessionId,
    targetSessionId,
    note,
  });
}

export function continueSession(
  sourceSessionId: string,
  targetAgentId: string,
  cwd: string,
  note: string,
): Promise<HandoverResult> {
  assertTauriRuntime();
  return invoke("continue_session", {
    sourceSessionId,
    targetAgentId,
    cwd,
    note,
    rows: 30,
    cols: 100,
  });
}

export function selectDirectory(): Promise<string | null> {
  if (!isTauriRuntime()) {
    return Promise.resolve(null);
  }
  return invoke("select_directory");
}
