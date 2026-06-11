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

const MIN_ROWS = 5;
const MIN_COLS = 10;
const MAX_ROWS = 240;
const MAX_COLS = 600;

function clampDimension(value: number, min: number, max: number): number {
  return Math.max(min, Math.min(max, value));
}

function TerminalView({ sessionId }: TerminalViewProps) {
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
        'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", monospace',
      fontSize: 13,
      lineHeight: 1.25,
      convertEol: false,
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

    const dataDisposable = terminal.onData((data) => {
      writeSession(sessionId, data).catch((err) => {
        terminal.writeln(`\r\n[waypoint write error] ${String(err)}`);
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
