import { useEffect, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import {
  attachSession,
  detachSession,
  resizeSession,
  writeSession,
} from "../api/tauri";
import type { PtyDataEvent } from "../types";

type TerminalViewProps = {
  sessionId: string;
};

function TerminalView({ sessionId }: TerminalViewProps) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const terminalRef = useRef<Terminal | null>(null);
  const fitAddonRef = useRef<FitAddon | null>(null);
  const [status, setStatus] = useState("connecting");

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    let disposed = false;
    let unlisten: UnlistenFn | null = null;

    const terminal = new Terminal({
      cursorBlink: true,
      cursorStyle: "bar",
      fontFamily:
        'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", monospace',
      fontSize: 13,
      lineHeight: 1.25,
      convertEol: true,
      scrollback: 8000,
      theme: {
        background: "#111312",
        foreground: "#f3f1e8",
        cursor: "#5eead4",
        selectionBackground: "#2f4f4a",
        black: "#111312",
        red: "#ef4444",
        green: "#22c55e",
        yellow: "#f59e0b",
        blue: "#38bdf8",
        magenta: "#c084fc",
        cyan: "#5eead4",
        white: "#f3f1e8",
        brightBlack: "#77766f",
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
    terminal.open(container);
    terminal.focus();

    terminalRef.current = terminal;
    fitAddonRef.current = fitAddon;

    const fitAndResize = () => {
      if (disposed) return;
      fitAddon.fit();
      resizeSession(sessionId, terminal.rows, terminal.cols).catch(() => undefined);
    };

    const resizeObserver = new ResizeObserver(fitAndResize);
    resizeObserver.observe(container);

    const dataDisposable = terminal.onData((data) => {
      writeSession(sessionId, data).catch((err) => {
        terminal.writeln(`\r\n[AgentRelay write error] ${String(err)}`);
      });
    });

    async function connect() {
      try {
        const snapshot = await attachSession(sessionId);
        if (disposed) return;
        fitAndResize();
        terminal.write(snapshot.replay);
        unlisten = await listen<PtyDataEvent>("pty:data", (event) => {
          if (event.payload.sessionId === sessionId) {
            terminal.write(event.payload.data);
          }
        });
        setStatus("attached");
      } catch (err) {
        setStatus("error");
        terminal.writeln(`[AgentRelay attach error] ${String(err)}`);
      }
    }

    connect();

    return () => {
      disposed = true;
      detachSession(sessionId).catch(() => undefined);
      dataDisposable.dispose();
      resizeObserver.disconnect();
      unlisten?.();
      terminal.dispose();
      terminalRef.current = null;
      fitAddonRef.current = null;
    };
  }, [sessionId]);

  return (
    <div className="terminal-shell" data-status={status}>
      <div className="terminal-surface" ref={containerRef} />
    </div>
  );
}

export default TerminalView;

