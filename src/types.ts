export type SessionStatus = "starting" | "running" | "exited" | "error";

export type SessionInfo = {
  id: string;
  title: string;
  command: string;
  cwd: string;
  status: SessionStatus;
  attached: boolean;
  createdAt: number;
  lastActiveAt: number;
};

export type SessionSnapshot = {
  session: SessionInfo;
  replay: string;
};

export type PtyDataEvent = {
  sessionId: string;
  data: string;
};

export type SessionEvent = {
  session: SessionInfo;
};

export type SessionErrorEvent = {
  sessionId: string;
  message: string;
};

