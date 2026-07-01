import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import {
  Bot,
  Check,
  ChevronDown,
  ChevronRight,
  Copy,
  Code2,
  ExternalLink,
  Eye,
  FileText,
  FilePlus,
  Folder,
  FolderPlus,
  MoreHorizontal,
  MessageSquare,
  PanelRightClose,
  PanelRightOpen,
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
  createHandoverFile,
  defaultWorkspace,
  deleteSession,
  detectEditors,
  getHandoverDraft,
  getHandoverPreview,
  killSession,
  isTauriRuntime,
  listAgentPresets,
  listSessions,
  openInEditor,
  previewFile,
  selectDirectory,
  selectFile,
} from "./api/tauri";
import type {
  AgentPresetInfo,
  FilePreview,
  HandoverContentMode,
  HandoverDraft,
  HandoverFileResult,
  HandoverPreview,
  HandoverResult,
  SessionInfo,
  WorkspaceFolder,
} from "./types";
import type { EditorInfo } from "./api/tauri";

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

function formatFileSize(bytes: number) {
  if (bytes >= 1024 * 1024) {
    return `${(bytes / 1024 / 1024).toFixed(bytes >= 10 * 1024 * 1024 ? 0 : 1)} MB`;
  }
  if (bytes >= 1024) {
    return `${(bytes / 1024).toFixed(bytes >= 10 * 1024 ? 0 : 1)} KB`;
  }
  return `${bytes} B`;
}

function formatPreviewTime(timestamp?: number | null) {
  if (!timestamp) {
    return "unknown";
  }
  return new Date(timestamp * 1000).toLocaleString();
}

function isMarkdownPreview(file: FilePreview | null) {
  return file?.kind === "text" && (file.extension === "md" || file.extension === "markdown");
}

function isImagePreview(file: FilePreview | null) {
  return file?.kind === "image" && Boolean(file.dataUrl);
}

const DANGEROUS_FLAGS: Record<string, string> = {
  "claude-code": "--dangerously-skip-permissions",
  codex: "--dangerously-bypass-approvals-and-sandbox",
};

function supportsDangerousFlag(agentId: string): boolean {
  return agentId in DANGEROUS_FLAGS;
}

