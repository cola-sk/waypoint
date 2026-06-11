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

function TerminalView({ sessionId, onSessionActivated, onActivationFailed }: TerminalViewProps) {
  const shellRef = useRef<HTMLDivElement | null>(null);
  const surfaceRef = useRef<HTMLDivElement | null>(null);
  const terminalRef = useRef<Terminal | null>(null);
  const fitAddonRef = useRef<FitAddon | null>(null);
  const [status, setStatus] = useState("connecting");

  useEffect(() => {
    const shell = shellRef.current;
    const surface = surfaceRef.current;
    if (!shell || !surface) return;

    let disposed = false;
    let unlisten: UnlistenFn | null = null;

    const terminal = new Terminal({
      cursorBlink: true,
      cursorStyle: "bar",
      fontFamily:
        'JetBrains Mono, ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", monospace',
      fontSize: 13,
      lineHeight: 1.25,
      convertEol: false,
      scrollback: 8000,
      theme: {
        background: "#11100e",
        foreground: "#eee8dc",
        cursor: "#ff6f4c",
        selectionBackground: "rgba(255, 111, 76, 0.24)",
        black: "#11100e",
        red: "#ff6b62",
        green: "#60d394",
        yellow: "#e9b44c",
        blue: "#7aa2f7",
        magenta: "#c7a0ff",
        cyan: "#68c6c1",
        white: "#eee8dc",
        brightBlack: "#6f6a60",
        brightRed: "#f87171",
        brightGreen: "#86efac",
        brightYellow: "#fbbf24",
        brightBlue: "#7dd3fc",
        brightMagenta: "#d8b4fe",
        brightCyan: "#99f6e4",
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
      reactivateSession(sessionId)
        .then((session) => {
          if (disposed) return;
          onSessionActivated?.(session);
          isLive = session.status === "running";
          isActivating = false;
          setStatus(isLive ? "attached" : "readonly");
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
          onActivationFailed?.(sessionId, String(err));
        });
    };

    const dataDisposable = terminal.onData((data) => {
      if (isReplaying) {
        return;
      }
      if (!isLive) {
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
        setTimeout(() => {
          if (!disposed) {
            terminal.focus();
          }
        }, 150);
      } catch (err) {
        setStatus("error");
        terminal.writeln(`[waypoint attach error] ${String(err)}`);
      }
    }

    connect();

    return () => {
      disposed = true;
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
      <div className="terminal-surface" ref={surfaceRef} />
    </div>
  );
}

export default TerminalView;
