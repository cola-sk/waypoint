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
  parentSessionId?: string | null;
  handoverRootId?: string | null;
  dangerous: boolean;
  noneWorkspace: boolean;
};

export type NativeSessionRef = {
  provider: string;
  id?: string | null;
  name?: string | null;
  project?: string | null;
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
  handoverMode: HandoverContentMode;
  handoverPath?: string | null;
  evidencePath?: string | null;
};

export type HandoverFileResult = {
  prompt: string;
  sourceSession: SessionInfo;
  handoverMode: "compact" | "full";
  handoverPath: string;
  evidencePath?: string | null;
};

export type HandoverContentMode = "recommended" | "compact" | "full";

export type HandoverPreview = {
  estimatedChars: number;
  largeThresholdChars: number;
  isLarge: boolean;
  recommendedMode: "compact" | "full";
  terminalContextChars: number;
  userInputChars: number;
  inheritedContextChars: number;
};

export type HandoverDraft = {
  prompt: string;
  effectiveMode: "compact" | "full";
  estimatedChars: number;
  evidencePath?: string | null;
};

export type SessionAttachmentInfo = {
  id: string;
  filename: string;
  path: string;
  mime: string;
  sizeBytes: number;
  createdAt: number;
  previewDataUrl?: string | null;
};

export type FilePreview = {
  path: string;
  name: string;
  extension: string;
  kind: "text" | "image";
  mime: string;
  sizeBytes: number;
  modifiedAt?: number | null;
  content: string;
  dataUrl?: string | null;
  truncated: boolean;
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

export type ChatRole = "user" | "assistant";

export type ChatMessage = {
  id: string;
  role: ChatRole;
  content: string;
  pending: boolean;
  createdAt: number;
  updatedAt: number;
};
