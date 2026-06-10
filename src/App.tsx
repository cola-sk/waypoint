import { useEffect, useMemo, useState } from "react";
import { Plus, Square, TerminalSquare } from "lucide-react";
import TerminalView from "./components/TerminalView";
import {
  createShellSession,
  killSession,
  listSessions,
} from "./api/tauri";
import type { SessionInfo } from "./types";

function App() {
  const [sessions, setSessions] = useState<SessionInfo[]>([]);
  const [activeSessionId, setActiveSessionId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const activeSession = useMemo(
    () => sessions.find((session) => session.id === activeSessionId) ?? null,
    [activeSessionId, sessions],
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

  async function handleNewShell() {
    setError(null);
    try {
      const session = await createShellSession();
      await refreshSessions(session.id);
    } catch (err) {
      setError(String(err));
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

  useEffect(() => {
    refreshSessions().catch((err) => setError(String(err)));
  }, []);

  return (
    <main className="app-shell">
      <aside className="sidebar">
        <div className="brand">
          <TerminalSquare aria-hidden="true" size={22} />
          <div>
            <h1>AgentRelay</h1>
            <span>MVP-1</span>
          </div>
        </div>

        <button className="primary-action" type="button" onClick={handleNewShell}>
          <Plus aria-hidden="true" size={16} />
          <span>New Shell</span>
        </button>

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
              <span className="session-command">{session.command}</span>
            </button>
          ))}
        </section>
      </aside>

      <section className="workspace">
        <header className="topbar">
          <div>
            <p className="eyebrow">Active Session</p>
            <h2>{activeSession?.title ?? "No session"}</h2>
          </div>
          <div className="topbar-actions">
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
              <button className="primary-action" type="button" onClick={handleNewShell}>
                <Plus aria-hidden="true" size={16} />
                <span>New Shell</span>
              </button>
            </div>
          )}
        </div>
      </section>
    </main>
  );
}

export default App;

