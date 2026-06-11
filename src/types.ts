export type SessionStatus = "running" | "exited" | "error";

export type SessionInfo = {
  id: string;
  agentId: string;
  agentName: string;
  title: string;
  command: string;
  cwd: string;
  status: SessionStatus;
  attached: boolean;
  createdAt: number;
  lastActiveAt: number;
  firstUserMessage?: string | null;
  nativeSessionRef?: NativeSessionRef | null;
};

export type NativeSessionRef = {
  provider: string;
  id?: string | null;
  name?: string | null;
  resumeCommand?: string | null;
  discoveredAt: number;
};

export type SessionSnapshot = {
  session: SessionInfo;
  replay: string;
  replayBase64?: string;
  mode: "live" | "replay-only";
};

export type PtyDataEvent = {
  sessionId: string;
  dataBase64?: string;
  data?: string;
};

export type SessionEvent = {
  session: SessionInfo;
};

export type SessionErrorEvent = {
  sessionId: string;
  message: string;
};

export type HandoverResult = {
  prompt: string;
  sourceSession: SessionInfo;
  targetSession: SessionInfo;
  mode: string;
};

export type AgentPresetInfo = {
  id: string;
  name: string;
  description: string;
  available: boolean;
  command: string;
  resolvedCommand?: string | null;
};

export type WorkspaceFolder = {
  path: string;
  name: string;
  isPinned: boolean;
};
