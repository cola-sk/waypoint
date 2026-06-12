import { useEffect, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import {
  attachSession,
  detachSession,
  reactivateSession,
  resizeSession,
  writeSession,
} from "../api/tauri";
import type { PtyDataEvent, SessionInfo } from "../types";

type TerminalViewProps = {
  sessionId: string;
  onSessionActivated?: (session: SessionInfo) => void;
  onActivationFailed?: (sessionId: string, reason: string) => Promise<void> | void;
};

const MIN_ROWS = 5;
const MIN_COLS = 10;
const MAX_ROWS = 240;
const MAX_COLS = 600;

function clampDimension(value: number, min: number, max: number): number {
  return Math.max(min, Math.min(max, value));
}

function decodeBase64Bytes(base64: string): Uint8Array | null {
  if (!base64) {
    return null;
  }
  try {
    const binary = window.atob(base64);
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i += 1) {
      bytes[i] = binary.charCodeAt(i);
    }
    return bytes;
  } catch {
    return null;
  }
}

function isFocusOrMouseSequence(data: string): boolean {
  return (
    data === "\x1b[I" ||
    data === "\x1b[O" ||
    data.startsWith("\x1b[M") ||
    data.startsWith("\x1b[<")
  );
}

function TerminalView({ sessionId, onSessionActivated, onActivationFailed }: TerminalViewProps) {
  const shellRef = useRef<HTMLDivElement | null>(null);
  const surfaceRef = useRef<HTMLDivElement | null>(null);
  const terminalRef = useRef<Terminal | null>(null);
  const fitAddonRef = useRef<FitAddon | null>(null);
  const [status, setStatus] = useState("connecting");
  const [isRestoring, setIsRestoring] = useState(false);
  const activateAndQueueRef = useRef<((data: string) => void) | null>(null);

  useEffect(() => {
    const shell = shellRef.current;
    const surface = surfaceRef.current;
    if (!shell || !surface) return;

    let disposed = false;
    let unlisten: UnlistenFn | null = null;
    setIsRestoring(false);
    setStatus("connecting");

    const terminal = new Terminal({
      cursorBlink: true,
      cursorStyle: "bar",
      fontFamily:
        'JetBrains Mono, ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "PingFang SC", "Microsoft YaHei", "Liberation Mono", monospace',
      fontSize: 14.5,
      lineHeight: 1.52,
      letterSpacing: 0.16,
      fontWeight: 430,
      fontWeightBold: 700,
      minimumContrastRatio: 4.5,
      convertEol: false,
      scrollback: 12000,
      theme: {
        background: "#101318",
        foreground: "#edf2f7",
        cursor: "#ff6f4c",
        selectionBackground: "rgba(255, 111, 76, 0.24)",
        black: "#0f1117",
        red: "#ff7875",
        green: "#7bd88f",
        yellow: "#f2c572",
        blue: "#85b7ff",
        magenta: "#d2a8ff",
        cyan: "#7ed1d8",
        white: "#edf2f7",
        brightBlack: "#7f8a9a",
        brightRed: "#f87171",
        brightGreen: "#86efac",
        brightYellow: "#f7d67c",
        brightBlue: "#9dc4ff",
        brightMagenta: "#d8b4fe",
        brightCyan: "#a7edf1",
        brightWhite: "#ffffff",
      },
    });
    const fitAddon = new FitAddon();

    terminal.loadAddon(fitAddon);
    terminal.open(surface);
    terminal.focus();

    terminalRef.current = terminal;
    fitAddonRef.current = fitAddon;

    let lastWidth = 0;
    let lastHeight = 0;

    const fitAndResize = () => {
      if (disposed) return;
      if (shell.clientWidth < 100 || shell.clientHeight < 50) {
        return;
      }

      const width = shell.clientWidth;
      const height = shell.clientHeight;
      if (width === lastWidth && height === lastHeight) {
        return;
      }

      try {
        const dims = fitAddon.proposeDimensions();
        if (dims) {
          const rows = clampDimension(dims.rows, MIN_ROWS, MAX_ROWS);
          const cols = clampDimension(dims.cols, MIN_COLS, MAX_COLS);
          if (rows !== terminal.rows || cols !== terminal.cols) {
            terminal.resize(cols, rows);
            resizeSession(sessionId, rows, cols).catch(() => undefined);
          }
        }
        lastWidth = width;
        lastHeight = height;
      } catch (err) {
        console.warn("Failed to fit terminal:", err);
      }
    };
    let resizeTimeout = 0;
    const debouncedFitAndResize = () => {
      window.clearTimeout(resizeTimeout);
      resizeTimeout = window.setTimeout(fitAndResize, 100);
    };

    window.addEventListener("resize", debouncedFitAndResize);

    let isReplaying = true;
    let isLive = false;
    let isActivating = false;
    let wasReplayOnly = false;
    let shouldClearReplayOnActivation = false;
    let pendingInput = "";
    const queuedLiveOutput: Array<string | Uint8Array> = [];

    const writePtyPayload = (payload: string | Uint8Array) => {
      if (wasReplayOnly && isActivating && !isLive) {
        queuedLiveOutput.push(payload);
        return;
      }
      terminal.write(payload);
    };

    const switchReplayToLive = () => {
      if (!wasReplayOnly) {
        return;
      }
      if (shouldClearReplayOnActivation) {
        terminal.reset();
        terminal.clear();
        fitAndResize();
      }
      wasReplayOnly = false;
      queuedLiveOutput.splice(0).forEach((payload) => terminal.write(payload));
    };

    const flushPendingInput = () => {
      if (!pendingInput) {
        return;
      }
      const data = pendingInput;
      pendingInput = "";
      writeSession(sessionId, data).catch((err) => {
        terminal.writeln(`\r\n[waypoint write error] ${String(err)}`);
      });
    };

    const activateAndQueue = (data: string) => {
      pendingInput += data;
      if (isActivating) {
        return;
      }
      isActivating = true;
      setStatus("activating");
      setIsRestoring(true);
      reactivateSession(sessionId)
        .then((session) => {
          if (disposed) return;
          onSessionActivated?.(session);
          isLive = session.status === "running";
          isActivating = false;
          setStatus(isLive ? "attached" : "readonly");
          setIsRestoring(false);
          if (isLive) {
            switchReplayToLive();
            flushPendingInput();
          }
        })
        .catch((err) => {
          if (disposed) return;
          isActivating = false;
          pendingInput = "";
          setStatus("readonly");
          setIsRestoring(false);
          onActivationFailed?.(sessionId, String(err));
        });
    };

    activateAndQueueRef.current = activateAndQueue;

    const dataDisposable = terminal.onData((data) => {
      if (isReplaying) {
        return;
      }
      if (!isLive) {
        if (isFocusOrMouseSequence(data)) {
          return;
        }
        activateAndQueue(data);
        return;
      }
      writeSession(sessionId, data).catch((err) => {
        terminal.writeln(`\r\n[waypoint write error] ${String(err)}`);
      });
    });

    async function connect() {
      try {
        const snapshot = await attachSession(sessionId);
        if (disposed) return;
        isLive = snapshot.mode === "live" && snapshot.session.status === "running";
        wasReplayOnly = !isLive;
        shouldClearReplayOnActivation = snapshot.session.agentId === "claude-code";
        fitAndResize();
        const onWriteComplete = () => {
          isReplaying = false;
          if (!disposed) {
            setIsRestoring(false);
          }
        };
        const replayBytes = snapshot.replayBase64 ? decodeBase64Bytes(snapshot.replayBase64) : null;
        if (replayBytes) {
          terminal.write(replayBytes, onWriteComplete);
        } else {
          terminal.write(snapshot.replay, onWriteComplete);
        }
        unlisten = await listen<PtyDataEvent>("pty:data", (event) => {
          if (event.payload.sessionId === sessionId) {
            if (event.payload.dataBase64) {
              const bytes = decodeBase64Bytes(event.payload.dataBase64);
              if (bytes) {
                writePtyPayload(bytes);
              }
            } else if (event.payload.data) {
              writePtyPayload(event.payload.data);
            }
          }
        });
        setStatus(isLive ? "attached" : "readonly");
        if (!snapshot.replay && !snapshot.replayBase64) {
          setIsRestoring(false);
        }
        setTimeout(() => {
          if (!disposed) {
            terminal.focus();
          }
        }, 150);
      } catch (err) {
        setStatus("error");
        setIsRestoring(false);
        terminal.writeln(`[waypoint attach error] ${String(err)}`);
      }
    }

    connect();

    return () => {
      disposed = true;
      setIsRestoring(false);
      activateAndQueueRef.current = null;
      window.clearTimeout(resizeTimeout);
      detachSession(sessionId).catch(() => undefined);
      dataDisposable.dispose();
      window.removeEventListener("resize", debouncedFitAndResize);
      unlisten?.();
      terminal.dispose();
      terminalRef.current = null;
      fitAddonRef.current = null;
    };
  }, [sessionId]);

  const handleContainerClick = () => {
    terminalRef.current?.focus();
  };

  return (
    <div className="terminal-shell" data-status={status} onClick={handleContainerClick} ref={shellRef}>
      {status === "readonly" && !isRestoring && (
        <div className="resume-banner">
          <span className="banner-icon">💡</span>
          <span>
            当前为历史只读会话。
            <button
              type="button"
              className="resume-link-btn"
              onClick={(e) => {
                e.stopPropagation();
                activateAndQueueRef.current?.("");
              }}
            >
              点击此处
            </button>
            或在终端输入任意内容以恢复会话。
          </span>
        </div>
      )}
      <div className="terminal-surface" ref={surfaceRef} />
      {status === "connecting" ? (
        <div className="terminal-restore-overlay" role="status" aria-live="polite">
          <div className="terminal-restore-panel">
            <span className="terminal-restore-spinner" aria-hidden="true" />
            <div>
              <strong>正在加载会话</strong>
              <span>正在加载终端内容，请稍候...</span>
            </div>
          </div>
        </div>
      ) : null}
      {isRestoring && status !== "connecting" ? (
        <div className="terminal-restore-overlay" role="status" aria-live="polite">
          <div className="terminal-restore-panel">
            <span className="terminal-restore-spinner" aria-hidden="true" />
            <div>
              <strong>正在恢复会话</strong>
              <span>正在连接 Agent 原生历史，恢复完成后会继续显示会话内容。</span>
            </div>
          </div>
        </div>
      ) : null}
    </div>
  );
}

export default TerminalView;
