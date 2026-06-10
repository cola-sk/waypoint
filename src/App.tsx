import { useEffect, useMemo, useState } from "react";
import {
  Bot,
  Folder,
  Play,
  RefreshCw,
  Send,
  Square,
  TerminalSquare,
  X,
} from "lucide-react";
import TerminalView from "./components/TerminalView";
import {
  continueSession,
  createAgentSession,
  defaultWorkspace,
  forwardSession,
  killSession,
  listAgentPresets,
  listSessions,
} from "./api/tauri";
import type { AgentPresetInfo, SessionInfo } from "./types";

function App() {
  const [sessions, setSessions] = useState<SessionInfo[]>([]);
  const [agents, setAgents] = useState<AgentPresetInfo[]>([]);
  const [activeSessionId, setActiveSessionId] = useState<string | null>(null);
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
  const activeSession = useMemo(
    () => sessions.find((session) => session.id === activeSessionId) ?? null,
    [activeSessionId, sessions],
  );
  const selectedAgent = useMemo(
    () => agents.find((agent) => agent.id === selectedAgentId) ?? null,
    [agents, selectedAgentId],
  );
  const handoverTargets = useMemo(
    () => sessions.filter((session) => session.id !== activeSessionId && session.status === "running"),
    [activeSessionId, sessions],
  );
  const continueAgent = useMemo(
    () => agents.find((agent) => agent.id === continueAgentId) ?? null,
    [agents, continueAgentId],
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
      setContinueAgentId(nextAgents[0]?.id ?? "shell");
    }
  }

  async function handleCreateSession() {
    setError(null);
    if (!selectedAgentId) {
      setError("Select an agent first.");
      return;
    }
    if (!workspacePath.trim()) {
      setError("Set a workspace directory first.");
      return;
    }
    setIsLaunching(true);
    try {
      const session = await createAgentSession(selectedAgentId, workspacePath.trim());
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

  function openHandover() {
    setError(null);
    const firstTarget = handoverTargets[0]?.id ?? "";
    const firstAvailableAgent =
      agents.find((agent) => agent.id !== activeSession?.agentId && agent.available)?.id ??
      agents.find((agent) => agent.available)?.id ??
      "shell";
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
      setHandoverOpen(false);
      setHandoverNote("");
      await refreshSessions(result.targetSession.id);
    } catch (err) {
      setError(String(err));
    } finally {
      setIsForwarding(false);
    }
  }

  useEffect(() => {
    Promise.all([refreshSessions(), refreshAgents(), defaultWorkspace()])
      .then(([, , cwd]) => {
        setWorkspacePath(cwd);
      })
      .catch((err) => setError(String(err)));
  }, []);

  return (
    <main className="app-shell">
      <aside className="sidebar">
        <div className="brand">
          <TerminalSquare aria-hidden="true" size={22} />
          <div>
            <h1>waypoint</h1>
            <span>MVP-2</span>
          </div>
        </div>

        <section className="launcher" aria-label="Create session">
          <div className="field">
            <label htmlFor="agent-select">
              <Bot aria-hidden="true" size={14} />
              <span>Agent</span>
            </label>
            <select
              id="agent-select"
              value={selectedAgentId}
              onChange={(event) => setSelectedAgentId(event.target.value)}
            >
              {agents.map((agent) => (
                <option key={agent.id} value={agent.id}>
                  {agent.name}
                  {agent.available ? "" : " (missing)"}
                </option>
              ))}
            </select>
          </div>

          <div className="agent-status">
            <span className={`status-dot ${selectedAgent?.available ? "running" : "error"}`} />
            <span>{selectedAgent?.resolvedCommand ?? selectedAgent?.command ?? "Detecting..."}</span>
          </div>

          <div className="field">
            <label htmlFor="workspace-path">
              <Folder aria-hidden="true" size={14} />
              <span>Workspace</span>
            </label>
            <input
              id="workspace-path"
              value={workspacePath}
              onChange={(event) => setWorkspacePath(event.target.value)}
              placeholder="/path/to/project"
              spellCheck={false}
            />
          </div>

          <div className="launcher-actions">
            <button
              className="icon-action"
              type="button"
              onClick={() => refreshAgents().catch((err) => setError(String(err)))}
              title="Refresh agent detection"
            >
              <RefreshCw aria-hidden="true" size={15} />
              <span>Detect</span>
            </button>
            <button
              className="primary-action"
              type="button"
              onClick={handleCreateSession}
              disabled={!selectedAgent?.available || isLaunching}
            >
              <Play aria-hidden="true" size={15} />
              <span>{isLaunching ? "Starting" : "Start"}</span>
            </button>
          </div>
        </section>

        <section className="session-list" aria-label="Sessions">
          {sessions.map((session) => (
            <button
              className={`session-item ${session.id === activeSessionId ? "active" : ""}`}
              key={session.id}
              type="button"
              onClick={() => setActiveSessionId(session.id)}
            >
              <span className={`status-dot ${session.status}`} />
              <span className="session-title">{session.title}</span>
              <span className="session-command">{session.agentName} · {session.cwd}</span>
            </button>
          ))}
        </section>
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

        <div className="terminal-frame">
          {activeSessionId ? (
            <TerminalView key={activeSessionId} sessionId={activeSessionId} />
          ) : (
            <div className="empty-state">
              <div className="empty-state-inner">
                <TerminalSquare aria-hidden="true" size={28} />
                <p>Select an agent and workspace to start a session.</p>
              </div>
            </div>
          )}
        </div>
      </section>

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
                    <input
                      id="continue-workspace"
                      value={continueWorkspacePath}
                      onChange={(event) => setContinueWorkspacePath(event.target.value)}
                      placeholder="/path/to/project"
                      spellCheck={false}
                    />
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
    </main>
  );
}

export default App;
