import { invoke } from "@tauri-apps/api/core";
import type {
  AgentPresetInfo,
  ChatMessage,
  HandoverContentMode,
  HandoverDraft,
  HandoverPreview,
  HandoverResult,
  SessionInfo,
  SessionSnapshot,
} from "../types";

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

const DEFAULT_TERMINAL_ROWS = 30;
const DEFAULT_TERMINAL_COLS = 160;

export function createAgentSession(agentId: string, cwd: string): Promise<SessionInfo> {
  assertTauriRuntime();
  return invoke("create_agent_session", {
    agentId,
    cwd,
    rows: DEFAULT_TERMINAL_ROWS,
    cols: DEFAULT_TERMINAL_COLS,
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
    rows: DEFAULT_TERMINAL_ROWS,
    cols: DEFAULT_TERMINAL_COLS,
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
  handoverMode: HandoverContentMode,
): Promise<HandoverResult> {
  assertTauriRuntime();
  return invoke("forward_session", {
    sourceSessionId,
    targetSessionId,
    note,
    handoverMode,
  });
}

export function continueSession(
  sourceSessionId: string,
  targetAgentId: string,
  cwd: string,
  note: string,
  handoverMode: HandoverContentMode,
): Promise<HandoverResult> {
  assertTauriRuntime();
  return invoke("continue_session", {
    sourceSessionId,
    targetAgentId,
    cwd,
    note,
    handoverMode,
    rows: DEFAULT_TERMINAL_ROWS,
    cols: DEFAULT_TERMINAL_COLS,
  });
}

export function getHandoverPreview(sourceSessionId: string): Promise<HandoverPreview> {
  assertTauriRuntime();
  return invoke("get_handover_preview", { sourceSessionId });
}

export function getHandoverDraft(params: {
  sourceSessionId: string;
  targetMode: "new" | "existing";
  targetSessionId?: string | null;
  targetAgentId?: string | null;
  cwd?: string | null;
  note: string;
  handoverMode: HandoverContentMode;
}): Promise<HandoverDraft> {
  assertTauriRuntime();
  return invoke("get_handover_draft", params);
}

export function listChatMessages(sessionId: string): Promise<ChatMessage[]> {
  assertTauriRuntime();
  return invoke("list_chat_messages", { sessionId });
}

export function selectDirectory(): Promise<string | null> {
  if (!isTauriRuntime()) {
    return Promise.resolve(null);
  }
  return invoke("select_directory");
}

export function openInEditor(path: string, editorBin: string): Promise<void> {
  assertTauriRuntime();
  return invoke("open_in_editor", { path, editorBin });
}

export interface EditorInfo {
  id: string;
  name: string;
  bin: string;
}

export function detectEditors(): Promise<EditorInfo[]> {
  if (!isTauriRuntime()) {
    return Promise.resolve([]);
  }
  return invoke("detect_editors");
}