function dangerousFlagLabel(agentId: string): string {
  return DANGEROUS_FLAGS[agentId] ?? "";
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

type SessionTreeNode = {
  session: SessionInfo;
  children: SessionTreeNode[];
};

function buildSessionForest(folderSessions: SessionInfo[]): SessionTreeNode[] {
  const sorted = [...folderSessions].sort((a, b) => a.createdAt - b.createdAt);
  const nodes = new Map<string, SessionTreeNode>();
  sorted.forEach((session) => {
    nodes.set(session.id, { session, children: [] });
  });

  const roots: SessionTreeNode[] = [];
  sorted.forEach((session) => {
    const node = nodes.get(session.id);
    if (!node) {
      return;
    }
    const parentId = session.parentSessionId?.trim();
    const parent = parentId && parentId !== session.id ? nodes.get(parentId) : null;
    if (parent) {
      parent.children.push(node);
    } else {
      roots.push(node);
    }
  });

  const sortNodes = (items: SessionTreeNode[]) => {
    items.sort((a, b) => a.session.createdAt - b.session.createdAt);
    items.forEach((item) => sortNodes(item.children));
  };
  sortNodes(roots);
  return roots;
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

const STORAGE_KEY_PREFIX = import.meta.env.DEV ? "waypoint-dev:" : "waypoint:";
function storageKey(name: string) {
  return `${STORAGE_KEY_PREFIX}${name}`;
}
const HIDDEN_WORKSPACE_STORAGE_KEY = storageKey("hidden_workspace_paths");
const PINNED_ITEMS_STORAGE_KEY = storageKey("pinned_items");
const NEW_CONVERSATION_WORKSPACE_HISTORY_STORAGE_KEY = storageKey("new_conversation_workspace_history");
const COLLAPSED_WORKSPACE_STORAGE_KEY = storageKey("collapsed_workspace_paths");
const COLLAPSED_SESSIONS_STORAGE_KEY = storageKey("collapsed_session_ids");
const PINNED_WORKSPACES_STORAGE_KEY = storageKey("pinned_workspaces");
const SELECTED_EDITOR_STORAGE_KEY = storageKey("selected_editor_id");
const SIDEBAR_WIDTH_STORAGE_KEY = storageKey("sidebar-width");
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

  return (
    <button
      className={`icon-only topbar-icon-action open-in-editor-btn open-in-editor-btn--${btnState}`}
      type="button"
      onClick={handleClick}
      disabled={btnState === "opening"}
      title={`在 ${editor.name} 中打开: ${cwd}`}
      aria-label={`在 ${editor.name} 中打开: ${cwd}`}
    >
      <EditorIcon editorId={editor.id} />
    </button>
  );
}

function OpenInEditorButton({ cwd, editors }: { cwd: string; editors: EditorInfo[] }) {
  if (editors.length === 0) return null;

  const [selectedEditorId, setSelectedEditorId] = useState<string>(() => {
    const saved = localStorage.getItem(SELECTED_EDITOR_STORAGE_KEY);
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

  return (
    <div className="custom-editor-dropdown" onClick={(e) => e.stopPropagation()}>
      <button
        className={`icon-only topbar-icon-action open-in-editor-btn open-in-editor-btn--main open-in-editor-btn--${btnState}`}
        type="button"
        onClick={handleOpenClick}
        disabled={btnState === "opening"}
        title={`在 ${selectedEditor.name} 中打开: ${cwd}`}
        aria-label={`在 ${selectedEditor.name} 中打开: ${cwd}`}
      >
        <EditorIcon editorId={selectedEditor.id} />
      </button>

      <button
        className="icon-only editor-dropdown-toggle"
        type="button"
        onClick={() => setIsOpen(!isOpen)}
        title="选择其他编辑器"
        aria-label="选择其他编辑器"
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
                localStorage.setItem(SELECTED_EDITOR_STORAGE_KEY, editor.id);
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
  const [continueAgentId, setContinueAgentId] = useState("codex");
  const [continueWorkspacePath, setContinueWorkspacePath] = useState("");
  const [handoverNote, setHandoverNote] = useState("");
  const [handoverContentMode, setHandoverContentMode] = useState<HandoverContentMode>("recommended");
  const [handoverPreview, setHandoverPreview] = useState<HandoverPreview | null>(null);
  const [handoverDraft, setHandoverDraft] = useState<HandoverDraft | null>(null);
  const [handoverResult, setHandoverResult] = useState<HandoverResult | null>(null);
  const [handoverFileResult, setHandoverFileResult] = useState<HandoverFileResult | null>(null);
  const [handoverPromptEdit, setHandoverPromptEdit] = useState("");
  const [handoverPromptEdited, setHandoverPromptEdited] = useState(false);
  const [isHandoverPreviewLoading, setIsHandoverPreviewLoading] = useState(false);
  const [isHandoverDraftLoading, setIsHandoverDraftLoading] = useState(false);
  const [handoverDraftError, setHandoverDraftError] = useState<string | null>(null);
  const [isCreatingHandoverFile, setIsCreatingHandoverFile] = useState(false);
  const [copiedHandoverPath, setCopiedHandoverPath] = useState(false);
  const [copiedHandoverPrompt, setCopiedHandoverPrompt] = useState(false);
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
  const [newConversationDangerous, setNewConversationDangerous] = useState(false);
  const [hiddenWorkspacePaths, setHiddenWorkspacePaths] = useState<string[]>([]);
  const [collapsedWorkspacePaths, setCollapsedWorkspacePaths] = useState<string[]>([]);
  const [collapsedSessionIds, setCollapsedSessionIds] = useState<string[]>([]);
  const [pinnedItems, setPinnedItems] = useState<PinnedItem[]>([]);
  const [isSelectingWorkspaceDirectory, setIsSelectingWorkspaceDirectory] = useState(false);

  // New Workspace state variables
  const [pinnedWorkspaces, setPinnedWorkspaces] = useState<WorkspaceFolder[]>([]);
  const [detectedEditors, setDetectedEditors] = useState<EditorInfo[]>([]);
  const [activeNewMenuFolder, setActiveNewMenuFolder] = useState<string | null>(null);
  const [activeWorkspaceMenuFolder, setActiveWorkspaceMenuFolder] = useState<string | null>(null);
  const [quickLaunchDangerous, setQuickLaunchDangerous] = useState(false);
  const [filePreviewOpen, setFilePreviewOpen] = useState(false);
  const [filePreviewPathInput, setFilePreviewPathInput] = useState("");
  const [filePreview, setFilePreview] = useState<FilePreview | null>(null);
  const [filePreviewError, setFilePreviewError] = useState<string | null>(null);
  const [isFilePreviewLoading, setIsFilePreviewLoading] = useState(false);
  const [filePreviewMode, setFilePreviewMode] = useState<"formatted" | "raw">("formatted");

  // Sidebar resizer state & logic
  const [sidebarWidth, setSidebarWidth] = useState<number>(() => {
    const saved = localStorage.getItem(SIDEBAR_WIDTH_STORAGE_KEY);
    return saved ? parseInt(saved, 10) : 320;
  });
  const [isDragging, setIsDragging] = useState(false);

  const startResize = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    setIsDragging(true);
  }, []);

  useEffect(() => {
    if (!isDragging) return;

    const handleMouseMove = (e: MouseEvent) => {
      const newWidth = Math.max(200, Math.min(600, e.clientX));
      setSidebarWidth(newWidth);
      localStorage.setItem(SIDEBAR_WIDTH_STORAGE_KEY, String(newWidth));
    };

    const handleMouseUp = () => {
      setIsDragging(false);
    };

    document.addEventListener("mousemove", handleMouseMove);
    document.addEventListener("mouseup", handleMouseUp);

    return () => {
      document.removeEventListener("mousemove", handleMouseMove);
      document.removeEventListener("mouseup", handleMouseUp);
    };
  }, [isDragging]);
  const activeSession = useMemo(
    () => sessions.find((session) => session.id === activeSessionId) ?? null,
    [activeSessionId, sessions],
  );
  const newConversationAgent = useMemo(
    () => agents.find((agent) => agent.id === selectedAgentId) ?? null,
    [agents, selectedAgentId],
  );
  const noneWorkspaceSessionIdSet = useMemo(
    () => new Set(sessions.filter((session) => session.noneWorkspace).map((session) => session.id)),
    [sessions],
  );
  const hiddenWorkspacePathSet = useMemo(
    () => new Set(hiddenWorkspacePaths),
    [hiddenWorkspacePaths],
  );
  const collapsedWorkspacePathSet = useMemo(
    () => new Set(collapsedWorkspacePaths),
    [collapsedWorkspacePaths],
  );
  const collapsedSessionIdSet = useMemo(
    () => new Set(collapsedSessionIds),
    [collapsedSessionIds],
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
  const continueAgent = useMemo(
    () => agents.find((agent) => agent.id === continueAgentId) ?? null,
    [agents, continueAgentId],
  );
  const effectiveHandoverMode =
    handoverContentMode === "recommended"
      ? (handoverPreview?.recommendedMode ?? "full")
      : handoverContentMode;
  const shownHandoverPrompt = handoverResult?.prompt ?? handoverFileResult?.prompt ?? handoverDraft?.prompt ?? "";
  const shownHandoverMode = handoverResult?.handoverMode ?? handoverFileResult?.handoverMode ?? handoverDraft?.effectiveMode ?? effectiveHandoverMode;
  const shownHandoverPath = handoverResult?.handoverPath ?? handoverFileResult?.handoverPath ?? null;
  const shownEvidencePath = handoverResult?.evidencePath ?? handoverFileResult?.evidencePath ?? handoverDraft?.evidencePath ?? null;
  const activeHandoverFile =
    handoverFileResult?.sourceSession.id === activeSessionId ? handoverFileResult : null;
  const preferredEditor = useMemo(() => {
    const saved = localStorage.getItem(SELECTED_EDITOR_STORAGE_KEY);
    return (
      detectedEditors.find((editor) => editor.id === saved) ??
      detectedEditors.find((editor) => editor.id === "vscode") ??
      detectedEditors[0] ??
      null
    );
  }, [detectedEditors]);
  const handleTerminalPreviewFile = useCallback(
    (path: string) => {
      void loadFilePreview(path, activeSession?.cwd ?? null);
    },
    [activeSession?.cwd],
  );
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
  const sessionById = useMemo(
    () => new Map(sessions.map((session) => [session.id, session])),
    [sessions],
  );

  async function refreshSessions(nextActiveId?: string) {
    const nextSessions = await listSessions();
    setSessions(nextSessions);
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

  function toggleWorkspaceCollapsed(path: string) {
    setCollapsedWorkspacePaths((current) => {
      const next = current.includes(path)
        ? current.filter((item) => item !== path)
        : [...current, path];
      localStorage.setItem(COLLAPSED_WORKSPACE_STORAGE_KEY, JSON.stringify(next));
      return next;
    });
  }

  function toggleSessionCollapsed(sessionId: string) {
    setCollapsedSessionIds((current) => {
      const next = current.includes(sessionId)
        ? current.filter((item) => item !== sessionId)
        : [...current, sessionId];
      localStorage.setItem(COLLAPSED_SESSIONS_STORAGE_KEY, JSON.stringify(next));
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
    localStorage.setItem(PINNED_WORKSPACES_STORAGE_KEY, JSON.stringify(nextFolders));
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

  async function loadFilePreview(path: string, baseDir = activeSession?.cwd ?? null) {
    const normalizedPath = path.trim();
    if (!normalizedPath) {
      setFilePreviewError("请输入文件路径。");
      return;
    }
    setFilePreviewOpen(true);
    setFilePreviewPathInput(normalizedPath);
    setFilePreviewError(null);
    setIsFilePreviewLoading(true);
    try {
      const preview = await previewFile(normalizedPath, baseDir);
      setFilePreview(preview);
      setFilePreviewPathInput(preview.path);
      setFilePreviewMode(isMarkdownPreview(preview) || isImagePreview(preview) ? "formatted" : "raw");
    } catch (err) {
      setFilePreview(null);
      setFilePreviewError(String(err));
    } finally {
      setIsFilePreviewLoading(false);
    }
  }

  async function handleSelectPreviewFile() {
    setFilePreviewOpen(true);
    setFilePreviewError(null);
    try {
      const selected = await selectFile();
      if (selected) {
        await loadFilePreview(selected, null);
      }
    } catch (err) {
      setFilePreviewError(`选择文件失败：${err instanceof Error ? err.message : String(err)}`);
    }
  }

  async function handleOpenPreviewInEditor() {
    if (!filePreview?.path || !preferredEditor) {
      return;
    }
    setError(null);
    try {
      await openInEditor(filePreview.path, preferredEditor.bin);
    } catch (err) {
      setError(String(err));
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
    localStorage.setItem(PINNED_WORKSPACES_STORAGE_KEY, JSON.stringify(nextFolders));
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

  // Create session for a specific agent and path
  async function handleCreateSessionForPath(agentId: string, path: string, dangerous = false) {
    setError(null);
    setIsLaunching(true);
    setActiveNewMenuFolder(null);
    try {
      const normalizedPath = normalizeWorkspacePath(path);
      if (!normalizedPath) {
        setError("目录路径无效。");
        return;
      }
      const session = await createAgentSession(agentId, normalizedPath, dangerous);
      rememberNewConversationWorkspace(session.cwd);
      revealWorkspacePath(session.cwd);
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
    setNewConversationDangerous(false);
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
      const session = await createAgentSession(
        selectedAgentId,
        launchPath,
        newConversationDangerous,
        useNoneWorkspace,
      );
      if (!useNoneWorkspace) {
        rememberNewConversationWorkspace(session.cwd);
        revealWorkspacePath(session.cwd);
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
      const nextSession = await createAgentSession(
        source.agentId,
        normalizeWorkspacePath(source.cwd),
        source.dangerous,
        Boolean(source.noneWorkspace),
      );
      if (!source.noneWorkspace) {
        revealWorkspacePath(nextSession.cwd);
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
    setHandoverPromptEdit("");
    setHandoverPromptEdited(false);
    if (activeSessionId) {
      setIsHandoverPreviewLoading(true);
      setHandoverPreview(null);
      void getHandoverPreview(activeSessionId)
        .then((preview) => setHandoverPreview(preview))
        .catch((err) => setError(String(err)))
        .finally(() => setIsHandoverPreviewLoading(false));
    }
    const firstAvailableAgent =
      agents.find((agent) => agent.id !== activeSession?.agentId && agent.available)?.id ??
      agents.find((agent) => agent.available)?.id ??
      "claude-code";
    setContinueAgentId(firstAvailableAgent);
    setContinueWorkspacePath(activeSession?.cwd ?? workspacePath);
    setHandoverMode("new");
    setHandoverContentMode("recommended");
    setHandoverOpen(true);
  }

  function closeHandover() {
    setHandoverOpen(false);
    setHandoverResult(null);
    setHandoverFileResult(null);
    setHandoverDraft(null);
    setHandoverDraftError(null);
    setHandoverPromptEdit("");
    setHandoverPromptEdited(false);
  }

  async function copyTextToClipboard(value: string) {
    if (isTauriRuntime()) {
      const { writeText } = await import("@tauri-apps/plugin-clipboard-manager");
      await writeText(value);
      return;
    }

    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(value);
      return;
    }

    const textarea = document.createElement("textarea");
    textarea.value = value;
    textarea.setAttribute("readonly", "true");
    textarea.style.position = "fixed";
    textarea.style.left = "-9999px";
    document.body.appendChild(textarea);
    textarea.select();
    const copied = document.execCommand("copy");
    document.body.removeChild(textarea);
    if (!copied) {
      throw new Error("Clipboard copy failed");
    }
  }

  async function handleCreateHandoverFile() {
    if (!activeSessionId || isCreatingHandoverFile) return;
    setError(null);
    setCopiedHandoverPath(false);
    setIsCreatingHandoverFile(true);
    try {
      const result = await createHandoverFile(activeSessionId, "", "recommended");
      setHandoverFileResult(result);
    } catch (err) {
      setError(String(err));
    } finally {
      setIsCreatingHandoverFile(false);
    }
  }

  async function handleCopyHandoverPath() {
    if (!activeHandoverFile?.handoverPath) return;
    setError(null);
    try {
      await copyTextToClipboard(activeHandoverFile.handoverPath);
      setCopiedHandoverPath(true);
      window.setTimeout(() => setCopiedHandoverPath(false), 1600);
    } catch (err) {
      setError(String(err));
    }
  }

  async function handleContinue() {
    if (!activeSessionId) return;
    if (handoverMode === "new" && (!continueAgentId || !continueWorkspacePath.trim())) return;
    if (handoverPromptEdited && !handoverPromptEdit.trim()) {
      setError("Handover Markdown 不能为空。");
      return;
    }
    setError(null);
    setHandoverResult(null);
    setIsForwarding(true);
    try {
      const editedPrompt = handoverPromptEdited ? handoverPromptEdit : "";
      if (handoverMode === "existing") {
        const fileResult = await createHandoverFile(
          activeSessionId,
          handoverNote,
          handoverContentMode,
        );
        const promptToCopy = editedPrompt || `A handover context file is referenced at ${fileResult.handoverPath}. Read only this exact file, acknowledge context loaded, then wait for my next instruction.`;
        await copyTextToClipboard(promptToCopy);
        setCopiedHandoverPrompt(true);
        window.setTimeout(() => setCopiedHandoverPrompt(false), 1600);
        setHandoverFileResult(fileResult);
        setHandoverNote("");
        setHandoverDraft(null);
        setHandoverDraftError(null);
        setHandoverPromptEdit("");
        setHandoverPromptEdited(false);
        setIsHandoverDraftLoading(false);
      } else {
        const result = await continueSession(
          activeSessionId,
          continueAgentId,
          continueWorkspacePath.trim(),
          handoverNote,
          handoverContentMode,
          editedPrompt,
        );
        revealWorkspacePath(result.targetSession.cwd);
        setHandoverResult(result);
        setHandoverNote("");
        setHandoverDraft(null);
        setHandoverDraftError(null);
        setHandoverPromptEdit("");
        setHandoverPromptEdited(false);
        setIsHandoverDraftLoading(false);
        await refreshSessions(result.targetSession.id);
      }
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
        targetSessionId: null,
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
  ]);

  useEffect(() => {
    if (!handoverOpen) {
      return;
    }
    if (!handoverPromptEdited) {
      setHandoverPromptEdit(shownHandoverPrompt);
    }
  }, [handoverOpen, handoverPromptEdited, shownHandoverPrompt]);

  function handleSelectSession(session: SessionInfo) {
    if (session.id === activeSessionId) {
      if (session.status !== "running") {
        setActiveTerminalReloadKey((current) => current + 1);
      }
      return;
    }
    setError(null);
    setActiveSessionId(session.id);
  }

  function renderSessionItem(
    session: SessionInfo,
    options: {
      childCount?: number;
      isCollapsed?: boolean;
      onToggleCollapsed?: () => void;
      parentSession?: SessionInfo | null;
    } = {},
  ) {
    const pinned = isSessionPinned(session.id);
    const parentLabel = options.parentSession
      ? `from ${sessionDisplayTitle(options.parentSession)}`
      : null;
    const hasChildren = Boolean(options.childCount);
    const collapsedLabel = options.isCollapsed ? "展开层级会话" : "折叠层级会话";
    return (
      <div
        className={`workspace-session-item chat-history-item ${options.parentSession ? "linked-child" : ""} ${
          hasChildren ? "linked-parent" : ""
        } ${session.id === activeSessionId ? "active" : ""} ${
          pinned ? "pinned" : ""
        }`}
        key={`session-${session.id}`}
        onClick={() => handleSelectSession(session)}
        title={session.firstUserMessage ?? session.title}
      >
        <div className="session-info-left">
          {hasChildren ? (
            <button
              type="button"
              className="session-collapse-toggle"
              onClick={(event) => {
                event.stopPropagation();
                options.onToggleCollapsed?.();
              }}
              aria-label={`${collapsedLabel}：${sessionDisplayTitle(session)}`}
              aria-expanded={!options.isCollapsed}
              title={collapsedLabel}
            >
              {options.isCollapsed ? <ChevronRight size={14} /> : <ChevronDown size={14} />}
            </button>
          ) : (
            <span className="session-collapse-spacer" aria-hidden="true" />
          )}
          <span className={`status-dot ${session.status}`} />
          <span className="session-copy">
            <span className="session-label">{sessionDisplayTitle(session)}</span>
            <span className="session-subtitle">
              <span className={`session-agent-badge agent-${session.agentId}`}>{session.agentName}</span>
              <span className="session-meta-text">
                {formatSessionTime(session.createdAt)} · {sessionStateHint(session.status)}
                {parentLabel ? ` · ${parentLabel}` : ""}
              </span>
            </span>
          </span>
        </div>
        <div className="session-actions">
          {options.childCount ? (
            <span className="session-lineage-count">{options.childCount} next</span>
          ) : null}
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

  function renderSessionNode(node: SessionTreeNode, depth = 0, lineage = new Set<string>()): ReactNode {
    if (lineage.has(node.session.id)) {
      return null;
    }
    const nextLineage = new Set(lineage);
    nextLineage.add(node.session.id);
    const parentSession = node.session.parentSessionId
      ? sessionById.get(node.session.parentSessionId) ?? null
      : null;
    const isCollapsed = collapsedSessionIdSet.has(node.session.id);

    return (
      <div className="session-tree-node" data-depth={depth} key={node.session.id}>
        {renderSessionItem(node.session, {
          childCount: node.children.length,
          isCollapsed,
          onToggleCollapsed: () => toggleSessionCollapsed(node.session.id),
          parentSession,
        })}
        {node.children.length > 0 && !isCollapsed ? (
          <div className="session-tree-children">
            {node.children.map((child) => renderSessionNode(child, depth + 1, nextLineage))}
          </div>
        ) : null}
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
        const saved = localStorage.getItem(PINNED_WORKSPACES_STORAGE_KEY);
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
              localStorage.setItem(PINNED_WORKSPACES_STORAGE_KEY, JSON.stringify(loadedPinnedWorkspaces));
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
          localStorage.setItem(PINNED_WORKSPACES_STORAGE_KEY, JSON.stringify(loadedPinnedWorkspaces));
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

        const savedCollapsedWorkspaces = localStorage.getItem(COLLAPSED_WORKSPACE_STORAGE_KEY);
        if (savedCollapsedWorkspaces) {
          try {
            const parsed = JSON.parse(savedCollapsedWorkspaces);
            if (Array.isArray(parsed)) {
              const normalized = Array.from(
                new Set(
                  parsed
                    .filter((item): item is string => typeof item === "string")
                    .map((item) => normalizeWorkspacePath(item))
                    .filter(Boolean),
                ),
              );
              setCollapsedWorkspacePaths(normalized);
              localStorage.setItem(COLLAPSED_WORKSPACE_STORAGE_KEY, JSON.stringify(normalized));
            }
          } catch (e) {
            console.error("[Waypoint] Failed to parse collapsed workspaces:", e);
          }
        }

        const savedCollapsedSessions = localStorage.getItem(COLLAPSED_SESSIONS_STORAGE_KEY);
        if (savedCollapsedSessions) {
          try {
            const parsed = JSON.parse(savedCollapsedSessions);
            if (Array.isArray(parsed)) {
              const normalized = Array.from(
                new Set(
                  parsed
                    .filter((item): item is string => typeof item === "string")
                    .map((item) => item.trim())
                    .filter(Boolean),
                ),
              );
              setCollapsedSessionIds(normalized);
              localStorage.setItem(COLLAPSED_SESSIONS_STORAGE_KEY, JSON.stringify(normalized));
            }
          } catch (e) {
            console.error("[Waypoint] Failed to parse collapsed sessions:", e);
          }
        }

        if (!isTauriRuntime()) {
          return;
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
    <main
      className={`app-shell ${isDragging ? "is-resizing" : ""}`}
      style={{ "--sidebar-width": `${sidebarWidth}px` } as React.CSSProperties}
    >
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
            {(() => {
              const noneCollapsed = collapsedWorkspacePathSet.has(NONE_WORKSPACE_VALUE);
              return (
                <div className={`workspace-folder-node none-workspace-node ${noneCollapsed ? "collapsed" : ""}`}>
                  <div
                    className="workspace-folder-header"
                    role="button"
                    tabIndex={0}
                    onClick={() => toggleWorkspaceCollapsed(NONE_WORKSPACE_VALUE)}
                    onKeyDown={(event) => {
                      if (event.key === "Enter" || event.key === " ") {
                        event.preventDefault();
                        toggleWorkspaceCollapsed(NONE_WORKSPACE_VALUE);
                      }
                    }}
                    aria-expanded={!noneCollapsed}
                  >
                    <div className="workspace-folder-info" title="none">
                      {noneCollapsed ? <ChevronRight size={13} /> : <ChevronDown size={13} />}
                      <Folder size={14} className="folder-icon" />
                      <span className="folder-name">无工作区会话</span>
                    </div>
                    <div className="workspace-folder-actions" onClick={(event) => event.stopPropagation()}>
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
                  {!noneCollapsed ? (
                    <div className="workspace-sessions-list">
                      {(() => {
                        const tree = buildSessionForest(noneWorkspaceSessions);
                        return tree.length > 0 ? (
                          tree.map((node) => renderSessionNode(node))
                        ) : (
                          <div className="no-sessions">暂无会话</div>
                        );
                      })()}
                    </div>
                  ) : null}
                </div>
              );
            })()}

            {workspacesWithSessions.map(({ folder, sessions: folderSessions }) => {
              const collapsed = collapsedWorkspacePathSet.has(folder.path);
              return (
                <div className={`workspace-folder-node ${collapsed ? "collapsed" : ""}`} key={folder.path}>
                  <div
                    className="workspace-folder-header"
                    role="button"
                    tabIndex={0}
                    onClick={() => toggleWorkspaceCollapsed(folder.path)}
                    onKeyDown={(event) => {
                      if (event.key === "Enter" || event.key === " ") {
                        event.preventDefault();
                        toggleWorkspaceCollapsed(folder.path);
                      }
                    }}
                    aria-expanded={!collapsed}
                  >
                    <div className="workspace-folder-info" title={folder.path}>
                      {collapsed ? <ChevronRight size={13} /> : <ChevronDown size={13} />}
                      <Folder size={14} className="folder-icon" />
                      <span className="folder-name">{folder.name}</span>
                    </div>
                    <div className="workspace-folder-actions" onClick={(event) => event.stopPropagation()}>
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
                                  onClick={() =>
                                    handleCreateSessionForPath(
                                      agent.id,
                                      folder.path,
                                      quickLaunchDangerous && supportsDangerousFlag(agent.id),
                                    )
                                  }
                                >
                                  <Bot size={13} />
                                  <div className="agent-option-text">
                                    <span className="agent-option-name">
                                      {agent.name}
                                      {quickLaunchDangerous && supportsDangerousFlag(agent.id) ? (
                                        <span className="agent-option-badge">dangerous</span>
                                      ) : null}
                                    </span>
                                    <span className="agent-option-desc">{agent.description}</span>
                                  </div>
                                </button>
                              ))}
                          </div>
                          {agents.some((a) => a.available && supportsDangerousFlag(a.id)) ? (
                            <div className="popover-footer">
                              <label className="checkbox-label">
                                <input
                                  type="checkbox"
                                  checked={quickLaunchDangerous}
                                  onChange={(event) => setQuickLaunchDangerous(event.target.checked)}
                                />
                                <span>
                                  跳过权限确认
                                  <span className="checkbox-hint">仅对支持的 Agent 生效</span>
                                </span>
                              </label>
                            </div>
                          ) : null}
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

                  {!collapsed ? (
                    <div className="workspace-sessions-list">
                      {(() => {
                        const tree = buildSessionForest(folderSessions);

                        if (tree.length === 0) {
                          return <div className="no-sessions">暂无会话</div>;
                        }

                        return tree.map((node) => renderSessionNode(node));
                      })()}
                    </div>
                  ) : null}
                </div>
              );
            })}
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

      <div
        className={`sidebar-resizer ${isDragging ? "is-dragging" : ""}`}
        onMouseDown={startResize}
      />

      <section className="workspace">
        <header className="topbar">
          <div className="topbar-session">
            <p className="eyebrow">Active Session</p>
            <h2>{activeSession?.title ?? "No session"}</h2>
            {activeSession ? (
              <p className="session-path">{activeSession.command} · {activeSession.cwd}</p>
            ) : null}
            {activeHandoverFile ? (
              <div className="handover-file-result" title={activeHandoverFile.handoverPath}>
                <code>{activeHandoverFile.handoverPath}</code>
                <button
                  className="icon-only handover-copy-btn"
                  type="button"
                  onClick={handleCopyHandoverPath}
                  title="Copy absolute path"
                >
                  {copiedHandoverPath ? (
                    <Check aria-hidden="true" size={14} />
                  ) : (
                    <Copy aria-hidden="true" size={14} />
                  )}
                </button>
              </div>
            ) : null}
          </div>
          <div className="topbar-actions">
            <button
              className={`icon-only topbar-icon-action ${filePreviewOpen ? "active" : ""}`}
              type="button"
              onClick={() => setFilePreviewOpen((current) => !current)}
              title={filePreviewOpen ? "Hide file preview" : "Show file preview"}
              aria-label={filePreviewOpen ? "Hide file preview" : "Show file preview"}
            >
              {filePreviewOpen ? (
                <PanelRightClose aria-hidden="true" size={16} />
              ) : (
                <PanelRightOpen aria-hidden="true" size={16} />
              )}
            </button>
            <button
              className="icon-only topbar-icon-action"
              type="button"
              onClick={handleCreateHandoverFile}
              disabled={!activeSessionId || isCreatingHandoverFile}
              title={isCreatingHandoverFile ? "Creating handover file" : "Create handover file"}
              aria-label={isCreatingHandoverFile ? "Creating handover file" : "Create handover file"}
            >
              <FilePlus aria-hidden="true" size={16} />
            </button>
            <button
              className="icon-only topbar-icon-action"
              type="button"
              onClick={openHandover}
              disabled={!activeSessionId}
              title="Handover to another session"
              aria-label="Handover to another session"
            >
              <Send aria-hidden="true" size={16} />
            </button>
            {activeSession?.cwd && (
              <OpenInEditorButton cwd={activeSession.cwd} editors={detectedEditors} />
            )}
          </div>
        </header>

        {error ? (
          <div className="error-banner" role="alert">
            <span>{error}</span>
            <button
              type="button"
              className="error-banner-close"
              onClick={() => setError(null)}
              title="Close"
              aria-label="Close error"
            >
              <X aria-hidden="true" size={14} />
            </button>
          </div>
        ) : null}

        <div className={`output-layout ${filePreviewOpen ? "with-file-preview" : ""}`}>
          <div className="terminal-frame">
            {activeSessionId ? (
              <TerminalView
                key={`${activeSessionId}:${activeTerminalReloadKey}`}
                sessionId={activeSessionId}
                cwd={activeSession?.cwd ?? workspacePath}
                onPreviewFile={handleTerminalPreviewFile}
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
          {filePreviewOpen ? (
            <aside className="file-preview-pane" aria-label="File preview">
              <header className="file-preview-header">
                <div>
                  <p className="eyebrow">File Preview</p>
                  <h3>{filePreview?.name || "Open local file"}</h3>
                </div>
                <button
                  className="icon-only"
                  type="button"
                  onClick={() => setFilePreviewOpen(false)}
                  title="Close file preview"
                  aria-label="Close file preview"
                >
                  <X aria-hidden="true" size={15} />
                </button>
              </header>

              <form
                className="file-preview-path-form"
                onSubmit={(event) => {
                  event.preventDefault();
                  void loadFilePreview(filePreviewPathInput);
                }}
              >
                <div className="input-group-with-btn">
                  <input
                    value={filePreviewPathInput}
                    onChange={(event) => setFilePreviewPathInput(event.target.value)}
                    placeholder="/path/to/file.md"
                    spellCheck={false}
                    aria-label="Local file path"
                  />
                  <button
                    type="button"
                    className="browse-dir-btn"
                    onClick={handleSelectPreviewFile}
                    title="选择文件"
                    aria-label="选择文件"
                  >
                    <FolderOpen size={14} />
                  </button>
                </div>
                <button className="primary-action" type="submit" disabled={isFilePreviewLoading}>
                  <FileText aria-hidden="true" size={15} />
                  <span>{isFilePreviewLoading ? "Opening" : "Open"}</span>
                </button>
              </form>

              {filePreview ? (
                <div className="file-preview-meta">
                  <code title={filePreview.path}>{filePreview.path}</code>
                  <div>
                    <span>{formatFileSize(filePreview.sizeBytes)}</span>
                    <span>{formatPreviewTime(filePreview.modifiedAt)}</span>
                    {filePreview.truncated ? <span>truncated</span> : null}
                  </div>
                </div>
              ) : null}

              <div className="file-preview-toolbar">
                <div className="file-preview-mode-toggle" role="group" aria-label="Preview mode">
                  <button
                    type="button"
                    className={filePreviewMode === "formatted" ? "active" : ""}
                    onClick={() => setFilePreviewMode("formatted")}
                    disabled={!isMarkdownPreview(filePreview) && !isImagePreview(filePreview)}
                    title={isImagePreview(filePreview) ? "Image preview" : "Formatted Markdown"}
                  >
                    <Eye aria-hidden="true" size={14} />
                    <span>{isImagePreview(filePreview) ? "Preview" : "Formatted"}</span>
                  </button>
                  <button
                    type="button"
                    className={filePreviewMode === "raw" ? "active" : ""}
                    onClick={() => setFilePreviewMode("raw")}
                    disabled={isImagePreview(filePreview)}
                    title="Raw text"
                  >
                    <Code2 aria-hidden="true" size={14} />
                    <span>Raw</span>
                  </button>
                </div>
                <button
                  className="icon-action"
                  type="button"
                  onClick={handleOpenPreviewInEditor}
                  disabled={!filePreview || !preferredEditor}
                  title={preferredEditor ? `Open in ${preferredEditor.name}` : "No supported editor detected"}
                >
                  <ExternalLink aria-hidden="true" size={14} />
                  <span>Editor</span>
                </button>
              </div>

              {filePreviewError ? (
                <div className="file-preview-error" role="alert">
                  {filePreviewError}
                </div>
              ) : null}

              <div className="file-preview-content">
                {isFilePreviewLoading ? (
                  <div className="file-preview-empty">Loading...</div>
                ) : filePreview ? (
                  isImagePreview(filePreview) ? (
                    <div className="image-preview">
                      <img src={filePreview.dataUrl ?? ""} alt={filePreview.name} />
                    </div>
                  ) : isMarkdownPreview(filePreview) && filePreviewMode === "formatted" ? (
                    <div className="markdown-preview">
                      <ReactMarkdown remarkPlugins={[remarkGfm]}>{filePreview.content}</ReactMarkdown>
                    </div>
                  ) : (
                    <pre className="file-preview-raw">{filePreview.content}</pre>
                  )
                ) : (
                  <div className="file-preview-empty">
                    Paste a local path or Command-click a path in the terminal.
                  </div>
                )}
              </div>
            </aside>
          ) : null}
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

              {supportsDangerousFlag(selectedAgentId) ? (
                <div className="checkbox-field">
                  <label htmlFor="new-conversation-dangerous" className="checkbox-label">
                    <input
                      id="new-conversation-dangerous"
                      type="checkbox"
                      checked={newConversationDangerous}
                      onChange={(event) => setNewConversationDangerous(event.target.checked)}
                    />
                    <span>
                      跳过权限确认（{dangerousFlagLabel(selectedAgentId)}）
                      <span className="checkbox-hint">
                        危险：将跳过该 Agent 的工具调用确认。请仅在可信工作区使用。
                      </span>
                    </span>
                  </label>
                </div>
              ) : null}

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
                    setHandoverFileResult(null);
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
                    setHandoverFileResult(null);
                    setHandoverMode("existing");
                  }}
                >
                  Copy Handover
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
                    <span>Inherited {formatHandoverChars(handoverPreview.inheritedContextChars)}</span>
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
                        setHandoverFileResult(null);
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
                        setHandoverFileResult(null);
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
                        setHandoverFileResult(null);
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
                <p className="handover-copy-hint">
                  Write handover file and copy prompt to clipboard. Paste into any session when ready.
                </p>
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
                      {handoverFileResult ? (
                        <small className="handover-copied-badge">
                          {copiedHandoverPrompt ? "Copied!" : "Prompt copied to clipboard"}
                        </small>
                      ) : (
                        <small>{shownHandoverMode}</small>
                      )}
                    </div>
                    {shownHandoverPath ? (
                      <div className="handover-path-row">
                        <code className="handover-path">
                          {handoverFileResult ? "Handover file: " : ""}
                          {shownHandoverPath}
                        </code>
                        {handoverFileResult ? (
                          <button
                            className="icon-action"
                            type="button"
                            onClick={handleCopyHandoverPath}
                            title="Copy file path"
                          >
                            {copiedHandoverPath ? "Copied!" : "Copy path"}
                          </button>
                        ) : null}
                      </div>
                    ) : null}
                    {handoverDraftError ? <div className="handover-preview-error">{handoverDraftError}</div> : null}
                    {!handoverFileResult ? (<>
                    <div className="handover-editor-row">
                      <span>Editable Markdown</span>
                      {handoverPromptEdited ? (
                        <button
                          className="icon-action"
                          type="button"
                          onClick={() => {
                            setHandoverPromptEdit(shownHandoverPrompt);
                            setHandoverPromptEdited(false);
                          }}
                        >
                          Reset to Draft
                        </button>
                      ) : null}
                    </div>
                    <textarea
                      className="handover-raw-markdown handover-edit-markdown"
                      value={handoverPromptEdit}
                      onChange={(event) => {
                        setHandoverPromptEdited(true);
                        setHandoverPromptEdit(event.target.value);
                      }}
                      placeholder="Select a valid target to render the raw handover Markdown."
                      spellCheck={false}
                    />
                    </>) : null}
                    {shownEvidencePath ? (
                      <small className="handover-evidence-path">Full evidence: {shownEvidencePath}</small>
                    ) : null}
                  </div>
                </aside>
              </div>
            </div>

            <footer className="modal-footer">
              <button className="icon-action" type="button" onClick={closeHandover}>
                {handoverResult || handoverFileResult ? "Done" : "Cancel"}
              </button>
              <button
                className="primary-action"
                type="button"
                onClick={(handoverResult || handoverFileResult) ? closeHandover : handleContinue}
                disabled={
                  (handoverResult || handoverFileResult) ? false :
                  isForwarding ||
                  (handoverMode === "new" && (!continueAgent?.available || !continueWorkspacePath.trim()))
                }
              >
                <Send aria-hidden="true" size={15} />
                <span>
                  {isForwarding
                    ? handoverMode === "new" ? "Creating" : "Copying"
                    : (handoverResult || handoverFileResult)
                      ? "Done"
                      : handoverMode === "new"
                        ? "Create & Continue"
                        : "Copy & Close"}
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
