import { useEffect, useMemo, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  Bot,
  ChevronDown,
  ChevronRight,
  Folder,
  FolderPlus,
  MoreHorizontal,
  MessageSquare,
  Pin,
  RefreshCw,
  Send,
  Square,
  Trash2,
  X,
  Plus,
  FolderOpen,
} from "lucide-react";
import TerminalView from "./components/TerminalView";
import WptLogo from "./components/WptLogo";
import {
  continueSession,
  createAgentSession,
  defaultWorkspace,
  deleteSession,
  detectEditors,
  forwardSession,
  getHandoverDraft,
  getHandoverPreview,
  killSession,
  listAgentPresets,
  listSessions,
  openInEditor,
  selectDirectory,
} from "./api/tauri";
import type {
  AgentPresetInfo,
  HandoverContentMode,
  HandoverDraft,
  HandoverPreview,
  HandoverResult,
  SessionInfo,
  WorkspaceFolder,
} from "./types";
import type { EditorInfo } from "./api/tauri";

function agentTreeKey(folderPath: string, agentId: string) {
  return `${folderPath}::${agentId}`;
}

function formatSessionTime(timestamp: number) {
  if (!timestamp) {
    return "unknown";
  }
  const date = new Date(timestamp * 1000);
  const month = String(date.getMonth() + 1).padStart(2, "0");
  const day = String(date.getDate()).padStart(2, "0");
  const hour = String(date.getHours()).padStart(2, "0");
  const minute = String(date.getMinutes()).padStart(2, "0");
  return `${month}/${day} ${hour}:${minute}`;
}

function formatHandoverChars(value: number) {
  if (value >= 1000) {
    return `${(value / 1000).toFixed(value >= 10000 ? 0 : 1)}k chars`;
  }
  return `${value} chars`;
}

function sessionStateLabel(status: SessionInfo["status"]) {
  if (status === "running") {
    return "Live";
  }
  if (status === "exited") {
    return "History";
  }
  return "Error";
}

function sessionStateHint(status: SessionInfo["status"]) {
  if (status === "running") {
    return "PTY running";
  }
  if (status === "exited") {
    return "Click to replay";
  }
  return "Needs attention";
}

function sessionDisplayTitle(session: SessionInfo) {
  const title = session.firstUserMessage?.trim() || session.title.trim();
  if (title.length <= 42) {
    return title;
  }
  return `${title.slice(0, 39).trimEnd()}...`;
}

function normalizeWorkspacePath(value: string) {
  const trimmed = value.trim();
  if (!trimmed) {
    return "";
  }
  const normalized = trimmed.replace(/\/\.(?=\/|$)/g, "");
  return normalized || "/";
}

function isLegacyRootWorkspace(folder: WorkspaceFolder) {
  const normalizedPath = normalizeWorkspacePath(folder.path);
  const normalizedName = folder.name.trim();
  const normalizedNameAsPath = normalizeWorkspacePath(normalizedName);
  return normalizedPath === "/" && (normalizedName === "" || normalizedNameAsPath === "/");
}

function isUsableDefaultWorkspace(path: string) {
  const normalizedPath = normalizeWorkspacePath(path);
  return normalizedPath !== "" && normalizedPath !== "/";
}

function normalizeWorkspaceFolders(folders: WorkspaceFolder[]): WorkspaceFolder[] {
  const seen = new Set<string>();
  const normalized: WorkspaceFolder[] = [];
  folders.forEach((folder) => {
    if (isLegacyRootWorkspace(folder)) {
      return;
    }
    const path = normalizeWorkspacePath(folder.path);
    if (!path || seen.has(path)) {
      return;
    }
    seen.add(path);
    normalized.push({
      ...folder,
      path,
      name: folder.name || path.split(/[/\\]/).pop() || path,
    });
  });
  return normalized;
}

function normalizeWorkspaceAgentHistory(
  value: unknown,
): Record<string, { agentId: string; agentName: string }[]> {
  if (!value || typeof value !== "object") {
    return {};
  }
  const result: Record<string, { agentId: string; agentName: string }[]> = {};
  for (const [path, agents] of Object.entries(value as Record<string, unknown>)) {
    const normalizedPath = normalizeWorkspacePath(path);
    if (!normalizedPath || !Array.isArray(agents)) {
      continue;
    }
    const seenAgentIds = new Set<string>();
    const normalizedAgents = agents.flatMap((agent) => {
      if (!agent || typeof agent !== "object") {
        return [];
      }
      const candidate = agent as Partial<{ agentId: string; agentName: string }>;
      const agentId = candidate.agentId?.trim();
      const agentName = candidate.agentName?.trim();
      if (!agentId || !agentName || seenAgentIds.has(agentId)) {
        return [];
      }
      seenAgentIds.add(agentId);
      return [{ agentId, agentName }];
    });
    if (normalizedAgents.length > 0) {
      result[normalizedPath] = normalizedAgents;
    }
  }
  return result;
}

function normalizeWorkspacePathHistory(value: unknown): string[] {
  if (!Array.isArray(value)) {
    return [];
  }

  const seen = new Set<string>();
  const normalized: string[] = [];
  value.forEach((item) => {
    if (typeof item !== "string") {
      return;
    }
    const path = normalizeWorkspacePath(item);
    if (!isUsableDefaultWorkspace(path) || seen.has(path)) {
      return;
    }
    seen.add(path);
    normalized.push(path);
  });

  return normalized.slice(0, NEW_CONVERSATION_WORKSPACE_HISTORY_LIMIT);
}

const NONE_WORKSPACE_STORAGE_KEY = "waypoint_none_workspace_session_ids";
const HIDDEN_WORKSPACE_STORAGE_KEY = "waypoint_hidden_workspace_paths";
const PINNED_ITEMS_STORAGE_KEY = "waypoint_pinned_items";
const NEW_CONVERSATION_WORKSPACE_HISTORY_STORAGE_KEY = "waypoint_new_conversation_workspace_history";
const NONE_WORKSPACE_VALUE = "__none_workspace__";
const CUSTOM_WORKSPACE_VALUE = "__custom_workspace__";
const NEW_CONVERSATION_WORKSPACE_HISTORY_LIMIT = 20;

type PinnedItem = {
  targetType: "session";
  targetId: string;
  createdAt: number;
};

type PinnedEntry = {
  item: PinnedItem;
  session: SessionInfo;
  subtitle: string;
};

function pinnedItemKey(targetType: PinnedItem["targetType"], targetId: string) {
  return `${targetType}:${targetId}`;
}

function parsePinnedItems(value: unknown): PinnedItem[] {
  if (!Array.isArray(value)) {
    return [];
  }

  const seen = new Set<string>();
  return value.flatMap((item) => {
    if (!item || typeof item !== "object") {
      return [];
    }
    const candidate = item as Partial<PinnedItem>;
    if (candidate.targetType !== "session") {
      return [];
    }
    if (typeof candidate.targetId !== "string" || !candidate.targetId.trim()) {
      return [];
    }
    const key = pinnedItemKey(candidate.targetType, candidate.targetId);
    if (seen.has(key)) {
      return [];
    }
    seen.add(key);
    return [
      {
        targetType: candidate.targetType,
        targetId: candidate.targetId,
        createdAt:
          typeof candidate.createdAt === "number"
            ? candidate.createdAt
            : Date.now(),
      },
    ];
  });
}

// ── Open-in-editor button ────────────────────────────────────────────────────

/** Tiny SVG logos for each supported editor */
function EditorIcon({ editorId }: { editorId: string }) {
  if (editorId === "vscode") {
    // VS Code logo (simplified)
    return (
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" aria-hidden="true">
        <path
          d="M17.5 1.5L8.9 10.7 4 6.3 1.5 7.9v8.2l2.5 1.6 4.9-4.4 8.6 9.2L22.5 20V4L17.5 1.5z"
          fill="#007ACC"
        />
        <path
          d="M17.5 1.5v21L22.5 20V4L17.5 1.5zM1.5 16.1L4 17.7l4.9-4.4-4.9-4.4-2.5 1.6v5.6z"
          fill="#1F9CF0"
          opacity="0.8"
        />
      </svg>
    );
  }
  if (editorId === "antigravity") {
    // Antigravity IDE logo (stylised "A" / star shape)
    return (
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" aria-hidden="true">
        <polygon
          points="12,2 15.09,8.26 22,9.27 17,14.14 18.18,21.02 12,17.77 5.82,21.02 7,14.14 2,9.27 8.91,8.26"
          fill="#FF6B3D"
          stroke="#FF6B3D"
          strokeWidth="1"
          strokeLinejoin="round"
        />
      </svg>
    );
  }
  // Generic open-external icon fallback
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6" />
      <polyline points="15 3 21 3 21 9" />
      <line x1="10" y1="14" x2="21" y2="3" />
    </svg>
  );
}

type EditorBtnState = "idle" | "opening" | "done";

function SingleEditorButton({ editor, cwd }: { editor: EditorInfo; cwd: string }) {
  const [btnState, setBtnState] = useState<EditorBtnState>("idle");

  async function handleClick() {
    if (btnState === "opening") return;
    setBtnState("opening");
    try {
      await openInEditor(cwd, editor.bin);
      setBtnState("done");
    } catch {
      setBtnState("done");
    } finally {
      setTimeout(() => setBtnState("idle"), 1800);
    }
  }

  const label = btnState === "opening" ? "打开中…" : btnState === "done" ? "已打开" : editor.name;

  return (
    <button
      className={`icon-action open-in-editor-btn open-in-editor-btn--${btnState}`}
      type="button"
      onClick={handleClick}
      disabled={btnState === "opening"}
      title={`在 ${editor.name} 中打开: ${cwd}`}
    >
      <EditorIcon editorId={editor.id} />
      <span>{label}</span>
    </button>
  );
}

