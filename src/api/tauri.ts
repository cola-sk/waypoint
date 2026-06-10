import { invoke } from "@tauri-apps/api/core";
import type { SessionInfo, SessionSnapshot } from "../types";

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

export function createShellSession(): Promise<SessionInfo> {
  assertTauriRuntime();
  return invoke("create_shell_session", {
    title: null,
    cwd: null,
    rows: 30,
    cols: 100,
  });
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
