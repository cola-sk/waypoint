import { useEffect, useMemo, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  Bot,
  ChevronDown,
  ChevronRight,
  Folder,
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
  forwardSession,
  killSession,
  listAgentPresets,
  listSessions,
  selectDirectory,
} from "./api/tauri";
import type { AgentPresetInfo, SessionInfo, WorkspaceFolder } from "./types";

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

const NONE_WORKSPACE_STORAGE_KEY = "waypoint_none_workspace_session_ids";
const NONE_WORKSPACE_VALUE = "__none_workspace__";
const CUSTOM_WORKSPACE_VALUE = "__custom_workspace__";

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
  const [isForwarding, setIsForwarding] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [deleteSessionId, setDeleteSessionId] = useState<string | null>(null);
  const [isDeletingSession, setIsDeletingSession] = useState(false);
  const [newConversationOpen, setNewConversationOpen] = useState(false);
  const [newConversationWorkspaceValue, setNewConversationWorkspaceValue] =
    useState<string>(NONE_WORKSPACE_VALUE);
  const [newConversationCustomWorkspace, setNewConversationCustomWorkspace] = useState("");
  const [noneWorkspaceSessionIds, setNoneWorkspaceSessionIds] = useState<string[]>([]);

  // New Workspace state variables
  const [pinnedWorkspaces, setPinnedWorkspaces] = useState<WorkspaceFolder[]>([]);
  const [newWorkspaceInput, setNewWorkspaceInput] = useState("");
  const [isAddingWorkspace, setIsAddingWorkspace] = useState(false);
  const [activeNewMenuFolder, setActiveNewMenuFolder] = useState<string | null>(null);
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
  const pendingDeleteSession = useMemo(
    () => sessions.find((session) => session.id === deleteSessionId) ?? null,
    [deleteSessionId, sessions],
  );

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
  }, [noneWorkspaceSessionIdSet, pinnedWorkspaces, sessions]);

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

  // Handle adding workspace folder
  function handleAddWorkspace(path: string) {
    const trimmed = path.trim();
    if (!trimmed) return;
    if (pinnedWorkspaces.some((w) => w.path === trimmed)) {
      setError("该目录已存在于工作区中。");
      return;
    }
    const name = trimmed.split(/[/\\]/).pop() || trimmed;
    const nextFolders = [...pinnedWorkspaces, { path: trimmed, name, isPinned: true }];
    setPinnedWorkspaces(nextFolders);
    localStorage.setItem("waypoint_pinned_workspaces", JSON.stringify(nextFolders));
    setNewWorkspaceInput("");
    setIsAddingWorkspace(false);
  }

  // Handle removing workspace folder
  function handleRemoveWorkspace(path: string) {
    const nextFolders = pinnedWorkspaces.filter((w) => w.path !== path);
    setPinnedWorkspaces(nextFolders);
    localStorage.setItem("waypoint_pinned_workspaces", JSON.stringify(nextFolders));
  }

  // Update workspace agent history
  function updateWorkspaceAgentHistory(path: string, agentId: string, agentName: string) {
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
    
    if (!history[path] || !Array.isArray(history[path])) {
      history[path] = [];
    }
    
    if (!history[path].some(a => a.agentId === agentId)) {
      history[path].push({ agentId, agentName });
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
      const session = await createAgentSession(agentId, path);
      updateWorkspaceAgentHistory(session.cwd, session.agentId, session.agentName);
      await refreshSessions(session.id);
    } catch (err) {
      setError(String(err));
    } finally {
      setIsLaunching(false);
    }
  }

  function openNewConversationModal() {
    setError(null);
    setActiveNewMenuFolder(null);
    const firstAvailableAgent =
      agents.find((agent) => agent.available)?.id ?? agents[0]?.id ?? "claude-code";
    const defaultWorkspaceChoice = pinnedWorkspaces[0]?.path ?? workspacePath.trim();
    setSelectedAgentId(firstAvailableAgent);
    setNewConversationWorkspaceValue(defaultWorkspaceChoice || NONE_WORKSPACE_VALUE);
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
      if (useNoneWorkspace && !launchPath) {
        launchPath = (await defaultWorkspace()).trim();
      }
      if (!launchPath) {
        setError("无法解析可用目录，请先选择一个工作区目录。");
        return;
      }
      const session = await createAgentSession(selectedAgentId, launchPath);
      if (useNoneWorkspace) {
        markSessionAsNoneWorkspace(session.id);
      } else {
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

  async function handleKill() {
    if (!activeSessionId) return;
    setError(null);
    try {
      await killSession(activeSessionId);
      const nextSessions = await listSessions();
      setSessions(nextSessions);
      setActiveSessionId(nextSessions[0]?.id ?? null);
    } catch (err) {
      setError(String(err));
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
      const nextSession = await createAgentSession(source.agentId, source.cwd);
      if (noneWorkspaceSessionIdSet.has(source.id)) {
        markSessionAsNoneWorkspace(nextSession.id);
      } else {
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
    const firstTarget = handoverTargets[0]?.id ?? "";
    const firstAvailableAgent =
      agents.find((agent) => agent.id !== activeSession?.agentId && agent.available)?.id ??
      agents.find((agent) => agent.available)?.id ??
      "claude-code";
    setHandoverTargetId(firstTarget);
    setContinueAgentId(firstAvailableAgent);
    setContinueWorkspacePath(activeSession?.cwd ?? workspacePath);
    setHandoverMode("new");
    setHandoverOpen(true);
  }

  async function handleContinue() {
    if (!activeSessionId) return;
    if (handoverMode === "existing" && !handoverTargetId) return;
    if (handoverMode === "new" && (!continueAgentId || !continueWorkspacePath.trim())) return;
    setError(null);
    setIsForwarding(true);
    try {
      const result =
        handoverMode === "existing"
          ? await forwardSession(activeSessionId, handoverTargetId, handoverNote)
          : await continueSession(
              activeSessionId,
              continueAgentId,
              continueWorkspacePath.trim(),
              handoverNote,
            );
      updateWorkspaceAgentHistory(
        result.targetSession.cwd,
        result.targetSession.agentId,
        result.targetSession.agentName,
      );
      setHandoverOpen(false);
      setHandoverNote("");
      await refreshSessions(result.targetSession.id);
    } catch (err) {
      setError(String(err));
    } finally {
      setIsForwarding(false);
    }
  }

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
    return (
      <div
        className={`workspace-session-item chat-history-item ${session.id === activeSessionId ? "active" : ""}`}
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

  useEffect(() => {
    let unlistenExited: UnlistenFn | null = null;
    let unlistenError: UnlistenFn | null = null;
    let unlistenCreated: UnlistenFn | null = null;

    Promise.all([refreshSessions(), refreshAgents(), defaultWorkspace()])
      .then(async ([, , cwd]) => {
        setWorkspacePath(cwd);
        // Load pinned workspaces
        const saved = localStorage.getItem("waypoint_pinned_workspaces");
        if (saved) {
          try {
            const parsed = JSON.parse(saved);
            if (Array.isArray(parsed)) {
              setPinnedWorkspaces(parsed);
            }
          } catch (e) {
            console.error("[Waypoint] Failed to parse pinned workspaces:", e);
          }
        } else if (cwd) {
          const defaultFolder: WorkspaceFolder = {
            path: cwd,
            name: cwd.split(/[/\\]/).pop() || cwd,
            isPinned: true,
          };
          setPinnedWorkspaces([defaultFolder]);
          localStorage.setItem("waypoint_pinned_workspaces", JSON.stringify([defaultFolder]));
        }

        // Load workspace agent history
        const savedHistory = localStorage.getItem("waypoint_workspace_agent_history");
        if (savedHistory) {
          try {
            const parsed = JSON.parse(savedHistory);
            if (parsed && typeof parsed === "object") {
              setWorkspaceAgentHistory(parsed);
            }
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
        <div className="popover-backdrop" onClick={() => setActiveNewMenuFolder(null)} />
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

        <button className="new-conversation-trigger" type="button" onClick={openNewConversationModal}>
          <Plus size={14} />
          <span>新对话</span>
        </button>

        <section className="workspace-list" aria-label="Workspaces">
          <div className="section-header">
            <h3>工作区目录</h3>
            <button
              className="add-workspace-btn"
              onClick={() => setIsAddingWorkspace(!isAddingWorkspace)}
              title="固定新工作区目录"
            >
              <Plus size={14} />
            </button>
          </div>

          {isAddingWorkspace && (
            <form
              className="add-workspace-form"
              onSubmit={(e) => {
                e.preventDefault();
                handleAddWorkspace(newWorkspaceInput);
              }}
            >
              <div className="input-group-with-btn">
                <input
                  type="text"
                  placeholder="输入或选择本地路径..."
                  value={newWorkspaceInput}
                  onChange={(e) => setNewWorkspaceInput(e.target.value)}
                  autoFocus
                />
                <button
                  type="button"
                  className="browse-dir-btn"
                  onClick={async () => {
                    const selected = await selectDirectory();
                    if (selected) {
                      setNewWorkspaceInput(selected);
                    }
                  }}
                  title="浏览选择文件夹"
                >
                  <FolderOpen size={14} />
                </button>
              </div>
              <div className="form-actions">
                <button type="button" onClick={() => setIsAddingWorkspace(false)}>取消</button>
                <button type="submit">添加</button>
              </div>
            </form>
          )}

          <div className="workspace-tree">
            {noneWorkspaceSessions.length > 0 ? (
              <div className="workspace-folder-node none-workspace-node">
                <div className="workspace-folder-header">
                  <div className="workspace-folder-info" title="none">
                    <Folder size={14} className="folder-icon" />
                    <span className="folder-name">无工作区会话</span>
                    <span className="temp-badge">None</span>
                  </div>
                </div>
                <div className="workspace-sessions-list">
                  {noneWorkspaceSessions.map((session) => renderSessionItem(session))}
                </div>
              </div>
            ) : null}

            {workspacesWithSessions.map(({ folder, sessions: folderSessions }) => (
              <div className="workspace-folder-node" key={folder.path}>
                <div className="workspace-folder-header">
                  <div className="workspace-folder-info" title={folder.path}>
                    <Folder size={14} className="folder-icon" />
                    <span className="folder-name">{folder.name}</span>
                    {!folder.isPinned && <span className="temp-badge">临时</span>}
                  </div>
                  <div className="workspace-folder-actions">
                    <div className="popover-wrapper">
                      <button
                        type="button"
                        className="new-session-btn"
                        onClick={(e) => {
                          e.stopPropagation();
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

                    {folder.isPinned && (
                      <button
                        className="remove-folder-btn"
                        onClick={(e) => {
                          e.stopPropagation();
                          handleRemoveWorkspace(folder.path);
                        }}
                        title="取消固定目录"
                      >
                        <X size={12} />
                      </button>
                    )}
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
              <span>Continue</span>
            </button>
            <button
              className="icon-action"
              type="button"
              onClick={handleKill}
              disabled={!activeSessionId}
              title="Kill session"
            >
              <Square aria-hidden="true" size={15} />
              <span>Kill</span>
            </button>
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
                  onChange={(event) => setNewConversationWorkspaceValue(event.target.value)}
                >
                  <option value={NONE_WORKSPACE_VALUE}>None（不绑定工作区）</option>
                  {pinnedWorkspaces.map((folder) => (
                    <option key={folder.path} value={folder.path}>
                      {folder.name} · {folder.path}
                    </option>
                  ))}
                  {workspacePath.trim() &&
                  !pinnedWorkspaces.some((folder) => folder.path === workspacePath.trim()) ? (
                    <option value={workspacePath.trim()}>
                      默认目录 · {workspacePath.trim()}
                    </option>
                  ) : null}
                  <option value={CUSTOM_WORKSPACE_VALUE}>选择其他目录...</option>
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
                      onClick={async () => {
                        const selected = await selectDirectory();
                        if (selected) {
                          setNewConversationCustomWorkspace(selected);
                        }
                      }}
                      title="浏览选择文件夹"
                    >
                      <FolderOpen size={14} />
                    </button>
                  </div>
                </div>
              ) : null}

              {newConversationWorkspaceValue === NONE_WORKSPACE_VALUE ? (
                <div className="workspace-none-hint">
                  该会话将归类到「无工作区会话」，并使用默认目录启动 Agent。
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
	          <section className="modal" role="dialog" aria-modal="true" aria-labelledby="handover-title">
            <header className="modal-header">
              <div>
                <p className="eyebrow">Handover</p>
                <h3 id="handover-title">Continue from current session</h3>
              </div>
              <button
                className="icon-only"
                type="button"
                onClick={() => setHandoverOpen(false)}
                title="Close"
              >
                <X aria-hidden="true" size={16} />
              </button>
            </header>

            <div className="modal-body">
              <div className="handover-source">
                <span>From</span>
                <strong>{activeSession?.title}</strong>
                <small>{activeSession?.agentName} · {activeSession?.cwd}</small>
              </div>

              <div className="mode-toggle" role="group" aria-label="Handover mode">
                <button
                  className={handoverMode === "new" ? "active" : ""}
                  type="button"
                  onClick={() => setHandoverMode("new")}
                >
                  New Session
                </button>
                <button
                  className={handoverMode === "existing" ? "active" : ""}
                  type="button"
                  onClick={() => setHandoverMode("existing")}
                  disabled={handoverTargets.length === 0}
                >
                  Existing Session
                </button>
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
                      onChange={(event) => setContinueAgentId(event.target.value)}
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
                        onChange={(event) => setContinueWorkspacePath(event.target.value)}
                        placeholder="/path/to/project"
                        spellCheck={false}
                      />
                      <button
                        type="button"
                        className="browse-dir-btn"
                        onClick={async () => {
                          const selected = await selectDirectory();
                          if (selected) {
                            setContinueWorkspacePath(selected);
                          }
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
                    onChange={(event) => setHandoverTargetId(event.target.value)}
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
                  onChange={(event) => setHandoverNote(event.target.value)}
                  placeholder="Optional: tell the target agent what to focus on next."
                  rows={6}
                />
              </div>
            </div>

            <footer className="modal-footer">
              <button className="icon-action" type="button" onClick={() => setHandoverOpen(false)}>
                Cancel
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