function OpenInEditorButton({ cwd, editors }: { cwd: string; editors: EditorInfo[] }) {
  if (editors.length === 0) return null;

  const [selectedEditorId, setSelectedEditorId] = useState<string>(() => {
    const saved = localStorage.getItem("waypoint_selected_editor_id");
    if (saved && editors.some((e) => e.id === saved)) {
      return saved;
    }
    // Prioritize vscode, otherwise use the first detected editor
    if (editors.some((e) => e.id === "vscode")) {
      return "vscode";
    }
    return editors[0]?.id || "";
  });

  const [isOpen, setIsOpen] = useState(false);
  const [btnState, setBtnState] = useState<EditorBtnState>("idle");

  useEffect(() => {
    if (!editors.some((e) => e.id === selectedEditorId)) {
      if (editors.some((e) => e.id === "vscode")) {
        setSelectedEditorId("vscode");
      } else {
        setSelectedEditorId(editors[0]?.id || "");
      }
    }
  }, [editors, selectedEditorId]);

  // Close dropdown when clicking outside
  useEffect(() => {
    if (!isOpen) return;
    const handleOutsideClick = () => setIsOpen(false);
    window.addEventListener("click", handleOutsideClick);
    return () => window.removeEventListener("click", handleOutsideClick);
  }, [isOpen]);

  const selectedEditor = editors.find((e) => e.id === selectedEditorId) || editors[0];

  async function handleOpenClick() {
    if (btnState === "opening") return;
    setBtnState("opening");
    try {
      await openInEditor(cwd, selectedEditor.bin);
      setBtnState("done");
    } catch {
      setBtnState("done");
    } finally {
      setTimeout(() => setBtnState("idle"), 1800);
    }
  }

  if (editors.length === 1) {
    return <SingleEditorButton editor={editors[0]} cwd={cwd} />;
  }

  const label =
    btnState === "opening"
      ? "打开中…"
      : btnState === "done"
      ? "已打开"
      : `在 ${selectedEditor.name} 中打开`;

  return (
    <div className="custom-editor-dropdown" onClick={(e) => e.stopPropagation()}>
      <button
        className={`icon-action open-in-editor-btn open-in-editor-btn--main open-in-editor-btn--${btnState}`}
        type="button"
        onClick={handleOpenClick}
        disabled={btnState === "opening"}
        title={`在 ${selectedEditor.name} 中打开: ${cwd}`}
      >
        <EditorIcon editorId={selectedEditor.id} />
        <span>{label}</span>
      </button>

      <button
        className="icon-action editor-dropdown-toggle"
        type="button"
        onClick={() => setIsOpen(!isOpen)}
        title="选择其他编辑器"
      >
        <ChevronDown size={14} />
      </button>

      {isOpen && (
        <div className="editor-dropdown-menu">
          {editors.map((editor) => (
            <button
              key={editor.id}
              type="button"
              className={`editor-dropdown-item ${editor.id === selectedEditorId ? "active" : ""}`}
              onClick={() => {
                setSelectedEditorId(editor.id);
                localStorage.setItem("waypoint_selected_editor_id", editor.id);
                setIsOpen(false);
              }}
            >
              <EditorIcon editorId={editor.id} />
              <span>{editor.name}</span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
// ────────────────────────────────────────────────────────────────────────────


function App() {
  const [sessions, setSessions] = useState<SessionInfo[]>([]);
  const [agents, setAgents] = useState<AgentPresetInfo[]>([]);
  const [activeSessionId, setActiveSessionId] = useState<string | null>(null);
  const [activeTerminalReloadKey, setActiveTerminalReloadKey] = useState(0);
  const [selectedAgentId, setSelectedAgentId] = useState("claude-code");
  const [workspacePath, setWorkspacePath] = useState("");
  const [isLaunching, setIsLaunching] = useState(false);
  const [handoverOpen, setHandoverOpen] = useState(false);
  const [handoverMode, setHandoverMode] = useState<"new" | "existing">("new");
  const [handoverTargetId, setHandoverTargetId] = useState("");
  const [continueAgentId, setContinueAgentId] = useState("codex");
  const [continueWorkspacePath, setContinueWorkspacePath] = useState("");
  const [handoverNote, setHandoverNote] = useState("");
  const [handoverContentMode, setHandoverContentMode] = useState<HandoverContentMode>("recommended");
  const [handoverPreview, setHandoverPreview] = useState<HandoverPreview | null>(null);
  const [handoverDraft, setHandoverDraft] = useState<HandoverDraft | null>(null);
  const [handoverResult, setHandoverResult] = useState<HandoverResult | null>(null);
  const [isHandoverPreviewLoading, setIsHandoverPreviewLoading] = useState(false);
  const [isHandoverDraftLoading, setIsHandoverDraftLoading] = useState(false);
  const [handoverDraftError, setHandoverDraftError] = useState<string | null>(null);
  const [isForwarding, setIsForwarding] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [deleteSessionId, setDeleteSessionId] = useState<string | null>(null);
  const [isDeletingSession, setIsDeletingSession] = useState(false);
  const [removeWorkspaceTarget, setRemoveWorkspaceTarget] = useState<WorkspaceFolder | null>(null);
  const [newConversationOpen, setNewConversationOpen] = useState(false);
  const [newConversationWorkspaceValue, setNewConversationWorkspaceValue] =
    useState<string>(NONE_WORKSPACE_VALUE);
  const [newConversationCustomWorkspace, setNewConversationCustomWorkspace] = useState("");
  const [newConversationWorkspaceHistory, setNewConversationWorkspaceHistory] = useState<string[]>([]);
  const [noneWorkspaceSessionIds, setNoneWorkspaceSessionIds] = useState<string[]>([]);
  const [hiddenWorkspacePaths, setHiddenWorkspacePaths] = useState<string[]>([]);
  const [pinnedItems, setPinnedItems] = useState<PinnedItem[]>([]);
  const [isSelectingWorkspaceDirectory, setIsSelectingWorkspaceDirectory] = useState(false);

  // New Workspace state variables
  const [pinnedWorkspaces, setPinnedWorkspaces] = useState<WorkspaceFolder[]>([]);
  const [detectedEditors, setDetectedEditors] = useState<EditorInfo[]>([]);
  const [activeNewMenuFolder, setActiveNewMenuFolder] = useState<string | null>(null);
  const [activeWorkspaceMenuFolder, setActiveWorkspaceMenuFolder] = useState<string | null>(null);
  const [expandedAgents, setExpandedAgents] = useState<Record<string, boolean>>({});
  const [workspaceAgentHistory, setWorkspaceAgentHistory] = useState<
    Record<string, { agentId: string; agentName: string }[]>
  >({});

  const activeSession = useMemo(
    () => sessions.find((session) => session.id === activeSessionId) ?? null,
    [activeSessionId, sessions],
  );
  const newConversationAgent = useMemo(
    () => agents.find((agent) => agent.id === selectedAgentId) ?? null,
    [agents, selectedAgentId],
  );
  const noneWorkspaceSessionIdSet = useMemo(
    () => new Set(noneWorkspaceSessionIds),
    [noneWorkspaceSessionIds],
  );
  const hiddenWorkspacePathSet = useMemo(
    () => new Set(hiddenWorkspacePaths),
    [hiddenWorkspacePaths],
  );
  const noneWorkspaceSessions = useMemo(
    () =>
      sessions
        .filter((session) => noneWorkspaceSessionIdSet.has(session.id))
        .sort((a, b) => b.createdAt - a.createdAt),
    [sessions, noneWorkspaceSessionIdSet],
  );
  const newConversationWorkspacePath = useMemo(() => {
    if (newConversationWorkspaceValue === NONE_WORKSPACE_VALUE) {
      return null;
    }
    if (newConversationWorkspaceValue === CUSTOM_WORKSPACE_VALUE) {
      return newConversationCustomWorkspace.trim();
    }
    return newConversationWorkspaceValue;
  }, [newConversationCustomWorkspace, newConversationWorkspaceValue]);
  const handoverTargets = useMemo(
    () => sessions.filter((session) => session.id !== activeSessionId && session.status === "running"),
    [activeSessionId, sessions],
  );
  const continueAgent = useMemo(
    () => agents.find((agent) => agent.id === continueAgentId) ?? null,
    [agents, continueAgentId],
  );
  const effectiveHandoverMode =
    handoverContentMode === "recommended"
      ? (handoverPreview?.recommendedMode ?? "full")
      : handoverContentMode;
  const shownHandoverPrompt = handoverResult?.prompt ?? handoverDraft?.prompt ?? "";
  const shownHandoverMode = handoverResult?.handoverMode ?? handoverDraft?.effectiveMode ?? effectiveHandoverMode;
  const shownHandoverPath = handoverResult?.handoverPath ?? null;
  const shownEvidencePath = handoverResult?.evidencePath ?? handoverDraft?.evidencePath ?? null;
  const pendingDeleteSession = useMemo(
    () => sessions.find((session) => session.id === deleteSessionId) ?? null,
    [deleteSessionId, sessions],
  );
  const pinnedItemKeySet = useMemo(
    () => new Set(pinnedItems.map((item) => pinnedItemKey(item.targetType, item.targetId))),
    [pinnedItems],
  );
  const workspaceNameByPath = useMemo(() => {
    const names = new Map<string, string>();
    pinnedWorkspaces.forEach((folder) => {
      names.set(folder.path, folder.name);
    });
    sessions.forEach((session) => {
      if (!names.has(session.cwd)) {
        names.set(session.cwd, session.cwd.split(/[/\\]/).pop() || session.cwd);
      }
    });
    return names;
  }, [pinnedWorkspaces, sessions]);

  // Compute workspaces with nested active sessions
  const workspacesWithSessions = useMemo(() => {
    const folderMap: Record<string, { folder: WorkspaceFolder; sessions: SessionInfo[] }> = {};
    
    // 1. Add pinned folders
    pinnedWorkspaces.forEach((w) => {
      folderMap[w.path] = { folder: w, sessions: [] };
    });

    // 2. Map sessions. Keep exited/error sessions visible so fast CLI exits do not look like no-ops.
    const unpinnedFolders: Record<string, { folder: WorkspaceFolder; sessions: SessionInfo[] }> = {};

    sessions.forEach((session) => {
      if (noneWorkspaceSessionIdSet.has(session.id)) {
        return;
      }
      if (folderMap[session.cwd]) {
        folderMap[session.cwd].sessions.push(session);
      } else {
        if (hiddenWorkspacePathSet.has(session.cwd)) {
          return;
        }
        if (!unpinnedFolders[session.cwd]) {
          unpinnedFolders[session.cwd] = {
            folder: {
              path: session.cwd,
              name: session.cwd.split(/[/\\]/).pop() || session.cwd,
              isPinned: false,
            },
            sessions: [],
          };
        }
        unpinnedFolders[session.cwd].sessions.push(session);
      }
    });

    return [
      ...Object.values(folderMap),
      ...Object.values(unpinnedFolders),
    ];
  }, [hiddenWorkspacePathSet, noneWorkspaceSessionIdSet, pinnedWorkspaces, sessions]);
  const pinnedEntries = useMemo<PinnedEntry[]>(() => {
    const sessionById = new Map(sessions.map((session) => [session.id, session]));

    return pinnedItems.flatMap((item): PinnedEntry[] => {
      const session = sessionById.get(item.targetId);
      if (!session) {
        return [];
      }
      const workspaceLabel = noneWorkspaceSessionIdSet.has(session.id)
        ? "无工作区"
        : workspaceNameByPath.get(session.cwd) ?? session.cwd;
      return [{
        item,
        session,
        subtitle: `${workspaceLabel} · ${session.agentName} · ${formatSessionTime(session.createdAt)}`,
      }];
    });
  }, [noneWorkspaceSessionIdSet, pinnedItems, sessions, workspaceNameByPath]);

  async function refreshSessions(nextActiveId?: string) {
    const nextSessions = await listSessions();
    setSessions(nextSessions);
    setNoneWorkspaceSessionIds((current) => {
      const liveIds = new Set(nextSessions.map((session) => session.id));
      const filtered = current.filter((id) => liveIds.has(id));
      if (filtered.length !== current.length) {
        localStorage.setItem(NONE_WORKSPACE_STORAGE_KEY, JSON.stringify(filtered));
      }
      return filtered;
    });
    if (nextActiveId) {
      setActiveSessionId(nextActiveId);
      return;
    }
    if (!activeSessionId && nextSessions.length > 0) {
      setActiveSessionId(nextSessions[0].id);
    }
  }

  async function refreshAgents() {
    const nextAgents = await listAgentPresets();
    setAgents(nextAgents);
    if (!nextAgents.some((agent) => agent.id === selectedAgentId)) {
      setSelectedAgentId(nextAgents[0]?.id ?? "claude-code");
    }
    if (!nextAgents.some((agent) => agent.id === continueAgentId)) {
      setContinueAgentId(nextAgents[0]?.id ?? "claude-code");
    }
  }

  function markSessionAsNoneWorkspace(sessionId: string) {
    setNoneWorkspaceSessionIds((current) => {
      if (current.includes(sessionId)) {
        return current;
      }
      const next = [...current, sessionId];
      localStorage.setItem(NONE_WORKSPACE_STORAGE_KEY, JSON.stringify(next));
      return next;
    });
  }

  function unmarkNoneWorkspaceSession(sessionId: string) {
    setNoneWorkspaceSessionIds((current) => {
      if (!current.includes(sessionId)) {
        return current;
      }
      const next = current.filter((id) => id !== sessionId);
      localStorage.setItem(NONE_WORKSPACE_STORAGE_KEY, JSON.stringify(next));
      return next;
    });
  }

  function updatePinnedItems(updater: (current: PinnedItem[]) => PinnedItem[]) {
    setPinnedItems((current) => {
      const next = updater(current);
      localStorage.setItem(PINNED_ITEMS_STORAGE_KEY, JSON.stringify(next));
      return next;
    });
  }

  function pinSession(targetId: string) {
    updatePinnedItems((current) => {
      const key = pinnedItemKey("session", targetId);
      if (current.some((item) => pinnedItemKey(item.targetType, item.targetId) === key)) {
        return current;
      }
      return [{ targetType: "session", targetId, createdAt: Date.now() }, ...current];
    });
  }

  function unpinSession(targetId: string) {
    updatePinnedItems((current) =>
      current.filter(
        (item) => pinnedItemKey(item.targetType, item.targetId) !== pinnedItemKey("session", targetId),
      ),
    );
  }

  function isSessionPinned(sessionId: string) {
    return pinnedItemKeySet.has(pinnedItemKey("session", sessionId));
  }

  // Handle adding workspace folder
  function handleAddWorkspace(path: string) {
    const normalizedPath = normalizeWorkspacePath(path);
    if (!normalizedPath) return;
    if (pinnedWorkspaces.some((w) => w.path === normalizedPath)) {
      revealWorkspacePath(normalizedPath);
      setError("该目录已存在于工作区中。");
      return;
    }
    const name = normalizedPath.split(/[/\\]/).pop() || normalizedPath;
    const nextFolders = [...pinnedWorkspaces, { path: normalizedPath, name, isPinned: true }];
    setPinnedWorkspaces(nextFolders);
    localStorage.setItem("waypoint_pinned_workspaces", JSON.stringify(nextFolders));
    revealWorkspacePath(normalizedPath);
  }

  async function pickDirectory(onSelected: (path: string) => void) {
    setError(null);
    try {
      const selected = await selectDirectory();
      if (selected) {
        onSelected(selected);
      }
    } catch (err) {
      setError(`选择目录失败：${err instanceof Error ? err.message : String(err)}`);
    }
  }

  async function handleAddWorkspaceFromPicker() {
    if (isSelectingWorkspaceDirectory) {
      return;
    }
    setActiveNewMenuFolder(null);
    setActiveWorkspaceMenuFolder(null);
    setIsSelectingWorkspaceDirectory(true);
    try {
      await pickDirectory(handleAddWorkspace);
    } finally {
      setIsSelectingWorkspaceDirectory(false);
    }
  }

  // Handle removing workspace folder
  function handleRemoveWorkspace(path: string) {
    const nextFolders = pinnedWorkspaces.filter((w) => w.path !== path);
    setPinnedWorkspaces(nextFolders);
    localStorage.setItem("waypoint_pinned_workspaces", JSON.stringify(nextFolders));
    setActiveNewMenuFolder((current) => (current === path ? null : current));
    setActiveWorkspaceMenuFolder((current) => (current === path ? null : current));
    setHiddenWorkspacePaths((current) => {
      if (current.includes(path)) {
        return current;
      }
      const next = [...current, path];
      localStorage.setItem(HIDDEN_WORKSPACE_STORAGE_KEY, JSON.stringify(next));
      return next;
    });
  }

  function requestRemoveWorkspace(folder: WorkspaceFolder) {
    setError(null);
    setActiveNewMenuFolder(null);
    setActiveWorkspaceMenuFolder(null);
    setRemoveWorkspaceTarget(folder);
  }

  function confirmRemoveWorkspace() {
    if (!removeWorkspaceTarget) return;
    handleRemoveWorkspace(removeWorkspaceTarget.path);
    setRemoveWorkspaceTarget(null);
  }

  function handleToggleSessionPin(session: SessionInfo) {
    if (isSessionPinned(session.id)) {
      unpinSession(session.id);
      return;
    }
    pinSession(session.id);
  }

  function revealWorkspacePath(path: string) {
    const normalizedPath = normalizeWorkspacePath(path);
    if (!normalizedPath) {
      return;
    }
    setHiddenWorkspacePaths((current) => {
      if (!current.includes(normalizedPath)) {
        return current;
      }
      const next = current.filter((item) => item !== normalizedPath);
      localStorage.setItem(HIDDEN_WORKSPACE_STORAGE_KEY, JSON.stringify(next));
      return next;
    });
  }

  function rememberNewConversationWorkspace(path: string) {
    const normalizedPath = normalizeWorkspacePath(path);
    if (!isUsableDefaultWorkspace(normalizedPath)) {
      return;
    }
    setNewConversationWorkspaceHistory((current) => {
      const next = [normalizedPath, ...current.filter((item) => item !== normalizedPath)]
        .slice(0, NEW_CONVERSATION_WORKSPACE_HISTORY_LIMIT);
      localStorage.setItem(NEW_CONVERSATION_WORKSPACE_HISTORY_STORAGE_KEY, JSON.stringify(next));
      return next;
    });
  }

  // Update workspace agent history
  function updateWorkspaceAgentHistory(path: string, agentId: string, agentName: string) {
    const normalizedPath = normalizeWorkspacePath(path);
    if (!normalizedPath) {
      return;
    }
    const saved = localStorage.getItem("waypoint_workspace_agent_history");
    let history: Record<string, { agentId: string; agentName: string }[]> = {};
    if (saved) {
      try {
        const parsed = JSON.parse(saved);
        if (parsed && typeof parsed === "object") {
          history = parsed;
        }
      } catch (e) {
        console.error("[Waypoint] Failed to parse workspace agent history:", e);
      }
    }
    
    if (!history[normalizedPath] || !Array.isArray(history[normalizedPath])) {
      history[normalizedPath] = [];
    }
    
    if (!history[normalizedPath].some(a => a.agentId === agentId)) {
      history[normalizedPath].push({ agentId, agentName });
      setWorkspaceAgentHistory(history);
      localStorage.setItem("waypoint_workspace_agent_history", JSON.stringify(history));
    }
  }

  // Remove agent from history for a folder path
  function handleRemoveAgentFromHistory(path: string, agentId: string) {
    const saved = localStorage.getItem("waypoint_workspace_agent_history");
    if (!saved) return;
    try {
      const history = JSON.parse(saved);
      if (history && typeof history === "object" && Array.isArray(history[path])) {
        history[path] = history[path].filter((a: any) => a.agentId !== agentId);
        if (history[path].length === 0) {
          delete history[path];
        }
        setWorkspaceAgentHistory(history);
        localStorage.setItem("waypoint_workspace_agent_history", JSON.stringify(history));
      }
    } catch (e) {
      console.error("[Waypoint] Failed to remove agent from history:", e);
    }
  }

  function toggleAgentGroup(path: string, agentId: string) {
    const key = agentTreeKey(path, agentId);
    setExpandedAgents((current) => ({
      ...current,
      [key]: !(current[key] ?? false),
    }));
  }

  // Create session for a specific agent and path
  async function handleCreateSessionForPath(agentId: string, path: string) {
    setError(null);
    setIsLaunching(true);
    setActiveNewMenuFolder(null);
    try {
      const normalizedPath = normalizeWorkspacePath(path);
      if (!normalizedPath) {
        setError("目录路径无效。");
        return;
      }
      const session = await createAgentSession(agentId, normalizedPath);
      rememberNewConversationWorkspace(session.cwd);
      revealWorkspacePath(session.cwd);
      updateWorkspaceAgentHistory(session.cwd, session.agentId, session.agentName);
      await refreshSessions(session.id);
    } catch (err) {
      setError(String(err));
    } finally {
      setIsLaunching(false);
    }
  }

  function openNewConversationModal(initialWorkspaceValue = NONE_WORKSPACE_VALUE) {
    setError(null);
    setActiveNewMenuFolder(null);
    const firstAvailableAgent =
      agents.find((agent) => agent.available)?.id ?? agents[0]?.id ?? "claude-code";
    const normalizedWorkspaceValue =
      initialWorkspaceValue === NONE_WORKSPACE_VALUE || initialWorkspaceValue === CUSTOM_WORKSPACE_VALUE
        ? initialWorkspaceValue
        : normalizeWorkspacePath(initialWorkspaceValue);
    if (
      normalizedWorkspaceValue !== NONE_WORKSPACE_VALUE &&
      normalizedWorkspaceValue !== CUSTOM_WORKSPACE_VALUE
    ) {
      rememberNewConversationWorkspace(normalizedWorkspaceValue);
    }
    setSelectedAgentId(firstAvailableAgent);
    setNewConversationWorkspaceValue(normalizedWorkspaceValue || NONE_WORKSPACE_VALUE);
    setNewConversationCustomWorkspace("");
    setNewConversationOpen(true);
  }

  async function handleCreateConversation() {
    setError(null);
    if (!selectedAgentId) {
      setError("请先选择 Agent。");
      return;
    }
    const useNoneWorkspace = newConversationWorkspaceValue === NONE_WORKSPACE_VALUE;
    const selectedWorkspace = newConversationWorkspacePath;
    if (!useNoneWorkspace && !selectedWorkspace) {
      setError("请选择工作区目录，或切换到 none。");
      return;
    }
    setIsLaunching(true);
    try {
      let launchPath = selectedWorkspace?.trim() || workspacePath.trim();
      if (useNoneWorkspace && !isUsableDefaultWorkspace(launchPath)) {
        launchPath = (await defaultWorkspace()).trim();
      }
      launchPath = normalizeWorkspacePath(launchPath);
      if (!launchPath) {
        setError("无法解析可用目录，请先选择一个工作区目录。");
        return;
      }
      const session = await createAgentSession(selectedAgentId, launchPath);
      if (useNoneWorkspace) {
        markSessionAsNoneWorkspace(session.id);
      } else {
        rememberNewConversationWorkspace(session.cwd);
        revealWorkspacePath(session.cwd);
        updateWorkspaceAgentHistory(session.cwd, session.agentId, session.agentName);
      }
      setNewConversationOpen(false);
      await refreshSessions(session.id);
    } catch (err) {
      setError(String(err));
    } finally {
      setIsLaunching(false);
    }
  }

  function requestDeleteSession(sessionId: string) {
    setError(null);
    setDeleteSessionId(sessionId);
  }

  async function confirmDeleteSession() {
    if (!deleteSessionId) return;
    setError(null);
    setIsDeletingSession(true);
    try {
      await deleteSession(deleteSessionId);
      unmarkNoneWorkspaceSession(deleteSessionId);
      unpinSession(deleteSessionId);
      const nextSessions = await listSessions();
      setSessions(nextSessions);
      if (activeSessionId === deleteSessionId) {
        setActiveSessionId(nextSessions[0]?.id ?? null);
      }
      setDeleteSessionId(null);
    } catch (err) {
      setError(String(err));
    } finally {
      setIsDeletingSession(false);
    }
  }

  async function handleActivationFailed(sessionId: string, reason: string) {
    const source = sessions.find((session) => session.id === sessionId);
    if (!source) {
      setError(`恢复失败：${reason}`);
      return;
    }

    try {
      const nextSession = await createAgentSession(source.agentId, normalizeWorkspacePath(source.cwd));
      if (noneWorkspaceSessionIdSet.has(source.id)) {
        markSessionAsNoneWorkspace(nextSession.id);
      } else {
        revealWorkspacePath(nextSession.cwd);
        updateWorkspaceAgentHistory(nextSession.cwd, nextSession.agentId, nextSession.agentName);
      }
      await refreshSessions(nextSession.id);
      setError(
        `恢复失败，已打开新的 ${source.agentName} 会话。请在终端中使用该 Agent 的 resume 命令手动恢复历史。`,
      );
    } catch (err) {
      setError(`恢复失败：${reason}；新会话创建失败：${String(err)}`);
    }
  }

  function openHandover() {
    setError(null);
    setHandoverResult(null);
    setHandoverDraft(null);
    setHandoverDraftError(null);
    if (activeSessionId) {
      setIsHandoverPreviewLoading(true);
      setHandoverPreview(null);
      void getHandoverPreview(activeSessionId)
        .then((preview) => setHandoverPreview(preview))
        .catch((err) => setError(String(err)))
        .finally(() => setIsHandoverPreviewLoading(false));
    }
    const firstTarget = handoverTargets[0]?.id ?? "";
    const firstAvailableAgent =
      agents.find((agent) => agent.id !== activeSession?.agentId && agent.available)?.id ??
      agents.find((agent) => agent.available)?.id ??
      "claude-code";
    setHandoverTargetId(firstTarget);
    setContinueAgentId(firstAvailableAgent);
    setContinueWorkspacePath(activeSession?.cwd ?? workspacePath);
    setHandoverMode("new");
    setHandoverContentMode("recommended");
    setHandoverOpen(true);
  }

  function closeHandover() {
    setHandoverOpen(false);
    setHandoverResult(null);
    setHandoverDraft(null);
    setHandoverDraftError(null);
  }

  async function handleContinue() {
    if (!activeSessionId) return;
    if (handoverMode === "existing" && !handoverTargetId) return;
    if (handoverMode === "new" && (!continueAgentId || !continueWorkspacePath.trim())) return;
    setError(null);
    setHandoverResult(null);
    setIsForwarding(true);
    try {
      const result =
        handoverMode === "existing"
          ? await forwardSession(activeSessionId, handoverTargetId, handoverNote, handoverContentMode)
          : await continueSession(
              activeSessionId,
              continueAgentId,
              continueWorkspacePath.trim(),
              handoverNote,
              handoverContentMode,
            );
      updateWorkspaceAgentHistory(
        result.targetSession.cwd,
        result.targetSession.agentId,
        result.targetSession.agentName,
      );
      revealWorkspacePath(result.targetSession.cwd);
      setHandoverResult(result);
      setHandoverNote("");
      await refreshSessions(result.targetSession.id);
    } catch (err) {
      setError(String(err));
    } finally {
      setIsForwarding(false);
    }
  }

  useEffect(() => {
    if (!handoverOpen || !activeSessionId) {
      setHandoverDraft(null);
      setHandoverDraftError(null);
      setIsHandoverDraftLoading(false);
      return;
    }
    if (handoverResult) {
      return;
    }
    if (handoverMode === "existing" && !handoverTargetId) {
      setHandoverDraft(null);
      setHandoverDraftError(null);
      setIsHandoverDraftLoading(false);
      return;
    }
    if (handoverMode === "new" && (!continueAgentId || !continueWorkspacePath.trim())) {
      setHandoverDraft(null);
      setHandoverDraftError(null);
      setIsHandoverDraftLoading(false);
      return;
    }

    let cancelled = false;
    setIsHandoverDraftLoading(true);
    setHandoverDraftError(null);
    const timeout = window.setTimeout(() => {
      void getHandoverDraft({
        sourceSessionId: activeSessionId,
        targetMode: handoverMode,
        targetSessionId: handoverMode === "existing" ? handoverTargetId : null,
        targetAgentId: handoverMode === "new" ? continueAgentId : null,
        cwd: handoverMode === "new" ? continueWorkspacePath.trim() : null,
        note: handoverNote,
        handoverMode: handoverContentMode,
      })
        .then((draft) => {
          if (!cancelled) {
            setHandoverDraft(draft);
          }
        })
        .catch((err) => {
          if (!cancelled) {
            setHandoverDraft(null);
            setHandoverDraftError(String(err));
          }
        })
        .finally(() => {
          if (!cancelled) {
            setIsHandoverDraftLoading(false);
          }
        });
    }, 250);

    return () => {
      cancelled = true;
      window.clearTimeout(timeout);
    };
  }, [
    activeSessionId,
    continueAgentId,
    continueWorkspacePath,
    handoverContentMode,
    handoverMode,
    handoverNote,
    handoverOpen,
    handoverResult,
    handoverTargetId,
  ]);

  function handleSelectSession(session: SessionInfo) {
    if (session.id === activeSessionId) {
      if (session.status !== "running") {
        setActiveTerminalReloadKey((current) => current + 1);
      }
      return;
    }
    setActiveSessionId(session.id);
  }

  function renderSessionItem(session: SessionInfo) {
    const pinned = isSessionPinned(session.id);
    return (
      <div
        className={`workspace-session-item chat-history-item ${session.id === activeSessionId ? "active" : ""} ${
          pinned ? "pinned" : ""
        }`}
        key={`session-${session.id}`}
        onClick={() => handleSelectSession(session)}
        title={session.firstUserMessage ?? session.title}
      >
        <div className="session-info-left">
          <span className={`status-dot ${session.status}`} />
          <span className="session-copy">
            <span className="session-label">{sessionDisplayTitle(session)}</span>
            <span className="session-subtitle">
              {formatSessionTime(session.createdAt)} · {sessionStateHint(session.status)}
            </span>
          </span>
        </div>
        <div className="session-actions">
          <span className={`session-state ${session.status}`}>{sessionStateLabel(session.status)}</span>
          <button
            className={`session-pin-btn ${pinned ? "active" : ""}`}
            onClick={(e) => {
              e.stopPropagation();
              handleToggleSessionPin(session);
            }}
            title={pinned ? "取消置顶会话" : "置顶会话"}
            aria-label={pinned ? "取消置顶会话" : "置顶会话"}
          >
            <Pin size={10} fill={pinned ? "currentColor" : "none"} />
          </button>
          {session.status === "running" ? (
            <button
              className="session-kill-btn"
              onClick={async (e) => {
                e.stopPropagation();
                setError(null);
                try {
                  await killSession(session.id);
                  await refreshSessions();
                } catch (err) {
                  setError(String(err));
                }
              }}
              title="强杀当前会话"
            >
              <Square size={8} fill="currentColor" />
            </button>
          ) : null}
          <button
            className="session-delete-btn"
            onClick={(e) => {
              e.stopPropagation();
              requestDeleteSession(session.id);
            }}
            title="删除本地历史记录"
          >
            <Trash2 size={10} />
          </button>
        </div>
      </div>
    );
  }

  function renderPinnedEntry(entry: PinnedEntry) {
    const pinned = isSessionPinned(entry.session.id);
    return (
      <div
        className={`pinned-item session-pinned-item ${entry.session.id === activeSessionId ? "active" : ""}`}
        key={pinnedItemKey(entry.item.targetType, entry.item.targetId)}
        title={entry.session.firstUserMessage ?? entry.session.title}
      >
        <button
          type="button"
          className="pinned-main-action pinned-session-action"
          onClick={() => handleSelectSession(entry.session)}
        >
          <span className="pinned-item-icon session">
            <MessageSquare size={14} />
          </span>
          <span className="pinned-item-copy">
            <span className="pinned-item-title">{sessionDisplayTitle(entry.session)}</span>
            <span className="pinned-item-subtitle">{entry.subtitle}</span>
          </span>
          <span className={`status-dot ${entry.session.status}`} />
        </button>
        <button
          type="button"
          className={`pinned-unpin-btn ${pinned ? "active" : ""}`}
          title="取消置顶会话"
          aria-label="取消置顶会话"
          onClick={(event) => {
            event.stopPropagation();
            unpinSession(entry.session.id);
          }}
          onKeyDown={(event) => {
            if (event.key === "Enter" || event.key === " ") {
              event.preventDefault();
              event.stopPropagation();
              unpinSession(entry.session.id);
            }
          }}
        >
          <Pin size={11} fill="currentColor" />
        </button>
      </div>
    );
  }

  useEffect(() => {
    let unlistenExited: UnlistenFn | null = null;
    let unlistenError: UnlistenFn | null = null;
    let unlistenCreated: UnlistenFn | null = null;

    detectEditors().then(setDetectedEditors).catch(console.error);

    Promise.all([refreshSessions(), refreshAgents(), defaultWorkspace()])
      .then(async ([, , cwd]) => {
        const normalizedDefaultWorkspace = normalizeWorkspacePath(cwd);
        setWorkspacePath(
          isUsableDefaultWorkspace(normalizedDefaultWorkspace) ? normalizedDefaultWorkspace : "",
        );
        // Load pinned workspaces
        let loadedPinnedWorkspaces: WorkspaceFolder[] = [];
        const saved = localStorage.getItem("waypoint_pinned_workspaces");
        if (saved) {
          try {
            const parsed = JSON.parse(saved);
            if (Array.isArray(parsed)) {
              loadedPinnedWorkspaces = normalizeWorkspaceFolders(
                parsed.filter(
                  (item): item is WorkspaceFolder =>
                    item &&
                    typeof item === "object" &&
                    typeof item.path === "string" &&
                    typeof item.name === "string",
                ),
              );
              setPinnedWorkspaces(loadedPinnedWorkspaces);
              localStorage.setItem("waypoint_pinned_workspaces", JSON.stringify(loadedPinnedWorkspaces));
            }
          } catch (e) {
            console.error("[Waypoint] Failed to parse pinned workspaces:", e);
          }
        } else if (isUsableDefaultWorkspace(normalizedDefaultWorkspace)) {
          const defaultFolder: WorkspaceFolder = {
            path: normalizedDefaultWorkspace,
            name: normalizedDefaultWorkspace.split(/[/\\]/).pop() || normalizedDefaultWorkspace,
            isPinned: true,
          };
          loadedPinnedWorkspaces = [defaultFolder];
          setPinnedWorkspaces(loadedPinnedWorkspaces);
          localStorage.setItem("waypoint_pinned_workspaces", JSON.stringify(loadedPinnedWorkspaces));
        }

        const savedPinnedItems = localStorage.getItem(PINNED_ITEMS_STORAGE_KEY);
        if (savedPinnedItems) {
          try {
            const parsed = parsePinnedItems(JSON.parse(savedPinnedItems));
            setPinnedItems(parsed);
            localStorage.setItem(PINNED_ITEMS_STORAGE_KEY, JSON.stringify(parsed));
          } catch (e) {
            console.error("[Waypoint] Failed to parse pinned items:", e);
          }
        }

        const savedWorkspaceHistory = localStorage.getItem(NEW_CONVERSATION_WORKSPACE_HISTORY_STORAGE_KEY);
        if (savedWorkspaceHistory) {
          try {
            const parsed = JSON.parse(savedWorkspaceHistory);
            const normalized = normalizeWorkspacePathHistory(parsed);
            setNewConversationWorkspaceHistory(normalized);
            localStorage.setItem(NEW_CONVERSATION_WORKSPACE_HISTORY_STORAGE_KEY, JSON.stringify(normalized));
          } catch (e) {
            console.error("[Waypoint] Failed to parse new conversation workspace history:", e);
          }
        } else {
          const seeded = normalizeWorkspacePathHistory(loadedPinnedWorkspaces.map((folder) => folder.path));
          setNewConversationWorkspaceHistory(seeded);
          if (seeded.length > 0) {
            localStorage.setItem(NEW_CONVERSATION_WORKSPACE_HISTORY_STORAGE_KEY, JSON.stringify(seeded));
          }
        }

        const savedHiddenWorkspaces = localStorage.getItem(HIDDEN_WORKSPACE_STORAGE_KEY);
        if (savedHiddenWorkspaces) {
          try {
            const parsed = JSON.parse(savedHiddenWorkspaces);
            if (Array.isArray(parsed)) {
              const normalized = Array.from(
                new Set(
                  parsed
                    .filter((item): item is string => typeof item === "string")
                    .map((item) => normalizeWorkspacePath(item))
                    .filter(Boolean),
                ),
              );
              setHiddenWorkspacePaths(normalized);
              localStorage.setItem(HIDDEN_WORKSPACE_STORAGE_KEY, JSON.stringify(normalized));
            }
          } catch (e) {
            console.error("[Waypoint] Failed to parse hidden workspaces:", e);
          }
        }

        // Load workspace agent history
        const savedHistory = localStorage.getItem("waypoint_workspace_agent_history");
        if (savedHistory) {
          try {
            const parsed = JSON.parse(savedHistory);
            const normalized = normalizeWorkspaceAgentHistory(parsed);
            setWorkspaceAgentHistory(normalized);
            localStorage.setItem("waypoint_workspace_agent_history", JSON.stringify(normalized));
          } catch (e) {
            console.error("[Waypoint] Failed to parse workspace agent history:", e);
          }
        }

        const savedNoneWorkspace = localStorage.getItem(NONE_WORKSPACE_STORAGE_KEY);
        if (savedNoneWorkspace) {
          try {
            const parsed = JSON.parse(savedNoneWorkspace);
            if (Array.isArray(parsed)) {
              const rawIds = parsed.filter((item): item is string => typeof item === "string");
              const liveIds = new Set((await listSessions()).map((session) => session.id));
              const filteredIds = rawIds.filter((id) => liveIds.has(id));
              setNoneWorkspaceSessionIds(filteredIds);
              localStorage.setItem(NONE_WORKSPACE_STORAGE_KEY, JSON.stringify(filteredIds));
            }
          } catch (e) {
            console.error("[Waypoint] Failed to parse none-workspace session ids:", e);
          }
        }

        // Listen to session events from Tauri
        unlistenExited = await listen<{ session: SessionInfo }>("session:exited", (event) => {
          setSessions((current) =>
            current.map((item) => (item.id === event.payload.session.id ? event.payload.session : item))
          );
        });

        unlistenError = await listen<{ sessionId: string; message: string }>("session:error", (event) => {
          setSessions((current) =>
            current.map((item) =>
              item.id === event.payload.sessionId ? { ...item, status: "error" } : item
            )
          );
        });

        unlistenCreated = await listen<{ session: SessionInfo }>("session:created", (event) => {
          setSessions((current) => {
            const exists = current.some((item) => item.id === event.payload.session.id);
            if (!exists) {
              return [...current, event.payload.session];
            }
            return current.map((item) => (item.id === event.payload.session.id ? event.payload.session : item));
          });
        });
      })
      .catch((err) => setError(String(err)));

    return () => {
      unlistenExited?.();
      unlistenError?.();
      unlistenCreated?.();
    };
  }, []);

  return (
    <main className="app-shell">
      {activeNewMenuFolder && (
        <div
          className="popover-backdrop"
          onClick={() => {
            setActiveNewMenuFolder(null);
            setActiveWorkspaceMenuFolder(null);
          }}
        />
      )}
      {!activeNewMenuFolder && activeWorkspaceMenuFolder && (
        <div className="popover-backdrop" onClick={() => setActiveWorkspaceMenuFolder(null)} />
      )}

      <aside className="sidebar">
        <div className="brand">
          <span className="brand-mark" aria-hidden="true">
            <WptLogo size={38} />
          </span>
          <div>
            <h1>Waypoint</h1>
            <span>AI AGENT</span>
          </div>
        </div>

        <button className="new-conversation-trigger" type="button" onClick={() => openNewConversationModal()}>
          <Plus size={14} />
          <span>新对话</span>
        </button>

        {pinnedEntries.length > 0 ? (
          <section className="pinned-list" aria-label="Pinned items">
            <div className="section-header pinned-section-header">
              <h3>置顶</h3>
            </div>
            <div className="pinned-items">
              {pinnedEntries.map((entry) => renderPinnedEntry(entry))}
            </div>
          </section>
        ) : null}

        <section className="workspace-list" aria-label="Workspaces">
          <div className="section-header">
            <h3>工作区目录</h3>
            <button
              type="button"
              className="add-workspace-btn"
              onClick={handleAddWorkspaceFromPicker}
              disabled={isSelectingWorkspaceDirectory}
              aria-label="选择并固定新工作区目录"
              title={isSelectingWorkspaceDirectory ? "正在打开目录选择器" : "固定新工作区目录"}
            >
              <FolderPlus size={15} />
            </button>
          </div>

          <div className="workspace-tree">
            <div className="workspace-folder-node none-workspace-node">
              <div className="workspace-folder-header">
                <div className="workspace-folder-info" title="none">
                  <Folder size={14} className="folder-icon" />
                  <span className="folder-name">无工作区会话</span>
                </div>
                <div className="workspace-folder-actions">
                  <button
                    type="button"
                    className="new-session-btn"
                    onClick={(event) => {
                      event.stopPropagation();
                      setActiveNewMenuFolder(null);
                      setActiveWorkspaceMenuFolder(null);
                      openNewConversationModal(NONE_WORKSPACE_VALUE);
                    }}
                    title="新建无工作区会话"
                  >
                    <Plus size={12} />
                    <span>New</span>
                  </button>
                </div>
              </div>
              <div className="workspace-sessions-list">
                {noneWorkspaceSessions.length > 0 ? (
                  noneWorkspaceSessions.map((session) => renderSessionItem(session))
                ) : (
                  <div className="no-sessions">暂无会话</div>
                )}
              </div>
            </div>

            {workspacesWithSessions.map(({ folder, sessions: folderSessions }) => (
              <div className="workspace-folder-node" key={folder.path}>
                <div className="workspace-folder-header">
                  <div className="workspace-folder-info" title={folder.path}>
                    <Folder size={14} className="folder-icon" />
                    <span className="folder-name">{folder.name}</span>
                  </div>
                  <div className="workspace-folder-actions">
                    <div className="popover-wrapper new-session-popover-wrapper">
                      <button
                        type="button"
                        className="new-session-btn"
                        onClick={(e) => {
                          e.stopPropagation();
                          setActiveWorkspaceMenuFolder(null);
                          setActiveNewMenuFolder(
                            activeNewMenuFolder === folder.path ? null : folder.path
                          );
                        }}
                        title="在此目录新建会话"
                      >
                        <Plus size={12} />
                        <span>New</span>
                      </button>

                      {activeNewMenuFolder === folder.path && (
                        <div className="agent-popover" onClick={(e) => e.stopPropagation()}>
                          <div className="popover-header">启动 Agent 会话</div>
                          <div className="agent-options">
                            {agents
                              .filter((a) => a.available)
                              .map((agent) => (
                                <button
                                  type="button"
                                  key={agent.id}
                                  className="agent-option-item"
                                  onClick={() => handleCreateSessionForPath(agent.id, folder.path)}
                                >
                                  <Bot size={13} />
                                  <div className="agent-option-text">
                                    <span className="agent-option-name">{agent.name}</span>
                                    <span className="agent-option-desc">{agent.description}</span>
                                  </div>
                                </button>
                              ))}
                          </div>
                        </div>
                      )}
                    </div>

                    <div className="popover-wrapper">
                      <button
                        type="button"
                        className="workspace-more-btn"
                        onClick={(e) => {
                          e.stopPropagation();
                          setActiveNewMenuFolder(null);
                          setActiveWorkspaceMenuFolder(
                            activeWorkspaceMenuFolder === folder.path ? null : folder.path
                          );
                        }}
                        aria-label={`${folder.name} 更多操作`}
                        title="更多项目操作"
                      >
                        <MoreHorizontal size={15} />
                      </button>

                      {activeWorkspaceMenuFolder === folder.path && (
                        <div className="workspace-action-popover" onClick={(e) => e.stopPropagation()}>
                          <button
                            type="button"
                            className="workspace-action-item"
                            onClick={() => requestRemoveWorkspace(folder)}
                          >
                            <Trash2 size={13} />
                            <span>移除</span>
                          </button>
                        </div>
                      )}
                    </div>
                  </div>
                </div>

                <div className="workspace-sessions-list">
                  {(() => {
                    const history = workspaceAgentHistory[folder.path] || [];
                    const agentGroups = new Map<
                      string,
                      { agentId: string; agentName: string; sessions: SessionInfo[]; rememberedOnly: boolean }
                    >();

                    folderSessions.forEach((session) => {
                      if (!agentGroups.has(session.agentId)) {
                        agentGroups.set(session.agentId, {
                          agentId: session.agentId,
                          agentName: session.agentName,
                          sessions: [],
                          rememberedOnly: false,
                        });
                      }
                      agentGroups.get(session.agentId)?.sessions.push(session);
                    });

                    history.forEach((histAgent) => {
                      if (!agentGroups.has(histAgent.agentId)) {
                        agentGroups.set(histAgent.agentId, {
                          agentId: histAgent.agentId,
                          agentName: histAgent.agentName,
                          sessions: [],
                          rememberedOnly: true,
                        });
                      }
                    });

                    const groups = Array.from(agentGroups.values()).map((group) => ({
                      ...group,
                      sessions: [...group.sessions].sort((a, b) => a.createdAt - b.createdAt),
                    }));

                    groups.sort((a, b) => {
                      const aOldest = a.sessions[0]?.createdAt ?? 0;
                      const bOldest = b.sessions[0]?.createdAt ?? 0;
                      if (aOldest !== bOldest) {
                        return aOldest - bOldest;
                      }
                      return a.agentName.localeCompare(b.agentName);
                    });

                    if (groups.length === 0) {
                      return <div className="no-sessions">无活跃会话或历史记录</div>;
                    }

                    return groups.map((group) => {
                      const groupKey = agentTreeKey(folder.path, group.agentId);
                      const activeInGroup = group.sessions.some((session) => session.id === activeSessionId);
                      const expanded = expandedAgents[groupKey] ?? activeInGroup;
                      const runningCount = group.sessions.filter((session) => session.status === "running").length;
                      const agentAvailable = agents.some(
                        (agent) => agent.id === group.agentId && agent.available,
                      );

                      return (
                        <div className="agent-history-group" key={groupKey}>
                          <div
                            role="button"
                            tabIndex={0}
                            className={`agent-history-header ${activeInGroup ? "active" : ""}`}
                            onClick={() => toggleAgentGroup(folder.path, group.agentId)}
                            onKeyDown={(event) => {
                              if (event.key === "Enter" || event.key === " ") {
                                event.preventDefault();
                                toggleAgentGroup(folder.path, group.agentId);
                              }
                            }}
                          >
                            {expanded ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
                            <Bot size={13} />
                            <span className="agent-history-name">{group.agentName}</span>
                            <button
                              type="button"
                              className="agent-history-new-btn"
                              disabled={isLaunching || !agentAvailable}
                              onClick={(event) => {
                                event.stopPropagation();
                                handleCreateSessionForPath(group.agentId, folder.path);
                              }}
                              onKeyDown={(event) => event.stopPropagation()}
                              title={
                                agentAvailable
                                  ? `在当前目录新建 ${group.agentName} 会话`
                                  : `${group.agentName} 当前不可用`
                              }
                            >
                              <Plus size={12} />
                            </button>
                            <span className="agent-history-badges">
                              <span className="agent-history-count">
                                {group.sessions.length > 0 ? `${group.sessions.length}` : "0"}
                              </span>
                              {runningCount > 0 ? (
                                <span className="agent-history-live">{runningCount} live</span>
                              ) : null}
                            </span>
                          </div>

                          {expanded ? (
                            <div className="agent-history-children">
                              {group.sessions.length === 0 ? (
                                <div className="agent-empty-history">
                                  <span>暂无历史会话</span>
                                  {group.rememberedOnly ? (
                                    <button
                                      type="button"
                                      className="session-remove-history-btn"
                                      onClick={() => handleRemoveAgentFromHistory(folder.path, group.agentId)}
                                      title="删除该 Agent 历史记录"
                                    >
                                      <X size={10} />
                                    </button>
                                  ) : null}
                                </div>
                              ) : (
                                group.sessions.map((session) => renderSessionItem(session))
                              )}
                            </div>
                          ) : null}
                        </div>
                      );
                    });
                  })()}
                </div>
              </div>
            ))}
          </div>
        </section>

        <div className="agent-status-panel">
          <div className="panel-title">
            <span>本地 Agent 环境</span>
            <button
              className="refresh-btn"
              onClick={() => refreshAgents().catch((err) => setError(String(err)))}
              title="重新检测本地 Agent"
            >
              <RefreshCw size={11} />
            </button>
          </div>
          <div className="agent-status-grid">
            {agents.map((agent) => (
              <div key={agent.id} className="agent-status-tag" title={agent.resolvedCommand ?? agent.command}>
                <span className={`status-dot ${agent.available ? "running" : "error"}`} />
                <span className="agent-tag-name">{agent.name}</span>
              </div>
            ))}
          </div>
        </div>
      </aside>

      <section className="workspace">
        <header className="topbar">
          <div>
            <p className="eyebrow">Active Session</p>
            <h2>{activeSession?.title ?? "No session"}</h2>
            {activeSession ? (
              <p className="session-path">{activeSession.command} · {activeSession.cwd}</p>
            ) : null}
          </div>
          <div className="topbar-actions">
            <button
              className="icon-action"
              type="button"
              onClick={openHandover}
              disabled={!activeSessionId}
              title="Continue from session"
            >
              <Send aria-hidden="true" size={15} />
              <span>Handover</span>
            </button>
            {activeSession?.cwd && (
              <OpenInEditorButton cwd={activeSession.cwd} editors={detectedEditors} />
            )}
          </div>
        </header>

        {error ? <div className="error-banner">{error}</div> : null}

        <div className="output-layout">
          <div className="terminal-frame">
            {activeSessionId ? (
              <TerminalView
                key={`${activeSessionId}:${activeTerminalReloadKey}`}
                sessionId={activeSessionId}
                onSessionActivated={(session) => {
                  setSessions((current) => {
                    const exists = current.some((item) => item.id === session.id);
                    if (!exists) {
                      return [...current, session];
                    }
                    return current.map((item) => (item.id === session.id ? session : item));
                  });
                }}
                onActivationFailed={handleActivationFailed}
              />
            ) : (
              <div className="empty-state">
                <div className="empty-state-inner">
                  <WptLogo size={48} />
                  <p className="empty-state-title">No active session</p>
                  <p className="empty-state-copy">Select a workspace directory and spin up an agent to mount the terminal console.</p>
                </div>
              </div>
            )}
          </div>
        </div>
      </section>

      {newConversationOpen ? (
        <div className="modal-backdrop" role="presentation">
          <section className="modal conversation-modal" role="dialog" aria-modal="true" aria-labelledby="new-chat-title">
            <header className="modal-header">
              <div>
                <p className="eyebrow">New Conversation</p>
                <h3 id="new-chat-title">新对话</h3>
              </div>
              <button
                className="icon-only"
                type="button"
                onClick={() => setNewConversationOpen(false)}
                title="Close"
              >
                <X aria-hidden="true" size={16} />
              </button>
            </header>

            <div className="modal-body">
              <div className="field">
                <label htmlFor="new-conversation-agent">
                  <Bot aria-hidden="true" size={14} />
                  <span>Agent</span>
                </label>
                <select
                  id="new-conversation-agent"
                  value={selectedAgentId}
                  onChange={(event) => setSelectedAgentId(event.target.value)}
                >
                  {agents.map((agent) => (
                    <option key={agent.id} value={agent.id} disabled={!agent.available}>
                      {agent.name}
                      {agent.available ? "" : " (missing)"}
                    </option>
                  ))}
                </select>
              </div>

              <div className="agent-status">
                <span className={`status-dot ${newConversationAgent?.available ? "running" : "error"}`} />
                <span>{newConversationAgent?.resolvedCommand ?? newConversationAgent?.command ?? "Detecting..."}</span>
              </div>

              <div className="field">
                <label htmlFor="new-conversation-workspace">
                  <Folder aria-hidden="true" size={14} />
                  <span>工作区</span>
                </label>
                <select
                  id="new-conversation-workspace"
                  value={newConversationWorkspaceValue}
                  onChange={(event) => {
                    const val = event.target.value;
                    setNewConversationWorkspaceValue(val);
                    if (val === CUSTOM_WORKSPACE_VALUE) {
                      pickDirectory(setNewConversationCustomWorkspace);
                    }
                  }}
                >
                  <option value={NONE_WORKSPACE_VALUE}>None（不绑定工作区）</option>
                  {newConversationWorkspaceHistory.map((path) => (
                    <option key={path} value={path}>
                      历史目录 · {path}
                    </option>
                  ))}
                  <option value={CUSTOM_WORKSPACE_VALUE}>选择其他工作区目录...</option>
                </select>
              </div>

              {newConversationWorkspaceValue === CUSTOM_WORKSPACE_VALUE ? (
                <div className="field">
                  <label htmlFor="new-conversation-custom-workspace">
                    <FolderOpen aria-hidden="true" size={14} />
                    <span>目录路径</span>
                  </label>
                  <div className="input-group-with-btn">
                    <input
                      id="new-conversation-custom-workspace"
                      value={newConversationCustomWorkspace}
                      onChange={(event) => setNewConversationCustomWorkspace(event.target.value)}
                      placeholder="/path/to/project"
                      spellCheck={false}
                    />
                    <button
                      type="button"
                      className="browse-dir-btn"
                      onClick={() => pickDirectory(setNewConversationCustomWorkspace)}
                      title="浏览选择文件夹"
                    >
                      <FolderOpen size={14} />
                    </button>
                  </div>
                </div>
              ) : null}

              {newConversationWorkspaceValue === NONE_WORKSPACE_VALUE ? (
                <div className="workspace-none-hint">
                  该会话将归类到「无工作区会话」，启动目录由系统自动选择。
                </div>
              ) : null}
            </div>

            <footer className="modal-footer">
              <button className="icon-action" type="button" onClick={() => setNewConversationOpen(false)}>
                Cancel
              </button>
              <button
                className="primary-action"
                type="button"
                onClick={handleCreateConversation}
                disabled={
                  isLaunching ||
                  !newConversationAgent?.available ||
                  (newConversationWorkspaceValue !== NONE_WORKSPACE_VALUE &&
                    !newConversationWorkspacePath)
                }
              >
                <Plus aria-hidden="true" size={15} />
                <span>{isLaunching ? "Creating" : "Create Conversation"}</span>
              </button>
            </footer>
          </section>
        </div>
      ) : null}

      {handoverOpen ? (
        <div className="modal-backdrop" role="presentation">
          <section className="modal handover-modal" role="dialog" aria-modal="true" aria-labelledby="handover-title">
            <header className="modal-header">
              <div>
                <p className="eyebrow">Handover</p>
                <h3 id="handover-title">Continue from current session</h3>
              </div>
              <button className="icon-only" type="button" onClick={closeHandover} title="Close">
                <X aria-hidden="true" size={16} />
              </button>
            </header>

            <div className="modal-body handover-modal-body">
              <div className="handover-layout">
                <div className="handover-controls">
                  <div className="handover-source">
                    <span>From</span>
                    <strong>{activeSession?.title}</strong>
                    <small>{activeSession?.agentName} · {activeSession?.cwd}</small>
                  </div>

              <div className="mode-toggle" role="group" aria-label="Handover mode">
                <button
                  className={handoverMode === "new" ? "active" : ""}
                  type="button"
                  onClick={() => {
                    setHandoverResult(null);
                    setHandoverMode("new");
                  }}
                >
                  New Session
                </button>
                <button
                  className={handoverMode === "existing" ? "active" : ""}
                  type="button"
                  onClick={() => {
                    setHandoverResult(null);
                    setHandoverMode("existing");
                  }}
                  disabled={handoverTargets.length === 0}
                >
                  Existing Session
                </button>
              </div>

              <div className="handover-context-panel">
                <div className="handover-context-heading">
                  <div>
                    <span>Context package</span>
                    <strong>
                      {isHandoverPreviewLoading
                        ? "Estimating..."
                        : handoverPreview
                          ? `${formatHandoverChars(handoverPreview.estimatedChars)} estimated`
                          : "Estimate unavailable"}
                    </strong>
                  </div>
                  {handoverPreview ? (
                    <small className={handoverPreview.isLarge ? "warning" : ""}>
                      {handoverPreview.isLarge ? "Large handover" : "Normal size"}
                    </small>
                  ) : null}
                </div>

                {handoverPreview ? (
                  <div className="handover-size-grid" aria-label="Handover size details">
                    <span>Terminal {formatHandoverChars(handoverPreview.terminalContextChars)}</span>
                    <span>Inputs {formatHandoverChars(handoverPreview.userInputChars)}</span>
                    <span>
                      Diffs {formatHandoverChars(handoverPreview.unstagedDiffChars + handoverPreview.stagedDiffChars)}
                    </span>
                  </div>
                ) : null}

                <div className="handover-content-options" role="radiogroup" aria-label="Context package mode">
                  <label className={handoverContentMode === "recommended" ? "active" : ""}>
                    <input
                      type="radio"
                      name="handover-content-mode"
                      value="recommended"
                      checked={handoverContentMode === "recommended"}
                      onChange={() => {
                        setHandoverResult(null);
                        setHandoverContentMode("recommended");
                      }}
                    />
                    <span>
                      <strong>Recommended</strong>
                      <small>
                        {effectiveHandoverMode === "compact"
                          ? "Compact first, full evidence saved"
                          : "Full structured handover"}
                      </small>
                    </span>
                  </label>
                  <label className={handoverContentMode === "compact" ? "active" : ""}>
                    <input
                      type="radio"
                      name="handover-content-mode"
                      value="compact"
                      checked={handoverContentMode === "compact"}
                      onChange={() => {
                        setHandoverResult(null);
                        setHandoverContentMode("compact");
                      }}
                    />
                    <span>
                      <strong>Compact + evidence</strong>
                      <small>Main file stays concise; full evidence is linked</small>
                    </span>
                  </label>
                  <label className={handoverContentMode === "full" ? "active" : ""}>
                    <input
                      type="radio"
                      name="handover-content-mode"
                      value="full"
                      checked={handoverContentMode === "full"}
                      onChange={() => {
                        setHandoverResult(null);
                        setHandoverContentMode("full");
                      }}
                    />
                    <span>
                      <strong>Full context</strong>
                      <small>Larger startup context, fewer automatic omissions</small>
                    </span>
                  </label>
                </div>
              </div>

              {handoverMode === "new" ? (
                <>
                  <div className="field">
                    <label htmlFor="continue-agent">
                      <Bot aria-hidden="true" size={14} />
                      <span>Target agent</span>
                    </label>
                    <select
                      id="continue-agent"
                      value={continueAgentId}
                      onChange={(event) => {
                        setHandoverResult(null);
                        setContinueAgentId(event.target.value);
                      }}
                    >
                      {agents.map((agent) => (
                        <option key={agent.id} value={agent.id} disabled={!agent.available}>
                          {agent.name}
                          {agent.available ? "" : " (missing)"}
                        </option>
                      ))}
                    </select>
                  </div>

                  <div className="agent-status">
                    <span className={`status-dot ${continueAgent?.available ? "running" : "error"}`} />
                    <span>{continueAgent?.resolvedCommand ?? continueAgent?.command ?? "Detecting..."}</span>
                  </div>

                  <div className="field">
                    <label htmlFor="continue-workspace">
                      <Folder aria-hidden="true" size={14} />
                      <span>Workspace</span>
                    </label>
                    <div className="input-group-with-btn">
                      <input
                        id="continue-workspace"
                        value={continueWorkspacePath}
                        onChange={(event) => {
                          setHandoverResult(null);
                          setContinueWorkspacePath(event.target.value);
                        }}
                        placeholder="/path/to/project"
                        spellCheck={false}
                      />
                      <button
                        type="button"
                        className="browse-dir-btn"
                        onClick={() => {
                          setHandoverResult(null);
                          pickDirectory(setContinueWorkspacePath);
                        }}
                        title="浏览选择文件夹"
                      >
                        <FolderOpen size={14} />
                      </button>
                    </div>
                  </div>
                </>
              ) : (
                <div className="field">
                  <label htmlFor="handover-target">
                    <Send aria-hidden="true" size={14} />
                    <span>Target session</span>
                  </label>
                  <select
                    id="handover-target"
                    value={handoverTargetId}
                    onChange={(event) => {
                      setHandoverResult(null);
                      setHandoverTargetId(event.target.value);
                    }}
                  >
                    {handoverTargets.map((session) => (
                      <option key={session.id} value={session.id}>
                        {session.title} · {session.agentName}
                      </option>
                    ))}
                  </select>
                </div>
              )}

              <div className="field">
                <label htmlFor="handover-note">Note</label>
                <textarea
                  id="handover-note"
                  value={handoverNote}
                  onChange={(event) => {
                    setHandoverResult(null);
                    setHandoverNote(event.target.value);
                  }}
                  placeholder="Optional: tell the target agent what to focus on next."
                  rows={6}
                />
              </div>

                </div>

                <aside className="handover-preview-pane">
                  <div className="handover-result-panel">
                    <div className="handover-result-heading">
                      <div>
                        <span>{handoverResult ? "Generated handover" : "Handover preview"}</span>
                        <strong>
                          {isHandoverDraftLoading && !handoverResult
                            ? "Rendering..."
                            : shownHandoverPrompt
                              ? formatHandoverChars(shownHandoverPrompt.length)
                              : "No preview"}
                        </strong>
                      </div>
                      <small>{shownHandoverMode}</small>
                    </div>
                    {shownHandoverPath ? <code className="handover-path">{shownHandoverPath}</code> : null}
                    {handoverDraftError ? <div className="handover-preview-error">{handoverDraftError}</div> : null}
                    <pre className="handover-raw-markdown">{shownHandoverPrompt || "Select a valid target to render the raw handover Markdown."}</pre>
                    {shownEvidencePath ? (
                      <small className="handover-evidence-path">Full evidence: {shownEvidencePath}</small>
                    ) : null}
                  </div>
                </aside>
              </div>
            </div>

            <footer className="modal-footer">
              <button className="icon-action" type="button" onClick={closeHandover}>
                {handoverResult ? "Done" : "Cancel"}
              </button>
              <button
                className="primary-action"
                type="button"
                onClick={handleContinue}
                disabled={
                  isForwarding ||
                  (handoverMode === "existing" && !handoverTargetId) ||
                  (handoverMode === "new" && (!continueAgent?.available || !continueWorkspacePath.trim()))
                }
              >
                <Send aria-hidden="true" size={15} />
                <span>
                  {isForwarding
                    ? "Continuing"
                    : handoverMode === "new"
                      ? "Create & Continue"
                      : "Forward"}
                </span>
              </button>
            </footer>
          </section>
        </div>
      ) : null}

	      {removeWorkspaceTarget ? (
	        <div className="modal-backdrop" role="presentation">
	          <section className="modal confirm-modal" role="dialog" aria-modal="true" aria-labelledby="remove-workspace-title">
	            <header className="modal-header">
	              <div>
	                <p className="eyebrow">Remove Project</p>
	                <h3 id="remove-workspace-title">从 Waypoint 移除项目</h3>
	              </div>
	              <button
	                className="icon-only"
	                type="button"
	                onClick={() => setRemoveWorkspaceTarget(null)}
	                title="Close"
	              >
	                <X aria-hidden="true" size={16} />
	              </button>
	            </header>
	            <div className="modal-body">
	              <p className="confirm-copy">
	                确认从 Waypoint 移除「{removeWorkspaceTarget.name}」吗？
	              </p>
	              <p className="confirm-copy">
	                这只会把项目从 Waypoint 侧边栏移除，不会删除本地目录，也不会删除任何会话记录。
	              </p>
	              <p className="confirm-meta">{removeWorkspaceTarget.path}</p>
	            </div>
	            <footer className="modal-footer">
	              <button
	                className="icon-action"
	                type="button"
	                onClick={() => setRemoveWorkspaceTarget(null)}
	              >
	                Cancel
	              </button>
	              <button
	                className="danger-action"
	                type="button"
	                onClick={confirmRemoveWorkspace}
	              >
	                <Trash2 aria-hidden="true" size={15} />
	                <span>移除</span>
	              </button>
	            </footer>
	          </section>
	        </div>
	      ) : null}

	      {pendingDeleteSession ? (
	        <div className="modal-backdrop" role="presentation">
	          <section className="modal confirm-modal" role="dialog" aria-modal="true" aria-labelledby="delete-session-title">
	            <header className="modal-header">
	              <div>
	                <p className="eyebrow">Delete History</p>
	                <h3 id="delete-session-title">删除本地会话历史</h3>
	              </div>
	              <button
	                className="icon-only"
	                type="button"
	                onClick={() => setDeleteSessionId(null)}
	                disabled={isDeletingSession}
	                title="Close"
	              >
	                <X aria-hidden="true" size={16} />
	              </button>
	            </header>
	            <div className="modal-body">
	              <p className="confirm-copy">
	                确认删除「{sessionDisplayTitle(pendingDeleteSession)}」的本地历史记录吗？此操作不可恢复。
	              </p>
	              <p className="confirm-meta">
	                {pendingDeleteSession.agentName} · {formatSessionTime(pendingDeleteSession.createdAt)}
	              </p>
	            </div>
	            <footer className="modal-footer">
	              <button
	                className="icon-action"
	                type="button"
	                onClick={() => setDeleteSessionId(null)}
	                disabled={isDeletingSession}
	              >
	                Cancel
	              </button>
	              <button
	                className="danger-action"
	                type="button"
	                onClick={confirmDeleteSession}
	                disabled={isDeletingSession}
	              >
	                <Trash2 aria-hidden="true" size={15} />
	                <span>{isDeletingSession ? "Deleting" : "Delete"}</span>
	              </button>
	            </footer>
	          </section>
	        </div>
	      ) : null}
	    </main>
	  );
	}

export default App;
